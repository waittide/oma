/** 与 Rust oma-contract / oma-config 序列化的 JSON 结构一一对应。 */

export type Role = 'system' | 'user' | 'assistant';
export type ClientType = 'tui' | 'web' | 'tauri' | 'cli';
export type ApprovalMode = 'normal' | 'strict' | 'auto';
export type ApprovalDecision = 'allow_once' | 'allow_session' | 'deny';
export type StopReason = 'end_turn' | 'tool_use' | 'max_tokens' | 'cancelled' | 'error';

/**
 * 模型能力：既能描述输入模态，也覆盖产出形态。
 * 取值与后端 oma-contract 的 ModelCapability 一一对应。
 */
export type ModelCapability =
  | 'thinking'
  | 'text_input'
  | 'text_output'
  | 'image_input'
  | 'image_output'
  | 'video_input'
  | 'video_output'
  | 'audio_input'
  | 'audio_output';

/** 全部能力及其 i18n 文案键，顺序即界面展示顺序（与后端 ALL 一致） */
export const MODEL_CAPABILITIES: { value: ModelCapability; label: string }[] = [
  { value: 'thinking', label: 'capabilityThinking' },
  { value: 'text_input', label: 'capabilityTextInput' },
  { value: 'text_output', label: 'capabilityTextOutput' },
  { value: 'image_input', label: 'capabilityImageInput' },
  { value: 'image_output', label: 'capabilityImageOutput' },
  { value: 'video_input', label: 'capabilityVideoInput' },
  { value: 'video_output', label: 'capabilityVideoOutput' },
  { value: 'audio_input', label: 'capabilityAudioInput' },
  { value: 'audio_output', label: 'capabilityAudioOutput' },
];

export type Block =
  | { type: 'text'; text: string }
  | { type: 'thinking'; thinking: string }
  | { type: 'image'; mime_type: string; data: string }
  | { type: 'tool_use'; id: string; name: string; input: unknown }
  | { type: 'tool_result'; tool_use_id: string; content: string; is_error: boolean }
  /**
   * task 工具的子代理过程（thinking / text / 工具调用）。
   *
   * 仅存在于流式缓冲：子代理上下文不落库，因此历史消息里不会出现该块。
   * `tool_use_id` 指向宿主 task 的 call_id，渲染时嵌进那张卡片内部。
   */
  | { type: 'subagent'; tool_use_id: string; blocks: Block[] };

export interface ChatMessage {
  id: string;
  parent_id: string | null;
  role: Role;
  content: Block[];
  created_at: number;
}

export interface ModelInfo {
  id: string;
  name: string;
  context_len: number;
  /** 模型能力集合；与后端 ModelCapability 取值一致 */
  capabilities: ModelCapability[];
  max_output?: number;
  /** 推理等级 → 厂商自定义字符串；未配置的等级按等级名下发 */
  reasoning_map?: Record<string, string>;
}

export interface AgentSummary {
  id: string;
  name: string;
  description: string;
}

export interface ToolCallStartedData {
  call_id: string;
  /** 被调用的工具名 */
  tool_name: string;
  input: unknown;
  subagent_id?: string | null;
}

export interface PermissionRequestedData {
  request_id: string;
  /** 待执行的工具名 */
  tool_name: string;
  /** 工具入参原文；与 ToolCallStartedData.input 同为结构化 JSON */
  input: unknown;
}

export interface ActiveTurnCatchUp {
  turn_id: string;
  accumulated_thinking: string;
  accumulated_text: string;
  active_tool_call?: ToolCallStartedData | null;
  pending_approval?: PermissionRequestedData | null;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
}

export type AgentEvent =
  | { type: 'turn_started'; data?: { turn_id: string; subagent_id?: string | null } }
  | { type: 'turn_finished'; data?: { turn_id: string; stop_reason: StopReason; usage: TokenUsage; subagent_id?: string | null } }
  | { type: 'user_message'; data?: { client_id: string; client_name: string; client_type: ClientType; content: string; queued: boolean } }
  | { type: 'session_running'; data?: { session_id: string; running: boolean } }
  | { type: 'queue_cleared'; data?: Record<string, never> }
  | { type: 'queue_updated'; data?: { pending: number } }
  | { type: 'sync_required'; data?: Record<string, never> }
  | { type: 'thinking_delta'; data?: { delta: string; subagent_id?: string | null } }
  | { type: 'text_delta'; data?: { delta: string; subagent_id?: string | null } }
  | { type: 'tool_call_started'; data?: ToolCallStartedData }
  | { type: 'tool_call_finished'; data?: { call_id: string; tool_name: string; output: string; is_error: boolean; subagent_id?: string | null } }
  | { type: 'permission_requested'; data?: PermissionRequestedData }
  | { type: 'permission_resolved'; data?: { request_id: string; decision: ApprovalDecision; resolved_by: string } }
  | { type: 'active_branch_changed'; data?: { current_leaf_id: string } }
  | { type: 'model_changed'; data?: { active_model: string } }
  | { type: 'agent_changed'; data?: { active_agent: string } }
  | { type: 'approval_mode_changed'; data?: { mode: ApprovalMode } }
  | { type: 'reasoning_level_changed'; data?: { level: string } }
  | { type: 'context_usage'; data?: { tokens: number; context_len: number } }
  | { type: 'active_turn_catch_up'; data?: ActiveTurnCatchUp }
  | { type: 'session_renamed'; data?: { session_id: string; title: string } }
  | { type: 'messages_deleted'; data?: { deleted_ids: string[]; current_leaf_id: string | null } }
  | { type: 'error'; data?: { message: string } };

/** MCP 服务器配置（与 Rust McpServerConfig 的内部 tag 序列化一致） */
export type McpServerConfig =
  | { type: 'local'; command: string; args?: string[]; env?: Record<string, string> }
  | { type: 'remote'; url: string; headers?: Record<string, string> };

/** Agent 预设文件条目：决定以什么角色、能用哪些工具运行；scope = bundled | global | project */
export interface AgentFile {
  id: string;
  name: string;
  description: string;
  tools: string[];
  scope: string;
  /** 完整 Markdown（含 frontmatter） */
  content: string;
  /** 仅正文：编辑器只展示/回写正文，元信息由服务端按字段重新渲染 */
  body: string;
}

/** 技能文件条目：按需读取的领域知识；scope = global | agent | project */
export interface SkillFile {
  id: string;
  name: string;
  description: string;
  scope: string;
  /** 完整 Markdown（含 frontmatter） */
  content: string;
  /** 仅正文 */
  body: string;
  /** SKILL.md 绝对路径（模型读取技能正文的入口） */
  path: string;
  /** 技能目录绝对路径（技能自带的 scripts/ 相对此目录解析） */
  dir: string;
}

export interface McpServerSummary {
  name: string;
  tool_count: number;
}

export interface Ready {
  version: string;
  session_id: string;
  workspace: string;
  active_model: string;
  active_agent: string;
  approval_mode: ApprovalMode;
  /** 当前会话推理等级；空串 = 未设置（回退模型默认） */
  reasoning_level: string;
  current_leaf_id: string | null;
  /** 可选模型目录（provider → 模型清单），与 OmaConfig.providers 的配置形态不同 */
  model_catalog: Record<string, ModelInfo[]>;
  agents: AgentSummary[];
  /** MCP 服务器概览（名称 + 工具数），非完整配置 */
  mcp_summaries: McpServerSummary[];
  /** 上次记录的上下文占用；用于重连/重启后立即恢复进度条 */
  context_usage?: ContextUsage | null;
  /** 已解析主题：终端与浏览器共用同一份配色数据 */
  active_theme: ResolvedTheme;
}

/** 上下文占用快照 */
export interface ContextUsage {
  /** 提示侧 token 总量（含缓存） */
  tokens: number;
  /** 模型上下文窗口 */
  context_len: number;
}

export type ServerMessage =
  | { kind: 'ready'; ready: Ready }
  | { kind: 'event'; event: AgentEvent }
  | { kind: 'error'; message: string };

export type AgentCommand =
  | { type: 'user_input'; data: { content: string; attachments?: string[] } }
  | { type: 'set_model'; data: { model: string } }
  | { type: 'set_agent'; data: { agent: string } }
  | { type: 'set_approval_mode'; data: { mode: ApprovalMode } }
  | { type: 'set_reasoning_level'; data: { level: string } }
  | { type: 'fork_and_run'; data: { parent_message_id: string; new_content?: string | null } }
  | { type: 'switch_branch'; data: { leaf_message_id: string } };

export type ClientMessage =
  | {
      kind: 'connect';
      client_id: string;
      workspace: string;
      session_id: string;
      client_type: ClientType;
      client_name: string;
      version: string;
    }
  | { kind: 'command'; command: AgentCommand }
  | { kind: 'approval'; response: { request_id: string; decision: ApprovalDecision } }
  | { kind: 'cancel' };

/** 会话索引记录 (SessionRecord) */
export interface SessionRecord {
  session_id: string;
  workspace: string;
  title: string;
  active_model: string;
  active_agent: string;
  approval_mode: ApprovalMode;
  /** 会话推理等级；空串 = 未设置（回退模型默认） */
  reasoning_level?: string;
  current_leaf_id: string | null;
  created_at: number;
  updated_at: number;
  /** 该会话当前是否有正在执行的轮次（由服务端回填/事件驱动） */
  is_running?: boolean;
}

/** 调色板自身的明暗属性：决定它归属浅色组还是深色组候选。 */
export type PaletteMode = 'light' | 'dark';

/** 主题显示模式；system 时由前端按 prefers-color-scheme 选择浅色或深色调色板。 */
export type ThemeMode = 'light' | 'dark' | 'system';

/** 调色板令牌名：对应 Palette 的 26 个字段，也是 CSS 变量的键。 */
export const PALETTE_TOKENS = [
  'crust',
  'mantle',
  'base',
  'surface0',
  'surface1',
  'surface2',
  'overlay0',
  'overlay1',
  'overlay2',
  'subtext0',
  'subtext1',
  'text',
  'lavender',
  'blue',
  'sapphire',
  'sky',
  'teal',
  'green',
  'yellow',
  'peach',
  'maroon',
  'red',
  'mauve',
  'pink',
  'flamingo',
  'rosewater',
] as const;
export type PaletteToken = (typeof PALETTE_TOKENS)[number];

/** 令牌分组：编辑器按「中性色 / 强调色」分区渲染，也用于语义提示。 */
export const NEUTRAL_TOKENS: PaletteToken[] = [
  'crust',
  'mantle',
  'base',
  'surface0',
  'surface1',
  'surface2',
  'overlay0',
  'overlay1',
  'overlay2',
  'subtext0',
  'subtext1',
  'text',
];
export const ACCENT_TOKENS: PaletteToken[] = [
  'rosewater',
  'flamingo',
  'pink',
  'mauve',
  'red',
  'maroon',
  'peach',
  'yellow',
  'green',
  'teal',
  'sky',
  'sapphire',
  'blue',
  'lavender',
];

/** 可被 theme.accent 选中的强调色令牌。 */
export const ACCENTS = ACCENT_TOKENS;
export type Accent = (typeof ACCENT_TOKENS)[number];

/** 一套完整调色板 (GET /api/palettes)。 */
export interface Palette {
  /** 稳定 slug：主题引用键，也是服务端 themes/<id>.json 的文件名 */
  id: string;
  /** 仅用于展示，可自由改名而不影响引用 */
  name: string;
  mode: PaletteMode;
  /** 内置调色板不可删改（由服务端判定） */
  builtin?: boolean;

  crust: string;
  mantle: string;
  base: string;

  surface0: string;
  surface1: string;
  surface2: string;
  overlay0: string;
  overlay1: string;
  overlay2: string;
  subtext0: string;
  subtext1: string;
  text: string;

  lavender: string;
  blue: string;
  sapphire: string;
  sky: string;
  teal: string;
  green: string;
  yellow: string;
  peach: string;
  maroon: string;
  red: string;
  mauve: string;
  pink: string;
  flamingo: string;
  rosewater: string;
}

/** 主题设置 (config::Theme)：浅色/深色各引用一套调色板 id。 */
export interface Theme {
  mode: ThemeMode;
  dark_palette: string;
  light_palette: string;
  accent: string;
}

/** 服务端解析好的主题：握手下发，含两套完整调色板。 */
export interface ResolvedTheme {
  mode: ThemeMode;
  accent: string;
  light: Palette;
  dark: Palette;
}

export interface ModelEntry {
  id: string;
  name: string;
  context_len: number;
  /** 模型能力集合；缺省时后端仅补文本输入与文本输出 */
  capabilities: ModelCapability[];
  /** 最大输出 Token；未设置时各协议使用内置默认 */
  max_output?: number;
  /** 推理等级 → 厂商自定义字符串；未配置的等级按等级名下发 */
  reasoning_map?: Record<string, string>;
  /** 模型级请求头：同名覆盖提供商级 */
  headers?: Record<string, string>;
  /** 模型级请求体字段：递归合并，覆盖提供商级 */
  body?: unknown;
}

export interface ProviderConfig {
  api_type: string;
  base_url: string;
  api_key: string;
  /** 脱敏时随 api_key="***" 下发：真实密钥的字符数，供前端渲染等长占位符 */
  api_key_len?: number;
  headers: Record<string, string>;
  body: unknown;
  models?: ModelEntry[];
}

/** 完整服务端配置 (GET/PUT /api/config) */
export interface OmaConfig {
  default_model: string;
  default_agent: string;
  default_approval_mode: ApprovalMode;
  /** 新会话默认推理等级；空串 = 未指定（回退模型默认） */
  default_reasoning_level?: string;
  theme: Theme;
  /** `token` 由 GET 原样下发、又随整份配置回传，缺字段会把配置里已设的 token 清空 */
  server: { listen_addr: string; token: string };
  providers: Record<string, ProviderConfig>;
  mcp_servers: Record<string, McpServerConfig>;
}

export interface FileNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: FileNode[];
}

export interface ServerStatus {
  version: string;
  active_sessions: number;
  uptime_secs: number;
}

/** 可授权工具条目（GET /api/tools） */
export interface ToolInfo {
  name: string;
  description: string;
  /** builtin | mcp */
  kind: string;
}

/** POST /api/sessions/{id}/attachments 的响应 */
export interface UploadAttachmentResp {
  success: boolean;
  attachments: string[];
}

/** client.json 中一条已保存的连接 */
export interface ClientConnection {
  name: string;
  url: string;
  token: string;
}

/**
 * 客户端本地配置（<配置目录>/oma/client.json）。
 *
 * 只有 `oma web` 的同源接口 GET/PUT /api/client/config 读写它，
 * 与 Daemon 的 /api/config 无关。
 */
export interface ClientConfig {
  web: { host: string; port: number };
  connections: ClientConnection[];
  /** 当前活动的连接名；空或不匹配时回落到列表首个 */
  active: string;
}
