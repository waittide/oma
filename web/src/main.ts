import { createApp } from 'vue';
import { bindUiDocumentLocale, initUiTheme, setUiLocale } from '@waittide/ui';
import App from './App.vue';
import { bindDocumentLocale } from './composables/i18n';
import { settingStore } from './stores/setting';

// 样式顺序：组件库令牌在前，应用样式在后，应用可覆盖。
import '@waittide/ui/style.css';
import './theme.css';
import './style.css';

// 主题由服务端配置驱动（见 stores/theme.ts），这里禁用组件库的 localStorage 持久化；
// 初始值先对齐 oma 的深色默认，避免首帧闪一下浅色。
initUiTheme({
  storageKey: null,
  theme: { mode: 'dark', dark_palette: 'pi-dark', light_palette: 'pi-light', accent: 'blue' },
});
// 组件库语言跟随应用语言，<html lang> 由组件库运行时统一维护。
setUiLocale(settingStore.locale);
bindUiDocumentLocale();
bindDocumentLocale();

createApp(App).mount('#app');
