<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { UiToaster } from '@waittide/ui';
import { api } from './api';
import Sidebar from './components/Sidebar.vue';
import ChatView from './components/ChatView.vue';
import SettingsModal from './components/SettingsModal.vue';
import { loadConfig } from './stores/theme';
import { initClientConfig } from './stores/clientConfig';
import { reset as resetChat } from './stores/chat';

const settingsOpen = ref(false);
const online = ref(false);

async function probe() {
  try {
    await api.status();
    online.value = true;
  } catch {
    online.value = false;
  }
}

onMounted(async () => {
  // 先应用 client.json 里保存的活动连接，再用它去探测 Daemon
  await initClientConfig();
  await loadConfig();
  await probe();
});

/** 设置里改完连接后重新握手：否则界面仍停在旧 Daemon 的数据上。 */
async function onReconnect() {
  resetChat();
  settingsOpen.value = false;
  await probe();
}
</script>

<template>
  <div class="shell">
    <Sidebar @open-settings="settingsOpen = true" />
    <main class="main">
      <ChatView :online="online" @need-settings="settingsOpen = true" />
    </main>
    <SettingsModal
      :open="settingsOpen"
      :online="online"
      @close="settingsOpen = false"
      @reconnect="onReconnect"
    />
  </div>
  <UiToaster position="bottom-right" :max="4" :close-button="true" />
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
