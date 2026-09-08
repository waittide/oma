import { computed, ref } from 'vue';
import { api } from '../api';
import { tr } from '../composables/i18n';
import type { CustomTheme, OmaConfig, Theme } from '../types';

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

export type Flavor = 'latte' | 'frappe' | 'macchiato' | 'mocha';

/** 内置 flavor 的中性调色板（自定义主题编辑器的基底值） */
export const PALETTES: Record<Flavor, Record<string, string>> = {
  latte: {
    base: '#eff1f5', mantle: '#e6e9ef', crust: '#dce0e8', text: '#4c4f69',
    subtext1: '#5c5f77', subtext0: '#6c6f85', surface2: '#acb0be', surface1: '#bcc0cc',
    surface0: '#ccd0da', overlay2: '#7c7f93', overlay1: '#8c8fa1', overlay0: '#9ca0b0',
  },
  frappe: {
    base: '#303446', mantle: '#292c3c', crust: '#232634', text: '#c6d0f5',
    subtext1: '#b5bfe2', subtext0: '#a5adce', surface2: '#626880', surface1: '#51576d',
    surface0: '#414559', overlay2: '#949cbb', overlay1: '#838ba7', overlay0: '#737994',
  },
  macchiato: {
    base: '#24273a', mantle: '#1e2030', crust: '#181926', text: '#cad3f5',
    subtext1: '#b8c0e0', subtext0: '#a5adcb', surface2: '#5b6078', surface1: '#494d64',
    surface0: '#363a4f', overlay2: '#939ab7', overlay1: '#8087a2', overlay0: '#6e738d',
  },
  mocha: {
    base: '#1e1e2e', mantle: '#181825', crust: '#11111b', text: '#cdd6f4',
    subtext1: '#bac2de', subtext0: '#a6adc8', surface2: '#585b70', surface1: '#45475a',
    surface0: '#313244', overlay2: '#9399b2', overlay1: '#7f849c', overlay0: '#6c7086',
  },
};

const DEFAULT_THEME: Theme = { mode: 'dark', dark_flavor: 'mocha', light_theme: 'latte', accent: 'blue' };

/** 后端下发的主题与完整配置（设置页复用同一份缓存）。 */
export const theme = ref<Theme>({ ...DEFAULT_THEME });
export const config = ref<OmaConfig | null>(null);
export const configReady = ref(false);

const media =
  typeof window !== 'undefined' ? window.matchMedia('(prefers-color-scheme: light)') : null;

export const customThemes = computed<CustomTheme[]>(() => config.value?.custom_themes ?? []);

/** 解析当前生效主题：引用自定义主题时给出基底 flavor 与覆盖表。 */
export function resolveActiveTheme(): { base: Flavor; custom?: CustomTheme } {
  const preferLight =
    theme.value.mode === 'light' || (theme.value.mode === 'system' && !!media && media.matches);
  const id = preferLight ? theme.value.light_theme : theme.value.dark_flavor;
  const custom = customThemes.value.find((t) => t.id === id);
  if (custom) {
    const base = (PALETTES[custom.base as Flavor] ? custom.base : 'mocha') as Flavor;
    return { base, custom };
  }
  return { base: (PALETTES[id as Flavor] ? id : 'mocha') as Flavor };
}

/** 将全部自定义主题的调色板覆盖注入一张样式表。 */
function injectCustomThemeStyles(): void {
  let el = document.getElementById('oma-custom-themes') as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement('style');
    el.id = 'oma-custom-themes';
    document.head.appendChild(el);
  }
  el.textContent = customThemes.value
    .map((t) => {
      const vars = Object.entries(t.colors)
        .map(([k, v]) => `--${k}: ${v};`)
        .join('');
      return `:root.theme-custom-${t.id}{${vars}}`;
    })
    .join('\n');
}

/** 将后端主题应用到 <html>：基底 flavor class + 自定义主题 class + accent class。 */
export function applyTheme(): void {
  const root = document.documentElement;
  const { base, custom } = resolveActiveTheme();
  root.classList.remove('theme-latte', 'theme-frappe', 'theme-macchiato', 'theme-mocha');
  customThemes.value.forEach((t) => root.classList.remove(`theme-custom-${t.id}`));
  root.classList.add(`theme-${base}`);
  if (custom) root.classList.add(`theme-custom-${custom.id}`);

  root.classList.remove(...ACCENTS.map((a) => `accent-${a}`));
  root.classList.add(`accent-${ACCENTS.includes(theme.value.accent as Accent) ? theme.value.accent : 'blue'}`);
  injectCustomThemeStyles();
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
  if (!config.value) throw new Error(tr('settings.configNotLoaded'));
  const payload: OmaConfig = { ...config.value, theme: next };
  await api.putConfig(payload);
  config.value = payload;
}

/** 提交任意配置修改（providers 等），成功后同步缓存。 */
export async function saveConfig(next: OmaConfig): Promise<void> {
  await api.putConfig(next);
  // 以服务端回读为准：本地回写会让脱敏密钥等字段停留在过期值上
  config.value = await api.getConfig();
  theme.value = { ...DEFAULT_THEME, ...config.value.theme };
  applyTheme();
}
