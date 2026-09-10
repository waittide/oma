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
  reasoning_effort?: string;
  input_types?: string[];
}

export interface AgentSummary {
  id: string;
  name: string;
  description: string;
}

export interface ToolCallStartedData {
  call_id: string;
  name: string;
  input: unknown;
  subagent_id?: string | null;
}

export interface PermissionRequestedData {
  request_id: string;
  name: string;
  summary: string;
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
  | { type: 'queue_cleared'; data?: Record<string, never> }
  | { type: 'queue_updated'; data?: { pending: number } }
  | { type: 'sync_required'; data?: Record<string, never> }
  | { type: 'thinking_delta'; data?: { delta: string; subagent_id?: string | null } }
  | { type: 'text_delta'; data?: { delta: string; subagent_id?: string | null } }
  | { type: 'tool_call_started'; data?: ToolCallStartedData }
  | { type: 'tool_call_finished'; data?: { call_id: string; name: string; output: string; is_error: boolean; subagent_id?: string | null } }
  | { type: 'permission_requested'; data?: PermissionRequestedData }
  | { type: 'permission_resolved'; data?: { request_id: string; decision: ApprovalDecision; resolved_by: string } }
  | { type: 'active_branch_changed'; data?: { current_leaf_id: string } }
  | { type: 'model_changed'; data?: { active_model: string } }
  | { type: 'agent_changed'; data?: { active_agent: string } }
  | { type: 'approval_mode_changed'; data?: { mode: ApprovalMode } }
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
  content: string;
}

/** 技能文件条目：按需读取的领域知识；scope = global | agent | project */
export interface SkillFile {
  id: string;
  name: string;
  description: string;
  scope: string;
  content: string;
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
  current_leaf_id: string | null;
  providers: Record<string, ModelInfo[]>;
  agents: AgentSummary[];
  mcp_servers: McpServerSummary[];
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
  current_leaf_id: string | null;
  created_at: number;
  updated_at: number;
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
  /** 推理等级："" (关闭) | low | medium | high */
  reasoning_effort?: string;
  /** 支持的输入模态：text / image / video */
  input_types?: string[];
}

export interface ProviderConfig {
  api_type: string;
  base_url: string;
  api_key: string;
  headers: Record<string, string>;
  body: unknown;
  models?: ModelEntry[];
}

/** 完整服务端配置 (GET/PUT /api/config) */
export interface OmaConfig {
  default_model: string;
  default_agent: string;
  default_approval_mode: ApprovalMode;
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

/** POST /api/sessions/{id}/attachments 的响应 */
export interface UploadAttachmentResp {
  success: boolean;
  attachments: string[];
}
