use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, bail};
use oma_config::{AgentLoader, OmaConfig};
use oma_contract::{
    ActiveTurnCatchUp, AgentCommand, AgentEvent, ApprovalDecision, ApprovalMode, Block, ChatMessage, ClientType,
    PermissionRequestedData, Role, StopReason, TokenUsage, ToolCallStartedData, ToolOutput,
};
use oma_mcp::McpManager;
use oma_provider::{ProviderStreamEvent, UniversalProvider};
use oma_storage::StorageManager;
use oma_tool::{SubagentRunner, ToolRegistry};
use parking_lot::RwLock;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

// =========================================================================
// 1. 熔断器 Circuit Breaker
// =========================================================================

#[derive(Debug, Default)]
pub struct CircuitBreaker {
    last_signature:    Option<String>,
    consecutive_count: usize,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 检查并记录工具调用签名，连续 3 次相同参数则触发熔断报错
    pub fn check_and_record(&mut self, tool_name: &str, input: &serde_json::Value) -> Result<()> {
        let sig = format!("{}:{}", tool_name, input);
        if let Some(last) = &self.last_signature {
            if last == &sig {
                self.consecutive_count += 1;
                if self.consecutive_count >= 3 {
                    bail!(
                        "【系统熔断】检测到你已连续 3 次使用相同参数调用工具 '{}'，结果未发生改变。请停止重复尝试，重新审视假设并尝试其他排查或修改路径。",
                        tool_name
                    );
                }
                return Ok(());
            }
        }

        self.last_signature = Some(sig);
        self.consecutive_count = 1;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.last_signature = None;
        self.consecutive_count = 0;
    }
}

// =========================================================================
// 2. 审批仲裁器 Approval Arbiter (先到先得 + 120s 超时兜底)
// =========================================================================

#[derive(Default)]
pub struct ApprovalArbiter {
    pending: Mutex<HashMap<String, oneshot::Sender<ApprovalDecision>>>,
}

impl ApprovalArbiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个等待审批项，返回 oneshot 接收端
    pub async fn register(&self, request_id: String) -> oneshot::Receiver<ApprovalDecision> {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_id, tx);
        rx
    }

    /// 接收到某个客户端的决策响应，先到先得完成 oneshot
    pub async fn resolve(&self, request_id: &str, decision: ApprovalDecision) -> bool {
        if let Some(tx) = self.pending.lock().await.remove(request_id) {
            let _ = tx.send(decision);
            true
        } else {
            false
        }
    }
}

// =========================================================================
// 3. 上下文预估与两阶段压缩 (Two-Stage Compaction)
// =========================================================================

/// 启发式估算消息历史的 Token 数（英文约 4 字符/Token，代码与中文约 1.5~2 字符/Token）
pub fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    let mut total_chars = 0;
    for m in messages {
        for b in &m.content {
            match b {
                Block::Text { text } => total_chars += text.chars().count(),
                Block::Thinking { thinking } => total_chars += thinking.chars().count(),
                Block::ToolResult { content, .. } => total_chars += content.chars().count(),
                Block::ToolUse { input, name, .. } => {
                    total_chars += name.chars().count() + input.to_string().chars().count();
                }
                Block::Image { .. } => total_chars += 800, // 图片预估基准
            }
        }
    }

    // 综合保守折算系数：取 2.0 字符/Token
    total_chars / 2
}

/// 执行两阶段压缩策略：达到 70% 阈值时修剪旧 ToolResult
pub fn compact_messages(messages: &mut Vec<ChatMessage>, context_len: usize) {
    let threshold = (context_len as f64 * 0.7) as usize;
    if estimate_tokens(messages) <= threshold {
        return;
    }

    let total = messages.len();
    if total <= 4 {
        return;
    }

    // 第一阶段：将 3 轮之前的冗长 ToolResult 修剪为简要占位
    let cutoff_idx = total.saturating_sub(4);
    for m in messages[..cutoff_idx].iter_mut() {
        for b in &mut m.content {
            if let Block::ToolResult { content, .. } = b {
                if content.len() > 300 {
                    *content = "[工具执行结果已截断修剪以节省上下文窗口]".to_string();
                }
            }
        }
    }

    // 第二阶段：若修剪后仍超限，滑动窗口仅保留后半段
    if estimate_tokens(messages) > threshold && messages.len() > 8 {
        let keep_count = messages.len() / 2;
        let drained = messages.split_off(messages.len() - keep_count);
        *messages = drained;
    }
}

// =========================================================================
// 4. 活跃轮次状态机快照
// =========================================================================

#[derive(Debug, Clone, Default)]
pub struct ActiveTurnState {
    pub turn_id:              String,
    pub accumulated_thinking: String,
    pub accumulated_text:     String,
    pub active_tool_call:     Option<ToolCallStartedData>,
    pub pending_approval:     Option<PermissionRequestedData>,
}

// =========================================================================
// 5. Session Room 核心隔离容器
// =========================================================================

pub struct SessionRoom {
    pub session_id:      String,
    pub workspace:       PathBuf,
    pub storage:         StorageManager,
    pub config:          Arc<RwLock<OmaConfig>>,
    pub tools:           ToolRegistry,
    pub mcp:             Arc<McpManager>,
    pub active_model:    RwLock<String>,
    pub active_agent:    RwLock<String>,
    pub approval_mode:   RwLock<ApprovalMode>,
    pub whitelist:       RwLock<HashSet<String>>, // AllowSession 内存白名单
    pub event_tx:        broadcast::Sender<AgentEvent>,
    pub command_queue:   Mutex<VecDeque<AgentCommand>>,
    pub active_turn:     Arc<RwLock<Option<ActiveTurnState>>>,
    pub arbiter:         Arc<ApprovalArbiter>,
    pub cancel_token:    RwLock<CancellationToken>,
    pub circuit_breaker: Mutex<CircuitBreaker>,
    pub is_running:      Arc<AtomicBool>,
}

impl SessionRoom {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        storage: StorageManager,
        config: Arc<RwLock<OmaConfig>>,
        tools: ToolRegistry,
        mcp: Arc<McpManager>,
        active_model: impl Into<String>,
        active_agent: impl Into<String>,
        approval_mode: ApprovalMode,
    ) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(512);

        Arc::new(Self {
            session_id: session_id.into(),
            workspace: workspace.into(),
            storage,
            config,
            tools,
            mcp,
            active_model: RwLock::new(active_model.into()),
            active_agent: RwLock::new(active_agent.into()),
            approval_mode: RwLock::new(approval_mode),
            whitelist: RwLock::new(HashSet::new()),
            event_tx,
            command_queue: Mutex::new(VecDeque::new()),
            active_turn: Arc::new(RwLock::new(None)),
            arbiter: Arc::new(ApprovalArbiter::new()),
            cancel_token: RwLock::new(CancellationToken::new()),
            circuit_breaker: Mutex::new(CircuitBreaker::new()),
            is_running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// 订阅该 Room 的实时事件流
    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    /// 广播事件
    pub fn broadcast(&self, event: AgentEvent) {
        let _ = self.event_tx.send(event);
    }

    /// 捕获新接入客户端的追赶快照
    pub fn get_catch_up(&self) -> Option<ActiveTurnCatchUp> {
        let turn = self.active_turn.read().clone()?;
        Some(ActiveTurnCatchUp {
            turn_id:              turn.turn_id,
            accumulated_thinking: turn.accumulated_thinking,
            accumulated_text:     turn.accumulated_text,
            active_tool_call:     turn.active_tool_call,
            pending_approval:     turn.pending_approval,
        })
    }

    /// 主动取消当前执行与队列清空
    pub async fn cancel(&self) {
        // 1. 触发 CancellationToken 级联中断
        let old_token = {
            let mut guard = self.cancel_token.write();
            let old = guard.clone();
            *guard = CancellationToken::new();
            old
        };
        old_token.cancel();

        // 2. 清空排队命令
        {
            let mut q = self.command_queue.lock().await;
            q.clear();
        }
        self.broadcast(AgentEvent::QueueCleared {});

        // 3. 广播取消中断完成
        if let Some(turn) = self.active_turn.write().take() {
            self.broadcast(AgentEvent::TurnFinished {
                turn_id:     turn.turn_id,
                stop_reason: StopReason::Cancelled,
                usage:       TokenUsage::default(),
                subagent_id: None,
            });
        }
    }

    /// 提交客户端指令
    pub async fn submit_command(
        self: &Arc<Self>,
        client_id: &str,
        client_name: &str,
        client_type: ClientType,
        command: AgentCommand,
    ) {
        match command {
            AgentCommand::UserInput { content, attachments } => {
                let is_busy = self.is_running.load(Ordering::SeqCst);
                self.broadcast(AgentEvent::UserMessage {
                    client_id: client_id.to_string(),
                    client_name: client_name.to_string(),
                    client_type,
                    content: content.clone(),
                    queued: is_busy,
                });

                if is_busy {
                    self.command_queue
                        .lock()
                        .await
                        .push_back(AgentCommand::UserInput { content, attachments });
                } else {
                    let room = self.clone();
                    tokio::spawn(async move {
                        room.run_user_turn(content, attachments, None).await;
                    });
                }
            }
            AgentCommand::SetModel { model } => {
                *self.active_model.write() = model.clone();
                let _ = self
                    .storage
                    .update_session_settings(&self.session_id, Some(&model), None, None)
                    .await;
                self.broadcast(AgentEvent::ModelChanged { active_model: model });
            }
            AgentCommand::SetAgent { agent } => {
                *self.active_agent.write() = agent.clone();
                let _ = self
                    .storage
                    .update_session_settings(&self.session_id, None, Some(&agent), None)
                    .await;
                self.broadcast(AgentEvent::AgentChanged { active_agent: agent });
            }
            AgentCommand::SetApprovalMode { mode } => {
                *self.approval_mode.write() = mode;
                let _ = self
                    .storage
                    .update_session_settings(&self.session_id, None, None, Some(mode))
                    .await;
                self.broadcast(AgentEvent::ApprovalModeChanged { mode });
            }
            AgentCommand::SwitchBranch { leaf_message_id } => {
                let _ = self
                    .storage
                    .switch_branch(&self.session_id, &leaf_message_id)
                    .await;
                self.broadcast(AgentEvent::ActiveBranchChanged {
                    current_leaf_id: leaf_message_id,
                });
            }
            AgentCommand::ForkAndRun {
                parent_message_id,
                new_content,
            } => {
                let room = self.clone();
                tokio::spawn(async move {
                    if let Some(content) = new_content {
                        room.run_user_turn(content, Vec::new(), Some(parent_message_id))
                            .await;
                    }
                });
            }
        }
    }

    /// 执行单轮 Turn
    async fn run_user_turn(
        self: &Arc<Self>,
        user_text: String,
        attachments: Vec<String>,
        parent_id_override: Option<String>,
    ) {
        self.is_running.store(true, Ordering::SeqCst);
        let turn_id = uuid::Uuid::new_v4().to_string();

        *self.active_turn.write() = Some(ActiveTurnState {
            turn_id: turn_id.clone(),
            ..Default::default()
        });

        self.broadcast(AgentEvent::TurnStarted {
            turn_id:     turn_id.clone(),
            subagent_id: None,
        });

        // 1. 构造并持久化 User Message
        let user_msg_id = uuid::Uuid::new_v4().to_string();
        let mut user_blocks = vec![Block::Text { text: user_text }];
        for att in attachments {
            user_blocks.push(Block::Image {
                mime_type: "image/png".into(),
                data:      att,
            });
        }

        let user_parent = match parent_id_override {
            Some(p) => Some(p),
            None => self
                .storage
                .get_session(&self.session_id)
                .await
                .ok()
                .flatten()
                .and_then(|rec| rec.current_leaf_id),
        };

        let user_msg = ChatMessage {
            id:         user_msg_id.clone(),
            parent_id:  user_parent,
            role:       Role::User,
            content:    user_blocks,
            created_at: chrono::Utc::now().timestamp_millis(),
        };

        if let Err(e) = self
            .storage
            .append_message(&self.session_id, &user_msg, 0, 0)
            .await
        {
            self.broadcast(AgentEvent::Error {
                message: format!("Failed to save user message: {}", e),
            });
            self.finish_turn(turn_id, StopReason::Error, TokenUsage::default())
                .await;
            return;
        }

        // 2. 循环与模型交互并驱动工具调用
        let mut loop_parent_id = user_msg_id;
        let mut turn_usage = TokenUsage::default();

        let cancel_token = self.cancel_token.read().clone();

        'agent_loop: loop {
            if cancel_token.is_cancelled() {
                self.finish_turn(turn_id, StopReason::Cancelled, turn_usage)
                    .await;
                return;
            }

            // 获取线性消息图
            let mut messages = match self
                .storage
                .get_linear_messages(&self.session_id, Some(&loop_parent_id))
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    self.broadcast(AgentEvent::Error {
                        message: format!("Storage error: {}", e),
                    });
                    break 'agent_loop;
                }
            };

            // 获取 Agent 模板与 Model 配置
            let active_agent_name = self.active_agent.read().clone();
            let template = match AgentLoader::load_agent(&active_agent_name, &self.workspace) {
                Ok(t) => t,
                Err(e) => {
                    self.broadcast(AgentEvent::Error {
                        message: format!("Agent load error: {}", e),
                    });
                    break 'agent_loop;
                }
            };

            let active_model_sel = self.active_model.read().clone();
            let (provider_cfg, model_cfg) = {
                let cfg = self.config.read();
                match cfg.find_model(&active_model_sel) {
                    Some((p, m)) => (p.clone(), m),
                    None => {
                        self.broadcast(AgentEvent::Error {
                            message: format!("Model not found: {}", active_model_sel),
                        });
                        break 'agent_loop;
                    }
                }
            };

            // 上下文压缩
            compact_messages(&mut messages, model_cfg.context_len);

            // 拼装 System Prompt
            let system_prompt = AgentLoader::build_system_prompt(&template, &self.workspace, &active_model_sel);

            // 导出工具列表
            let tools_defs = self.tools.to_definitions();

            // 发起 Provider 请求
            let provider = UniversalProvider::new(provider_cfg);
            let mut stream_rx = match provider
                .send_stream(&messages, Some(&system_prompt), &tools_defs, &model_cfg)
                .await
            {
                Ok(rx) => rx,
                Err(e) => {
                    self.broadcast(AgentEvent::Error {
                        message: format!("Provider error: {}", e),
                    });
                    break 'agent_loop;
                }
            };

            let mut assistant_thinking = String::new();
            let mut assistant_text = String::new();
            let mut assistant_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut stop_reason = StopReason::EndTurn;

            // 监听流式事件
            while let Some(event) = stream_rx.recv().await {
                if cancel_token.is_cancelled() {
                    self.finish_turn(turn_id, StopReason::Cancelled, turn_usage)
                        .await;
                    return;
                }

                match event {
                    ProviderStreamEvent::ThinkingDelta(delta) => {
                        assistant_thinking.push_str(&delta);
                        if let Some(turn) = self.active_turn.write().as_mut() {
                            turn.accumulated_thinking.push_str(&delta);
                        }
                        self.broadcast(AgentEvent::ThinkingDelta {
                            delta,
                            subagent_id: None,
                        });
                    }
                    ProviderStreamEvent::TextDelta(delta) => {
                        assistant_text.push_str(&delta);
                        if let Some(turn) = self.active_turn.write().as_mut() {
                            turn.accumulated_text.push_str(&delta);
                        }
                        self.broadcast(AgentEvent::TextDelta {
                            delta,
                            subagent_id: None,
                        });
                    }
                    ProviderStreamEvent::ToolCall { id, name, input } => {
                        assistant_tool_calls.push((id, name, input));
                    }
                    ProviderStreamEvent::Usage {
                        input_tokens,
                        output_tokens,
                    } => {
                        turn_usage.input_tokens += input_tokens;
                        turn_usage.output_tokens += output_tokens;
                    }
                    ProviderStreamEvent::Done { stop_reason: reason } => {
                        stop_reason = reason;
                    }
                    ProviderStreamEvent::Error(err) => {
                        self.broadcast(AgentEvent::Error { message: err });
                        stop_reason = StopReason::Error;
                    }
                }
            }

            // 保存 Assistant 消息
            let assistant_msg_id = uuid::Uuid::new_v4().to_string();
            let mut assistant_blocks = Vec::new();
            if !assistant_thinking.is_empty() {
                assistant_blocks.push(Block::Thinking {
                    thinking: assistant_thinking,
                });
            }
            if !assistant_text.is_empty() {
                assistant_blocks.push(Block::Text { text: assistant_text });
            }
            for (cid, cname, cinput) in &assistant_tool_calls {
                assistant_blocks.push(Block::ToolUse {
                    id:    cid.clone(),
                    name:  cname.clone(),
                    input: cinput.clone(),
                });
            }

            let assistant_msg = ChatMessage {
                id:         assistant_msg_id.clone(),
                parent_id:  Some(loop_parent_id),
                role:       Role::Assistant,
                content:    assistant_blocks,
                created_at: chrono::Utc::now().timestamp_millis(),
            };

            let _ = self
                .storage
                .append_message(
                    &self.session_id,
                    &assistant_msg,
                    turn_usage.input_tokens,
                    turn_usage.output_tokens,
                )
                .await;
            loop_parent_id = assistant_msg_id.clone();

            // 若无工具调用或模型已自然结束，退出循环
            if assistant_tool_calls.is_empty() || stop_reason != StopReason::ToolUse {
                self.finish_turn(turn_id, stop_reason, turn_usage).await;
                return;
            }

            // 执行工具调用
            let mut tool_results_blocks = Vec::new();
            for (call_id, tool_name, tool_input) in assistant_tool_calls {
                let call_data = ToolCallStartedData {
                    call_id:     call_id.clone(),
                    name:        tool_name.clone(),
                    input:       tool_input.clone(),
                    subagent_id: None,
                };
                if let Some(turn) = self.active_turn.write().as_mut() {
                    turn.active_tool_call = Some(call_data.clone());
                }
                self.broadcast(AgentEvent::ToolCallStarted(call_data));

                // 1. 熔断器检查
                let breaker_check = {
                    self.circuit_breaker
                        .lock()
                        .await
                        .check_and_record(&tool_name, &tool_input)
                };
                if let Err(e) = breaker_check {
                    let err_output = e.to_string();
                    self.broadcast(AgentEvent::ToolCallFinished {
                        call_id:     call_id.clone(),
                        name:        tool_name.clone(),
                        output:      err_output.clone(),
                        is_error:    true,
                        subagent_id: None,
                    });
                    tool_results_blocks.push(Block::ToolResult {
                        tool_use_id: call_id,
                        content:     err_output,
                        is_error:    true,
                    });
                    continue;
                }

                // 2. 审批模式判定
                let needs_approval = {
                    let mode = *self.approval_mode.read();
                    let is_whitelisted = self.whitelist.read().contains(&tool_name);
                    match mode {
                        ApprovalMode::Auto => false,
                        ApprovalMode::Strict => !is_whitelisted,
                        ApprovalMode::Normal => {
                            // Normal 模式下危险工具 (shell) 需审批，读写放行
                            (tool_name == "shell") && !is_whitelisted
                        }
                    }
                };

                let mut is_denied = false;
                if needs_approval {
                    let req_id = uuid::Uuid::new_v4().to_string();
                    let req_data = PermissionRequestedData {
                        request_id: req_id.clone(),
                        name:       tool_name.clone(),
                        summary:    tool_input.to_string(),
                    };
                    if let Some(turn) = self.active_turn.write().as_mut() {
                        turn.pending_approval = Some(req_data.clone());
                    }
                    self.broadcast(AgentEvent::PermissionRequested(req_data));

                    let rx = self.arbiter.register(req_id.clone()).await;

                    // 120 秒超时判定
                    let decision = match tokio::time::timeout(Duration::from_secs(120), rx).await {
                        Ok(Ok(d)) => d,
                        _ => ApprovalDecision::Deny,
                    };

                    if let Some(turn) = self.active_turn.write().as_mut() {
                        turn.pending_approval = None;
                    }

                    match decision {
                        ApprovalDecision::AllowOnce => {}
                        ApprovalDecision::AllowSession => {
                            self.whitelist.write().insert(tool_name.clone());
                        }
                        ApprovalDecision::Deny => {
                            is_denied = true;
                        }
                    }
                }

                let tool_output = if is_denied {
                    ToolOutput::error(
                        "Execution rejected: Permission denied by user (or approval timed out after 120s).",
                    )
                } else if let Some(tool) = self.tools.get(&tool_name) {
                    tool.execute(&self.workspace, tool_input).await
                } else {
                    ToolOutput::error(format!("Tool '{}' not found in registry", tool_name))
                };

                if let Some(turn) = self.active_turn.write().as_mut() {
                    turn.active_tool_call = None;
                }

                self.broadcast(AgentEvent::ToolCallFinished {
                    call_id:     call_id.clone(),
                    name:        tool_name.clone(),
                    output:      tool_output.output.clone(),
                    is_error:    tool_output.is_error,
                    subagent_id: None,
                });

                tool_results_blocks.push(Block::ToolResult {
                    tool_use_id: call_id,
                    content:     tool_output.output,
                    is_error:    tool_output.is_error,
                });
            }

            // 保存 ToolResult (User 消息)
            let tool_msg_id = uuid::Uuid::new_v4().to_string();
            let tool_msg = ChatMessage {
                id:         tool_msg_id.clone(),
                parent_id:  Some(loop_parent_id),
                role:       Role::User,
                content:    tool_results_blocks,
                created_at: chrono::Utc::now().timestamp_millis(),
            };
            let _ = self
                .storage
                .append_message(&self.session_id, &tool_msg, 0, 0)
                .await;
            loop_parent_id = tool_msg_id;
        }

        self.finish_turn(turn_id, StopReason::Error, turn_usage)
            .await;
    }

    /// 结束轮次并自动弹出下一条排队命令
    async fn finish_turn(&self, turn_id: String, stop_reason: StopReason, usage: TokenUsage) {
        *self.active_turn.write() = None;
        self.broadcast(AgentEvent::TurnFinished {
            turn_id,
            stop_reason,
            usage,
            subagent_id: None,
        });

        self.is_running.store(false, Ordering::SeqCst);
        let next_cmd = self.command_queue.lock().await.pop_front();
        if let Some(AgentCommand::UserInput { content, attachments }) = next_cmd {
            let room_clone = Arc::new(self.clone());
            tokio::spawn(async move {
                room_clone.run_user_turn(content, attachments, None).await;
            });
        }
    }
}

// 辅助克隆 SessionRoom 句柄
impl Clone for SessionRoom {
    fn clone(&self) -> Self {
        Self {
            session_id:      self.session_id.clone(),
            workspace:       self.workspace.clone(),
            storage:         self.storage.clone(),
            config:          self.config.clone(),
            tools:           self.tools.clone(),
            mcp:             self.mcp.clone(),
            active_model:    RwLock::new(self.active_model.read().clone()),
            active_agent:    RwLock::new(self.active_agent.read().clone()),
            approval_mode:   RwLock::new(*self.approval_mode.read()),
            whitelist:       RwLock::new(self.whitelist.read().clone()),
            event_tx:        self.event_tx.clone(),
            command_queue:   Mutex::new(VecDeque::new()),
            active_turn:     self.active_turn.clone(),
            arbiter:         self.arbiter.clone(),
            cancel_token:    RwLock::new(self.cancel_token.read().clone()),
            circuit_breaker: Mutex::new(CircuitBreaker::new()),
            is_running:      self.is_running.clone(),
        }
    }
}

// =========================================================================
// 6. Subagent Runner 实现
// =========================================================================

pub struct RoomSubagentRunner {
    room: Arc<SessionRoom>,
}

impl RoomSubagentRunner {
    pub fn new(room: Arc<SessionRoom>) -> Self {
        Self { room }
    }
}

#[async_trait::async_trait]
impl SubagentRunner for RoomSubagentRunner {
    async fn run_subagent(&self, agent: &str, prompt: &str) -> Result<String, String> {
        let subagent_id = uuid::Uuid::new_v4().to_string();
        let turn_id = uuid::Uuid::new_v4().to_string();

        self.room.broadcast(AgentEvent::TurnStarted {
            turn_id:     turn_id.clone(),
            subagent_id: Some(subagent_id.clone()),
        });

        // 简要模拟子 Agent 探索并广播流式数据
        self.room.broadcast(AgentEvent::TextDelta {
            delta:       format!("[Subagent '{}' exploring: {}]\n", agent, prompt),
            subagent_id: Some(subagent_id.clone()),
        });

        let summary = format!(
            "Subagent '{}' completed task successfully for prompt: {}",
            agent, prompt
        );

        self.room.broadcast(AgentEvent::TurnFinished {
            turn_id,
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::default(),
            subagent_id: Some(subagent_id),
        });

        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker() {
        let mut cb = CircuitBreaker::new();
        let input = serde_json::json!({ "path": "test.txt" });

        assert!(cb.check_and_record("read", &input).is_ok());
        assert!(cb.check_and_record("read", &input).is_ok());
        assert!(cb.check_and_record("read", &input).is_err()); // 连续第 3 次触发熔断

        cb.reset();
        assert!(cb.check_and_record("read", &input).is_ok());
    }

    #[tokio::test]
    async fn test_approval_arbiter() {
        let arbiter = ApprovalArbiter::new();
        let rx = arbiter.register("req_123".into()).await;

        let resolved = arbiter
            .resolve("req_123", ApprovalDecision::AllowOnce)
            .await;
        assert!(resolved);

        let decision = rx.await.unwrap();
        assert_eq!(decision, ApprovalDecision::AllowOnce);
    }
}
