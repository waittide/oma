<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { Toaster } from 'vue-sonner';
import { isDark } from './stores/theme';
import { api } from './api';
import Sidebar from './components/Sidebar.vue';
import ChatView from './components/ChatView.vue';
import SettingsModal from './components/SettingsModal.vue';
import { loadConfig } from './stores/theme';

const settingsOpen = ref(false);
const online = ref(false);

onMounted(async () => {
  await loadConfig();
  try {
    await api.status();
    online.value = true;
  } catch {
    online.value = false;
  }
});
</script>

<template>
  <div class="shell">
    <Sidebar @open-settings="settingsOpen = true" />
    <main class="main">
      <ChatView :online="online" @need-settings="settingsOpen = true" />
    </main>
    <SettingsModal :open="settingsOpen" @close="settingsOpen = false" />
  </div>
  <Toaster
    position="bottom-right"
    :expand="false"
    :theme="isDark ? 'dark' : 'light'"
    :toast-options="{ style: { fontFamily: 'var(--font-sans)' } }"
    rich-colors
    close-button
  />
</template>

<style scoped>
.shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
  background: var(--surface);
  color: var(--ink);
}
.main {
  flex: 1;
  min-width: 0;
  display: flex;
}
</style>
