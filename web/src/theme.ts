import { computed, reactive, watchEffect } from 'vue';
import type { DarkFlavor, ThemeAccent, ThemeMode } from './types';

/**
 * 主题系统：单一事实来源为 Daemon 配置 (/api/config 的 theme 字段)。
 * localStorage 仅缓存最近一次 Daemon 下发的设置，用于首帧防闪烁；
 * 联网后立即以 Daemon 值校正。
 */

export const ACCENTS = [
  'rosewater',
  'flamingo',
  'pink',
  'mauve',
  'red',
  'maroon',
  'peach',
  'yellow',
  'green',
  'teal',
  'sky',
  'sapphire',
  'blue',
  'lavender',
] as const;

export type AccentName = ThemeAccent;
export type Flavor = 'latte' | DarkFlavor;

const CACHE_KEY = 'oma_theme_cache';
const DARK_FLAVORS: DarkFlavor[] = ['frappe', 'macchiato', 'mocha'];
const FLAVORS: Flavor[] = ['latte', ...DARK_FLAVORS];

export interface ThemePrefs {
  mode: ThemeMode;
  dark_flavor: DarkFlavor;
  accent: AccentName;
}

function sanitize(raw: Partial<ThemePrefs> | null): ThemePrefs {
  return {
    mode: raw?.mode === 'light' || raw?.mode === 'dark' || raw?.mode === 'system' ? raw.mode : 'dark',
    dark_flavor: DARK_FLAVORS.includes(raw?.dark_flavor as DarkFlavor)
      ? (raw!.dark_flavor as DarkFlavor)
      : 'mocha',
    accent: ACCENTS.includes(raw?.accent as AccentName) ? (raw!.accent as AccentName) : 'blue',
  };
}

function loadCache(): ThemePrefs {
  try {
    return sanitize(JSON.parse(localStorage.getItem(CACHE_KEY) || 'null'));
  } catch {
    return sanitize(null);
  }
}

export const themeState = reactive<ThemePrefs>(loadCache());

function prefersLight(): boolean {
  return window.matchMedia('(prefers-color-scheme: light)').matches;
}

/** 当前生效 flavor：浅色恒为 latte，深色由 dark_flavor 决定 */
export const activeFlavor = computed<Flavor>(() => {
  const light = themeState.mode === 'light' || (themeState.mode === 'system' && prefersLight());
  return light ? 'latte' : themeState.dark_flavor;
});

export const isDark = computed(() => activeFlavor.value !== 'latte');

function applyClasses(): void {
  const root = document.documentElement;
  root.classList.remove(...FLAVORS.map((f) => `theme-${f}`), ...ACCENTS.map((a) => `accent-${a}`));
  root.classList.add(`theme-${activeFlavor.value}`, `accent-${themeState.accent}`);
}

function persistCache(): void {
  localStorage.setItem(CACHE_KEY, JSON.stringify({ ...themeState }));
}

/** 用 Daemon 下发的主题设置覆盖本地状态 (单一事实来源) */
export function applyThemeFromDaemon(prefs: Partial<ThemePrefs> | null | undefined): void {
  const next = sanitize({ ...themeState, ...(prefs ?? {}) });
  themeState.mode = next.mode;
  themeState.dark_flavor = next.dark_flavor;
  themeState.accent = next.accent;
  persistCache();
}

/** 提交给 Daemon 的 theme 配置段 */
export function themeForConfig(): ThemePrefs {
  return { mode: themeState.mode, dark_flavor: themeState.dark_flavor, accent: themeState.accent };
}

/** 侧边栏快捷二态切换：从 system 显式化为 light/dark 对面 */
export function toggleMode(): void {
  themeState.mode = isDark.value ? 'light' : 'dark';
  persistCache();
}

/** 初始化：立即应用并跟随系统配色变化 (system 模式) */
export function initThemeSystem(): () => void {
  applyClasses();
  const stop = watchEffect(applyClasses);
  const media = window.matchMedia('(prefers-color-scheme: light)');
  const onChange = () => applyClasses();
  media.addEventListener('change', onChange);
  return () => {
    stop();
    media.removeEventListener('change', onChange);
  };
}
