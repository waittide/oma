import { reactive } from 'vue';

export type Locale = 'zh_hans' | 'zh_hant' | 'en';

const STORAGE_KEY = 'oma.locale';

export const LOCALES: { value: Locale; label: string; htmlTag: string }[] = [
  { value: 'zh_hans', label: '简体中文', htmlTag: 'zh-Hans' },
  { value: 'zh_hant', label: '繁體中文', htmlTag: 'zh-Hant' },
  { value: 'en', label: 'English', htmlTag: 'en' },
];

function detectLocale(): Locale {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === 'zh_hans' || saved === 'zh_hant' || saved === 'en') return saved;
  } catch {
    // ignore storage read error
  }
  const nav = navigator.language.toLowerCase();
  if (nav.startsWith('zh')) {
    return /tw|hk|mo|hant/.test(nav) ? 'zh_hant' : 'zh_hans';
  }
  return 'en';
}

export const settingStore = reactive<{ locale: Locale }>({ locale: detectLocale() });

export function setLocale(locale: Locale) {
  settingStore.locale = locale;
  try {
    localStorage.setItem(STORAGE_KEY, locale);
  } catch {
    // ignore storage write error
  }
}

export function localeTag(locale: Locale = settingStore.locale): string {
  return LOCALES.find((l) => l.value === locale)?.htmlTag ?? 'zh-Hans';
}
