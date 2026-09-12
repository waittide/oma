/** 与 Rust oma-contract / oma-config 序列化的 JSON 结构一一对应。 */

export type Role = 'system' | 'user' | 'assistant';
export type ClientType = 'tui' | 'web' | 'tauri' | 'cli';
export type ApprovalMode = 'normal' | 'strict' | 'auto';
export type ApprovalDecision = 'allow_once' | 'allow_session' | 'deny';
export type StopReason = 'end_turn' | 'tool_use' | 'max_tokens' | 'cancelled' | 'error';

export type Block =
  | { type: 'text'; text: string }
  | { type: 'thinking'; thinking: string }
  | { type: 'image'; mime_type: string; data: string }
  | { type: 'tool_use'; id: string; name: string; input: unknown }
  | { type: 'tool_result'; tool_use_id: string; content: string; is_error: boolean };

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
  supports_vision: boolean;
  supports_thinking: boolean;
  max_output?: number;
  /** 推理等级 → 厂商自定义字符串；未配置的等级按等级名下发 */
  reasoning_map?: Record<string, string>;
  input_types?: string[];
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

/** ask 工具：单个选项 */
export interface AskOption {
  label: string;
  description?: string;
}

/** ask 工具：单个问题 */
export interface AskQuestion {
  id: string;
  question: string;
  options: AskOption[];
  /** true = 多选 */
  is_multi?: boolean;
  /** 推荐选项下标 */
  recommended?: number | null;
}

/** ask 工具：用户对单个问题的回答 */
export interface AskAnswer {
  selected: string[];
  custom_input?: string;
}

/** ask 工具：提问请求 */
export interface AskRequestedData {
  request_id: string;
  questions: AskQuestion[];
}

/** ask 工具：作答响应 */
export interface AskResponse {
  request_id: string;
  answers: AskAnswer[];
  is_cancelled: boolean;
}

export interface ActiveTurnCatchUp {
  turn_id: string;
  accumulated_thinking: string;
  accumulated_text: string;
  active_tool_call?: ToolCallStartedData | null;
  pending_approval?: PermissionRequestedData | null;
  pending_ask?: AskRequestedData | null;
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
  | { type: 'ask_requested'; data?: AskRequestedData }
  | { type: 'ask_resolved'; data?: { request_id: string; is_cancelled: boolean; resolved_by: string } }
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
  | { kind: 'ask'; response: AskResponse }
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

/** 用户自定义主题：以内置 flavor 为基底的调色板覆盖 */
export interface CustomTheme {
  id: string;
  name: string;
  mode: 'light' | 'dark';
  base: string;
  colors: Record<string, string>;
}

/** 后端主题设置 (config::Theme)；浅色/深色均可引用内置或自定义主题 id */
export interface Theme {
  mode: 'light' | 'dark' | 'system';
  dark_flavor: string;
  light_theme: string;
  accent: string;
}

export interface ModelEntry {
  id: string;
  name: string;
  context_len: number;
  supports_vision: boolean;
  supports_thinking: boolean;
  /** 最大输出 Token；未设置时各协议使用内置默认 */
  max_output?: number;
  /** 推理等级 → 厂商自定义字符串；未配置的等级按等级名下发 */
  reasoning_map?: Record<string, string>;
  /** 支持的输入模态：text / image / video */
  input_types?: string[];
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
  server: { listen_addr: string };
  providers: Record<string, ProviderConfig>;
  mcp_servers: Record<string, McpServerConfig>;
  custom_themes: CustomTheme[];
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
