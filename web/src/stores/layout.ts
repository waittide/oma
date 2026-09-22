import { computed, ref, watch, type Ref } from 'vue';

/**
 * 工作区外壳的面板布局状态。
 *
 * 尺寸与断点对齐 pi-web 的 `lib/panel-layout.ts`：
 * 侧栏默认 260（180–480），右侧面板默认 42% 视口并夹在 360–640（300–1200），
 * 视口 < 960 视为紧凑布局，< 640 视为移动端。宽度持久化在 localStorage。
 */
export const SIDEBAR_DEFAULT_WIDTH = 340;
export const SIDEBAR_MIN_WIDTH = 280;
export const SIDEBAR_MAX_WIDTH = 480;

export const RIGHT_PANEL_FALLBACK_WIDTH = 560;
export const RIGHT_PANEL_MIN_WIDTH = 300;
export const RIGHT_PANEL_MAX_WIDTH = 1200;

export const MOBILE_MAX_WIDTH = 640;
export const SPLIT_PANEL_MIN_WIDTH = 960;

const STORAGE_KEY = 'oma.layout';

interface Persisted {
  sidebarOpen: boolean;
  sidebarWidth: number;
  rightOpen: boolean;
  rightWidth: number;
  rightTab: RightTab;
}

export type RightTab = 'files' | 'terminal' | 'changes' | 'tree';

function clamp(width: number, min: number, max: number): number {
  const finite = Number.isFinite(width) ? width : min;
  return Math.round(Math.max(min, Math.min(Math.max(min, max), finite)));
}

/** 右侧面板默认宽度：视口 42%，夹在 360–640（与 pi-web 一致）。 */
export function defaultRightPanelWidth(viewportWidth: number): number {
  return clamp(viewportWidth * 0.42, 360, 640);
}

function read(): Persisted {
  const fallback: Persisted = {
    sidebarOpen: true,
    sidebarWidth: SIDEBAR_DEFAULT_WIDTH,
    rightOpen: false,
    rightWidth: defaultRightPanelWidth(typeof window === 'undefined' ? 1440 : window.innerWidth),
    rightTab: 'files',
  };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<Persisted>;
    return {
      sidebarOpen: parsed.sidebarOpen ?? fallback.sidebarOpen,
      sidebarWidth: clamp(parsed.sidebarWidth ?? fallback.sidebarWidth, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH),
      rightOpen: parsed.rightOpen ?? fallback.rightOpen,
      rightWidth: clamp(parsed.rightWidth ?? fallback.rightWidth, RIGHT_PANEL_MIN_WIDTH, RIGHT_PANEL_MAX_WIDTH),
      rightTab: parsed.rightTab ?? fallback.rightTab,
    };
  } catch {
    return fallback;
  }
}

const state = read();

export const sidebarOpen = ref(state.sidebarOpen);
export const sidebarWidth = ref(state.sidebarWidth);
export const rightOpen = ref(state.rightOpen);
export const rightWidth = ref(state.rightWidth);
export const rightTab = ref<RightTab>(state.rightTab);

function persist() {
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        sidebarOpen: sidebarOpen.value,
        sidebarWidth: sidebarWidth.value,
        rightOpen: rightOpen.value,
        rightWidth: rightWidth.value,
        rightTab: rightTab.value,
      } satisfies Persisted),
    );
  } catch {
    // 隐私模式下 localStorage 可能不可写：布局退化为不持久化，不影响使用
  }
}

export function toggleSidebar() {
  sidebarOpen.value = !sidebarOpen.value;
  persist();
}

export function toggleRight() {
  rightOpen.value = !rightOpen.value;
  persist();
}

/** 打开右侧面板并切到指定标签页（已打开时仅切换标签）。 */
export function openRight(tab: RightTab) {
  rightTab.value = tab;
  rightOpen.value = true;
  persist();
}

export function setRightTab(tab: RightTab) {
  rightTab.value = tab;
  persist();
}

/**
 * 当前视口宽度：由 App 在挂载与 resize 时写入。
 *
 * 面板宽度必须按视口夹一次——侧栏与右侧面板都是 `flex-shrink: 0`，三列放不下时
 * 被挤扁的只有中间那列：聊天区会窄到输入框控件换行、越堆越高，最终把输入框顶出
 * 屏幕外（外壳是 `overflow: hidden`，连滚都滚不回来）。
 */
export const viewportWidth = ref(typeof window === 'undefined' ? 1440 : window.innerWidth);

/** 聊天区至少要留出的宽度；紧凑视口下再放宽一点给两栏 */
function chatMinWidth(width: number): number {
  return width < SPLIT_PANEL_MIN_WIDTH ? 320 : 420;
}

/** 渲染用右侧面板宽度：三列放不下时返回 0，App 据此整块不渲染 */
export const rightRenderWidth = computed(() => {
  if (!rightOpen.value || viewportWidth.value < SPLIT_PANEL_MIN_WIDTH) return 0;
  const room = viewportWidth.value - chatMinWidth(viewportWidth.value) - SIDEBAR_MIN_WIDTH;
  return clamp(rightWidth.value, RIGHT_PANEL_MIN_WIDTH, Math.max(RIGHT_PANEL_MIN_WIDTH, room));
});

/** 渲染用侧栏宽度：先给聊天空出下限，再给右侧面板留出最小宽度 */
export const sidebarRenderWidth = computed(() => {
  const reserved = rightRenderWidth.value > 0 ? RIGHT_PANEL_MIN_WIDTH : 0;
  const room = viewportWidth.value - chatMinWidth(viewportWidth.value) - reserved;
  return clamp(sidebarWidth.value, SIDEBAR_MIN_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, room));
});

/**
 * 右侧面板「文件」页的状态：按工作区记住当前打开的文件。
 *
 * 面板关掉（或切走标签页）时组件会卸载，选中项若只放在组件里就会丢；
 * 放在这里既能跨开关恢复，也不至于把不同工作区的路径混在一起。
 */
const FILE_STATE_KEY = 'oma.panelFile';

interface FilePanelState {
  /** 工作区 → 正在预览的文件相对路径 */
  open: Record<string, string>;
  /** markdown 是否直接看源码（默认渲染后的预览） */
  mdSource: boolean;
}

function readFileState(): FilePanelState {
  const fallback: FilePanelState = { open: {}, mdSource: false };
  try {
    const raw = localStorage.getItem(FILE_STATE_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<FilePanelState>;
    return { open: parsed.open ?? {}, mdSource: parsed.mdSource ?? false };
  } catch {
    return fallback;
  }
}

const fileState = readFileState();

/** 当前工作区正在预览的文件（空串表示显示文件树） */
export const openFilePath = ref('');
/** markdown 文件的「预览 / 源码」开关 */
export const markdownSource = ref(fileState.mdSource);

function persistFileState() {
  try {
    localStorage.setItem(FILE_STATE_KEY, JSON.stringify(fileState));
  } catch {
    // 隐私模式下写不了：状态退化为仅当前会话有效
  }
}

/** 切换工作区时载入该工作区上次打开的文件 */
export function useWorkspaceFileState(workspace: Ref<string>) {
  watch(
    workspace,
    (ws) => {
      openFilePath.value = ws ? (fileState.open[ws] ?? '') : '';
    },
    { immediate: true },
  );
}

/** 选中文件（空串表示退回文件树），并记到当前工作区名下 */
export function selectFilePath(workspace: string, path: string) {
  openFilePath.value = path;
  if (!workspace) return;
  if (path) fileState.open[workspace] = path;
  else delete fileState.open[workspace];
  persistFileState();
}

/** 切换 markdown 的预览/源码视图；该偏好与工作区无关，全局记住 */
export function setMarkdownSource(source: boolean) {
  markdownSource.value = source;
  fileState.mdSource = source;
  persistFileState();
}

/**
 * 文件树里已展开的目录：`{ 工作区 → { 相对路径: true } }`。
 *
 * 放在 store 并落盘，有两个原因：打开文件预览时树会被卸载（状态留在组件里就会整棵
 * 折起来），以及刷新页面后应当回到原来的展开位置。键按工作区区分——同一批相对路径
 * 换到别的仓库里指的不是同一批目录。
 */
const TREE_OPEN_KEY = 'oma.panelTreeOpen';

function readTreeOpenState(): Record<string, Record<string, boolean>> {
  try {
    const raw = localStorage.getItem(TREE_OPEN_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, Record<string, boolean>>;
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

const treeOpenState = readTreeOpenState();
/** 当前工作区的展开集合（与持久化结构共享同一份对象，改动会被写回） */
export const fileTreeOpen = ref<Record<string, boolean>>({});

/** 载入某工作区的展开集合（换工作区时调用） */
export function loadFileTreeOpen(workspace: string) {
  if (!workspace) {
    fileTreeOpen.value = {};
    return;
  }
  treeOpenState[workspace] ??= {};
  fileTreeOpen.value = treeOpenState[workspace];
}

watch(fileTreeOpen, persistTreeOpen, { deep: true });

function persistTreeOpen() {
  try {
    localStorage.setItem(TREE_OPEN_KEY, JSON.stringify(treeOpenState));
  } catch {
    // 隐私模式下写不了：展开状态退化为仅当前页面有效
  }
}

export function setSidebarWidth(width: number) {
  sidebarWidth.value = clamp(width, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
  persist();
}

export function setRightWidth(width: number) {
  rightWidth.value = clamp(width, RIGHT_PANEL_MIN_WIDTH, RIGHT_PANEL_MAX_WIDTH);
  persist();
}

/** 视口是否窄于分栏阈值（紧凑布局：左右面板不同时占位）。 */
export const isCompact = computed(() => {
  if (typeof window === 'undefined') return false;
  return window.innerWidth < SPLIT_PANEL_MIN_WIDTH;
});
