import { computed, watch } from 'vue';
import { localeTag, settingStore } from '../stores/setting';

import zhHansMessages from '../locales/zh_hans.json';
import zhHantMessages from '../locales/zh_hant.json';
import enMessages from '../locales/en.json';
import jaMessages from '../locales/ja.json';

const messages: Record<string, unknown> = {
  zh_hans: zhHansMessages,
  zh_hant: zhHantMessages,
  en: enMessages,
  ja: jaMessages,
};

// 在命名空间字典内按点路径逐级取值（如 'pagination.total'）。
function lookup(dict: unknown, path: string): string | undefined {
  let node: unknown = dict;
  for (const part of path.split('.')) {
    if (node && typeof node === 'object' && part in (node as Record<string, unknown>)) {
      node = (node as Record<string, unknown>)[part];
    } else {
      return undefined;
    }
  }
  return typeof node === 'string' ? node : undefined;
}

function translate(
  path: string,
  values?: Record<string, string | number>,
): string {
  const dict = messages[settingStore.locale];
  const raw = lookup(dict, path) ?? path;
  if (!values) return raw;
  return raw.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in values ? String(values[name]) : match,
  );
}

/**
 * useTranslations 对应 llmgate 的 useTranslations：
 * 返回支持 {name} 插值与点路径键的 t 函数；locale 切换后模板自动更新。
 */
export function useTranslations(namespace: string) {
  const t = (key: string, values?: Record<string, string | number>): string =>
    translate(`${namespace}.${key}`, values);
  const locale = computed(() => settingStore.locale);
  return { t, locale };
}

/** 组件外（store 等）的全路径翻译。 */
export function tr(
  path: string,
  values?: Record<string, string | number>,
): string {
  return translate(path, values);
}

// 应用根组件调用：同步 <html lang> 与文档标题。
export function bindDocumentLocale() {
  const sync = () => {
    document.documentElement.lang = localeTag();
    document.title = tr('app.title');
  };
  sync();
  watch(() => settingStore.locale, sync);
}
