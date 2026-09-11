import { computed, ref } from 'vue';
import { toast } from 'vue-sonner';
import { api, getToken, wsUrl } from '../api';
import type {
  ActiveTurnCatchUp,
  McpServerSummary,
  AgentCommand,
  AgentEvent,
  AgentSummary,
  ApprovalDecision,
  ApprovalMode,
  AskAnswer,
  AskRequestedData,
  Block,
  ChatMessage,
  ClientMessage,
  ModelInfo,
  PermissionRequestedData,
  ServerMessage,
  TokenUsage,
} from '../types';
import { tr } from '../composables/i18n';
import { activeSessionId, applyRemoteRename, applyRemoteRunning } from './sessions';
import { releaseAll } from '../lib/attachments';

/** 流式轮次缓冲：按到达顺序排列的实时段（thinking/text/tool 交错）。 */
export interface LiveTool {
  call_id: string;
  /** 被调用的工具名 */
  tool_name: string;
  input: unknown;
  output?: string;
  is_error?: boolean;
  done: boolean;
}

export type LiveSegment =
  | { kind: 'thinking'; key: string; text: string }
  | { kind: 'text'; key: string; text: string }
  | { kind: 'tool'; key: string; tool: LiveTool };

export interface LiveTurn {
  segments: LiveSegment[];
}

let liveSeq = 0;

function emptyLive(): LiveTurn {
  return { segments: [] };
}

export const connected = ref(false);
export const messages = ref<ChatMessage[]>([]);
/** 全量消息树（含非当前分支的兄弟节点），用于分支切换候选。 */
export const tree = ref<ChatMessage[]>([]);
export const live = ref<LiveTurn>(emptyLive());
export const running = ref(false);
export const pendingApproval = ref<PermissionRequestedData | null>(null);
/** 待作答的提问（ask 工具）；null 表示无待办 */
export const pendingAsk = ref<AskRequestedData | null>(null);
export const currentLeafId = ref<string | null>(null);
/** 本轮已结束、正在回读持久化消息：期间保留流式缓冲，避免内容先消失再出现造成跳动 */
export const finalizing = ref(false);

export const activeModel = ref('');
export const activeAgent = ref('');
export const approvalMode = ref<ApprovalMode>('normal');
/** 当前会话推理等级；空串 = 未设置（回退模型默认） */
export const reasoningLevel = ref('');
/** 可选模型目录（provider → 模型清单），来自握手载荷的 model_catalog */
export const modelCatalog = ref<Record<string, ModelInfo[]>>({});
export const agents = ref<AgentSummary[]>([]);
export const mcpServers = ref<McpServerSummary[]>([]);
export const lastUsage = ref<TokenUsage | null>(null);
export const queued = ref(0);

export const modelList = computed(() => {
  const out: { selector: string; info: ModelInfo }[] = [];
  for (const [pid, models] of Object.entries(modelCatalog.value)) {
    for (const m of models) out.push({ selector: `${pid}/${m.id}`, info: m });
  }
  return out;
});

let ws: WebSocket | null = null;
let sessionId = '';
/** 当前会话 ID（供附件解析等需要鉴权路径的场景使用） */
export const currentSessionId = () => sessionId;
let workspacePath = '';
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
/** 重连代次：每次 open/close 自增，旧 socket 与本代次不符时一律不再自我恢复 */
let epoch = 0;
let disposed = false;

const clientId = `web_${crypto.randomUUID()}`;

/** 发送一帧；未连接时返回 false，调用方据此提示而不是静默丢弃。 */
function send(msg: ClientMessage): boolean {
  if (!ws || ws.readyState !== WebSocket.OPEN) return false;
  ws.send(JSON.stringify(msg));
  return true;
}

export function command(cmd: AgentCommand): boolean {
  return send({ kind: 'command', command: cmd });
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
      // 子代理轮次沿用同一事件类型：不能据此清空主流式缓冲或结束轮次状态
      if (ev.data?.subagent_id) break;
      running.value = true;
      live.value = emptyLive();
      break;
    case 'thinking_delta': {
      // 同类段连续则续写，否则新开一段，保持与真实到达顺序一致
      const last = live.value.segments[live.value.segments.length - 1];
      if (last && last.kind === 'thinking') last.text += ev.data?.delta ?? '';
      else live.value.segments.push({ kind: 'thinking', key: `th${liveSeq++}`, text: ev.data?.delta ?? '' });
      break;
    }
    case 'text_delta': {
      const last = live.value.segments[live.value.segments.length - 1];
      if (last && last.kind === 'text') last.text += ev.data?.delta ?? '';
      else live.value.segments.push({ kind: 'text', key: `tx${liveSeq++}`, text: ev.data?.delta ?? '' });
      break;
    }
    case 'tool_call_started': {
      // 同一 call_id 只保留一个段：重复事件会让单个工具渲染出多张卡片，
      // 而完成事件只能命中第一张，其余永远停在「执行中」
      const d = ev.data;
      if (d && !live.value.segments.some((s) => s.kind === 'tool' && s.tool.call_id === d.call_id)) {
        live.value.segments.push({ kind: 'tool', key: d.call_id, tool: { ...d, done: false } });
      }
      break;
    }
    case 'tool_call_finished': {
      const d = ev.data;
      if (!d) break;
      // 完成事件作用于全部同 id 段，已存在的重复段不会残留为「执行中」
      for (const seg of live.value.segments) {
        if (seg.kind === 'tool' && seg.tool.call_id === d.call_id) {
          seg.tool.output = d.output;
          seg.tool.is_error = d.is_error;
          seg.tool.done = true;
        }
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
      }
      break;
    case 'queue_cleared':
      queued.value = 0;
      break;
    case 'queue_updated':
      // 服务端权威队列深度，客户端不再自行累加（避免与丢弃的指令漂移）
      queued.value = ev.data?.pending ?? 0;
      break;
    case 'sync_required':
      // 事件流出现缺口：整体回读持久化状态，避免局部状态永久失真
      void reload();
      break;
    case 'turn_finished': {
      // 子代理结束时主流式轮次仍在继续，忽略其生命周期事件
      if (ev.data?.subagent_id) break;
      running.value = false;
      finalizing.value = true;
      if (ev.data) {
        lastUsage.value = ev.data.usage;
        if (ev.data.stop_reason === 'error') toast.error(tr('chat.turnError'));
      }
      currentLeafId.value = null; // 让服务端解析默认 leaf
      // 回读完成后再清空缓冲，持久化消息与流式内容同帧交接，界面不跳动
      void reload().finally(() => {
        live.value = emptyLive();
        finalizing.value = false;
      });
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
    case 'ask_requested':
      pendingAsk.value = ev.data ?? null;
      break;
    case 'ask_resolved':
      if (ev.data && pendingAsk.value?.request_id === ev.data.request_id) {
        pendingAsk.value = null;
      }
      break;
    case 'active_branch_changed':
      currentLeafId.value = ev.data?.current_leaf_id ?? null;
      // 编辑重发同样广播此事件：轮次进行中保留实时缓冲，避免打断流式渲染
      if (!running.value) live.value = emptyLive();
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
    case 'reasoning_level_changed':
      if (ev.data) reasoningLevel.value = ev.data.level;
      break;
    case 'active_turn_catch_up':
      applyCatchUp(ev.data ?? null);
      break;
    case 'session_renamed':
      if (ev.data) applyRemoteRename(ev.data.session_id, ev.data.title);
      break;
    case 'session_running':
      // 其他会话的运行状态由服务端广播：侧栏不依赖当前打开哪个会话
      if (ev.data) applyRemoteRunning(ev.data.session_id, ev.data.running);
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
  // 中途接入正在执行的轮次：侧栏同步显示运行中（该轮开始时的广播本端未收到）
  applyRemoteRunning(sessionId, true);
  // catch-up 快照不含到达顺序，按 thinking → text → 活动工具 重建
  const segments: LiveSegment[] = [];
  if (c.accumulated_thinking) segments.push({ kind: 'thinking', key: 'cu-th', text: c.accumulated_thinking });
  if (c.accumulated_text) segments.push({ kind: 'text', key: 'cu-tx', text: c.accumulated_text });
  if (c.active_tool_call) segments.push({ kind: 'tool', key: c.active_tool_call.call_id, tool: { ...c.active_tool_call, done: false } });
  live.value = { segments };
  pendingApproval.value = c.pending_approval ?? null;
  pendingAsk.value = c.pending_ask ?? null;
}

function connect() {
  if (disposed || !sessionId) return;
  const myEpoch = epoch;
  const socket = new WebSocket(wsUrl({}));
  ws = socket;

  socket.onopen = () => {
    // 旧连接若晚于新连接完成握手，会把自己当成当前连接覆盖全局状态
    if (myEpoch !== epoch) {
      socket.close();
      return;
    }
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

  socket.onmessage = (ev) => {
    // 已过期的连接不得再写入 store，否则旧会话的事件会污染当前会话视图
    if (myEpoch !== epoch) return;
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
      reasoningLevel.value = r.reasoning_level ?? '';
      modelCatalog.value = r.model_catalog;
      agents.value = r.agents;
      mcpServers.value = r.mcp_summaries ?? [];
      currentLeafId.value = r.current_leaf_id;
      void reload();
    } else if (msg.kind === 'event') {
      handleEvent(msg.event);
    } else if (msg.kind === 'error') {
      toast.error(msg.message);
    }
  };

  socket.onclose = () => {
    // 过期连接（含被 open() 主动关闭的）不得清空全局状态或另起重连，
    // 否则会在新连接之外再拉起第二条连接，同一事件被处理多次
    if (myEpoch !== epoch) return;
    connected.value = false;
    ws = null;
    if (!disposed && getToken()) {
      reconnectTimer = setTimeout(connect, 2000);
    }
  };
}

export async function open(id: string, workspace: string) {
  if (sessionId === id && connected.value) return;
  // reset() 内部 close() 已让代次自增，旧连接及其重连定时器全部作废
  reset();
  disposed = false;
  sessionId = id;
  workspacePath = workspace;
  connect();
}

/**
 * 断开连接并清空会话态（不含连接代次，由 close() 负责）。
 * 取消选中或删除当前会话时调用，避免悬空的 sessionId 与遗留消息被后续渲染。
 */
export function reset() {
  close();
  releaseAll();
  sessionId = '';
  workspacePath = '';
  messages.value = [];
  tree.value = [];
  live.value = emptyLive();
  pendingApproval.value = null;
  pendingAsk.value = null;
  running.value = false;
  finalizing.value = false;
  currentLeafId.value = null;
  lastUsage.value = null;
  queued.value = 0;
}

function close() {
  disposed = true;
  // 代次自增让在途 socket 的 open/close/重连回调全部失效
  epoch += 1;
  if (reconnectTimer) clearTimeout(reconnectTimer);
  reconnectTimer = null;
  ws?.close();
  ws = null;
  connected.value = false;
}

/**
 * 提交用户输入。返回是否已送达服务端；未连接时不做乐观插入，
 * 避免界面出现一条永远不会执行的消息。
 */
export function submit(content: string, attachments: string[] = []): boolean {
  if (!content.trim() && attachments.length === 0) return false;
  if (!command({ type: 'user_input', data: { content, attachments } })) return false;
  const blocks: Block[] = [];
  if (content) blocks.push({ type: 'text', text: content });
  for (const ref of attachments) blocks.push({ type: 'image', mime_type: '', data: ref });
  messages.value.push({
    id: `local_${crypto.randomUUID()}`,
    parent_id: currentLeafId.value,
    role: 'user',
    content: blocks,
    created_at: Date.now(),
  });
  return true;
}

/** 上传附件并返回可提交的引用列表。 */
export async function upload(files: File[]): Promise<string[]> {
  if (!sessionId) return [];
  return api.uploadAttachments(sessionId, files);
}

export function cancel(): boolean {
  return send({ kind: 'cancel' });
}

export function respond(decision: ApprovalDecision) {
  if (!pendingApproval.value) return;
  send({
    kind: 'approval',
    response: { request_id: pendingApproval.value.request_id, decision },
  });
}

/** 回答 ask 提问；is_cancelled = true 表示跳过作答。 */
export function respondAsk(answers: AskAnswer[], is_cancelled = false) {
  const pending = pendingAsk.value;
  if (!pending) return;
  send({
    kind: 'ask',
    response: { request_id: pending.request_id, answers, is_cancelled },
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

/** 设置当前会话推理等级；空串 = 回退模型默认。 */
export function setReasoningLevel(level: string) {
  command({ type: 'set_reasoning_level', data: { level } });
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

/** 从某条消息处分叉重跑（编辑重发）。返回是否已送达服务端。 */
export function forkAndRun(parentMessageId: string, newContent: string): boolean {
  return command({
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

/** 渲染序列：流式缓冲按到达顺序展开为块。 */
export const renderBlocks = computed<Block[]>(() => {
  const out: Block[] = [];
  const l = live.value;
  if (!running.value && l.segments.length === 0) return out;
  for (const seg of l.segments) {
    if (seg.kind === 'thinking') out.push({ type: 'thinking', thinking: seg.text });
    else if (seg.kind === 'text') out.push({ type: 'text', text: seg.text });
    else {
      out.push({ type: 'tool_use', id: seg.tool.call_id, name: seg.tool.tool_name, input: seg.tool.input });
      if (seg.tool.done) {
        out.push({
          type: 'tool_result',
          tool_use_id: seg.tool.call_id,
          content: seg.tool.output ?? '',
          is_error: !!seg.tool.is_error,
        });
      }
    }
  }
  return out;
});
