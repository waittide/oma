<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { UiToaster } from '@waittide/ui';
import { api } from './api';
import Sidebar from './components/Sidebar.vue';
import ChatView from './components/ChatView.vue';
import SettingsModal from './components/SettingsModal.vue';
import TopToolbar from './components/TopToolbar.vue';
import RightPanel from './components/RightPanel.vue';
import PanelResizer from './components/PanelResizer.vue';
import { loadConfig } from './stores/theme';
import { initClientConfig } from './stores/clientConfig';
import { reset as resetChat } from './stores/chat';
import * as layout from './stores/layout';

const settingsOpen = ref(false);
const settingsSection = ref<string | undefined>(undefined);
const online = ref(false);

function openSettings(section?: string) {
  settingsSection.value = section;
  settingsOpen.value = true;
}

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
  window.addEventListener('resize', onViewportResize);
  clampPanelsToViewport();
});

onUnmounted(() => window.removeEventListener('resize', onViewportResize));

/** 视口变窄时把两侧面板夹回可用范围，避免聊天区被挤没。 */
function clampPanelsToViewport() {
  const width = window.innerWidth;
  const chatMin = width < layout.SPLIT_PANEL_MIN_WIDTH ? 320 : 420;
  const rightVisible = layout.rightOpen.value && width >= layout.SPLIT_PANEL_MIN_WIDTH ? layout.rightWidth.value : 0;
  const maxSidebar = Math.min(layout.SIDEBAR_MAX_WIDTH, Math.max(layout.SIDEBAR_MIN_WIDTH, width - chatMin - rightVisible));
  if (layout.sidebarWidth.value > maxSidebar) layout.setSidebarWidth(maxSidebar);
}

function onViewportResize() {
  clampPanelsToViewport();
}

/** 设置里改完连接后重新握手：否则界面仍停在旧 Daemon 的数据上。 */
async function onReconnect() {
  resetChat();
  settingsOpen.value = false;
  await probe();
}
</script>

<template>
  <div class="shell">
    <Sidebar v-show="layout.sidebarOpen.value" @open-settings="openSettings" />
    <PanelResizer
      v-if="layout.sidebarOpen.value"
      @resize="(d) => layout.setSidebarWidth(layout.sidebarWidth.value + d)"
    />

    <main class="main">
      <TopToolbar />
      <ChatView :online="online" @need-settings="openSettings()" />
    </main>

    <template v-if="layout.rightOpen.value">
      <PanelResizer invert @resize="(d) => layout.setRightWidth(layout.rightWidth.value + d)" />
      <RightPanel class="right" :style="{ width: layout.rightWidth.value + 'px' }" />
    </template>

    <SettingsModal
      :open="settingsOpen"
      :online="online"
      :initial-section="settingsSection"
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
  background: var(--paper);
  color: var(--ink);
}
.main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.right {
  flex-shrink: 0;
}
</style>
