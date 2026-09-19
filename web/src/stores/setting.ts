import { reactive } from 'vue';
import {
  UI_LOCALES,
  localeHtmlTag,
  setUiLocale,
  uiLocale,
  type UiLocale,
} from '@waittide/ui';

/**
 * 应用语言。
 *
 * 语言是组件库与应用共用的同一份状态：`uiLocale` 是唯一事实来源，
 * 组件库据此翻译自身文案，应用据此翻译页面文案，二者永远一致。
 * 持久化与浏览器语言探测都由组件库运行时负责。
 */
export type Locale = UiLocale;

export const LOCALES = UI_LOCALES;

export const settingStore = reactive({
  get locale(): Locale {
    return uiLocale.value;
  },
});

export function setLocale(locale: Locale) {
  setUiLocale(locale);
}

export function localeTag(locale: Locale = settingStore.locale) {
  return localeHtmlTag(locale);
}
