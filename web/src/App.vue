<script setup lang="ts">
import { onMounted } from 'vue';
import { Toaster } from 'vue-sonner';
import { loadConfig } from './stores/theme';

onMounted(() => loadConfig());
</script>

<template>
  <div class="shell">
    <header class="bar">Oma</header>
  </div>
  <Toaster position="bottom-right" :expand="false" />
</template>

<style scoped>
.shell {
  height: 100vh;
  background: var(--paper);
  color: var(--ink);
}
.bar {
  padding: 14px 18px;
  background: var(--surface);
  border-bottom: 1px solid var(--line);
  font-weight: 700;
}
</style>
