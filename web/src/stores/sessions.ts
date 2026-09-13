import { computed, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import { api } from '../api';
import { tr } from '../composables/i18n';
import type { SessionRecord } from '../types';

const COLLAPSED_KEY = 'oma.sidebar.collapsed';
/** 已发现过的工作区路径：会话全部删除后分组仍保留，供新建/删除整个工作区 */
const WORKSPACES_KEY = 'oma.workspaces';
/** 侧栏排序方式（持久化，刷新后保持） */
const SORT_KEY = 'oma.sidebar.sort';

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

/** 已知工作区集合（含已无会话者）；只在本地记录，不占服务端 schema。 */
export const knownWorkspaces = ref<string[]>(
  (() => {
    try {
      const raw = JSON.parse(localStorage.getItem(WORKSPACES_KEY) ?? '[]') as unknown;
      return Array.isArray(raw) ? raw.filter((v): v is string => typeof v === 'string') : [];
    } catch {
      return [];
    }
  })(),
);

function persistWorkspaces() {
  localStorage.setItem(WORKSPACES_KEY, JSON.stringify(knownWorkspaces.value));
}

/** 记住某个工作区：新建会话（含服务端返回的已有会话）时调用。 */
export function rememberWorkspace(workspace: string) {
  if (!workspace || knownWorkspaces.value.includes(workspace)) return;
  knownWorkspaces.value.push(workspace);
  persistWorkspaces();
}

/** 彻底移除工作区记录：仅在用户显式删除工作区时调用。 */
export function forgetWorkspace(workspace: string) {
  knownWorkspaces.value = knownWorkspaces.value.filter((w) => w !== workspace);
  persistWorkspaces();
  delete collapsed.value[workspace];
  persistCollapsed();
}

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

/** 侧栏排序字段与方向 */
export type SortKey = 'name' | 'created' | 'updated';
export type SortDir = 'asc' | 'desc';
export const SORT_KEYS: SortKey[] = ['name', 'created', 'updated'];
export const SORT_DIRS: SortDir[] = ['asc', 'desc'];

/** 侧栏搜索关键词（匹配工作区名称与会话标题） */
export const query = ref('');

/** 当前排序：按“名称 / 创建时间 / 修改时间”与升降序组合。 */
export const sort = ref<{ key: SortKey; dir: SortDir }>(
  (() => {
    try {
      const raw = JSON.parse(localStorage.getItem(SORT_KEY) ?? 'null') as {
        key?: SortKey;
        dir?: SortDir;
      } | null;
      if (raw && SORT_KEYS.includes(raw.key as SortKey) && SORT_DIRS.includes(raw.dir as SortDir)) {
        return { key: raw.key as SortKey, dir: raw.dir as SortDir };
      }
    } catch {
      // 存储损坏时回落默认排序
    }
    // 默认与旧行为一致：最近修改优先
    return { key: 'updated' as SortKey, dir: 'desc' as SortDir };
  })(),
);

/** 排序值字符串：`名称:创建时间` 形式的单一下拉项 */
export const sortValue = computed(() => `${sort.value.key}:${sort.value.dir}`);

/** 选择排序：解析下拉项并持久化。 */
export function setSort(value: string) {
  const [key, dir] = value.split(':') as [SortKey, SortDir];
  if (!SORT_KEYS.includes(key) || !SORT_DIRS.includes(dir)) return;
  sort.value = { key, dir };
  localStorage.setItem(SORT_KEY, JSON.stringify(sort.value));
}

/** 比较函数：名称走本地化比较（大小写不敏感），时间戳直接相减。 */
function compare(a: SessionRecord, b: SessionRecord, key: SortKey): number {
  if (key === 'name') return a.title.localeCompare(b.title, undefined, { sensitivity: 'base' });
  if (key === 'created') return a.created_at - b.created_at;
  return a.updated_at - b.updated_at;
}

/**
 * 分组工作区：会话按 workspace 聚合，再并入「已知但已无会话」的工作区。
 *
 * 搜索在分组前做：工作区名称命中时保留其全部会话（否则搜工作区名会得到一个空壳），
 * 会话标题命中时只保留命中的会话。
 */
export const groups = computed<WorkspaceGroup[]>(() => {
  const q = query.value.trim().toLowerCase();
  const dir = sort.value.dir === 'asc' ? 1 : -1;

  const map = new Map<string, SessionRecord[]>();
  for (const s of sessions.value) {
    const list = map.get(s.workspace);
    if (list) list.push(s);
    else map.set(s.workspace, [s]);
  }
  // 已知但已无会话的工作区也要能建分组与参与搜索
  for (const workspace of knownWorkspaces.value) {
    if (!map.has(workspace)) map.set(workspace, []);
  }

  const groupList: WorkspaceGroup[] = [...map.entries()]
    .map(([workspace, items]) => ({ workspace, label: basename(workspace), items }))
    .filter((g) => {
      if (!q) return true;
      if (g.label.toLowerCase().includes(q) || g.workspace.toLowerCase().includes(q)) return true;
      return g.items.some((s) => s.title.toLowerCase().includes(q));
    });

  for (const g of groupList) {
    const wsHit = !q || g.label.toLowerCase().includes(q) || g.workspace.toLowerCase().includes(q);
    // 工作区名命中时保留全部会话；否则只留标题命中的
    g.items = (wsHit ? g.items : g.items.filter((s) => s.title.toLowerCase().includes(q)))
      .slice()
      .sort((a, b) => compare(a, b, sort.value.key) * dir);
  }

  // 工作区分组之间用同一种排序：名称比分组名，时间取组内极值
  groupList.sort((a, b) => {
    if (sort.value.key === 'name') {
      return a.label.localeCompare(b.label, undefined, { sensitivity: 'base' }) * dir;
    }
    const pick = (g: WorkspaceGroup, fn: (s: SessionRecord) => number) => {
      if (g.items.length === 0) return null;
      const values = g.items.map(fn);
      // 升序看最早、降序看最晚，保证组序与组内顺序方向一致
      return dir === 1 ? Math.min(...values) : Math.max(...values);
    };
    const fn = sort.value.key === 'created' ? (s: SessionRecord) => s.created_at : (s: SessionRecord) => s.updated_at;
    const va = pick(a, fn);
    const vb = pick(b, fn);
    // 空分组没有时间可比：始终排在末尾，避免它们随着升降序乱跳
    if (va === null && vb === null) return a.label.localeCompare(b.label, undefined, { sensitivity: 'base' });
    if (va === null) return 1;
    if (vb === null) return -1;
    return (va - vb) * dir;
  });

  return groupList;
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
    // 服务端存在的会话所属工作区也计入已知集合：换浏览器后仍能看到其分组
    sessions.value.forEach((s) => rememberWorkspace(s.workspace));
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
    rememberWorkspace(workspace);
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
