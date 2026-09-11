use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 角色模型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Role::System),
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            _ => None,
        }
    }
}

/// 内容块模型
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    Image {
        mime_type: String,
        data:      String, // Base64 或 session_attachment:// 资源 URI
    },
    ToolUse {
        id:    String,
        name:  String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content:     String,
        is_error:    bool,
    },
}

/// 树状聊天消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id:         String,
    pub parent_id:  Option<String>,
    pub role:       Role,
    pub content:    Vec<Block>,
    pub created_at: i64,
}

/// 客户端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    Tui,
    Web,
    Tauri,
    Cli,
}

/// WebSocket 握手参数
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectParams {
    pub client_id:   String,
    pub workspace:   String,
    pub session_id:  String,
    pub client_type: ClientType,
    pub client_name: String,
    pub version:     String,
}

/// 审批决策
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowSession,
    Deny,
}

/// 审批模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    #[default]
    Normal,
    Strict,
    Auto,
}

impl ApprovalMode {
    /// 数据库列存形态（与 serde rename_all 保持一致）
    pub fn as_str(&self) -> &'static str {
        match self {
            ApprovalMode::Normal => "normal",
            ApprovalMode::Strict => "strict",
            ApprovalMode::Auto => "auto",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "normal" => Some(ApprovalMode::Normal),
            "strict" => Some(ApprovalMode::Strict),
            "auto" => Some(ApprovalMode::Auto),
            _ => None,
        }
    }
}

/// 审批响应
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub request_id: String,
    pub decision:   ApprovalDecision,
}

/// 客户端发向 Agent 的指令
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentCommand {
    UserInput {
        content:     String,
        #[serde(default)]
        attachments: Vec<String>,
    },
    SetModel {
        model: String,
    },
    SetAgent {
        agent: String,
    },
    SetApprovalMode {
        mode: ApprovalMode,
    },
    ForkAndRun {
        parent_message_id: String,
        new_content:       Option<String>,
    },
    SwitchBranch {
        leaf_message_id: String,
    },
}

/// 客户端上行消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    Connect {
        #[serde(flatten)]
        params: ConnectParams,
    },
    Command {
        command: AgentCommand,
    },
    Approval {
        response: ApprovalResponse,
    },
    /// 提问回答（ask 工具）
    Ask {
        response: AskResponse,
    },
    Cancel {},
}

/// 模型元数据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id:                String,
    pub name:              String,
    pub context_len:       usize,
    pub supports_vision:   bool,
    pub supports_thinking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:        Option<usize>,
    /// 推理等级: "" | low | medium | high
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning_effort:  String,
    /// 支持的输入模态: text / image / video
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_types:       Vec<String>,
}

/// Agent 模板元数据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id:          String,
    pub name:        String,
    pub description: String,
}

/// MCP 服务器工具概览（供主界面指示器展示）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSummary {
    pub name:       String,
    pub tool_count: usize,
}

/// 握手成功就绪载荷
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ready {
    pub version:         String,
    pub session_id:      String,
    pub workspace:       String,
    pub active_model:    String,
    pub active_agent:    String,
    pub approval_mode:   ApprovalMode,
    pub current_leaf_id: Option<String>,
    pub providers:       BTreeMap<String, Vec<ModelInfo>>,
    pub agents:          Vec<AgentSummary>,
    pub mcp_servers:     Vec<McpServerSummary>,
}

/// 结束原因
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Cancelled,
    Error,
}

/// Token 消耗统计
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens:  usize,
    pub output_tokens: usize,
}

/// 工具调用发起数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallStartedData {
    pub call_id:     String,
    pub name:        String,
    pub input:       serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<String>,
}

/// 模型向用户提问的单个选项
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskOption {
    pub label:       String,
    /// 补充说明：解释该选项的取舍，展示在标签下方
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// 模型向用户提问的单个问题
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskQuestion {
    pub id:          String,
    pub question:    String,
    pub options:     Vec<AskOption>,
    /// true = 多选（复选），false = 单选
    #[serde(default)]
    pub multi:       bool,
    /// 推荐选项下标，供界面标注默认值；越界则忽略
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<usize>,
}

/// 用户对单个问题的回答
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskAnswer {
    /// 选中的选项标签（按选择顺序）
    #[serde(default)]
    pub selected:     Vec<String>,
    /// 「其他」自定义输入；为空表示未使用
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub custom_input: String,
}

/// 提问请求（服务端下行，等待用户作答）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequestedData {
    pub request_id: String,
    pub questions:  Vec<AskQuestion>,
}

/// 提问回答（客户端上行）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskResponse {
    pub request_id: String,
    /// 与请求 questions 一一对应；为空数组表示用户取消
    #[serde(default)]
    pub answers:    Vec<AskAnswer>,
    /// true = 用户取消/拒绝作答
    #[serde(default)]
    pub cancelled:  bool,
}

/// 权限审批请求数据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequestedData {
    pub request_id: String,
    pub name:       String,
    pub summary:    String,
}

/// 活跃轮次重连追赶快照
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveTurnCatchUp {
    pub turn_id:              String,
    pub accumulated_thinking: String,
    pub accumulated_text:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tool_call:     Option<ToolCallStartedData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_approval:     Option<PermissionRequestedData>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_ask:          Option<AskRequestedData>,
}

/// Agent 运行事件集
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStarted {
        turn_id:     String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    TurnFinished {
        turn_id:     String,
        stop_reason: StopReason,
        usage:       TokenUsage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    UserMessage {
        client_id:   String,
        client_name: String,
        client_type: ClientType,
        content:     String,
        queued:      bool,
    },
    /// 会话轮次占用状态变化：服务端在轮次开始/全部结束时广播，
    /// 所有已连接客户端据此更新侧栏的运行中标记（不限当前会话）。
    SessionRunning {
        session_id: String,
        running:    bool,
    },
    QueueCleared {},
    /// 队列深度变化（服务端权威计数，客户端不再自行累加）
    QueueUpdated {
        pending: usize,
    },
    /// 客户端落后的历史事件已溢出广播缓冲，必须整体回读持久化状态
    SyncRequired {},
    ThinkingDelta {
        delta:       String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    TextDelta {
        delta:       String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    ToolCallStarted(ToolCallStartedData),
    ToolCallFinished {
        call_id:     String,
        name:        String,
        output:      String,
        is_error:    bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    PermissionRequested(PermissionRequestedData),
    PermissionResolved {
        request_id:  String,
        decision:    ApprovalDecision,
        resolved_by: String,
    },
    /// 模型请求用户作答（ask 工具）
    AskRequested(AskRequestedData),
    /// 提问已被回答或取消
    AskResolved {
        request_id:  String,
        #[serde(default)]
        cancelled:   bool,
        resolved_by: String,
    },
    ActiveBranchChanged {
        current_leaf_id: String,
    },
    ModelChanged {
        active_model: String,
    },
    AgentChanged {
        active_agent: String,
    },
    ApprovalModeChanged {
        mode: ApprovalMode,
    },
    ActiveTurnCatchUp(ActiveTurnCatchUp),
    SessionRenamed {
        session_id: String,
        title:      String,
    },
    MessagesDeleted {
        deleted_ids:     Vec<String>,
        current_leaf_id: Option<String>,
    },
    Error {
        message: String,
    },
}

/// 服务端下行消息
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready { ready: Ready },
    Event { event: AgentEvent },
    Error { message: String },
}

/// 工具输出结果统一结构
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub output:   String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output:   output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output:   output.into(),
            is_error: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_message_serde() {
        let msg = ClientMessage::Connect {
            params: ConnectParams {
                client_id:   "c1".into(),
                workspace:   "/tmp".into(),
                session_id:  "s1".into(),
                client_type: ClientType::Web,
                client_name: "Chrome".into(),
                version:     "1.0".into(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let de: ClientMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, de);
    }

    #[test]
    fn test_agent_event_serde() {
        let event = AgentEvent::ToolCallStarted(ToolCallStartedData {
            call_id:     "call_1".into(),
            name:        "read".into(),
            input:       serde_json::json!({ "path": "src/lib.rs" }),
            subagent_id: Some("sub_1".into()),
        });
        let json = serde_json::to_string(&event).unwrap();
        let de: AgentEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, de);
    }

    #[test]
    fn test_server_message_catch_up() {
        let catch_up = ActiveTurnCatchUp {
            turn_id:              "t1".into(),
            accumulated_thinking: "thinking...".into(),
            accumulated_text:     "hello".into(),
            active_tool_call:     None,
            pending_approval:     Some(PermissionRequestedData {
                request_id: "req_1".into(),
                name:       "shell".into(),
                summary:    "cargo test".into(),
            }),
            pending_ask:          None,
        };
        let server_msg = ServerMessage::Event {
            event: AgentEvent::ActiveTurnCatchUp(catch_up),
        };
        let json = serde_json::to_string(&server_msg).unwrap();
        let de: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(server_msg, de);
    }
}
