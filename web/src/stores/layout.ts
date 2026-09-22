import { computed, ref } from 'vue';

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
