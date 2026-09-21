use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// 本构建的版本标识，握手包两侧共用。
///
/// CI 会注入形如 `0.2.0+1a2b3c4` 的值（版本号 + 提交短 hash），
/// 便于把任何一个发布出去的二进制对应回具体提交；
/// 本地构建未注入时回退到包版本。
pub const VERSION: &str = match option_env!("OMA_BUILD_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

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
    /// 产出该消息时生效的模型（`provider/model`）。
    ///
    /// 会话中途可以切模型，而「这轮是哪个模型产出的」只能跟消息走；
    /// 用户消息与早期数据没有该字段（`None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model:      Option<String>,
    /// 产生该消息时本轮累计的 token 消耗；用户消息与早期数据为 `None`。
    ///
    /// 一次用户输入可能产生多条助手消息（工具循环），每条都记下当时的累计值，
    /// 界面按「本轮最后一条」展示整轮开销。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage:      Option<TokenUsage>,
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
    /// 设置当前会话的推理等级（必须在 REASONING_LEVELS 内，空串 = 未设置）
    SetReasoningLevel {
        level: String,
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
    Cancel {},
}

/// 规范化的推理等级集合。
///
/// 作为跨端共享的唯一定义：模型侧用它做「等级 → 厂商自定义字符串」映射的键，
/// 会话侧用它做可选值校验，界面用它渲染下拉项。
pub const REASONING_LEVELS: [&str; 7] = ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

/// 推理等级是否为规范集合中的合法值。
///
/// 空串**不是**合法等级：模型支持思考时必须落在具体等级上，不允许「留空」语义。
pub fn is_valid_reasoning_level(level: &str) -> bool {
    REASONING_LEVELS.contains(&level)
}

/// 规范化的强调色令牌集合（对应 `Palette` 中的强调色字段）。
///
/// `Theme.accent` 只能取其中之一：前端据此挑 CSS 变量，终端客户端据此挑前景色。
pub const ACCENTS: [&str; 14] = [
    "rosewater",
    "flamingo",
    "pink",
    "mauve",
    "red",
    "maroon",
    "peach",
    "yellow",
    "green",
    "teal",
    "sky",
    "sapphire",
    "blue",
    "lavender",
];

/// 调色板自身的明暗属性：决定它归属浅色组还是深色组候选。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteMode {
    Light,
    Dark,
}

impl PaletteMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PaletteMode::Light => "light",
            PaletteMode::Dark => "dark",
        }
    }
}

/// 主题显示模式：跟随系统时由客户端按 `prefers-color-scheme` 在
/// `light_palette` / `dark_palette` 之间切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    #[default]
    Dark,
    System,
}

impl ThemeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
            ThemeMode::System => "system",
        }
    }
}

/// 主题设置：浅色与深色各引用一套调色板 id，另叠加一个强调色令牌。
///
/// 拒绝未知字段：旧版键名（如 `dark_flavor` / `light_theme`）必须直接报错，
/// 静默忽略会让用户以为设置已生效。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    #[serde(default)]
    pub mode:          ThemeMode,
    /// 深色模式使用的调色板 id（须为 `PaletteMode::Dark` 的调色板）
    #[serde(default = "default_dark_palette")]
    pub dark_palette:  String,
    /// 浅色模式使用的调色板 id（须为 `PaletteMode::Light` 的调色板）
    #[serde(default = "default_light_palette")]
    pub light_palette: String,
    /// 强调色令牌名，ACCENTS 之一
    #[serde(default = "default_accent")]
    pub accent:        String,
}

/// 默认深色调色板：`mocha`（完整的 Catppuccin 深色，语义色齐全）
fn default_dark_palette() -> String {
    "mocha".to_string()
}

/// 默认浅色调色板：`latte`（与 `mocha` 同一套语义色的浅色版本，明暗切换不跳色）
fn default_light_palette() -> String {
    "latte".to_string()
}

fn default_accent() -> String {
    "blue".to_string()
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            mode:          ThemeMode::Dark,
            dark_palette:  default_dark_palette(),
            light_palette: default_light_palette(),
            accent:        default_accent(),
        }
    }
}

/// 一套完整调色板。
///
/// 26 个令牌的划分对齐 Catppuccin：中性色由暗到亮（crust → text）用于背景与文字，
/// 14 个强调色用于语义高亮。`id` 是稳定引用键（被 `Theme` 引用、即文件名），
/// `name` 仅用于展示，可随用户改名而不破坏引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Palette {
    /// 稳定 slug：主题引用键，也是 `<配置目录>/oma/themes/<id>.json` 的文件名
    pub id:   String,
    pub name: String,
    pub mode: PaletteMode,

    pub crust:  String,
    pub mantle: String,
    pub base:   String,

    pub surface0: String,
    pub surface1: String,
    pub surface2: String,
    pub overlay0: String,
    pub overlay1: String,
    pub overlay2: String,
    pub subtext0: String,
    pub subtext1: String,
    pub text:     String,

    pub lavender:  String,
    pub blue:      String,
    pub sapphire:  String,
    pub sky:       String,
    pub teal:      String,
    pub green:     String,
    pub yellow:    String,
    pub peach:     String,
    pub maroon:    String,
    pub red:       String,
    pub mauve:     String,
    pub pink:      String,
    pub flamingo:  String,
    pub rosewater: String,
}

impl Palette {
    /// 按令牌名取色值；令牌名即 `Theme.accent` 与前端 CSS 变量的键。
    pub fn token(&self, token: &str) -> Option<&str> {
        let value = match token {
            "crust" => &self.crust,
            "mantle" => &self.mantle,
            "base" => &self.base,
            "surface0" => &self.surface0,
            "surface1" => &self.surface1,
            "surface2" => &self.surface2,
            "overlay0" => &self.overlay0,
            "overlay1" => &self.overlay1,
            "overlay2" => &self.overlay2,
            "subtext0" => &self.subtext0,
            "subtext1" => &self.subtext1,
            "text" => &self.text,
            "lavender" => &self.lavender,
            "blue" => &self.blue,
            "sapphire" => &self.sapphire,
            "sky" => &self.sky,
            "teal" => &self.teal,
            "green" => &self.green,
            "yellow" => &self.yellow,
            "peach" => &self.peach,
            "maroon" => &self.maroon,
            "red" => &self.red,
            "mauve" => &self.mauve,
            "pink" => &self.pink,
            "flamingo" => &self.flamingo,
            "rosewater" => &self.rosewater,
            _ => return None,
        };
        Some(value)
    }

    /// 全部 26 个令牌的 (名称, 色值) 对，供校验与逐令牌下发。
    pub fn tokens(&self) -> Vec<(&'static str, &str)> {
        PALETTE_TOKENS
            .iter()
            .filter_map(|t| self.token(t).map(|v| (*t, v)))
            .collect()
    }
}

/// 调色板令牌的规范顺序（与前端 CSS 变量、自定义编辑器渲染顺序一致）。
pub const PALETTE_TOKENS: [&str; 26] = [
    "crust",
    "mantle",
    "base",
    "surface0",
    "surface1",
    "surface2",
    "overlay0",
    "overlay1",
    "overlay2",
    "subtext0",
    "subtext1",
    "text",
    "lavender",
    "blue",
    "sapphire",
    "sky",
    "teal",
    "green",
    "yellow",
    "peach",
    "maroon",
    "red",
    "mauve",
    "pink",
    "flamingo",
    "rosewater",
];

/// 校验调色板色值是否为合法的 `#rgb` / `#rrggbb` 十六进制颜色。
pub fn is_valid_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// 已解析的主题：把 `Theme` 的两套引用展开成真实调色板，直接下发给客户端。
///
/// 客户端无需读配置目录、也无需内置任何色值 —— 终端与浏览器拿到的是同一份数据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTheme {
    pub mode:   ThemeMode,
    /// 强调色令牌名（ACCENTS 之一）
    pub accent: String,
    pub light:  Palette,
    pub dark:   Palette,
}

/// 模型能力：描述模型能做什么，取代原先分散的 `supports_vision` /
/// `supports_thinking` / `input_types` 三个字段。
///
/// 单一枚举兼顾「输入模态」与「产出形态」：文本输入与文本输出是所有模型
/// 的底线能力，图像/视频/音频的输入与输出则按模型差异声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    /// 思考（推理过程可见）
    Thinking,
    /// 文本输入
    TextInput,
    /// 文本输出
    TextOutput,
    /// 图像输入
    ImageInput,
    /// 图像输出
    ImageOutput,
    /// 视频输入
    VideoInput,
    /// 视频输出
    VideoOutput,
    /// 音频输入
    AudioInput,
    /// 音频输出
    AudioOutput,
}

impl ModelCapability {
    /// 全部能力，顺序即界面展示顺序
    pub const ALL: [ModelCapability; 9] = [
        ModelCapability::Thinking,
        ModelCapability::TextInput,
        ModelCapability::TextOutput,
        ModelCapability::ImageInput,
        ModelCapability::ImageOutput,
        ModelCapability::VideoInput,
        ModelCapability::VideoOutput,
        ModelCapability::AudioInput,
        ModelCapability::AudioOutput,
    ];

    /// 配置与协议中的字符串表示（与 `serde` 的 snake_case 一致）
    pub fn as_str(self) -> &'static str {
        match self {
            ModelCapability::Thinking => "thinking",
            ModelCapability::TextInput => "text_input",
            ModelCapability::TextOutput => "text_output",
            ModelCapability::ImageInput => "image_input",
            ModelCapability::ImageOutput => "image_output",
            ModelCapability::VideoInput => "video_input",
            ModelCapability::VideoOutput => "video_output",
            ModelCapability::AudioInput => "audio_input",
            ModelCapability::AudioOutput => "audio_output",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ModelCapability::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// 未显式配置能力时的默认值：仅文本输入与文本输出。
///
/// 刻意不含 `Thinking` 与视觉类能力 —— 少数派能力交给用户显式声明，
/// 未声明的模型不会被误判成能看图，避免把图片发给看不见图的模型。
pub fn default_model_capabilities() -> BTreeSet<ModelCapability> {
    [ModelCapability::TextInput, ModelCapability::TextOutput]
        .into_iter()
        .collect()
}

/// 模型元数据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id:            String,
    pub name:          String,
    pub context_len:   usize,
    /// 模型能力集合
    #[serde(default = "default_model_capabilities")]
    pub capabilities:  BTreeSet<ModelCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:    Option<usize>,
    /// 推理等级 → 厂商自定义字符串；未配置的等级按等级名下发
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_map: BTreeMap<String, String>,
}

/// Agent 模板元数据
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id:          String,
    pub name:        String,
    pub description: String,
}

/// MCP 服务器配置定义
///
/// 放在契约 crate 而不是 MCP 客户端：它是被配置与界面共用的**数据类型**，
/// 而 MCP 客户端要依赖 `oma-tool`（注册工具）——若定义在客户端，
/// `config → mcp → tool → config` 会成环。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpServerConfig {
    Local {
        command: String,
        #[serde(default)]
        args:    Vec<String>,
        #[serde(default)]
        env:     BTreeMap<String, String>,
    },
    Remote {
        url:     String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
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
    /// 当前会话的推理等级（空 = 未设置）
    #[serde(default)]
    pub reasoning_level: String,
    pub current_leaf_id: Option<String>,
    /// 可选模型目录（provider → 模型清单），与 OmaConfig.providers 的配置形态不同
    pub model_catalog:   BTreeMap<String, Vec<ModelInfo>>,
    pub agents:          Vec<AgentSummary>,
    /// MCP 服务器概览（供主界面指示器展示）
    pub mcp_summaries:   Vec<McpServerSummary>,
    /// 上次记录的上下文占用与模型窗口，供重连后立即恢复进度条；无记录时为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage:   Option<ContextUsage>,
    /// 已解析的主题（含两套完整调色板）：终端客户端据此直接上色，
    /// 无需自行读取配置目录或内置任何色值
    pub active_theme:    ResolvedTheme,
}

/// 上下文占用快照
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUsage {
    /// 提示侧 token 总量（含缓存）
    pub tokens:      usize,
    /// 模型上下文窗口
    pub context_len: usize,
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
    /// 提示侧（输入）总量：统一含缓存读写，与各厂商口径对齐
    pub input_tokens:       usize,
    pub output_tokens:      usize,
    /// 缓存命中的输入 token（Anthropic `cache_read_input_tokens` / OpenAI `cached_tokens`）
    #[serde(default)]
    pub cache_read_tokens:  usize,
    /// 写入缓存的输入 token（Anthropic `cache_creation_input_tokens`）
    #[serde(default)]
    pub cache_write_tokens: usize,
}

/// 工具调用发起数据
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallStartedData {
    pub call_id:   String,
    /// 被调用的工具名
    pub tool_name: String,
    pub input:     serde_json::Value,
}

/// 活跃轮次重连追赶快照
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActiveTurnCatchUp {
    pub turn_id:              String,
    pub accumulated_thinking: String,
    pub accumulated_text:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tool_call:     Option<ToolCallStartedData>,
}

/// Agent 运行事件集
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStarted {
        turn_id: String,
    },
    TurnFinished {
        turn_id:     String,
        stop_reason: StopReason,
        usage:       TokenUsage,
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
        delta: String,
    },
    TextDelta {
        delta: String,
    },
    ToolCallStarted(ToolCallStartedData),
    ToolCallFinished {
        call_id:   String,
        /// 被调用的工具名
        tool_name: String,
        output:    String,
        is_error:  bool,
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
    /// 上下文占用更新：每次模型请求拿到用量后广播，供界面展示进度。
    /// `tokens` 为提示侧总量（含缓存），`context_len` 为模型窗口。
    ContextUsage {
        tokens:      usize,
        context_len: usize,
    },
    ReasoningLevelChanged {
        level: String,
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
    /// 握手载荷内嵌两套完整调色板，体积远大于其他变体，故做 Box 间接
    Ready {
        ready: Box<Ready>,
    },
    /// `AgentEvent` 使用外部 tag 风格声明了 `data` 字段，
    /// 与另外两个变体同时存在时仍会撑大整个枚举；同样做 Box 间接
    Event {
        event: Box<AgentEvent>,
    },
    Error {
        message: String,
    },
}

/// 工具产出的图片附件。
///
/// 工具输出默认只有文本；能“看见”图片的工具（如 `read` 读图片文件）把图片
/// 另附在这里，由上层决定是内联给模型还是降级成元数据文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolImage {
    pub mime_type: String,
    /// 图片字节的 base64 编码（不含 data URI 前缀）
    pub data:      String,
}

/// 工具输出结果统一结构
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub output:   String,
    pub is_error: bool,
    /// 随工具输出一并回到模型的图片；默认空
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images:   Vec<ToolImage>,
}

impl ToolOutput {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output:   output.into(),
            is_error: false,
            images:   Vec::new(),
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output:   output.into(),
            is_error: true,
            images:   Vec::new(),
        }
    }

    /// 附带图片的成功输出（文本作为图片说明一并发给模型）。
    pub fn success_with_images(output: impl Into<String>, images: Vec<ToolImage>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            images,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 能力集合在协议上序列化为 snake_case 字符串数组，且顺序由 BTreeSet 决定。
    #[test]
    fn test_model_capability_serde() {
        let caps = default_model_capabilities();
        assert_eq!(
            serde_json::to_value(&caps).unwrap(),
            serde_json::json!(["text_input", "text_output"])
        );

        let parsed: BTreeSet<ModelCapability> = serde_json::from_str(
            r#"["thinking", "text_input", "text_output", "image_input", "audio_input", "audio_output"]"#,
        )
        .unwrap();
        assert!(parsed.contains(&ModelCapability::Thinking));
        assert!(parsed.contains(&ModelCapability::AudioInput));
        assert!(parsed.contains(&ModelCapability::AudioOutput));
        assert!(!parsed.contains(&ModelCapability::VideoInput));
    }

    /// 字符串表示与 serde 保持一致，且能被 `parse` 反向解析（供前后端共享取值）。
    #[test]
    fn test_model_capability_str_roundtrip() {
        for cap in ModelCapability::ALL {
            assert_eq!(ModelCapability::parse(cap.as_str()), Some(cap));
            assert_eq!(serde_json::to_value(cap).unwrap(), serde_json::json!(cap.as_str()));
        }
        assert_eq!(ModelCapability::parse("vision"), None);
        assert_eq!(ModelCapability::parse("embedding"), None);
        // 旧命名不再兼容：理解/生成 已分别改为 输入/输出
        assert_eq!(ModelCapability::parse("text_understanding"), None);
        assert_eq!(ModelCapability::parse("text_generation"), None);
        assert_eq!(ModelCapability::parse("image_understanding"), None);
        assert_eq!(ModelCapability::ALL.len(), 9);
    }

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
            call_id:   "call_1".into(),
            tool_name: "read".into(),
            input:     serde_json::json!({ "path": "src/lib.rs" }),
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
        };
        let server_msg = ServerMessage::Event {
            event: Box::new(AgentEvent::ActiveTurnCatchUp(catch_up)),
        };
        let json = serde_json::to_string(&server_msg).unwrap();
        let de: ServerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(server_msg, de);
    }
}
