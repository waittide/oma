import { ref } from 'vue';
import { api } from '../api';
import type { OmaConfig, Theme } from '../types';

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
export type Accent = (typeof ACCENTS)[number];

const DEFAULT_THEME: Theme = { mode: 'dark', dark_flavor: 'mocha', accent: 'blue' };

/** 后端下发的主题与完整配置（设置页复用同一份缓存）。 */
export const theme = ref<Theme>({ ...DEFAULT_THEME });
export const config = ref<OmaConfig | null>(null);
export const configReady = ref(false);

const media =
  typeof window !== 'undefined' ? window.matchMedia('(prefers-color-scheme: light)') : null;

function resolveFlavor(t: Theme): string {
  const preferLight =
    t.mode === 'light' || (t.mode === 'system' && !!media && media.matches);
  if (preferLight) return 'latte';
  return t.dark_flavor === 'frappe' || t.dark_flavor === 'macchiato' ? t.dark_flavor : 'mocha';
}

/** 将后端主题应用到 <html>：flavor class + accent class。 */
export function applyTheme(): void {
  const root = document.documentElement;
  const flavor = resolveFlavor(theme.value);
  root.classList.remove('theme-latte', 'theme-frappe', 'theme-macchiato', 'theme-mocha');
  root.classList.add(`theme-${flavor}`);

  root.classList.remove(...ACCENTS.map((a) => `accent-${a}`));
  root.classList.add(`accent-${ACCENTS.includes(theme.value.accent as Accent) ? theme.value.accent : 'blue'}`);
}

media?.addEventListener('change', () => {
  if (theme.value.mode === 'system') applyTheme();
});

/** 启动时拉取后端配置并应用主题；失败时回落默认主题。 */
export async function loadConfig(): Promise<void> {
  try {
    config.value = await api.getConfig();
    theme.value = { ...DEFAULT_THEME, ...config.value.theme };
  } catch {
    config.value = null;
  }
  configReady.value = true;
  applyTheme();
}

/** 更新主题并写回后端 (PUT /api/config)。 */
export async function saveTheme(next: Theme): Promise<void> {
  theme.value = next;
  applyTheme();
  if (!config.value) throw new Error('服务端配置未加载');
  const payload: OmaConfig = { ...config.value, theme: next };
  await api.putConfig(payload);
  config.value = payload;
}

/** 提交任意配置修改（providers 等），成功后同步缓存。 */
export async function saveConfig(next: OmaConfig): Promise<void> {
  await api.putConfig(next);
  config.value = next;
  theme.value = { ...DEFAULT_THEME, ...next.theme };
  applyTheme();
}
