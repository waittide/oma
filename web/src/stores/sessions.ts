import { computed, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import { api } from '../api';
import { tr } from '../composables/i18n';
import type { SessionRecord } from '../types';

const COLLAPSED_KEY = 'oma.sidebar.collapsed';

/** 会话列表状态：按工作区路径分组、折叠持久化。 */
export const sessions = ref<SessionRecord[]>([]);
export const loading = ref(false);
export const activeSessionId = ref<string | null>(null);
/** 跨组件请求打开「新建会话」弹窗的信号（空态 CTA → 侧栏）。 */
export const newSessionRequested = ref(false);

export function requestNewSession() {
  newSessionRequested.value = true;
}

export const collapsed = ref<Record<string, boolean>>(
  (() => {
    try {
      return JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? '{}') as Record<string, boolean>;
    } catch {
      return {};
    }
  })(),
);

function persistCollapsed() {
  localStorage.setItem(COLLAPSED_KEY, JSON.stringify(collapsed.value));
}

export function toggleGroup(workspace: string) {
  collapsed.value[workspace] = !collapsed.value[workspace];
  persistCollapsed();
}

export interface WorkspaceGroup {
  workspace: string;
  label: string;
  items: SessionRecord[];
}

/** 按 workspace 分组；组按最近活跃时间排序，组内按 updated_at 倒序（服务端已排序）。 */
export const groups = computed<WorkspaceGroup[]>(() => {
  const map = new Map<string, SessionRecord[]>();
  for (const s of sessions.value) {
    const list = map.get(s.workspace);
    if (list) list.push(s);
    else map.set(s.workspace, [s]);
  }
  return [...map.entries()]
    .map(([workspace, items]) => ({
      workspace,
      label: basename(workspace),
      items,
    }))
    .sort((a, b) => {
      const ta = a.items[0]?.updated_at ?? 0;
      const tb = b.items[0]?.updated_at ?? 0;
      return tb - ta;
    });
});

export function basename(p: string): string {
  const trimmed = p.replace(/[/\\]+$/, '');
  const parts = trimmed.split(/[/\\]/);
  return parts[parts.length - 1] || trimmed || '/';
}

export const activeSession = computed(
  () => sessions.value.find((s) => s.session_id === activeSessionId.value) ?? null,
);

export async function refresh() {
  loading.value = true;
  try {
    sessions.value = await api.listSessions();
  } catch (e) {
    toast.error(tr('sessions.loadListFailed', { message: (e as Error).message }));
  } finally {
    loading.value = false;
  }
}

export async function create(workspace: string, title: string): Promise<SessionRecord | null> {
  try {
    const { session } = await api.createSession({ workspace, title });
    sessions.value.unshift(session);
    collapsed.value[workspace] = false;
    persistCollapsed();
    toast.success(tr('sessions.created'));
    return session;
  } catch (e) {
    toast.error(tr('sessions.createFailed', { message: (e as Error).message }));
    return null;
  }
}

export async function rename(id: string, title: string) {
  const target = sessions.value.find((s) => s.session_id === id);
  const old = target?.title;
  if (target) target.title = title;
  try {
    await api.renameSession(id, title);
  } catch (e) {
    if (target && old !== undefined) target.title = old;
    toast.error(tr('sessions.renameFailed', { message: (e as Error).message }));
  }
}

/** WebSocket 广播驱动的本地重命名。 */
export function applyRemoteRename(id: string, title: string) {
  const target = sessions.value.find((s) => s.session_id === id);
  if (target) target.title = title;
}

/** WebSocket 广播驱动的运行状态更新。 */
export function applyRemoteRunning(id: string, isRunning: boolean) {
  const target = sessions.value.find((s) => s.session_id === id);
  if (target) target.is_running = isRunning;
}

/** 是否存在正在执行的会话：决定是否需要轮询校准运行状态。 */
const anyRunning = computed(() => sessions.value.some((s) => s.is_running));

let watchTimer: ReturnType<typeof setInterval> | null = null;

/**
 * 校准运行状态：本端只订阅当前会话的事件，离开运行中的会话后
 * 收不到它的结束广播，故仅在存在运行中会话时轮询列表接口。
 */
async function syncRunningState() {
  if (document.hidden) return;
  try {
    const list = await api.listSessions();
    const running = new Map(list.map((s) => [s.session_id, !!s.is_running]));
    for (const s of sessions.value) s.is_running = running.get(s.session_id) ?? false;
  } catch {
    // 静默重试：只是状态校准，不干扰用户
  }
}

watch(anyRunning, (active) => {
  if (active && !watchTimer) watchTimer = setInterval(() => void syncRunningState(), 3000);
  else if (!active && watchTimer) {
    clearInterval(watchTimer);
    watchTimer = null;
  }
});

export async function remove(id: string) {
  try {
    await api.deleteSession(id);
    sessions.value = sessions.value.filter((s) => s.session_id !== id);
    if (activeSessionId.value === id) activeSessionId.value = null;
    toast.success(tr('sessions.deleted'));
  } catch (e) {
    toast.error(tr('sessions.deleteFailed', { message: (e as Error).message }));
  }
}

/**
 * 清空某工作区下的全部会话。运行中的会话会被服务端拒绝，跳过并计数，
 * 不中断其余会话的删除，最后按结果给出提示。
 */
export async function clearWorkspace(workspace: string) {
  const targets = sessions.value.filter((s) => s.workspace === workspace);
  if (targets.length === 0) return;

  const removed = new Set<string>();
  let failed = 0;
  for (const s of targets) {
    try {
      await api.deleteSession(s.session_id);
      removed.add(s.session_id);
    } catch {
      failed += 1;
    }
  }

  sessions.value = sessions.value.filter((s) => !removed.has(s.session_id));
  if (activeSessionId.value && removed.has(activeSessionId.value)) activeSessionId.value = null;

  if (failed === 0) toast.success(tr('sessions.cleared', { count: removed.size }));
  else toast.error(tr('sessions.clearPartial', { done: removed.size, failed }));
}
