import { computed, ref } from 'vue';
import { setUiPalettes, setUiTheme, type Palette as UiPalette } from '@waittide/ui';
import { api } from '../api';
import { tr } from '../composables/i18n';
import {
  ACCENTS,
  ACCENT_TOKENS,
  NEUTRAL_TOKENS,
  PALETTE_TOKENS,
  type Accent,
  type OmaConfig,
  type Palette,
  type PaletteMode,
  type PaletteToken,
  type ResolvedTheme,
  type Theme,
} from '../types';

export { ACCENTS, NEUTRAL_TOKENS, ACCENT_TOKENS, PALETTE_TOKENS };
export type { Accent, Palette, PaletteToken };

const DEFAULT_THEME: Theme = {
  mode: 'dark',
  dark_palette: 'pi-dark',
  light_palette: 'pi-light',
  accent: 'blue',
};

/** 服务端配置与主题缓存（设置页复用同一份）。 */
export const theme = ref<Theme>({ ...DEFAULT_THEME });
export const config = ref<OmaConfig | null>(null);
export const configReady = ref(false);
/** 全部可用调色板（GET /api/palettes）。 */
export const palettes = ref<Palette[]>([]);

const media =
  typeof window !== 'undefined' ? window.matchMedia('(prefers-color-scheme: light)') : null;

/** 当前生效的明暗：mode=system 时跟随系统。 */
export const isDark = computed(() => {
  if (theme.value.mode === 'light') return false;
  if (theme.value.mode === 'dark') return true;
  return !(media?.matches ?? false);
});

export const lightPalettes = computed(() => palettes.value.filter((p) => p.mode === 'light'));
export const darkPalettes = computed(() => palettes.value.filter((p) => p.mode === 'dark'));

function findPalette(id: string, mode: PaletteMode): Palette | undefined {
  return palettes.value.find((p) => p.id === id && p.mode === mode);
}

/**
 * 当前生效的调色板：按明暗挑选 id，找不到时回落到同明暗组的第一个。
 *
 * 服务端已保证引用可用，这里的兜底只为「调色板刚被删除而列表尚未刷新」这类
 * 瞬时窗口服务，避免整页配色变空。
 */
export const activePalette = computed<Palette | undefined>(() => {
  const wanted = isDark.value ? theme.value.dark_palette : theme.value.light_palette;
  const group = isDark.value ? darkPalettes.value : lightPalettes.value;
  return group.find((p) => p.id === wanted) ?? group[0];
});

/** 当前强调色令牌（非法值回落 blue）。 */
export const activeAccent = computed<PaletteToken>(() =>
  ACCENTS.includes(theme.value.accent as Accent) ? (theme.value.accent as Accent) : 'blue',
);

/**
 * 把当前主题与调色板同步给 @waittide/ui 的运行时。
 *
 * 具体的 CSS 变量写入由组件库负责（它监听自己的主题/调色板并写 <html>），
 * 这里只做「服务端配置 → 组件库状态」的投射，不再自己维护一套主题样式表。
 * 调色板为空（配置尚未拉取成功）时保留组件库内置方案，避免整页无色。
 */
export function applyTheme(): void {
  if (palettes.value.length > 0) {
    setUiPalettes(palettes.value as unknown as UiPalette[]);
  }
  setUiTheme({
    mode: theme.value.mode,
    dark_palette: theme.value.dark_palette,
    light_palette: theme.value.light_palette,
    accent: theme.value.accent,
  });
}

media?.addEventListener('change', () => {
  if (theme.value.mode === 'system') applyTheme();
});

/** 拉取调色板与配置；失败时保留默认主题以便界面仍可渲染。 */
export async function loadConfig(): Promise<void> {
  const [cfg, list] = await Promise.allSettled([api.getConfig(), api.palettes()]);
  config.value = cfg.status === 'fulfilled' ? cfg.value : null;
  if (list.status === 'fulfilled') palettes.value = list.value;
  if (config.value) theme.value = { ...DEFAULT_THEME, ...config.value.theme };
  configReady.value = true;
  applyTheme();
}

/** 主题保存：服务端校验引用合法后持久化到 settings.json。 */
export async function saveTheme(next: Theme): Promise<void> {
  const prev = theme.value;
  theme.value = next;
  applyTheme();
  if (!config.value) {
    theme.value = prev;
    applyTheme();
    throw new Error(tr('settings.configNotLoaded'));
  }
  const payload: OmaConfig = { ...config.value, theme: next };
  try {
    await api.putConfig(payload);
    config.value = payload;
  } catch (e) {
    // 服务端拒绝时回滚，否则界面显示的主题与落盘配置会长期不一致
    theme.value = prev;
    applyTheme();
    throw e;
  }
}

/** 提交任意配置修改（providers 等），成功后同步缓存。 */
export async function saveConfig(next: OmaConfig): Promise<void> {
  await api.putConfig(next);
  // 以服务端回读为准：本地回写会让脱敏密钥等字段停留在过期值上
  config.value = await api.getConfig();
  theme.value = { ...DEFAULT_THEME, ...config.value.theme };
  applyTheme();
}

/** 重新拉取调色板列表并刷新界面（调色板增删改后调用）。 */
export async function refreshPalettes(): Promise<void> {
  palettes.value = await api.palettes();
  applyTheme();
}

/** 把服务端握手下发的已解析主题应用到当前主题缓存。 */
export function applyResolvedTheme(resolved: ResolvedTheme): void {
  theme.value = {
    mode: resolved.mode,
    dark_palette: resolved.dark.id,
    light_palette: resolved.light.id,
    accent: resolved.accent,
  };
  applyTheme();
}
