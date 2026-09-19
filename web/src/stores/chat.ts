import { computed, ref } from 'vue';
import { toast } from '@waittide/ui';
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
import { notifyHumanEvent } from '../lib/notify';
import { activeSession, activeSessionId, applyRemoteRename, applyRemoteRunning } from './sessions';
import { applyResolvedTheme } from './theme';
import { releaseAll } from '../lib/attachments';
import {
  emptyLive,
  foldSegments,
  SubagentHostStack,
  type LiveSegment,
  type LiveTurn,
} from '../lib/liveSegments';

let liveSeq = 0;
/**
 * 轮次代次。
 *
 * 排队交棒时，下一轮会在上一轮 `turn_finished` 的回读尚未返回时就已开始；
 * 回读完成时据它判断是否已换轮，避免把新一轮刚吐出的流式内容一并抹掉。
 */
let turnSeq = 0;

export const connected = ref(false);
export const messages = ref<ChatMessage[]>([]);
/** 全量消息树（含非当前分支的兄弟节点），用于分支切换候选。 */
export const tree = ref<ChatMessage[]>([]);
export const live = ref<LiveTurn>(emptyLive());

/**
 * 子代理宿主栈：栈顶是当前正在执行的子代理所属的 task call_id。
 *
 * 推栈/弹栈必须与子代理的 turn_started / turn_finished 严格配对，
 * 具体理由见 [`SubagentHostStack`]。
 */
const hostStack = new SubagentHostStack();

/** 重置流式缓冲；栈与缓冲同生命周期，必须一并清空。 */
function setLive(next: LiveTurn) {
  live.value = next;
  hostStack.clear();
}

export const running = ref(false);
export const pendingApproval = ref<PermissionRequestedData | null>(null);
/** 待作答的提问（ask 工具）；null 表示无待办 */
export const pendingAsk = ref<AskRequestedData | null>(null);
export const currentLeafId = ref<string | null>(null);
/**
 * 视图截断点。
 *
 * - `null`：跟随服务端当前叶子（默认）；
 * - 消息 id：只显示到该消息为止；
 * - [`EMPTY_VIEW`]：空视图（预览首条消息之前——它前面没有内容）。
 *
 * 历史树预览某条分支时设置，不写入服务端（写服务端会让下次普通发送挂错分支）。
 */
export const viewLeafId = ref<string | null>(null);

/**
 * 空视图哨兵。
 *
 * 不能用空串：接口层会把假值当成“未指定 leaf_id”，服务端于是回退到已存的当前叶子，
 * 显示的是最新分支而不是空视图。而服务端对**未知** leaf 会返回空列表，因此用一个
 * 不可能与真实消息 id（UUID）碰撞的值来表达“没有内容”。
 */
const EMPTY_VIEW = '__empty_view__';
/** 输入框草稿；放在 store 里，历史树切换对话时需回填 */
export const draft = ref('');
/** 非空表示下一条发送将从该消息处分叉重跑（编辑重发） */
export const forkFrom = ref<string | null>(null);
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
/** 最近一次模型请求的上下文占用（提示侧总量，含缓存） */
export const contextUsage = ref<{ tokens: number; contextLen: number } | null>(null);
export const queued = ref(0);
/**
 * 排队中的用户输入（尚未落库）。
 *
 * 必须与 [`messages`] 分开：`messages` 是已持久化的线性消息流，而排队输入在轮到自己
 * 之前不会落库。混进去会让它被排在正在流式的助手回复**之前**渲染——也就是排队消息
 * 反而跑到上一条回复上面去。它们单独渲染在流式块下方。
 */
export const queuedMessages = ref<ChatMessage[]>([]);

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

/** 当前会话标题：通知正文用它指代是哪个会话完成了任务。 */
function sessionTitle(): string {
  return activeSession.value?.title || tr('chat.noSession');
}

/** 发送一帧；未连接时返回 false，调用方据此提示而不是静默丢弃。 */
function send(msg: ClientMessage): boolean {
  if (!ws || ws.readyState !== WebSocket.OPEN) return false;
  ws.send(JSON.stringify(msg));
  return true;
}

export function command(cmd: AgentCommand): boolean {
  return send({ kind: 'command', command: cmd });
}

/**
 * 用户消息的指纹：文本 + 附件引用。
 *
 * 用于乐观消息与服务端持久化消息的对账。附件也必须计入：只按文本时，
 * 「仅附件」的消息文本为空串，会与任意一条同类消息误匹配（或永远匹配不上）。
 * 服务端持久化的图片块存的就是 `session_attachment://` 引用，两边口径一致。
 */
function messageKey(m: ChatMessage): string {
  const text = m.content
    .filter((b) => b.type === 'text')
    .map((b) => b.text)
    .join('\n');
  const images = m.content
    .filter((b) => b.type === 'image')
    .map((b) => b.data)
    .join(',');
  return `${text}\u{0}${images}`;
}

/**
 * 排队输入一旦落库就从「排队中」区移除，改由正式消息流渲染。
 *
 * 排队输入要等轮到自己才开始落库，因此只能在每次回读后回来校准。
 */
function pruneQueued(server: ChatMessage[]) {
  if (queuedMessages.value.length === 0) return;
  const persisted = new Set(server.filter((m) => m.role === 'user').map(messageKey));
  queuedMessages.value = queuedMessages.value.filter((m) => !persisted.has(messageKey(m)));
}

async function reload() {
  if (!sessionId) return;
  try {
    const [linear, all] = await Promise.all([
      api.messages(sessionId, viewLeafId.value === EMPTY_VIEW ? EMPTY_VIEW : (viewLeafId.value ?? currentLeafId.value)),
      api.messageTree(sessionId),
    ]);
    messages.value = linear;
    tree.value = all;
    pruneQueued(linear);
    if (viewLeafId.value === null && currentLeafId.value === null && linear.length > 0) {
      currentLeafId.value = linear[linear.length - 1]!.id;
    }
  } catch (e) {
    toast.error(tr('chat.loadMessagesFailed', { message: (e as Error).message }));
  }
}

function handleEvent(ev: AgentEvent) {
  switch (ev.type) {
    case 'turn_started':
      // 子代理轮次沿用同一事件类型：仅压入宿主栈，不清空主流式缓冲
      if (ev.data?.subagent_id) {
        hostStack.push(live.value.segments);
        break;
      }
      turnSeq += 1;
      running.value = true;
      setLive(emptyLive());
      break;
    case 'thinking_delta': {
      // 同类段连续则续写，否则新开一段，保持与真实到达顺序一致
      const host = ev.data?.subagent_id ? hostStack.current() : undefined;
      const last = live.value.segments[live.value.segments.length - 1];
      if (last && last.kind === 'thinking' && last.host_call_id === host) last.text += ev.data?.delta ?? '';
      else
        live.value.segments.push({
          kind: 'thinking',
          key: `th${liveSeq++}`,
          text: ev.data?.delta ?? '',
          host_call_id: host,
        });
      break;
    }
    case 'text_delta': {
      const host = ev.data?.subagent_id ? hostStack.current() : undefined;
      const last = live.value.segments[live.value.segments.length - 1];
      if (last && last.kind === 'text' && last.host_call_id === host) last.text += ev.data?.delta ?? '';
      else
        live.value.segments.push({
          kind: 'text',
          key: `tx${liveSeq++}`,
          text: ev.data?.delta ?? '',
          host_call_id: host,
        });
      break;
    }
    case 'tool_call_started': {
      // 同一 call_id 只保留一个段：重复事件会让单个工具渲染出多张卡片，
      // 而完成事件只能命中第一张，其余永远停在「执行中」
      const d = ev.data;
      if (d && !live.value.segments.some((s) => s.kind === 'tool' && s.tool.call_id === d.call_id)) {
        live.value.segments.push({
          kind: 'tool',
          key: d.call_id,
          host_call_id: d.subagent_id ? hostStack.current() : undefined,
          tool: { ...d, done: false },
        });
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
        const msg: ChatMessage = {
          id: `remote_${ev.data.client_id}_${Date.now()}`,
          parent_id: currentLeafId.value,
          role: 'user',
          content: [{ type: 'text', text: ev.data.content }],
          created_at: Date.now(),
        };
        // 排队中的输入尚未落库，进队列区；否则会插到正在流式的回复上方
        if (ev.data.queued) queuedMessages.value.push(msg);
        else messages.value.push(msg);
      }
      break;
    case 'queue_cleared':
      queued.value = 0;
      // 队列被清空（取消）时这些输入从未落库，直接丢弃
      queuedMessages.value = [];
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
      // 子代理结束时主流式轮次仍在继续：仅弹出宿主栈，忽略其生命周期事件
      if (ev.data?.subagent_id) {
        hostStack.pop();
        break;
      }
      running.value = false;
      finalizing.value = true;
      if (ev.data) {
        lastUsage.value = ev.data.usage;
        if (ev.data.stop_reason === 'error') toast.error(tr('chat.turnError'));
        // 出错时已单独报错，不再以「完成」重复打扰
        else notifyHumanEvent('turn', sessionTitle());
      }
      currentLeafId.value = null; // 让服务端解析默认 leaf
      // 回读完成后再清空缓冲，持久化消息与流式内容同帧交接，界面不跳动
      const seq = turnSeq;
      void reload().finally(() => {
        // 回读期间若已开始新一轮（排队交棒），缓冲已归新一轮所有，不能抹掉
        if (turnSeq === seq) setLive(emptyLive());
        finalizing.value = false;
      });
      break;
    }
    case 'permission_requested':
      pendingApproval.value = ev.data ?? null;
      if (ev.data) notifyHumanEvent('approval', ev.data.tool_name);
      break;
    case 'permission_resolved':
      if (ev.data && pendingApproval.value?.request_id === ev.data.request_id) {
        pendingApproval.value = null;
      }
      break;
    case 'ask_requested':
      pendingAsk.value = ev.data ?? null;
      if (ev.data?.questions.length) {
        notifyHumanEvent('ask', ev.data.questions[0]!.question);
      }
      break;
    case 'ask_resolved':
      if (ev.data && pendingAsk.value?.request_id === ev.data.request_id) {
        pendingAsk.value = null;
      }
      break;
    case 'active_branch_changed':
      currentLeafId.value = ev.data?.current_leaf_id ?? null;
      // 编辑重发同样广播此事件：轮次进行中保留实时缓冲，避免打断流式渲染
      if (!running.value) setLive(emptyLive());
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
    case 'context_usage':
      if (ev.data) {
        contextUsage.value = { tokens: ev.data.tokens, contextLen: ev.data.context_len };
      }
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
  // 快照里的活动工具若正是 task，说明接入时子代理正在跑：后续子代理事件
  // 会照常压栈归位；此前已发生的子代理输出无法恢复（服务端不留存）
  const segments: LiveSegment[] = [];
  if (c.accumulated_thinking)
    segments.push({ kind: 'thinking', key: 'cu-th', text: c.accumulated_thinking });
  if (c.accumulated_text) segments.push({ kind: 'text', key: 'cu-tx', text: c.accumulated_text });
  if (c.active_tool_call)
    segments.push({
      kind: 'tool',
      key: c.active_tool_call.call_id,
      tool: { ...c.active_tool_call, done: false },
    });
  setLive({ segments });
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
      // 握手下发的是服务端已解析的主题：优先于 GET /api/config 的缓存，
      // 保证终端与浏览器看到的配色完全一致
      if (r.active_theme) applyResolvedTheme(r.active_theme);
      activeModel.value = r.active_model;
      activeAgent.value = r.active_agent;
      approvalMode.value = r.approval_mode;
      reasoningLevel.value = r.reasoning_level ?? '';
      modelCatalog.value = r.model_catalog;
      agents.value = r.agents;
      mcpServers.value = r.mcp_summaries ?? [];
      // 重启/重连后无内存态：用握手携带的上次占用立即恢复进度条
      contextUsage.value = r.context_usage
        ? { tokens: r.context_usage.tokens, contextLen: r.context_usage.context_len }
        : null;
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
  setLive(emptyLive());
  pendingApproval.value = null;
  pendingAsk.value = null;
  running.value = false;
  finalizing.value = false;
  currentLeafId.value = null;
  viewLeafId.value = null;
  draft.value = '';
  forkFrom.value = null;
  lastUsage.value = null;
  contextUsage.value = null;
  queued.value = 0;
  queuedMessages.value = [];
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
  // 发送即回到最新分支：预览态的截断点不应影响新消息的落位
  viewLeafId.value = null;
  const blocks: Block[] = [];
  if (content) blocks.push({ type: 'text', text: content });
  for (const ref of attachments) blocks.push({ type: 'image', mime_type: '', data: ref });
  const msg: ChatMessage = {
    id: `local_${crypto.randomUUID()}`,
    parent_id: currentLeafId.value,
    role: 'user',
    content: blocks,
    created_at: Date.now(),
  };
  // 轮次运行中时服务端会把这条输入排队，此时它尚未落库：放进队列区单独渲染，
  // 否则它会排在正在流式的回复上方（排队消息反而跑到上一条回复前面去）。
  if (running.value) queuedMessages.value.push(msg);
  else messages.value.push(msg);
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

/**
 * 只在本端预览到某条消息（不告诉服务端）。
 *
 * `leafId` 为 null 表示空视图（如预览首条用户消息时，它之前没有内容）。
 * 服务端当前叶子保持不动，因此下次发送时 `append_message` 会按用户消息的 parent
 * 重新落位；这也是“在某个中间点分叉后继续对话”能正常追加新分支的原因。
 */
export function showAt(leafId: string | null) {
  viewLeafId.value = leafId ?? EMPTY_VIEW;
  void reload();
}

/** 回到服务端当前叶子，退出预览状态。 */
export function followCurrent() {
  if (viewLeafId.value === null) return;
  viewLeafId.value = null;
  void reload();
}

/** 用户消息之前的截断点：它的父节点；无父节点（首条）时返回 null 空视图。 */
export function truncatePointFor(messageId: string): string | null {
  const msg = messages.value.find((m) => m.id === messageId) ?? tree.value.find((m) => m.id === messageId);
  return msg?.parent_id ?? null;
}

/**
 * 进入编辑重发（分叉）模式：下一条发送从 `messageId` 处分叉。
 *
 * `messageId` 为 null 表示无处可分叉（例如被点的是会话首条用户消息，它前面没有节点），
 * 此时只保留草稿、退出分叉模式。
 * `content` 省略时保留当前草稿（点助手回复的场景）。
 */
export function startForkFrom(messageId: string | null, content?: string) {
  forkFrom.value = messageId;
  if (content !== undefined) draft.value = content;
}

/** 从某条消息处分叉重跑（编辑重发）。返回是否已送达服务端。 */
export function forkAndRun(parentMessageId: string, newContent: string): boolean {
  const sent = command({
    type: 'fork_and_run',
    data: { parent_message_id: parentMessageId, new_content: newContent },
  });
  if (sent) viewLeafId.value = null;
  return sent;
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

/**
 * 运行时内部消息：不渲染成用户气泡的机器消息。
 *
 * 含 `tool_result` 的 user 消息都是工具回执（可能还带图片，如图像类工具的输出），
 * 它不是用户说的话，因此一律按内部消息处理。这里按「包含」而非「全部是」判断，
 * 否则回执后追加任意块（如 read 读图带回来的图片）就会让它变成一个假的用户气泡。
 */
export function isInternalMessage(m: ChatMessage): boolean {
  return m.content.length === 0 || m.content.some((b) => b.type === 'tool_result');
}

/** 渲染序列：流式缓冲按到达顺序展开为块。 */
export const renderBlocks = computed<Block[]>(() => {
  if (!running.value && live.value.segments.length === 0) return [];
  return foldSegments(live.value.segments);
});
