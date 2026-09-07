<script setup lang="ts">
import { onMounted } from 'vue';
import { Toaster } from 'vue-sonner';
import Sidebar from './components/Sidebar.vue';
import { loadConfig } from './stores/theme';

onMounted(() => loadConfig());
</script>

<template>
  <div class="shell">
    <Sidebar @open-settings="() => {}" @back="() => {}" />
    <main class="main">
      <div class="placeholder">选择或新建会话</div>
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
  display: grid;
  place-items: center;
}
.placeholder {
  color: var(--overlay0);
  font-size: 13px;
}
</style>
