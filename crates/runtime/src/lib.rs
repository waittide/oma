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
use oma_config::{AgentLoader, AgentTemplate, OmaConfig};
use oma_contract::{
    ActiveTurnCatchUp, AgentCommand, AgentEvent, ApprovalDecision, ApprovalMode, Block, ChatMessage, ClientType,
    PermissionRequestedData, Role, StopReason, TokenUsage, ToolCallStartedData, ToolOutput,
};
use oma_provider::{ProviderStreamEvent, UniversalProvider};
use oma_storage::{StorageError, StorageManager};
use oma_tool::{SubagentRunner, ToolRegistry};
use parking_lot::RwLock;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_util::sync::CancellationToken;

/// 审批等待上限（超时按拒绝处理并广播解绑）
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// 子 Agent 单次委托的最大工具轮次，防止无限自我调用
const MAX_SUBAGENT_ROUNDS: usize = 16;
/// 上下文压缩后保留的工具结果占位文案
const PRUNED_TOOL_RESULT: &str = "[工具执行结果已截断修剪以节省上下文窗口]";
/// 超过该字符数的工具结果在压缩时会被替换为占位
const PRUNE_MIN_CHARS: usize = 300;

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
// 2. 审批仲裁器 Approval Arbiter (先到先得 + 超时兜底)
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

    /// 撤销未完成的等待项（超时/取消路径），避免注册表随轮次累积
    pub async fn forget(&self, request_id: &str) {
        self.pending.lock().await.remove(request_id);
    }

    /// 当前挂起数量（供观测与测试）
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

/// 审批判定结果：放行 / 拒绝 / 轮次被取消
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Allowed,
    Denied,
    Cancelled,
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

/// 真实用户轮次起点：role=user 且含非 tool_result 内容块。
/// tool_result 回执同为 role=user，但它必须紧跟其 tool_use，不能作为裁剪边界。
fn is_turn_start(message: &ChatMessage) -> bool {
    message.role == Role::User
        && message
            .content
            .iter()
            .any(|b| !matches!(b, Block::ToolResult { .. }))
}

/// 执行两阶段压缩策略：达到 70% 阈值时修剪旧 ToolResult
pub fn compact_messages(messages: &mut Vec<ChatMessage>, context_len: usize) {
    let threshold = (context_len as f64 * 0.7) as usize;
    if estimate_tokens(messages) <= threshold {
        return;
    }

    // 第一阶段：把冗长 ToolResult 的内容替换为占位。
    // 始终保留最后一条消息（模型当前要处理的输出）；替换内容不改变
    // tool_use / tool_result 的配对关系，因此跨轮次与轮内都可安全执行。
    if messages.len() > 1 {
        let tail = messages.len() - 1;
        for m in messages[..tail].iter_mut() {
            for b in &mut m.content {
                if let Block::ToolResult { content, .. } = b {
                    if content.chars().count() > PRUNE_MIN_CHARS {
                        *content = PRUNED_TOOL_RESULT.to_string();
                    }
                }
            }
        }
    }

    if estimate_tokens(messages) <= threshold {
        return;
    }

    // 第二阶段：按整轮裁剪前缀。绝不在轮次中途切断——否则会留下孤儿 tool_result
    // 或让 assistant 消息成为首条（Anthropic/OpenAI 均会 400）。
    let turn_starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| is_turn_start(m))
        .map(|(i, _)| i)
        .collect();
    if turn_starts.len() <= 1 {
        return; // 只剩单轮，无法再丢弃消息
    }
    for &start in &turn_starts[1..] {
        if estimate_tokens(&messages[start..]) <= threshold {
            messages.drain(..start);
            return;
        }
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
// 5. Room 级错误
// =========================================================================

/// 会话房间错误：调用方据此映射 HTTP 状态码，禁止靠错误文案判定类别。
#[derive(Debug)]
pub enum RoomError {
    /// 运行中轮次禁止消息树变更
    Busy(String),
    Invalid(String),
    NotFound(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoomError::Busy(m) | RoomError::Invalid(m) | RoomError::NotFound(m) => write!(f, "{}", m),
            RoomError::Internal(e) => write!(f, "{:#}", e),
        }
    }
}

impl std::error::Error for RoomError {}

impl From<StorageError> for RoomError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::InvalidId(m) => RoomError::Invalid(m),
            StorageError::NotFound(m) => RoomError::NotFound(m),
            StorageError::Internal(e) => RoomError::Internal(e),
        }
    }
}

// =========================================================================
// 6. Session Room 核心隔离容器
// =========================================================================
//
// 共享语义：房间本身只经 `Arc<SessionRoom>` 传递，所有可变状态都是字段级互斥保护，
// 因此多个客户端/后台任务看到的是同一份模型、审批模式、白名单与命令队列。
// （此前的 `impl Clone` 会深拷贝这些字段，导致排队指令被丢弃，已删除。）

pub struct SessionRoom {
    pub session_id:      String,
    pub workspace:       PathBuf,
    pub storage:         StorageManager,
    pub config:          Arc<RwLock<OmaConfig>>,
    pub tools:           ToolRegistry,
    pub active_model:    RwLock<String>,
    pub active_agent:    RwLock<String>,
    pub approval_mode:   RwLock<ApprovalMode>,
    pub whitelist:       RwLock<HashSet<String>>, // AllowSession 内存白名单
    pub event_tx:        broadcast::Sender<AgentEvent>,
    pub command_queue:   Mutex<VecDeque<AgentCommand>>,
    pub active_turn:     RwLock<Option<ActiveTurnState>>,
    pub arbiter:         ApprovalArbiter,
    pub cancel_token:    RwLock<CancellationToken>,
    pub circuit_breaker: Mutex<CircuitBreaker>,
    /// 轮次占用权：由 CAS 抢占，确保同一房间任意时刻至多一个执行中的轮次
    pub is_running:      AtomicBool,
}

impl SessionRoom {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: impl Into<String>,
        workspace: impl Into<PathBuf>,
        storage: StorageManager,
        config: Arc<RwLock<OmaConfig>>,
        tools: ToolRegistry,
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
            active_model: RwLock::new(active_model.into()),
            active_agent: RwLock::new(active_agent.into()),
            approval_mode: RwLock::new(approval_mode),
            whitelist: RwLock::new(HashSet::new()),
            event_tx,
            command_queue: Mutex::new(VecDeque::new()),
            active_turn: RwLock::new(None),
            arbiter: ApprovalArbiter::new(),
            cancel_token: RwLock::new(CancellationToken::new()),
            circuit_breaker: Mutex::new(CircuitBreaker::new()),
            is_running: AtomicBool::new(false),
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

    pub fn is_busy(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
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
        // 1. 触发 CancellationToken 级联中断；运行中的轮次由自己广播 TurnFinished
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
        self.broadcast_queue_len().await;

        // 3. 挂起中的审批一并解绑，避免客户端继续显示等待中的弹窗
        //    （先取出再 await：parking_lot 守卫不得跨越 await）
        let pending = {
            let mut turn = self.active_turn.write();
            turn.as_mut().and_then(|t| t.pending_approval.take())
        };
        if let Some(req) = pending {
            let _ = self
                .arbiter
                .resolve(&req.request_id, ApprovalDecision::Deny)
                .await;
            self.broadcast(AgentEvent::PermissionResolved {
                request_id:  req.request_id,
                decision:    ApprovalDecision::Deny,
                resolved_by: "system (cancelled)".into(),
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
                self.dispatch_user_input(content, attachments, None, client_id, client_name, client_type)
                    .await;
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
                match self
                    .storage
                    .switch_branch(&self.session_id, &leaf_message_id)
                    .await
                {
                    Ok(()) => self.broadcast(AgentEvent::ActiveBranchChanged {
                        current_leaf_id: leaf_message_id,
                    }),
                    Err(e) => self.broadcast(AgentEvent::Error {
                        message: format!("切换分支失败: {}", e),
                    }),
                }
            }
            AgentCommand::ForkAndRun {
                parent_message_id,
                new_content,
            } => {
                if let Some(content) = new_content {
                    // 与普通输入共用调度：忙碌时入队，避免并发轮次撕裂会话状态
                    self.dispatch_user_input(
                        content,
                        Vec::new(),
                        Some(parent_message_id),
                        client_id,
                        client_name,
                        client_type,
                    )
                    .await;
                }
            }
        }
    }

    /// 用户输入统一入口：空闲则立即抢占轮次，否则入队等待（FIFO）
    async fn dispatch_user_input(
        self: &Arc<Self>,
        content: String,
        attachments: Vec<String>,
        parent_id_override: Option<String>,
        client_id: &str,
        client_name: &str,
        client_type: ClientType,
    ) {
        let accepted = self
            .is_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();

        self.broadcast(AgentEvent::UserMessage {
            client_id: client_id.to_string(),
            client_name: client_name.to_string(),
            client_type,
            content: content.clone(),
            queued: !accepted,
        });

        if accepted {
            let room = self.clone();
            tokio::spawn(async move {
                room.run_user_turn(content, attachments, parent_id_override)
                    .await;
            });
        } else {
            self.command_queue
                .lock()
                .await
                .push_back(AgentCommand::UserInput { content, attachments });
            self.broadcast_queue_len().await;
        }
    }

    /// 广播服务端权威队列深度
    async fn broadcast_queue_len(&self) {
        let pending = self.command_queue.lock().await.len();
        self.broadcast(AgentEvent::QueueUpdated { pending });
    }

    /// 删除消息及其子树；运行中的轮次禁止删除，避免撕裂进行中的会话状态。
    pub async fn delete_message(&self, message_id: &str) -> Result<(Vec<String>, Option<String>), RoomError> {
        if self.is_busy() {
            return Err(RoomError::Busy("Cannot delete messages while a turn is running".into()));
        }
        let (deleted, leaf) = self
            .storage
            .delete_message_subtree(&self.session_id, message_id)
            .await?;
        self.broadcast(AgentEvent::MessagesDeleted {
            deleted_ids:     deleted.clone(),
            current_leaf_id: leaf.clone(),
        });
        Ok((deleted, leaf))
    }

    // ---------------------------------------------------------------------
    // 工具授权与执行
    // ---------------------------------------------------------------------

    /// 该工具在当前审批模式下是否需要人工确认
    fn needs_approval(&self, tool_name: &str) -> bool {
        let mode = *self.approval_mode.read();
        let is_whitelisted = self.whitelist.read().contains(tool_name);
        match mode {
            ApprovalMode::Auto => false,
            ApprovalMode::Strict => !is_whitelisted,
            // Normal 模式下危险工具 (shell) 需审批，读写放行
            ApprovalMode::Normal => tool_name == "shell" && !is_whitelisted,
        }
    }

    /// 请求人工审批：先到先得响应，超时或取消按拒绝处理，并保证等待项被回收与广播解绑。
    async fn request_approval(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        cancel: &CancellationToken,
    ) -> ApprovalOutcome {
        if !self.needs_approval(tool_name) {
            return ApprovalOutcome::Allowed;
        }

        let request_id = uuid::Uuid::new_v4().to_string();
        let request = PermissionRequestedData {
            request_id: request_id.clone(),
            name:       tool_name.to_string(),
            summary:    tool_input.to_string(),
        };
        if let Some(turn) = self.active_turn.write().as_mut() {
            turn.pending_approval = Some(request.clone());
        }
        self.broadcast(AgentEvent::PermissionRequested(request));

        let rx = self.arbiter.register(request_id.clone()).await;
        let (outcome, resolved_by): (ApprovalOutcome, Option<&str>) = tokio::select! {
            biased;
            _ = cancel.cancelled() => (ApprovalOutcome::Cancelled, Some("system (cancelled)")),
            waited = tokio::time::timeout(APPROVAL_TIMEOUT, rx) => match waited {
                Ok(Ok(ApprovalDecision::AllowOnce)) => (ApprovalOutcome::Allowed, None),
                Ok(Ok(ApprovalDecision::AllowSession)) => {
                    self.whitelist.write().insert(tool_name.to_string());
                    (ApprovalOutcome::Allowed, None)
                }
                Ok(Ok(ApprovalDecision::Deny)) => (ApprovalOutcome::Denied, None),
                // 超时或发送端消失（轮次被取消）
                Ok(Err(_)) | Err(_) => (ApprovalOutcome::Denied, Some("system (timeout)")),
            },
        };

        // 无论走哪条分支都要回收等待项，否则注册表会随轮次无限增长
        self.arbiter.forget(&request_id).await;
        if let Some(turn) = self.active_turn.write().as_mut() {
            turn.pending_approval = None;
        }
        if let Some(by) = resolved_by {
            self.broadcast(AgentEvent::PermissionResolved {
                request_id,
                decision: ApprovalDecision::Deny,
                resolved_by: by.to_string(),
            });
        }

        outcome
    }

    /// 执行单个工具调用：白名单 → 熔断 → 审批 → 执行。
    /// 返回 (工具输出, 轮次是否被取消)；取消信号可中断审批等待与工具执行。
    async fn execute_tool_call(
        &self,
        call_id: &str,
        tool_name: &str,
        tool_input: serde_json::Value,
        allowed: Option<&HashSet<String>>,
        cancel: &CancellationToken,
        subagent_id: Option<&str>,
    ) -> (ToolOutput, bool) {
        let started = ToolCallStartedData {
            call_id:     call_id.to_string(),
            name:        tool_name.to_string(),
            input:       tool_input.clone(),
            subagent_id: subagent_id.map(str::to_string),
        };
        if subagent_id.is_none() {
            if let Some(turn) = self.active_turn.write().as_mut() {
                turn.active_tool_call = Some(started.clone());
            }
        }
        self.broadcast(AgentEvent::ToolCallStarted(started));

        // 0. Agent 模板工具白名单（模板未声明工具时不限制）
        if let Some(allowed) = allowed {
            if !allowed.contains(tool_name) {
                let output = ToolOutput::error(format!("Tool '{}' is not permitted for the active agent.", tool_name));
                return (
                    self.finish_tool_call(call_id, tool_name, output, subagent_id)
                        .await,
                    false,
                );
            }
        }

        // 1. 熔断器检查
        let breaker_check = {
            self.circuit_breaker
                .lock()
                .await
                .check_and_record(tool_name, &tool_input)
        };
        if let Err(e) = breaker_check {
            return (
                self.finish_tool_call(call_id, tool_name, ToolOutput::error(e.to_string()), subagent_id)
                    .await,
                false,
            );
        }

        // 2. 审批
        match self.request_approval(tool_name, &tool_input, cancel).await {
            ApprovalOutcome::Allowed => {}
            ApprovalOutcome::Denied => {
                return (
                    self.finish_tool_call(
                        call_id,
                        tool_name,
                        ToolOutput::error(
                            "Execution rejected: Permission denied by user (or approval timed out after 120s).",
                        ),
                        subagent_id,
                    )
                    .await,
                    false,
                );
            }
            ApprovalOutcome::Cancelled => {
                return (
                    self.finish_tool_call(
                        call_id,
                        tool_name,
                        ToolOutput::error("Tool execution cancelled."),
                        subagent_id,
                    )
                    .await,
                    true,
                );
            }
        }

        // 3. 执行（可被取消信号中断，避免长命令拖住整个轮次）
        let Some(tool) = self.tools.get(tool_name) else {
            return (
                self.finish_tool_call(
                    call_id,
                    tool_name,
                    ToolOutput::error(format!("Tool '{}' not found in registry", tool_name)),
                    subagent_id,
                )
                .await,
                false,
            );
        };
        let output = tokio::select! {
            biased;
            _ = cancel.cancelled() => ToolOutput::error("Tool execution cancelled."),
            out = tool.execute(&self.workspace, tool_input) => out,
        };
        let cancelled = cancel.is_cancelled();
        (
            self.finish_tool_call(call_id, tool_name, output, subagent_id)
                .await,
            cancelled,
        )
    }

    /// 广播工具执行结果并清理活跃工具槽位，把输出交回调用方用于落库
    async fn finish_tool_call(
        &self,
        call_id: &str,
        tool_name: &str,
        output: ToolOutput,
        subagent_id: Option<&str>,
    ) -> ToolOutput {
        if subagent_id.is_none() {
            if let Some(turn) = self.active_turn.write().as_mut() {
                turn.active_tool_call = None;
            }
        }
        self.broadcast(AgentEvent::ToolCallFinished {
            call_id:     call_id.to_string(),
            name:        tool_name.to_string(),
            output:      output.output.clone(),
            is_error:    output.is_error,
            subagent_id: subagent_id.map(str::to_string),
        });
        output
    }

    // ---------------------------------------------------------------------
    // 主 Agent Loop
    // ---------------------------------------------------------------------

    /// 执行单轮 Turn（调用方必须已通过 `is_running` CAS 占据轮次）
    async fn run_user_turn(
        self: &Arc<Self>,
        user_text: String,
        attachments: Vec<String>,
        parent_id_override: Option<String>,
    ) {
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
        // 编辑重发（fork_and_run）失败的自动回滚目标：本轮错误时删除该分叉用户消息
        let fork_user_msg: Option<String> = parent_id_override.as_ref().map(|_| user_msg_id.clone());
        let pre_fork_leaf: Option<String> = if parent_id_override.is_some() {
            self.storage
                .get_session(&self.session_id)
                .await
                .ok()
                .flatten()
                .and_then(|rec| rec.current_leaf_id)
        } else {
            None
        };

        let mut user_blocks = Vec::new();
        if !user_text.is_empty() {
            user_blocks.push(Block::Text { text: user_text });
        }
        for att in attachments {
            user_blocks.push(att.into_block());
        }

        let is_fork = parent_id_override.is_some();
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
            self.rollback_failed_fork(fork_user_msg.as_deref(), pre_fork_leaf.as_deref())
                .await;
            self.finish_turn(turn_id, StopReason::Error, TokenUsage::default())
                .await;
            return;
        }

        // 编辑重发：分叉用户消息即成为当前叶子，广播让所有客户端立即切换到新分支视图，
        // 旧分支的上一条回答随即从界面消失，无需等本轮跑完
        if is_fork {
            self.broadcast(AgentEvent::ActiveBranchChanged {
                current_leaf_id: user_msg_id.clone(),
            });
        }

        // 2. 线性历史只在轮次开始时读取一次，之后随追加增量维护，
        //    避免每一轮工具调用都全量回读并反序列化整个会话
        let mut history = match self
            .storage
            .get_linear_messages(&self.session_id, Some(&user_msg_id))
            .await
        {
            Ok(m) => m,
            Err(e) => {
                self.broadcast(AgentEvent::Error {
                    message: format!("Storage error: {}", e),
                });
                self.finish_turn(turn_id, StopReason::Error, TokenUsage::default())
                    .await;
                return;
            }
        };

        let cancel_token = self.cancel_token.read().clone();
        let mut turn_usage = TokenUsage::default();

        let outcome = self
            .agent_loop(&mut history, &cancel_token, &mut turn_usage)
            .await;

        match outcome {
            TurnOutcome::Finished(stop_reason) => {
                if stop_reason == StopReason::Error {
                    self.rollback_failed_fork(fork_user_msg.as_deref(), pre_fork_leaf.as_deref())
                        .await;
                }
                self.finish_turn(turn_id, stop_reason, turn_usage).await;
            }
        }
    }

    /// 驱动模型流式交互与工具执行，直到自然结束或需要整轮中止。
    async fn agent_loop(
        self: &Arc<Self>,
        history: &mut Vec<ChatMessage>,
        cancel_token: &CancellationToken,
        turn_usage: &mut TokenUsage,
    ) -> TurnOutcome {
        loop {
            if cancel_token.is_cancelled() {
                return TurnOutcome::Finished(StopReason::Cancelled);
            }

            // 获取 Agent 模板与 Model 配置
            let active_agent_name = self.active_agent.read().clone();
            let template = match AgentLoader::load_agent(&active_agent_name, &self.workspace) {
                Ok(t) => t,
                Err(e) => {
                    self.broadcast(AgentEvent::Error {
                        message: format!("Agent load error: {}", e),
                    });
                    return TurnOutcome::Finished(StopReason::Error);
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
                        return TurnOutcome::Finished(StopReason::Error);
                    }
                }
            };

            // 上下文压缩（只作用于本次请求的副本，持久化历史保持不变）
            let mut messages = history.clone();
            compact_messages(&mut messages, model_cfg.context_len);
            // session_attachment:// 引用在此内联为 data URI，各厂商协议拿到的都是完整载荷
            self.inline_attachments(&mut messages).await;

            // 拼装 System Prompt 与按 Agent 模板裁剪的工具清单
            let system_prompt = AgentLoader::build_system_prompt(&template, &self.workspace, &active_model_sel);
            let tools_defs = self.tools.to_definitions(&template.tools);
            let allowed = allowed_tools(&template);

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
                    return TurnOutcome::Finished(StopReason::Error);
                }
            };

            let mut assistant_thinking = String::new();
            let mut assistant_text = String::new();
            let mut assistant_tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut stop_reason = StopReason::EndTurn;
            let mut cancelled = false;

            // 监听流式事件（可被取消信号立即打断，无需等待下一个增量）
            loop {
                let event = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        cancelled = true;
                        break;
                    }
                    ev = stream_rx.recv() => match ev {
                        Some(e) => e,
                        None => break,
                    },
                };

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

            // 工具仅在正常以 tool_use 收尾且未取消时执行
            let run_tools = !cancelled && stop_reason == StopReason::ToolUse && !assistant_tool_calls.is_empty();

            let mut assistant_msg_id = None;
            if !assistant_blocks.is_empty() {
                let msg_id = uuid::Uuid::new_v4().to_string();
                let assistant_msg = ChatMessage {
                    id:         msg_id.clone(),
                    parent_id:  history.last().map(|m| m.id.clone()),
                    role:       Role::Assistant,
                    content:    assistant_blocks,
                    created_at: chrono::Utc::now().timestamp_millis(),
                };
                if let Err(e) = self
                    .storage
                    .append_message(
                        &self.session_id,
                        &assistant_msg,
                        turn_usage.input_tokens,
                        turn_usage.output_tokens,
                    )
                    .await
                {
                    self.broadcast(AgentEvent::Error {
                        message: format!("Failed to save assistant message: {}", e),
                    });
                    return TurnOutcome::Finished(StopReason::Error);
                }
                history.push(assistant_msg);
                assistant_msg_id = Some(msg_id);
            }

            if !run_tools {
                // 未执行工具时也必须补齐 tool_result 占位，
                // 否则历史里会留下无回执的 tool_use（下一次请求必被厂商 API 拒绝）
                if !assistant_tool_calls.is_empty() {
                    if let Some(parent) = &assistant_msg_id {
                        let reason = if cancelled {
                            "Tool call cancelled before execution."
                        } else {
                            "Tool call not executed: the turn ended before tool execution."
                        };
                        self.append_tool_results(
                            parent,
                            assistant_tool_calls
                                .iter()
                                .map(|(id, _, _)| (id.clone(), reason.to_string(), true))
                                .collect(),
                            history,
                        )
                        .await;
                    }
                }
                let reason = if cancelled { StopReason::Cancelled } else { stop_reason };
                return TurnOutcome::Finished(reason);
            }

            // 执行工具调用
            let parent_id = assistant_msg_id.expect("assistant message persisted for tool use");
            let mut results: Vec<(String, String, bool)> = Vec::with_capacity(assistant_tool_calls.len());
            let mut cancelled = false;
            for (call_id, tool_name, tool_input) in assistant_tool_calls {
                if cancelled {
                    results.push((call_id, "Tool call cancelled.".into(), true));
                    continue;
                }
                let (output, was_cancelled) = self
                    .execute_tool_call(&call_id, &tool_name, tool_input, allowed.as_ref(), cancel_token, None)
                    .await;
                cancelled = was_cancelled;
                results.push((call_id, output.output, output.is_error));
            }

            self.append_tool_results(&parent_id, results, history).await;

            if cancelled {
                return TurnOutcome::Finished(StopReason::Cancelled);
            }
        }
    }

    /// 持久化工具回执消息（role = user），并同步到内存历史
    async fn append_tool_results(
        &self,
        parent_id: &str,
        results: Vec<(String, String, bool)>,
        history: &mut Vec<ChatMessage>,
    ) {
        if results.is_empty() {
            return;
        }
        let blocks: Vec<Block> = results
            .into_iter()
            .map(|(tool_use_id, content, is_error)| Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            })
            .collect();

        let msg = ChatMessage {
            id:         uuid::Uuid::new_v4().to_string(),
            parent_id:  Some(parent_id.to_string()),
            role:       Role::User,
            content:    blocks,
            created_at: chrono::Utc::now().timestamp_millis(),
        };
        if let Err(e) = self
            .storage
            .append_message(&self.session_id, &msg, 0, 0)
            .await
        {
            self.broadcast(AgentEvent::Error {
                message: format!("Failed to save tool results: {}", e),
            });
            return;
        }
        history.push(msg);
    }

    /// 编辑重发失败时回滚：删除分叉出的用户消息子树，并恢复分叉前的当前叶子。
    async fn rollback_failed_fork(&self, fork_user_msg: Option<&str>, pre_fork_leaf: Option<&str>) {
        let Some(msg_id) = fork_user_msg else {
            return;
        };
        if let Ok((deleted, fallback_leaf)) = self
            .storage
            .delete_message_subtree(&self.session_id, msg_id)
            .await
        {
            // 优先回到分叉前用户所在的叶子（它不在被删子树内）
            let leaf = pre_fork_leaf
                .filter(|l| !deleted.iter().any(|d| d == l))
                .map(|l| l.to_string())
                .or(fallback_leaf);
            if let Some(l) = &leaf {
                let _ = self.storage.switch_branch(&self.session_id, l).await;
            }
            self.broadcast(AgentEvent::MessagesDeleted {
                deleted_ids:     deleted,
                current_leaf_id: leaf,
            });
        }
    }

    /// 结束轮次并自动接管下一条排队命令
    async fn finish_turn(self: &Arc<Self>, turn_id: String, stop_reason: StopReason, usage: TokenUsage) {
        *self.active_turn.write() = None;
        self.broadcast(AgentEvent::TurnFinished {
            turn_id,
            stop_reason,
            usage,
            subagent_id: None,
        });

        self.pump_queue().await;
    }

    /// 排空命令队列：保持已占据的轮次名额串行交棒，
    /// 队列空时才释放名额，并复查一次避免「释放瞬间入队」造成的丢唤醒。
    async fn pump_queue(self: &Arc<Self>) {
        loop {
            let next = {
                let mut q = self.command_queue.lock().await;
                let item = q.pop_front();
                let pending = q.len();
                (item, pending)
            };

            match next {
                (Some(AgentCommand::UserInput { content, attachments }), pending) => {
                    self.broadcast(AgentEvent::QueueUpdated { pending });
                    let room = self.clone();
                    tokio::spawn(async move {
                        room.run_user_turn(content, attachments, None).await;
                    });
                    return; // 交棒给下一轮，由它负责后续排空
                }
                (Some(_), pending) => {
                    // 队列中只可能存放用户输入；其余指令就地丢弃并继续
                    self.broadcast(AgentEvent::QueueUpdated { pending });
                }
                (None, _) => {
                    self.is_running.store(false, Ordering::SeqCst);
                    if self.command_queue.lock().await.is_empty() {
                        return;
                    }
                    if self
                        .is_running
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_err()
                    {
                        return; // 已有其他轮次接管并由其负责排空
                    }
                }
            }
        }
    }

    /// 解析 `session_attachment://` 资源为内联 data URI，供各厂商协议直接使用。
    pub async fn inline_attachments(&self, messages: &mut [ChatMessage]) {
        for m in messages.iter_mut() {
            for b in m.content.iter_mut() {
                let Block::Image { mime_type, data } = b else {
                    continue;
                };
                let Some(name) = data.strip_prefix("session_attachment://") else {
                    continue;
                };
                let path = match self.storage.attachment_path(&self.session_id, name) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                match tokio::fs::read(&path).await {
                    Ok(bytes) => {
                        if mime_type.is_empty() || mime_type == "application/octet-stream" {
                            *mime_type = guess_mime(name);
                        }
                        *data = format!("data:{};base64,{}", mime_type, base64_encode(&bytes));
                    }
                    Err(e) => {
                        self.broadcast(AgentEvent::Error {
                            message: format!("附件 {} 读取失败: {}", name, e),
                        });
                    }
                }
            }
        }
    }
}

/// 轮次结束语义
enum TurnOutcome {
    Finished(StopReason),
}

/// Agent 模板声明的工具白名单（空声明 = 不限制）
fn allowed_tools(template: &AgentTemplate) -> Option<HashSet<String>> {
    if template.tools.is_empty() {
        None
    } else {
        Some(template.tools.iter().cloned().collect())
    }
}

fn guess_mime(name: &str) -> String {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
    {
        Some(ext) if ext == "jpg" || ext == "jpeg" => "image/jpeg".to_string(),
        Some(ext) if ext == "gif" => "image/gif".to_string(),
        Some(ext) if ext == "webp" => "image/webp".to_string(),
        _ => "image/png".to_string(),
    }
}

/// 附件载荷 → 消息块；`session_attachment://` 资源交由 Provider 请求前内联解析。
impl AttachmentRef for String {
    fn into_block(self) -> Block {
        Block::Image {
            mime_type: "image/png".into(),
            data:      self,
        }
    }
}

trait AttachmentRef {
    fn into_block(self) -> Block;
}

/// 标准 base64（无外部依赖，附件内联用）
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

// =========================================================================
// 7. Subagent Runner 实现（真实子循环：独立上下文，不落库）
// =========================================================================

pub struct RoomSubagentRunner {
    room: Arc<SessionRoom>,
}

impl RoomSubagentRunner {
    pub fn new(room: Arc<SessionRoom>) -> Self {
        Self { room }
    }

    async fn run_inner(&self, agent: &str, prompt: &str, subagent_id: &str) -> Result<String, String> {
        let room = &self.room;
        let template = AgentLoader::load_agent(agent, &room.workspace).map_err(|e| e.to_string())?;

        let active_model = room.active_model.read().clone();
        let (provider_cfg, model_cfg) = {
            let cfg = room.config.read();
            cfg.find_model(&active_model)
                .map(|(p, m)| (p.clone(), m))
                .ok_or_else(|| format!("Model not found: {}", active_model))?
        };
        let system_prompt = AgentLoader::build_system_prompt(&template, &room.workspace, &active_model);
        let tools_defs = room.tools.to_definitions(&template.tools);
        let allowed = allowed_tools(&template);
        let cancel = room.cancel_token.read().clone();

        // 子 Agent 上下文完全独立且不落库：仅返回最终文本给父轮次
        let mut messages = vec![ChatMessage {
            id:         uuid::Uuid::new_v4().to_string(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![Block::Text {
                text: prompt.to_string(),
            }],
            created_at: chrono::Utc::now().timestamp_millis(),
        }];
        let mut last_text = String::new();

        for _round in 0..MAX_SUBAGENT_ROUNDS {
            if cancel.is_cancelled() {
                return Err("Subagent cancelled.".into());
            }

            let provider = UniversalProvider::new(provider_cfg.clone());
            let mut stream_rx = provider
                .send_stream(&messages, Some(&system_prompt), &tools_defs, &model_cfg)
                .await
                .map_err(|e| format!("Subagent provider error: {}", e))?;

            let mut thinking = String::new();
            let mut text = String::new();
            let mut calls: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut stop_reason = StopReason::EndTurn;

            loop {
                let event = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err("Subagent cancelled.".into()),
                    ev = stream_rx.recv() => match ev {
                        Some(e) => e,
                        None => break,
                    },
                };
                match event {
                    ProviderStreamEvent::ThinkingDelta(delta) => {
                        thinking.push_str(&delta);
                        room.broadcast(AgentEvent::ThinkingDelta {
                            delta,
                            subagent_id: Some(subagent_id.to_string()),
                        });
                    }
                    ProviderStreamEvent::TextDelta(delta) => {
                        text.push_str(&delta);
                        room.broadcast(AgentEvent::TextDelta {
                            delta,
                            subagent_id: Some(subagent_id.to_string()),
                        });
                    }
                    ProviderStreamEvent::ToolCall { id, name, input } => calls.push((id, name, input)),
                    ProviderStreamEvent::Usage { .. } => {}
                    ProviderStreamEvent::Done { stop_reason: reason } => stop_reason = reason,
                    ProviderStreamEvent::Error(err) => return Err(format!("Subagent stream error: {}", err)),
                }
            }

            let mut blocks = Vec::new();
            if !thinking.is_empty() {
                blocks.push(Block::Thinking { thinking });
            }
            if !text.is_empty() {
                blocks.push(Block::Text { text: text.clone() });
                last_text = text;
            }
            for (id, name, input) in &calls {
                blocks.push(Block::ToolUse {
                    id:    id.clone(),
                    name:  name.clone(),
                    input: input.clone(),
                });
            }

            let run_tools = stop_reason == StopReason::ToolUse && !calls.is_empty();
            let mut assistant_id = None;
            if !blocks.is_empty() {
                let msg = ChatMessage {
                    id:         uuid::Uuid::new_v4().to_string(),
                    parent_id:  messages.last().map(|m| m.id.clone()),
                    role:       Role::Assistant,
                    content:    blocks,
                    created_at: chrono::Utc::now().timestamp_millis(),
                };
                assistant_id = Some(msg.id.clone());
                messages.push(msg);
            }

            if !run_tools {
                if !calls.is_empty() {
                    if let Some(parent) = assistant_id {
                        messages.push(subagent_tool_results(
                            &parent,
                            calls
                                .iter()
                                .map(|(id, _, _)| (id.clone(), "Tool call not executed.".to_string(), true))
                                .collect(),
                        ));
                    }
                }
                break;
            }

            let parent = assistant_id.expect("assistant message pushed for tool use");
            let mut results = Vec::with_capacity(calls.len());
            let mut cancelled = false;
            for (call_id, tool_name, tool_input) in calls {
                if cancelled {
                    results.push((call_id, "Tool call cancelled.".to_string(), true));
                    continue;
                }
                let (output, was_cancelled) = room
                    .execute_tool_call(
                        &call_id,
                        &tool_name,
                        tool_input,
                        allowed.as_ref(),
                        &cancel,
                        Some(subagent_id),
                    )
                    .await;
                cancelled = was_cancelled;
                results.push((call_id, output.output, output.is_error));
            }
            messages.push(subagent_tool_results(&parent, results));
            if cancelled {
                return Err("Subagent cancelled.".into());
            }
        }

        if last_text.trim().is_empty() {
            return Ok("(subagent finished without a textual summary)".to_string());
        }
        Ok(last_text)
    }
}

fn subagent_tool_results(parent_id: &str, results: Vec<(String, String, bool)>) -> ChatMessage {
    ChatMessage {
        id:         uuid::Uuid::new_v4().to_string(),
        parent_id:  Some(parent_id.to_string()),
        role:       Role::User,
        content:    results
            .into_iter()
            .map(|(tool_use_id, content, is_error)| Block::ToolResult {
                tool_use_id,
                content,
                is_error,
            })
            .collect(),
        created_at: chrono::Utc::now().timestamp_millis(),
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

        let result = self.run_inner(agent, prompt, &subagent_id).await;

        self.room.broadcast(AgentEvent::TurnFinished {
            turn_id,
            stop_reason: if result.is_ok() {
                StopReason::EndTurn
            } else {
                StopReason::Error
            },
            usage: TokenUsage::default(),
            subagent_id: Some(subagent_id),
        });

        result
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
        assert_eq!(arbiter.pending_count().await, 0);

        let decision = rx.await.unwrap();
        assert_eq!(decision, ApprovalDecision::AllowOnce);
    }

    /// 超时/取消路径必须回收等待项，否则注册表随轮次无限增长。
    #[tokio::test]
    async fn test_approval_arbiter_forget_releases_slot() {
        let arbiter = ApprovalArbiter::new();
        let _rx = arbiter.register("req_timeout".into()).await;
        assert_eq!(arbiter.pending_count().await, 1);

        arbiter.forget("req_timeout").await;
        assert_eq!(arbiter.pending_count().await, 0);
        // 已回收的请求不再接受迟到响应
        assert!(
            !arbiter
                .resolve("req_timeout", ApprovalDecision::AllowOnce)
                .await
        );
    }

    fn text(role: Role, id: &str, parent: Option<&str>, body: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            role,
            content: vec![Block::Text { text: body.into() }],
            created_at: 0,
        }
    }

    fn tool_use_msg(id: &str, parent: &str, call_id: &str) -> ChatMessage {
        ChatMessage {
            id:         id.into(),
            parent_id:  Some(parent.into()),
            role:       Role::Assistant,
            content:    vec![Block::ToolUse {
                id:    call_id.into(),
                name:  "read".into(),
                input: serde_json::json!({ "path": "big.rs" }),
            }],
            created_at: 0,
        }
    }

    fn tool_result_msg(id: &str, parent: &str, call_id: &str, body: &str) -> ChatMessage {
        ChatMessage {
            id:         id.into(),
            parent_id:  Some(parent.into()),
            role:       Role::User,
            content:    vec![Block::ToolResult {
                tool_use_id: call_id.into(),
                content:     body.into(),
                is_error:    false,
            }],
            created_at: 0,
        }
    }

    /// 压缩后仍然必须是合法对话：首条为用户输入，且每个 tool_result 都有前置 tool_use。
    fn assert_well_formed(messages: &[ChatMessage]) {
        assert!(!messages.is_empty(), "compaction must not empty the history");
        assert!(
            is_turn_start(&messages[0]),
            "history must start at a user turn boundary, got role={:?} blocks={:?}",
            messages[0].role,
            messages[0].content
        );

        let mut seen_tool_use = HashSet::new();
        for m in messages {
            for b in &m.content {
                match b {
                    Block::ToolUse { id, .. } => {
                        seen_tool_use.insert(id.clone());
                    }
                    Block::ToolResult { tool_use_id, .. } => {
                        assert!(
                            seen_tool_use.contains(tool_use_id),
                            "orphan tool_result {} without preceding tool_use",
                            tool_use_id
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn test_compaction_keeps_tool_use_result_pairing() {
        // 构造 6 轮长会话，每轮含一次工具调用与其回执（回执为 role=user 的内部消息）
        let mut messages = Vec::new();
        let mut parent: Option<String> = None;
        for i in 0..6 {
            let user_id = format!("u{}", i);
            messages.push(text(Role::User, &user_id, parent.as_deref(), &"问".repeat(400)));
            let assistant_id = format!("a{}", i);
            messages.push(tool_use_msg(&assistant_id, &user_id, &format!("call{}", i)));
            let result_id = format!("r{}", i);
            messages.push(tool_result_msg(
                &result_id,
                &assistant_id,
                &format!("call{}", i),
                &"答".repeat(4000),
            ));
            let reply_id = format!("t{}", i);
            messages.push(text(Role::Assistant, &reply_id, Some(&result_id), &"ok".repeat(400)));
            parent = Some(reply_id);
        }

        let before = messages.len();
        compact_messages(&mut messages, 4000); // 极小窗口，强制两阶段压缩都生效

        assert!(messages.len() < before, "compaction should drop old turns");
        assert_well_formed(&messages);
        // 最后一个轮次必须完整保留（用户能看到自己刚发的消息）
        assert!(
            messages.iter().any(|m| m.id == "u5"),
            "the latest turn must survive compaction"
        );
    }

    #[test]
    fn test_compaction_within_budget_is_noop() {
        let mut messages = vec![
            text(Role::User, "u1", None, "hello"),
            text(Role::Assistant, "a1", Some("u1"), "hi"),
        ];
        let original = messages.clone();
        compact_messages(&mut messages, 128_000);
        assert_eq!(messages, original);
    }

    /// 旧实现用「消息条数一半」切窗，会切出孤儿 tool_result 或以 assistant 开头；
    /// 单轮次历史没有更早的轮次可裁，必须整轮原样保留。
    #[test]
    fn test_compaction_never_splits_single_turn() {
        let mut messages = vec![
            text(Role::User, "u1", None, &"问".repeat(2000)),
            tool_use_msg("a1", "u1", "call1"),
            tool_result_msg("r1", "a1", "call1", &"答".repeat(8000)),
        ];
        compact_messages(&mut messages, 2000);
        assert_well_formed(&messages);
        // 一条都不许丢：轮次内部不允许按条数切窗
        assert_eq!(
            messages.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["u1", "a1", "r1"]
        );
    }

    /// 轮内多轮工具调用：旧的冗长回执被替换为占位，最后一条（模型当前要读的输出）保持完整。
    #[test]
    fn test_compaction_prunes_old_results_within_turn() {
        let mut messages = vec![text(Role::User, "u1", None, &"问".repeat(400))];
        let mut parent = "u1".to_string();
        for i in 0..4 {
            let assistant_id = format!("a{}", i);
            let result_id = format!("r{}", i);
            messages.push(tool_use_msg(&assistant_id, &parent, &format!("call{}", i)));
            messages.push(tool_result_msg(
                &result_id,
                &assistant_id,
                &format!("call{}", i),
                &"答".repeat(4000),
            ));
            parent = result_id;
        }

        compact_messages(&mut messages, 4000);
        assert_well_formed(&messages);

        let pruned = messages
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, Block::ToolResult { content, .. } if content == PRUNED_TOOL_RESULT))
            .count();
        assert!(pruned >= 1, "old tool results must be pruned");

        // 最后一条消息的内容必须保持原样
        let last = messages.last().unwrap();
        assert_eq!(last.id, "r3");
        assert!(
            matches!(&last.content[0], Block::ToolResult { content, .. } if content.chars().count() == 4000),
            "the newest message must survive intact"
        );
    }

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(&[0xFF, 0xFE, 0xFD]), "//79");
    }

    #[test]
    fn test_guess_mime() {
        assert_eq!(guess_mime("a.png"), "image/png");
        assert_eq!(guess_mime("a.JPG"), "image/jpeg");
        assert_eq!(guess_mime("a.jpeg"), "image/jpeg");
        assert_eq!(guess_mime("a.gif"), "image/gif");
        assert_eq!(guess_mime("a.webp"), "image/webp");
        assert_eq!(guess_mime("noext"), "image/png");
    }

    /// 队列必须被完整排空：此前 `finish_turn` 用深拷贝房间接管下一条指令，
    /// 拷贝出来的空队列会永久丢弃后续排队输入。
    #[tokio::test]
    async fn test_queued_commands_are_drained_in_order() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_q", "/w", "T", "missing/model", "task", ApprovalMode::Normal)
            .await?;

        let room = SessionRoom::new(
            "s_q",
            PathBuf::from(tmp.path()),
            storage.clone(),
            Arc::new(RwLock::new(OmaConfig::default())),
            ToolRegistry::new(),
            "missing/model",
            "task",
            ApprovalMode::Normal,
        );

        let submit = |text: &str| {
            let room = room.clone();
            let text = text.to_string();
            async move {
                room.submit_command(
                    "cli",
                    "CLI",
                    ClientType::Cli,
                    AgentCommand::UserInput {
                        content:     text,
                        attachments: vec![],
                    },
                )
                .await;
            }
        };

        // 首条指令占用轮次（未配置模型会立即以 Error 收尾），其余入队
        for text in ["c1", "c2", "c3"] {
            submit(text).await;
            tokio::task::yield_now().await;
        }
        // 等待队列排空（每条都会因缺少模型而快速结束）
        for _ in 0..200 {
            if !room.is_busy() && room.command_queue.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let all = storage.get_all_messages("s_q").await?;
        let texts: Vec<String> = all
            .iter()
            .filter(|m| m.role == Role::User)
            .flat_map(|m| &m.content)
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["c1", "c2", "c3"], "every queued input must run, in order");
        assert!(!room.is_busy());
        assert_eq!(room.command_queue.lock().await.len(), 0);
        Ok(())
    }

    /// 工具白名单：模板声明只读时，写工具不可见亦不可执行。
    #[test]
    fn test_allowed_tools_from_template() {
        let template = AgentTemplate {
            id:                 "explore".into(),
            name:               "Explore".into(),
            description:        String::new(),
            tools:              vec!["read".into(), "shell".into()],
            system_prompt_body: String::new(),
        };
        let allowed = allowed_tools(&template).unwrap();
        assert!(allowed.contains("read") && allowed.contains("shell"));
        assert!(!allowed.contains("write"));

        let unrestricted = AgentTemplate {
            tools: Vec::new(),
            ..template
        };
        assert!(allowed_tools(&unrestricted).is_none());
    }
}
