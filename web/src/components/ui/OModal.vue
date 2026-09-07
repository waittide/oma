<script setup lang="ts">
import { LuX } from 'vue-icons-plus/lu';
import OButton from './OButton.vue';

withDefaults(
  defineProps<{
    open: boolean;
    title?: string;
    width?: string;
  }>(),
  { title: '', width: '440px' },
);

const emit = defineEmits<{ close: [] }>();
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="open" class="scrim" @mousedown.self="emit('close')">
        <div class="panel" :style="{ width }" role="dialog" aria-modal="true">
          <header class="head">
            <h3>{{ title }}</h3>
            <OButton variant="ghost" size="sm" title="关闭" @click="emit('close')">
              <template #icon><LuX :size="15" /></template>
            </OButton>
          </header>
          <div class="body"><slot /></div>
          <footer v-if="$slots.footer" class="foot"><slot name="footer" /></footer>
        </div>
      </div>
    </Transition>
  </Teleport>
</template>

<style scoped>
.scrim {
  position: fixed;
  inset: 0;
  z-index: 90;
  display: grid;
  place-items: center;
  background: var(--overlay-scrim);
}
.panel {
  max-width: calc(100vw - 48px);
  max-height: calc(100vh - 96px);
  display: flex;
  flex-direction: column;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 14px;
  box-shadow: 0 16px 48px var(--shadow);
  overflow: hidden;
}
.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 18px 10px;
}
.head h3 {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: var(--ink);
}
.body {
  padding: 4px 18px 16px;
  overflow-y: auto;
}
.foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 18px;
  border-top: 1px solid var(--line);
}
.modal-enter-active,
.modal-leave-active {
  transition: opacity 0.15s ease;
}
.modal-enter-active .panel,
.modal-leave-active .panel {
  transition: transform 0.15s ease;
}
.modal-enter-from,
.modal-leave-to {
  opacity: 0;
}
.modal-enter-from .panel,
.modal-leave-to .panel {
  transform: translateY(8px) scale(0.98);
}
</style>
