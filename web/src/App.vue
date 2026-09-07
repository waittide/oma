<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { Toaster } from 'vue-sonner';
import { api } from './api';
import Sidebar from './components/Sidebar.vue';
import ChatView from './components/ChatView.vue';
import SettingsView from './components/SettingsView.vue';
import { loadConfig } from './stores/theme';

const view = ref<'chat' | 'settings'>('chat');
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
    <Sidebar
      @open-settings="view = 'settings'"
      @back="view = 'chat'"
    />
    <main class="main">
      <SettingsView v-if="view === 'settings'" @back="view = 'chat'" />
      <ChatView v-else :online="online" @need-settings="view = 'settings'" />
    </main>
  </div>
  <Toaster position="bottom-right" :expand="false" />
</template>

<style scoped>
.shell {
  display: flex;
  height: 100vh;
  overflow: hidden;
  background: var(--paper);
  color: var(--ink);
}
.main {
  flex: 1;
  min-width: 0;
  display: flex;
}
</style>
