import { reactive } from 'vue';

export type Locale = 'zh_hans' | 'zh_hant' | 'en' | 'ja';

const STORAGE_KEY = 'oma.locale';

export const LOCALES: { value: Locale; label: string; htmlTag: string }[] = [
  { value: 'zh_hans', label: '简体中文', htmlTag: 'zh-Hans' },
  { value: 'zh_hant', label: '繁體中文', htmlTag: 'zh-Hant' },
  { value: 'en', label: 'English', htmlTag: 'en' },
  { value: 'ja', label: '日本語', htmlTag: 'ja' },
];

function isLocale(v: string | null): v is Locale {
  return v === 'zh_hans' || v === 'zh_hant' || v === 'en' || v === 'ja';
}

function detectLocale(): Locale {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (isLocale(saved)) return saved;
  } catch {
    // ignore storage read error
  }
  const nav = navigator.language.toLowerCase();
  if (nav.startsWith('zh')) {
    return /tw|hk|mo|hant/.test(nav) ? 'zh_hant' : 'zh_hans';
  }
  if (nav.startsWith('ja')) return 'ja';
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
