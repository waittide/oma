import { createApp } from 'vue';
import App from './App.vue';
import { vTip } from './useTip';
import { initThemeSystem } from './theme';
import './theme.css';
import './style.css';

initThemeSystem();
createApp(App).directive('tip', vTip).mount('#app');
