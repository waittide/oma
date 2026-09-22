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
import { refreshSystemPrompt, reset as resetChat } from './stores/chat';
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
  layout.viewportWidth.value = window.innerWidth;
  window.addEventListener('resize', onViewportResize);
});

onUnmounted(() => window.removeEventListener('resize', onViewportResize));

/**
 * 视口变窄时只更新用于渲染的宽度（`layout` 里的 computed 会夹），
 * 不改用户拖出来的面板宽度——拉回去时布局应恢复原样。
 */
function onViewportResize() {
  layout.viewportWidth.value = window.innerWidth;
}

/** 设置里改完连接后重新握手：否则界面仍停在旧 Daemon 的数据上。 */
async function onReconnect() {
  resetChat();
  settingsOpen.value = false;
  await probe();
}

/**
 * 关掉设置后重取系统提示词：预设、默认模型等都可能在里面被改过，
 * 呈现的提示词必须跟着变，否则界面上是上一份内容。
 */
function onSettingsClose() {
  settingsOpen.value = false;
  void refreshSystemPrompt(true);
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

    <template v-if="layout.rightRenderWidth.value > 0">
      <PanelResizer invert @resize="(d) => layout.setRightWidth(layout.rightWidth.value + d)" />
      <RightPanel class="right" :style="{ width: layout.rightRenderWidth.value + 'px' }" />
    </template>

    <SettingsModal
      :open="settingsOpen"
      :online="online"
      :initial-section="settingsSection"
      @close="onSettingsClose"
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
  /* 竖向也必须允许收缩：否则聊天列的内容（输入框控件换行变高）会把整列顶出外壳 */
  min-height: 0;
  display: flex;
  flex-direction: column;
}
.right {
  flex-shrink: 0;
}
</style>
