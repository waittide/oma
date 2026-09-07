import { createApp } from 'vue';
import App from './App.vue';
import { bindDocumentLocale } from './composables/i18n';
import './theme.css';
import 'vue-sonner/style.css';
import './style.css';

bindDocumentLocale();

createApp(App).mount('#app');
