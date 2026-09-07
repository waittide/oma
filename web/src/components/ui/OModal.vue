<script setup lang="ts">
import { LuX } from 'vue-icons-plus/lu';
import OButton from './OButton.vue';
import { useTranslations } from '../../composables/i18n';

withDefaults(
  defineProps<{
    open: boolean;
    title?: string;
    width?: string;
    /** true 时 body 不带内边距与滚动，由内容自行布局（如左右分栏） */
    flush?: boolean;
    /** true 时不渲染标题栏，关闭按钮悬浮于面板右上角 */
    floatingClose?: boolean;
  }>(),
  { title: '', width: '440px', flush: false, floatingClose: false },
);

const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('common');
</script>

<template>
  <Teleport to="body">
    <Transition name="modal">
      <div v-if="open" class="scrim" @mousedown.self="emit('close')">
        <div class="panel" :style="{ width }" role="dialog" aria-modal="true">
          <header v-if="!floatingClose" class="head">
            <h3>{{ title }}</h3>
            <OButton variant="ghost" size="sm" :title="t('close')" @click="emit('close')">
              <template #icon><LuX :size="15" /></template>
            </OButton>
          </header>
          <button
            v-else
            type="button"
            class="close-float"
            :title="t('close')"
            @click="emit('close')"
          >
            <LuX :size="16" />
          </button>
          <div class="body" :class="{ flush }"><slot /></div>
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
.panel {
  position: relative;
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
.close-float {
  position: absolute;
  top: 8px;
  right: 8px;
  z-index: 5;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: var(--text-tertiary);
  cursor: pointer;
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
}
.close-float:hover {
  background: var(--surface-hover);
  color: var(--ink);
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
