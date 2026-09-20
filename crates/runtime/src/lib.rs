use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use oma_config::{AgentLoader, AgentTemplate, OmaConfig};
use oma_contract::{
    ActiveTurnCatchUp, AgentCommand, AgentEvent, Block, ChatMessage, ClientType, Role, StopReason, TokenUsage,
    ToolCallStartedData, ToolImage, ToolOutput,
};
use oma_provider::{ModelConfig, ProviderStreamEvent, UniversalProvider};
use oma_storage::{StorageError, StorageManager};
use oma_tool::{SubagentRunner, ToolContext, ToolRegistry};
use parking_lot::RwLock;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;

/// 子 Agent 单次委托的最大工具轮次，防止无限自我调用
const MAX_SUBAGENT_ROUNDS: usize = 16;
/// 上下文压缩后保留的工具结果占位文案
const PRUNED_TOOL_RESULT: &str = "[工具执行结果已截断修剪以节省上下文窗口]";
/// 超过该字符数的工具结果在压缩时会被替换为占位
const PRUNE_MIN_CHARS: usize = 300;

/// 把会话推理等级落到请求参数上：
/// `resolve_reasoning_effort` 已处理「映射为空 = 不下发」，这里把结果回写到
/// `reasoning_effort`，各协议组装逻辑无需感知等级概念。
///
/// **仅对支持思考的模型生效**：不支持思考的模型收到 reasoning 类参数可能被
/// 厂商拒绝或产生非预期行为，等级配置在那类模型上无意义。
fn apply_reasoning_level(model_cfg: &mut ModelConfig, level: &str) {
    if !model_cfg.supports_thinking() {
        model_cfg.reasoning_effort = String::new();
        return;
    }
    model_cfg.reasoning_effort = model_cfg
        .resolve_reasoning_effort(level)
        .unwrap_or_default();
}

/// 会话自动命名的系统提示：输出要短、无修饰、不改写语言
const NAMING_SYSTEM_PROMPT: &str = "You name chat sessions. Read the conversation and reply with ONLY a \
short title: 2-6 words, no quotes, no trailing punctuation, same language as the user. \
Output the title and nothing else.";

/// 自动命名标题长度上限（字符）
const TITLE_MAX_CHARS: usize = 60;
/// 构造命名提示词所需的对话文本上限
const NAMING_CONTEXT_CHARS: usize = 4000;

/// 从会话历史构造命名提示词；无有效内容（如只有仅带工具的轮次）时返回 None。
fn build_naming_prompt(messages: &[ChatMessage]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for m in messages {
        let role = match m.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => continue,
        };
        let text: String = m
            .content
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        parts.push(format!("{}: {}", role, text));
        if parts.iter().map(|p| p.chars().count()).sum::<usize>() >= NAMING_CONTEXT_CHARS {
            break;
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut joined = parts.join("\n\n");
    if joined.chars().count() > NAMING_CONTEXT_CHARS {
        joined = joined.chars().take(NAMING_CONTEXT_CHARS).collect();
    }
    Some(format!(
        "Conversation so far:\n\n{}\n\nReply with only a short title for this session.",
        joined
    ))
}

/// 将模型输出清洗为可用标题：去掉引号/前缀/换行，超长截断；无有效内容时返回 None。
fn sanitize_title(raw: &str) -> Option<String> {
    let first_line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let cleaned = first_line
        .trim_start_matches(|c: char| c == '#' || c == '-' || c.is_whitespace())
        .trim()
        // 模型常把标题包在引号或书名号里
        .trim_matches(|c: char| matches!(c, '"' | '\'' | '“' | '”' | '‘' | '’' | '《' | '》' | '`'))
        .trim_start_matches("Title:")
        .trim_start_matches("标题：")
        .trim()
        .to_string();
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.chars().take(TITLE_MAX_CHARS).collect())
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

/// 权威上下文锚点。
///
/// 厂商每次响应都会上报该次请求的真实 `input_tokens`，其中已包含 system prompt 与
/// 工具声明等固定开销。把它记下来，只需对锚点之后**新增**的消息做启发式增量，
/// 就能得到远比纯字符折算准确的占用估计——纯折算对英文与代码会高估约一倍。
///
/// 锚点以「历史列表的前 `covered` 条」为基准；历史在轮次内只追加、不重排，
/// 因此前缀被裁剪（压缩）后锚点自动失效并回退启发式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenAnchor {
    /// 记录时刻的请求所覆盖的条数（即当次请求发出时的历史长度）
    covered: usize,
    /// 该前缀对应的权威输入 token 数（含固定开销）
    tokens:  usize,
}

impl TokenAnchor {
    /// 记录一次真实请求的结果。`covered` 为该请求发出时的历史长度。
    /// `input_tokens` 为 0 表示厂商未上报用量，返回 None 以保留既有锚点。
    pub fn record(covered: usize, input_tokens: usize) -> Option<Self> {
        (input_tokens > 0).then_some(Self {
            covered,
            tokens: input_tokens,
        })
    }

    /// 估算当前消息列表的上下文占用。
    ///
    /// 列表必须以锚点覆盖的前缀开头；前缀已被裁剪（压缩）时锚点不再适用，
    /// 返回 None 由调用方回退到纯启发式（偏保守）。
    pub fn estimate(&self, messages: &[ChatMessage]) -> Option<usize> {
        if messages.len() < self.covered {
            return None;
        }
        Some(self.tokens + estimate_tokens(&messages[self.covered..]))
    }
}

/// 真实用户轮次起点：role=user、含文本或图片，且不是工具回执。
///
/// 不能只判断「含文本或图片」：工具回执里可能附有图片（见 read 读图），
/// 那样它会被当成新一轮，压缩时把轮次从中间切断、留下孤儿 tool_result。
fn is_turn_start(message: &ChatMessage) -> bool {
    if message.role != Role::User {
        return false;
    }
    if message
        .content
        .iter()
        .any(|b| matches!(b, Block::ToolResult { .. }))
    {
        return false;
    }
    message
        .content
        .iter()
        .any(|b| matches!(b, Block::Text { .. } | Block::Image { .. }))
}

/// 执行两阶段压缩策略：达到 70% 阈值时修剪旧 ToolResult 并按整轮裁剪前缀。
///
/// `anchor` 为最近一次请求的权威输入 token 锚点；命中时以其为基准（并只对新增
/// 消息做启发式增量），未命中时回退纯字符折算。
pub fn compact_messages(messages: &mut Vec<ChatMessage>, context_len: usize, anchor: Option<TokenAnchor>) {
    let threshold = (context_len as f64 * 0.7) as usize;

    // 权威锚点只适用于「以历史前缀开头的完整列表」——即尚未裁剪的当前历史。
    // 一旦开始丢弃消息，被保留段的权威占比无从得知，只能回退字符折算（偏保守，
    // 宁可多裁一点也不会低估后超窗）。锚点未命中时同样回退。
    let anchored = |list: &[ChatMessage]| -> usize {
        anchor
            .and_then(|a| a.estimate(list))
            .unwrap_or_else(|| estimate_tokens(list))
    };

    if anchored(messages) <= threshold {
        return;
    }

    // 第一阶段：把冗长 ToolResult 的内容替换为占位。
    // 始终保留最后一条消息（模型当前要处理的输出）；替换内容不改变
    // tool_use / tool_result 的配对关系，且不改变条数，故锚点仍然适用。
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

    if anchored(messages) <= threshold {
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
        // 候选是历史的尾部切片，锚点前缀已不完整：用字符折算评估保留量
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
// 因此多个客户端/后台任务看到的是同一份模型与命令队列。
// （此前的 `impl Clone` 会深拷贝这些字段，导致排队指令被丢弃，已删除。）

pub struct SessionRoom {
    pub session_id:      String,
    pub workspace:       PathBuf,
    pub storage:         StorageManager,
    pub config:          Arc<RwLock<OmaConfig>>,
    pub tools:           ToolRegistry,
    pub active_model:    RwLock<String>,
    pub active_agent:    RwLock<String>,
    /// 当前会话推理等级（REASONING_LEVELS 之一；空 = 未设置，回退模型默认）
    pub reasoning_level: RwLock<String>,
    pub event_tx:        broadcast::Sender<AgentEvent>,
    pub command_queue:   Mutex<VecDeque<AgentCommand>>,
    pub active_turn:     RwLock<Option<ActiveTurnState>>,
    pub cancel_token:    RwLock<CancellationToken>,
    /// 轮次占用权：由 CAS 抢占，确保同一房间任意时刻至多一个执行中的轮次
    pub is_running:      AtomicBool,
    /// 插件宿主（工具已在装配期注册进 ToolRegistry；此处用于 `tool_call` 钩子）
    pub plugins:         RwLock<Option<Arc<oma_plugin::PluginHost>>>,
    /// 本进程内是否已发起过自动命名：标题一旦生成就不再重复请求模型
    named:               AtomicBool,
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
            reasoning_level: RwLock::new(String::new()),
            event_tx,
            command_queue: Mutex::new(VecDeque::new()),
            active_turn: RwLock::new(None),
            cancel_token: RwLock::new(CancellationToken::new()),
            is_running: AtomicBool::new(false),
            plugins: RwLock::new(None),
            named: AtomicBool::new(false),
        })
    }

    /// 注入插件宿主：工具已在装配期注册进 `ToolRegistry`，此处提供 `tool_call` 钩子。
    pub fn set_plugins(&self, host: Arc<oma_plugin::PluginHost>) {
        *self.plugins.write() = Some(host);
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
            AgentCommand::SetReasoningLevel { level } => {
                // 校验收敛在服务端：非法等级一律拒绝，避免脏值落库
                if !oma_contract::is_valid_reasoning_level(&level) {
                    self.broadcast(AgentEvent::Error {
                        message: format!("Invalid reasoning level: {}", level),
                    });
                    return;
                }
                *self.reasoning_level.write() = level.clone();
                let _ = self
                    .storage
                    .update_session_settings(&self.session_id, None, None, Some(&level))
                    .await;
                self.broadcast(AgentEvent::ReasoningLevelChanged { level });
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
            self.broadcast(AgentEvent::SessionRunning {
                session_id: self.session_id.clone(),
                running:    true,
            });
            let room = self.clone();
            tokio::spawn(async move {
                room.run_user_turn(content, attachments, parent_id_override)
                    .await;
            });
        } else {
            // 排队时必须连同分叉点一起存：只压一份裸的 `UserInput` 会让编辑重发
            // 轮到执行时退化成普通追加，分叉点静默丢失。
            let queued = match parent_id_override {
                Some(parent_message_id) => AgentCommand::ForkAndRun {
                    parent_message_id,
                    new_content: Some(content),
                },
                None => AgentCommand::UserInput { content, attachments },
            };
            self.command_queue.lock().await.push_back(queued);
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

    /// 执行单个工具调用：白名单 → 插件钩子 → 执行。
    /// 返回 (工具输出, 轮次是否被取消)。
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
            tool_name:   tool_name.to_string(),
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

        // 1. 插件 tool_call 钩子：任一插件返回 block 即拦截（插件为同步 JS，放到阻塞线程池）
        let plugin_host = self.plugins.read().clone();
        if let Some(host) = plugin_host {
            let name = tool_name.to_string();
            let input = tool_input.clone();
            let blocked = tokio::task::spawn_blocking(move || host.before_tool_call(&name, &input))
                .await
                .unwrap_or(None);
            if let Some(reason) = blocked {
                return (
                    self.finish_tool_call(
                        call_id,
                        tool_name,
                        ToolOutput::error(format!("Execution blocked by plugin: {}", reason)),
                        subagent_id,
                    )
                    .await,
                    false,
                );
            }
        }

        // 4. 执行（可被取消信号中断，避免长命令拖住整个轮次）
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
        // 先取出模型能力再进 select：临时值不能在 select 分支里悬空
        let vision_ctx = ImageInputCtx(self.model_supports_vision());
        let output = tokio::select! {
            biased;
            _ = cancel.cancelled() => ToolOutput::error("Tool execution cancelled."),
            out = tool.execute_with(&self.workspace, tool_input, &vision_ctx) => out,
        };
        let cancelled = cancel.is_cancelled();
        (
            self.finish_tool_call(call_id, tool_name, output, subagent_id)
                .await,
            cancelled,
        )
    }

    /// 当前模型能否直接接收图片输入。
    ///
    /// 只认 `ImageInput` 能力：它同时驱动 provider 侧的图片编码与会话
    /// 内的 `read` 图像回传，两处判断必须同源，否则会把图发给看不见图的模型。
    fn model_supports_vision(&self) -> bool {
        let selector = self.active_model.read().clone();
        self.config
            .read()
            .find_model(&selector)
            .is_some_and(|(_, m)| m.supports_image_input())
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
            tool_name:   tool_name.to_string(),
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
            // 引用形态的附件在发起 Provider 请求前内联（见 inline_attachments）
            user_blocks.push(Block::Image {
                mime_type: "image/png".into(),
                data:      att,
            });
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

        // 标题为空说明本会话从未成功命名过，允许本轮在首次回复后进行命名；
        // 标题非空（用户已填或此前已生成）则本次轮次内不再浪费一次模型请求
        self.named.store(
            self.storage
                .get_session(&self.session_id)
                .await
                .ok()
                .flatten()
                .is_none_or(|r| !r.title.trim().is_empty()),
            Ordering::SeqCst,
        );

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
        // 首次模型回复落库后即可命名：不等整轮结束，界面侧栏标题尽早出现
        let mut naming_attempted = self.named.load(Ordering::SeqCst);
        // 最近一次请求的权威输入 token 锚点：让后续轮次的预算检查以厂商上报的
        // 真实占用为基准，只对新增消息做启发式增量（见 TokenAnchor）。
        let mut anchor: Option<TokenAnchor> = None;

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

            // 本次请求覆盖的历史前缀长度（用于回填权威锚点）
            let covered = history.len();

            // 上下文压缩（只作用于本次请求的副本，持久化历史保持不变）
            let mut messages = history.clone();
            compact_messages(&mut messages, model_cfg.context_len, anchor);
            // session_attachment:// 引用在此内联为 data URI，各厂商协议拿到的都是完整载荷
            self.inline_attachments(&mut messages).await;

            // 拼装 System Prompt 与按 Agent 模板裁剪的工具清单
            let system_prompt = AgentLoader::build_system_prompt(&template, &self.workspace, &active_model_sel);
            let tools_defs = self.tools.to_definitions(&template.tools);
            let allowed = allowed_tools(&template);

            // 发起 Provider 请求：会话推理等级在此映射为厂商可识别参数
            let mut model_cfg = model_cfg.clone();
            apply_reasoning_level(&mut model_cfg, &self.reasoning_level.read());
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
            // 本次请求的权威输入占用（Anthropic 在 message_start 上报，OpenAI 等在收尾）
            let mut request_input_tokens = 0usize;

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
                        if input_tokens > 0 {
                            request_input_tokens = input_tokens;
                        }
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

            // 回填权威锚点：本次请求的真实输入占用对应 history[..covered]。
            // 厂商未上报用量时保留旧锚点，绝不写入 0 覆盖。
            if let Some(recorded) = TokenAnchor::record(covered, request_input_tokens) {
                anchor = Some(recorded);
                // 每次请求后广播上下文占用，供界面进度条实时更新
                self.broadcast(AgentEvent::ContextUsage {
                    tokens:      request_input_tokens,
                    context_len: model_cfg.context_len,
                });
                // 同时落库：重启后无内存态，靠它恢复进度条。
                // `covered` 一并存下，恢复时才能只估算新增部分
                let _ = self
                    .storage
                    .set_context_usage(&self.session_id, request_input_tokens, model_cfg.context_len, covered)
                    .await;
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

                // 首次回复已完成：立刻后台命名，不再等整轮（含工具调用）结束
                if !naming_attempted {
                    naming_attempted = true;
                    let room = self.clone();
                    let snapshot = history.clone();
                    tokio::spawn(async move { room.maybe_autoname(snapshot).await });
                }
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
                                .map(|(id, _, _)| (id.clone(), reason.to_string(), true, Vec::new()))
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
            let mut results: Vec<(String, String, bool, Vec<ToolImage>)> =
                Vec::with_capacity(assistant_tool_calls.len());
            let mut cancelled = false;
            for (call_id, tool_name, tool_input) in assistant_tool_calls {
                if cancelled {
                    results.push((call_id, "Tool call cancelled.".into(), true, Vec::new()));
                    continue;
                }
                let (output, was_cancelled) = self
                    .execute_tool_call(&call_id, &tool_name, tool_input, allowed.as_ref(), cancel_token, None)
                    .await;
                cancelled = was_cancelled;
                results.push((call_id, output.output, output.is_error, output.images));
            }

            self.append_tool_results(&parent_id, results, history).await;

            if cancelled {
                return TurnOutcome::Finished(StopReason::Cancelled);
            }
        }
    }

    /// 持久化工具回执消息（role = user），并同步到内存历史
    ///
    /// `results` 为 (tool_use_id, 文本, is_error, 图片)。图片与回执同处一条消息：
    /// 它属于工具输出而非用户发言，落库形态与展示形态都据此统一（见
    /// `isInternalMessage`）。只有 Anthropic 能原样消费这种结构，completion /
    /// response 在发请求时才把图片拆到紧随其后的用户消息里（协议翻译属 provider 职责）。
    async fn append_tool_results(
        &self,
        parent_id: &str,
        results: Vec<(String, String, bool, Vec<ToolImage>)>,
        history: &mut Vec<ChatMessage>,
    ) {
        if results.is_empty() {
            return;
        }
        let blocks: Vec<Block> = results
            .into_iter()
            .flat_map(|(tool_use_id, content, is_error, images)| {
                let mut out = vec![Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                }];
                out.extend(images.into_iter().map(|img| Block::Image {
                    mime_type: img.mime_type,
                    data:      img.data,
                }));
                out
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

    /// 首次模型回复落库后自动命名：标题为空时用小模型生成一次。
    ///
    /// 传入的 `messages` 是本轮已有的线性历史快照，避免再全量回读存储。
    /// 用户在创建时已填写或自己重命名过则跳过（由 `set_title_if_empty` 原子校验）。
    /// 任何失败都只记录不影响轮次结果。
    async fn maybe_autoname(self: &Arc<Self>, messages: Vec<ChatMessage>) {
        let Some(prompt) = build_naming_prompt(&messages) else {
            return;
        };

        let active_model = self.active_model.read().clone();
        let (provider_cfg, model_cfg) = {
            let cfg = self.config.read();
            match cfg.find_model(&active_model) {
                Some((p, m)) => (p.clone(), m),
                None => return,
            }
        };

        let request = vec![ChatMessage {
            id:         "autoname".to_string(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![Block::Text { text: prompt }],
            created_at: chrono::Utc::now().timestamp_millis(),
        }];

        // 复用流式接口：命名请求不带工具、不需要 thinking，仅拼接文本增量
        let provider = UniversalProvider::new(provider_cfg);
        let mut rx = match provider
            .send_stream(&request, Some(NAMING_SYSTEM_PROMPT), &[], &model_cfg)
            .await
        {
            Ok(rx) => rx,
            Err(_) => return,
        };

        let mut raw = String::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                ProviderStreamEvent::TextDelta(d) => raw.push_str(&d),
                ProviderStreamEvent::Error(_) | ProviderStreamEvent::Done { .. } => break,
                _ => {}
            }
            if raw.chars().count() > 512 {
                break; // 命名结果很短，防止异常长输出
            }
        }

        let Some(title) = sanitize_title(&raw) else {
            return;
        };
        if let Ok(true) = self
            .storage
            .set_title_if_empty(&self.session_id, &title)
            .await
        {
            self.broadcast(AgentEvent::SessionRenamed {
                session_id: self.session_id.clone(),
                title,
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

        // 自动命名不在此处触发：模型首次回复落库时已后台发起（见 agent_loop），
        // 单靠工具调用、没有文本产出的轮次自然也没有可命名的对话内容
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
                (Some(cmd), pending) => {
                    self.broadcast(AgentEvent::QueueUpdated { pending });
                    let Some((content, attachments, parent_id)) = queued_run(cmd) else {
                        continue; // 不认识的指令就地丢弃，继续排空
                    };
                    let room = self.clone();
                    tokio::spawn(async move {
                        room.run_user_turn(content, attachments, parent_id).await;
                    });
                    return; // 交棒给下一轮，由它负责后续排空
                }
                (None, _) => {
                    self.is_running.store(false, Ordering::SeqCst);
                    if self.command_queue.lock().await.is_empty() {
                        // 确实不再有后续轮次时才广播：交棒或被他轮接管时不发，
                        // 否则会把仍在运行的状态误报为已结束
                        self.broadcast(AgentEvent::SessionRunning {
                            session_id: self.session_id.clone(),
                            running:    false,
                        });
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

/// 把「当前模型能不能看图」包成工具可用的上下文
struct ImageInputCtx(bool);

impl ToolContext for ImageInputCtx {
    fn supports_image_input(&self) -> bool {
        self.0
    }
}

/// 把队列指令还原为「要跑的一轮输入」。
///
/// 队列里只会出现用户输入类指令：普通输入，以及带分叉点的编辑重发。
/// 其余指令不应入队，真出现了就地丢弃（返回 `None`）。
fn queued_run(cmd: AgentCommand) -> Option<(String, Vec<String>, Option<String>)> {
    match cmd {
        AgentCommand::UserInput { content, attachments } => Some((content, attachments, None)),
        AgentCommand::ForkAndRun {
            parent_message_id,
            new_content: Some(content),
        } => Some((content, Vec::new(), Some(parent_message_id))),
        _ => None,
    }
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
        // 子 Agent 会连续多轮调用工具，上下文同样需要受窗口约束
        let mut anchor: Option<TokenAnchor> = None;

        for _round in 0..MAX_SUBAGENT_ROUNDS {
            if cancel.is_cancelled() {
                return Err("Subagent cancelled.".into());
            }

            let covered = messages.len();
            // 压缩只作用于本次请求的副本，子 Agent 的内存历史保持完整
            let mut request = messages.clone();
            compact_messages(&mut request, model_cfg.context_len, anchor);

            // 子 Agent 继承会话推理等级，保证主/子轮次行为一致
            let mut sub_model_cfg = model_cfg.clone();
            apply_reasoning_level(&mut sub_model_cfg, &self.room.reasoning_level.read());
            let provider = UniversalProvider::new(provider_cfg.clone());
            let mut stream_rx = provider
                .send_stream(&request, Some(&system_prompt), &tools_defs, &sub_model_cfg)
                .await
                .map_err(|e| format!("Subagent provider error: {}", e))?;

            let mut thinking = String::new();
            let mut text = String::new();
            let mut calls: Vec<(String, String, serde_json::Value)> = Vec::new();
            let mut stop_reason = StopReason::EndTurn;
            let mut request_input_tokens = 0usize;

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
                    ProviderStreamEvent::Usage { input_tokens, .. } => {
                        if input_tokens > 0 {
                            request_input_tokens = input_tokens;
                        }
                    }
                    ProviderStreamEvent::Done { stop_reason: reason } => stop_reason = reason,
                    ProviderStreamEvent::Error(err) => return Err(format!("Subagent stream error: {}", err)),
                }
            }

            if let Some(recorded) = TokenAnchor::record(covered, request_input_tokens) {
                anchor = Some(recorded);
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
                                .map(|(id, _, _)| (id.clone(), "Tool call not executed.".to_string(), true, Vec::new()))
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
                    results.push((call_id, "Tool call cancelled.".to_string(), true, Vec::new()));
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
                results.push((call_id, output.output, output.is_error, output.images));
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

/// 子 Agent 的工具回执：与主轮次同构，图片按协议需求附在同一消息内。
///
/// 子 Agent 的上下文不落库也不渲染，不存在「被当作用户消息」的问题，
/// 因此统一沿用 Anthropic 的内联形态，由各协议自行拆解。
fn subagent_tool_results(parent_id: &str, results: Vec<(String, String, bool, Vec<ToolImage>)>) -> ChatMessage {
    ChatMessage {
        id:         uuid::Uuid::new_v4().to_string(),
        parent_id:  Some(parent_id.to_string()),
        role:       Role::User,
        content:    results
            .into_iter()
            .flat_map(|(tool_use_id, content, is_error, images)| {
                let mut out = vec![Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                }];
                // 子 Agent 同样支持图片回传，否则「让子代理看图」会静默降级
                out.extend(images.into_iter().map(|img| Block::Image {
                    mime_type: img.mime_type,
                    data:      img.data,
                }));
                out
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

    /// 带图片的工具回执（read 读图）不得被当成真实用户轮次起点。
    #[test]
    fn test_tool_result_with_image_is_not_a_turn_start() {
        let mut msg = tool_result_msg("r1", "a1", "call_1", "mime: image/png");
        msg.content.push(Block::Image {
            mime_type: "image/png".into(),
            data:      "QUJD".into(),
        });
        assert!(
            !is_turn_start(&msg),
            "an image carried by a tool result must not open a new turn"
        );

        // 对照：真实用户消息（文本或纯图片）仍是轮次起点
        let mut user_image = ChatMessage {
            id:         "u1".into(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![Block::Image {
                mime_type: "image/png".into(),
                data:      "QUJD".into(),
            }],
            created_at: 0,
        };
        assert!(is_turn_start(&user_image), "a user-sent image opens a turn");
        user_image.content.push(Block::Text { text: "hi".into() });
        assert!(is_turn_start(&user_image));
        assert!(!is_turn_start(&tool_result_msg("r2", "a1", "call_2", "ok")));
    }

    /// 回执带图的会话压缩后同样合法：图片不得把轮次边界切到回执中间。
    #[test]
    fn test_compaction_survives_image_tool_results() {
        let mut messages = Vec::new();
        let mut parent: Option<String> = None;
        for i in 0..6 {
            let user_id = format!("u{i}");
            messages.push(ChatMessage {
                id:         user_id.clone(),
                parent_id:  parent.clone(),
                role:       Role::User,
                content:    vec![Block::Text {
                    text: format!("看看图 {i} {}", "详情".repeat(200)),
                }],
                created_at: i as i64,
            });
            let call_id = format!("call_{i}");
            let assistant_id = format!("a{i}");
            messages.push(tool_use_msg(&assistant_id, &user_id, &call_id));
            let mut result = tool_result_msg(&format!("r{i}"), &assistant_id, &call_id, &"结果".repeat(200));
            result.content.push(Block::Image {
                mime_type: "image/png".into(),
                data:      "QUJD".into(),
            });
            messages.push(result);
            parent = Some(format!("r{i}"));
        }

        compact_messages(&mut messages, 4_000, None);
        assert!(messages.len() < 18, "compaction should have dropped turns");
        assert_well_formed(&messages);
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
        compact_messages(&mut messages, 4000, None); // 极小窗口，强制两阶段压缩都生效

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
        compact_messages(&mut messages, 128_000, None);
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
        compact_messages(&mut messages, 2000, None);
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

        compact_messages(&mut messages, 4000, None);
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

    /// 锚点必须用权威值替代启发式：估算 = 权威输入 + 新增消息的启发式增量。
    #[test]
    fn test_token_anchor_prefers_authoritative_count() {
        let messages = vec![
            text(Role::User, "u1", None, "hello"),
            text(Role::Assistant, "a1", Some("u1"), "world"),
            text(Role::User, "u2", Some("a1"), &"新".repeat(100)),
        ];
        // 权威值远大于纯字符折算：估算必须以权威值为基准
        let anchor = TokenAnchor::record(2, 50_000).unwrap();
        let estimated = anchor.estimate(&messages).unwrap();
        assert_eq!(estimated, 50_000 + estimate_tokens(&messages[2..]));
        assert!(estimated > estimate_tokens(&messages), "锚点应覆盖启发式的低估");

        // 厂商未上报用量（0）时不得生成锚点，避免用 0 覆盖既有基准
        assert!(TokenAnchor::record(2, 0).is_none());
    }

    /// 前缀被裁剪后锚点失效：必须回退启发式，而不是给出错误的低估值。
    #[test]
    fn test_token_anchor_invalidated_after_prefix_trim() {
        let anchor = TokenAnchor::record(5, 10_000).unwrap();
        let trimmed = vec![text(Role::User, "u9", None, "only one left")];
        assert!(
            anchor.estimate(&trimmed).is_none(),
            "list shorter than the anchored prefix must invalidate the anchor"
        );
    }

    /// 关键行为：纯启发式会高估英文/代码，导致过早压缩；
    /// 有了权威锚点，实际占用未超阈值时不得裁剪历史。
    #[test]
    fn test_anchor_prevents_unnecessary_compaction() {
        // 构造多轮英文历史，字符折算会明显超过阈值
        let build = |chars: usize| {
            let mut messages = Vec::new();
            let mut parent: Option<String> = None;
            for i in 0..6 {
                let user_id = format!("u{}", i);
                messages.push(text(Role::User, &user_id, parent.as_deref(), &"a".repeat(chars)));
                let assistant_id = format!("a{}", i);
                messages.push(text(Role::Assistant, &assistant_id, Some(&user_id), &"b".repeat(chars)));
                parent = Some(assistant_id);
            }
            messages
        };
        let context_len = 30_000;
        let threshold = (context_len as f64 * 0.7) as usize; // 21_000

        // 纯启发式：12 × 4000 字符按 2 字符/Token 折算 = 24_000 → 触发裁剪
        let mut heuristic_only = build(4_000);
        assert!(estimate_tokens(&heuristic_only) > threshold);
        let before = heuristic_only.len();
        compact_messages(&mut heuristic_only, context_len, None);
        assert!(heuristic_only.len() < before, "heuristic path must compact");

        // 同一份历史，厂商按英文约 4 字符/Token 上报：锚点覆盖 10 条 = 10_000，
        // 加上新增 2 条的启发式增量 4_000 → 14_000，未超阈值，不得裁剪
        let mut anchored = build(4_000);
        let anchor = TokenAnchor::record(10, 10_000).unwrap();
        assert!(anchor.estimate(&anchored).unwrap() <= threshold);
        compact_messages(&mut anchored, context_len, Some(anchor));
        assert_eq!(
            anchored.len(),
            build(4_000).len(),
            "authoritative anchor must suppress unnecessary compaction"
        );
        assert_well_formed(&anchored);
    }

    /// 锚点存在但真实占用确实超阈值时，压缩仍须生效并按整轮裁剪。
    #[test]
    fn test_anchor_still_compacts_when_authority_exceeds_budget() {
        let mut messages = Vec::new();
        let mut parent: Option<String> = None;
        for i in 0..6 {
            let user_id = format!("u{}", i);
            messages.push(text(Role::User, &user_id, parent.as_deref(), &"问".repeat(200)));
            let assistant_id = format!("a{}", i);
            messages.push(tool_use_msg(&assistant_id, &user_id, &format!("call{}", i)));
            let result_id = format!("r{}", i);
            messages.push(tool_result_msg(
                &result_id,
                &assistant_id,
                &format!("call{}", i),
                &"答".repeat(2_000),
            ));
            parent = Some(result_id);
        }
        let context_len = 4_000;
        let anchor = TokenAnchor::record(2, context_len).expect("authoritative count recorded"); // 权威值本身已超阈值
        let before = messages.len();
        compact_messages(&mut messages, context_len, Some(anchor));
        assert!(
            messages.len() < before,
            "over-budget anchored history must still compact"
        );
        assert_well_formed(&messages);
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
            .create_session("s_q", "/w", "T", "missing/model", "task", "medium")
            .await?;

        let room = SessionRoom::new(
            "s_q",
            PathBuf::from(tmp.path()),
            storage.clone(),
            Arc::new(RwLock::new(OmaConfig::default())),
            ToolRegistry::new(),
            "missing/model",
            "task",
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
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
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

    /// 编辑重发在忙碌时排队必须连同分叉点一起保留，
    /// 否则轮到执行时会退化成普通追加，分叉点静默丢失。
    #[test]
    fn test_queued_fork_keeps_parent() {
        let queued = queued_run(AgentCommand::ForkAndRun {
            parent_message_id: "m_parent".into(),
            new_content:       Some("edited".into()),
        });
        assert_eq!(
            queued,
            Some(("edited".to_string(), Vec::new(), Some("m_parent".to_string())))
        );

        // 无分叉点的普通输入照旧
        assert_eq!(
            queued_run(AgentCommand::UserInput {
                content:     "hi".into(),
                attachments: vec!["a".into()],
            }),
            Some(("hi".to_string(), vec!["a".to_string()], None))
        );

        // 与用户输入无关的指令不入队；真出现时必须被丢弃而不是当成一轮输入
        assert!(queued_run(AgentCommand::SetModel { model: "m".into() }).is_none());
        assert!(
            queued_run(AgentCommand::ForkAndRun {
                parent_message_id: "m_parent".into(),
                new_content:       None,
            })
            .is_none()
        );
    }

    // ---------- 推理等级下发 ----------

    fn thinking_model(supports_thinking: bool, map: &[(&str, &str)]) -> oma_provider::ModelConfig {
        let mut reasoning_map = std::collections::BTreeMap::new();
        for (k, v) in map {
            reasoning_map.insert(k.to_string(), v.to_string());
        }
        let mut capabilities = oma_contract::default_model_capabilities();
        capabilities.insert(oma_contract::ModelCapability::ImageInput);
        if supports_thinking {
            capabilities.insert(oma_contract::ModelCapability::Thinking);
        }
        oma_provider::ModelConfig {
            id: "m".into(),
            name: "m".into(),
            context_len: 1000,
            capabilities,
            max_output: None,
            reasoning_effort: String::new(),
            reasoning_map,
            headers: std::collections::BTreeMap::new(),
            body: serde_json::json!({}),
        }
    }

    /// 支持思考的模型：等级经映射后落到 reasoning_effort。
    #[test]
    fn test_apply_reasoning_level_maps_for_thinking_model() {
        let mut m = thinking_model(true, &[("ultra", "think-ultra")]);
        apply_reasoning_level(&mut m, "ultra");
        assert_eq!(m.reasoning_effort, "think-ultra");

        // 未配置映射的等级按等级名下发
        let mut m2 = thinking_model(true, &[("low", "x")]);
        apply_reasoning_level(&mut m2, "medium");
        assert_eq!(m2.reasoning_effort, "medium");
    }

    /// 不支持思考的模型：即使会话有具体等级也不得下发推理参数。
    #[test]
    fn test_apply_reasoning_level_skipped_for_non_thinking_model() {
        let mut m = thinking_model(false, &[("ultra", "think-ultra")]);
        apply_reasoning_level(&mut m, "ultra");
        assert!(
            m.reasoning_effort.is_empty(),
            "non-thinking model must not receive reasoning params, got {:?}",
            m.reasoning_effort
        );
    }

    // ---------- 会话自动命名 ----------

    fn msg(role: Role, text: &str) -> ChatMessage {
        ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            role,
            content: vec![Block::Text { text: text.to_string() }],
            created_at: 0,
        }
    }

    #[test]
    fn test_sanitize_title_strips_decorations() {
        assert_eq!(sanitize_title("修复登录超时").unwrap(), "修复登录超时");
        assert_eq!(
            sanitize_title("  \"Fix login timeout\"  ").unwrap(),
            "Fix login timeout"
        );
        assert_eq!(sanitize_title("Title: Cache warmup plan").unwrap(), "Cache warmup plan");
        assert_eq!(sanitize_title("标题：缓存预热方案").unwrap(), "缓存预热方案");
        assert_eq!(sanitize_title("# 会话标题").unwrap(), "会话标题");
        // 多行只取第一个非空行，避免把解释性文字带进标题
        assert_eq!(sanitize_title("\n\n短标题\n这里是一段说明").unwrap(), "短标题");
        // 纯空内容不得产出空标题
        assert!(sanitize_title("   ").is_none());
        assert!(sanitize_title("\n\n").is_none());
        assert!(sanitize_title("\"\"").is_none());
    }

    #[test]
    fn test_sanitize_title_truncates_overlong() {
        let long = "字".repeat(TITLE_MAX_CHARS + 50);
        let out = sanitize_title(&long).unwrap();
        assert_eq!(out.chars().count(), TITLE_MAX_CHARS);
    }

    #[test]
    fn test_build_naming_prompt_uses_dialogue_only() {
        let messages = vec![
            msg(Role::User, "帮我优化数据库查询"),
            // 仅含工具调用的消息不应污染命名上下文
            ChatMessage {
                id:         "t".into(),
                parent_id:  None,
                role:       Role::Assistant,
                content:    vec![Block::ToolUse {
                    id:    "c1".into(),
                    name:  "read".into(),
                    input: serde_json::json!({ "path": "x" }),
                }],
                created_at: 0,
            },
            msg(Role::Assistant, "好的，我先看索引"),
        ];
        let prompt = build_naming_prompt(&messages).unwrap();
        assert!(prompt.contains("User: 帮我优化数据库查询"));
        assert!(prompt.contains("Assistant: 好的，我先看索引"));
        assert!(!prompt.contains("read"), "tool blocks must not leak into naming prompt");
    }

    #[test]
    fn test_build_naming_prompt_none_without_text() {
        assert!(build_naming_prompt(&[]).is_none());
        let only_tools = vec![ChatMessage {
            id:         "t".into(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![Block::ToolResult {
                tool_use_id: "c1".into(),
                content:     "ok".into(),
                is_error:    false,
            }],
            created_at: 0,
        }];
        assert!(build_naming_prompt(&only_tools).is_none());
    }

    #[test]
    fn test_build_naming_prompt_bounded() {
        let messages: Vec<ChatMessage> = (0..50)
            .map(|i| msg(Role::User, &format!("{}-{}", i, "很长的内容".repeat(200))))
            .collect();
        let prompt = build_naming_prompt(&messages).unwrap();
        assert!(
            prompt.chars().count() < NAMING_CONTEXT_CHARS + 200,
            "prompt must stay bounded: {}",
            prompt.chars().count()
        );
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
