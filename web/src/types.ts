export type Role = 'system' | 'user' | 'assistant';

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

export type ClientType = 'tui' | 'web' | 'tauri' | 'cli';

export interface ConnectParams {
  client_id: string;
  workspace: string;
  session_id: string;
  client_type: ClientType;
  client_name: string;
  version: string;
}

export type ApprovalDecision = 'allow_once' | 'allow_session' | 'deny';
export type ApprovalMode = 'normal' | 'strict' | 'auto';

export interface ApprovalResponse {
  request_id: string;
  decision: ApprovalDecision;
}

export type AgentCommand =
  | { type: 'user_input'; data: { content: string; attachments?: string[] } }
  | { type: 'set_model'; data: { model: string } }
  | { type: 'set_agent'; data: { agent: string } }
  | { type: 'set_approval_mode'; data: { mode: ApprovalMode } }
  | { type: 'fork_and_run'; data: { parent_message_id: string; new_content?: string } }
  | { type: 'switch_branch'; data: { leaf_message_id: string } };

export type ClientMessage =
  | ({ kind: 'connect' } & ConnectParams)
  | { kind: 'command'; command: AgentCommand }
  | { kind: 'approval'; response: ApprovalResponse }
  | { kind: 'cancel' };

export interface ModelInfo {
  id: string;
  name: string;
  context_len: number;
  supports_vision: boolean;
  supports_thinking: boolean;
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
  subagent_id?: string;
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
  active_tool_call?: ToolCallStartedData;
  pending_approval?: PermissionRequestedData;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
}
export type StopReason = 'end_turn' | 'tool_use' | 'max_tokens' | 'cancelled' | 'error';

export type AgentEvent =
  | { type: 'turn_started'; data: { turn_id: string; subagent_id?: string } }
  | { type: 'turn_finished'; data: { turn_id: string; stop_reason: StopReason; usage: TokenUsage; subagent_id?: string } }
  | { type: 'user_message'; data: { client_id: string; client_name: string; client_type: ClientType; content: string; queued: boolean } }
  | { type: 'queue_cleared'; data?: unknown }
  | { type: 'thinking_delta'; data: { delta: string; subagent_id?: string } }
  | { type: 'text_delta'; data: { delta: string; subagent_id?: string } }
  | { type: 'tool_call_started'; data: ToolCallStartedData }
  | { type: 'tool_call_finished'; data: { call_id: string; name: string; output: string; is_error: boolean; subagent_id?: string } }
  | { type: 'permission_requested'; data: PermissionRequestedData }
  | { type: 'permission_resolved'; data: { request_id: string; decision: ApprovalDecision; resolved_by: string } }
  | { type: 'active_branch_changed'; data: { current_leaf_id: string } }
  | { type: 'model_changed'; data: { active_model: string } }
  | { type: 'agent_changed'; data: { active_agent: string } }
  | { type: 'approval_mode_changed'; data: { mode: ApprovalMode } }
  | { type: 'active_turn_catch_up'; data: ActiveTurnCatchUp }
  | { type: 'error'; data: { message: string } };

export interface Ready {
  version: string;
  session_id: string;
  workspace: string;
  active_model: string;
  active_agent: string;
  approval_mode: ApprovalMode;
  current_leaf_id?: string;
  providers: Record<string, ModelInfo[]>;
  agents: AgentSummary[];
}

export type ServerMessage =
  | { kind: 'ready'; ready: Ready }
  | { kind: 'event'; event: AgentEvent }
  | { kind: 'error'; message: string };

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

export interface FileNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: FileNode[];
}
