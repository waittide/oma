import { computed, ref } from 'vue';
import { toast } from 'vue-sonner';
import { api, getToken, wsUrl } from '../api';
import type {
  ActiveTurnCatchUp,
  AgentCommand,
  AgentEvent,
  AgentSummary,
  ApprovalDecision,
  ApprovalMode,
  Block,
  ChatMessage,
  ClientMessage,
  ModelInfo,
  PermissionRequestedData,
  ServerMessage,
  TokenUsage,
} from '../types';
import { tr } from '../composables/i18n';
import { activeSessionId, applyRemoteRename } from './sessions';

/** 流式轮次缓冲：当前 Turn 的实时块序列。 */
export interface LiveTool {
  call_id: string;
  name: string;
  input: unknown;
  output?: string;
  is_error?: boolean;
  done: boolean;
}

export interface LiveTurn {
  thinking: string;
  text: string;
  tools: LiveTool[];
}

function emptyLive(): LiveTurn {
  return { thinking: '', text: '', tools: [] };
}

export const connected = ref(false);
export const messages = ref<ChatMessage[]>([]);
/** 全量消息树（含非当前分支的兄弟节点），用于分支切换候选。 */
export const tree = ref<ChatMessage[]>([]);
export const live = ref<LiveTurn>(emptyLive());
export const running = ref(false);
export const pendingApproval = ref<PermissionRequestedData | null>(null);
export const currentLeafId = ref<string | null>(null);

export const activeModel = ref('');
export const activeAgent = ref('');
export const approvalMode = ref<ApprovalMode>('normal');
export const providers = ref<Record<string, ModelInfo[]>>({});
export const agents = ref<AgentSummary[]>([]);
export const lastUsage = ref<TokenUsage | null>(null);
export const queued = ref(0);

export const modelList = computed(() => {
  const out: { selector: string; info: ModelInfo }[] = [];
  for (const [pid, models] of Object.entries(providers.value)) {
    for (const m of models) out.push({ selector: `${pid}/${m.id}`, info: m });
  }
  return out;
});

let ws: WebSocket | null = null;
let sessionId = '';
let workspacePath = '';
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let disposed = false;

const clientId = `web_${crypto.randomUUID()}`;

function send(msg: ClientMessage) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

export function command(cmd: AgentCommand) {
  send({ kind: 'command', command: cmd });
}

async function reload() {
  if (!sessionId) return;
  try {
    const [linear, all] = await Promise.all([
      api.messages(sessionId, currentLeafId.value),
      api.messageTree(sessionId),
    ]);
    messages.value = linear;
    tree.value = all;
    if (currentLeafId.value === null && linear.length > 0) {
      currentLeafId.value = linear[linear.length - 1]!.id;
    }
  } catch (e) {
    toast.error(tr('chat.loadMessagesFailed', { message: (e as Error).message }));
  }
}

function handleEvent(ev: AgentEvent) {
  switch (ev.type) {
    case 'turn_started':
      running.value = true;
      live.value = emptyLive();
      break;
    case 'thinking_delta':
      live.value.thinking += ev.data?.delta ?? '';
      break;
    case 'text_delta':
      live.value.text += ev.data?.delta ?? '';
      break;
    case 'tool_call_started':
      if (ev.data) live.value.tools.push({ ...ev.data, done: false });
      break;
    case 'tool_call_finished': {
      const t = live.value.tools.find((x) => x.call_id === ev.data?.call_id);
      if (t && ev.data) {
        t.output = ev.data.output;
        t.is_error = ev.data.is_error;
        t.done = true;
      }
      break;
    }
    case 'user_message':
      // 其他客户端发送的消息实时可见；自己的发送由 submit 本地乐观落地
      if (ev.data && ev.data.client_id !== clientId) {
        messages.value.push({
          id: `remote_${ev.data.client_id}_${Date.now()}`,
          parent_id: currentLeafId.value,
          role: 'user',
          content: [{ type: 'text', text: ev.data.content }],
          created_at: Date.now(),
        });
        if (ev.data.queued) queued.value += 1;
      }
      break;
    case 'queue_cleared':
      queued.value = 0;
      break;
    case 'turn_finished': {
      running.value = false;
      live.value = emptyLive();
      if (ev.data) {
        lastUsage.value = ev.data.usage;
        if (ev.data.stop_reason === 'error') toast.error(tr('chat.turnError'));
      }
      currentLeafId.value = null; // 让服务端解析默认 leaf
      void reload();
      break;
    }
    case 'permission_requested':
      pendingApproval.value = ev.data ?? null;
      break;
    case 'permission_resolved':
      if (ev.data && pendingApproval.value?.request_id === ev.data.request_id) {
        pendingApproval.value = null;
      }
      break;
    case 'active_branch_changed':
      currentLeafId.value = ev.data?.current_leaf_id ?? null;
      live.value = emptyLive();
      running.value = false;
      void reload();
      break;
    case 'model_changed':
      if (ev.data) activeModel.value = ev.data.active_model;
      break;
    case 'agent_changed':
      if (ev.data) activeAgent.value = ev.data.active_agent;
      break;
    case 'approval_mode_changed':
      if (ev.data) approvalMode.value = ev.data.mode;
      break;
    case 'active_turn_catch_up':
      applyCatchUp(ev.data ?? null);
      break;
    case 'session_renamed':
      if (ev.data) applyRemoteRename(ev.data.session_id, ev.data.title);
      break;
    case 'messages_deleted':
      // 本端或他端删除消息（含编辑重发失败自动回滚）后统一回读
      currentLeafId.value = ev.data?.current_leaf_id ?? null;
      void reload();
      break;
    case 'error':
      toast.error(ev.data?.message ?? tr('chat.serverError'));
      break;
  }
}

function applyCatchUp(c: ActiveTurnCatchUp | null) {
  if (!c) return;
  running.value = true;
  live.value = {
    thinking: c.accumulated_thinking,
    text: c.accumulated_text,
    tools: c.active_tool_call ? [{ ...c.active_tool_call, done: false }] : [],
  };
  pendingApproval.value = c.pending_approval ?? null;
}

function connect() {
  if (disposed || !sessionId) return;
  ws = new WebSocket(wsUrl({}));

  ws.onopen = () => {
    connected.value = true;
    send({
      kind: 'connect',
      client_id: clientId,
      workspace: workspacePath,
      session_id: sessionId,
      client_type: 'web',
      client_name: 'Web',
      version: '1.0.0',
    });
  };

  ws.onmessage = (ev) => {
    let msg: ServerMessage;
    try {
      msg = JSON.parse(ev.data as string) as ServerMessage;
    } catch {
      return;
    }
    if (msg.kind === 'ready') {
      const r = msg.ready;
      activeModel.value = r.active_model;
      activeAgent.value = r.active_agent;
      approvalMode.value = r.approval_mode;
      providers.value = r.providers;
      agents.value = r.agents;
      currentLeafId.value = r.current_leaf_id;
      void reload();
    } else if (msg.kind === 'event') {
      handleEvent(msg.event);
    } else if (msg.kind === 'error') {
      toast.error(msg.message);
    }
  };

  ws.onclose = () => {
    connected.value = false;
    ws = null;
    if (!disposed && getToken()) {
      reconnectTimer = setTimeout(connect, 2000);
    }
  };
}

export async function open(id: string, workspace: string) {
  if (sessionId === id && connected.value) return;
  close();
  disposed = false;
  sessionId = id;
  workspacePath = workspace;
  messages.value = [];
  tree.value = [];
  live.value = emptyLive();
  pendingApproval.value = null;
  running.value = false;
  currentLeafId.value = null;
  lastUsage.value = null;
  connect();
}

export function close() {
  disposed = true;
  if (reconnectTimer) clearTimeout(reconnectTimer);
  reconnectTimer = null;
  ws?.close();
  ws = null;
  connected.value = false;
}

export function submit(content: string, attachments: string[] = []) {
  if (!content.trim() && attachments.length === 0) return;
  command({ type: 'user_input', data: { content, attachments } });
  messages.value.push({
    id: `local_${crypto.randomUUID()}`,
    parent_id: currentLeafId.value,
    role: 'user',
    content: [{ type: 'text', text: content }],
    created_at: Date.now(),
  });
}

export function cancel() {
  send({ kind: 'cancel' });
}

export function respond(decision: ApprovalDecision) {
  if (!pendingApproval.value) return;
  send({
    kind: 'approval',
    response: { request_id: pendingApproval.value.request_id, decision },
  });
}

export function setModel(model: string) {
  command({ type: 'set_model', data: { model } });
}

export function setAgent(agent: string) {
  command({ type: 'set_agent', data: { agent } });
}

export function setApprovalMode(mode: ApprovalMode) {
  command({ type: 'set_approval_mode', data: { mode } });
}

/** 删除消息及其子树；结果经 messages_deleted 事件广播回读。 */
export async function deleteMessage(messageId: string) {
  const sid = activeSessionId.value;
  if (!sid) return;
  try {
    await api.deleteMessage(sid, messageId);
    toast.success(tr('chat.deleted'));
  } catch (e) {
    toast.error(tr('chat.deleteFailed', { message: (e as Error).message }));
  }
}

export function switchBranch(leafId: string) {
  command({ type: 'switch_branch', data: { leaf_message_id: leafId } });
}

/** 从某条消息处分叉重跑（编辑重发）。 */
export function forkAndRun(parentMessageId: string, newContent: string) {
  command({
    type: 'fork_and_run',
    data: { parent_message_id: parentMessageId, new_content: newContent },
  });
}

/** 全树 tool_use_id → tool_result 映射：跨消息配对，重载后工具卡片仍为完成态。 */
export const toolResults = computed(() => {
  const out: Record<string, { content: string; is_error: boolean }> = {};
  for (const m of messages.value) {
    for (const b of m.content) {
      if (b.type === 'tool_result') out[b.tool_use_id] = { content: b.content, is_error: b.is_error };
    }
  }
  return out;
});

/** 运行时内部消息（仅 tool_result 的回执或空壳），不渲染气泡。 */
export function isInternalMessage(m: ChatMessage): boolean {
  return (
    m.content.length === 0 ||
    (m.role === 'user' && m.content.every((b) => b.type === 'tool_result'))
  );
}

/** 渲染序列：持久化消息 + 流式缓冲块。 */
export const renderBlocks = computed<Block[]>(() => {
  const out: Block[] = [];
  const l = live.value;
  if (!running.value && l.thinking === '' && l.text === '' && l.tools.length === 0) return out;
  if (l.thinking) out.push({ type: 'thinking', thinking: l.thinking });
  if (l.text) out.push({ type: 'text', text: l.text });
  for (const t of l.tools) {
    out.push({ type: 'tool_use', id: t.call_id, name: t.name, input: t.input });
    if (t.done) {
      out.push({
        type: 'tool_result',
        tool_use_id: t.call_id,
        content: t.output ?? '',
        is_error: !!t.is_error,
      });
    }
  }
  return out;
});
