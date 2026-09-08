<script setup lang="ts">
import { LuX } from 'vue-icons-plus/lu';
import OButton from './OButton.vue';
import OTooltip from './OTooltip.vue';
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
            <OTooltip :label="t('close')" align="end">
              <OButton variant="ghost" size="sm" :ariaLabel="t('close')" @click="emit('close')">
                <template #icon><LuX :size="15" /></template>
              </OButton>
            </OTooltip>
          </header>
          <OTooltip v-else :label="t('close')" align="end">
            <button type="button" class="close-float" :aria-label="t('close')" @click="emit('close')">
              <LuX :size="16" />
            </button>
          </OTooltip>
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
  position: relative;
  max-width: calc(100vw - 48px);
  max-height: calc(100vh - 96px);
  display: flex;
  flex-direction: column;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 14px;
  box-shadow: var(--shadow-overlay);
  overflow: hidden;
}
.head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 14px 18px;
  border-bottom: 1px solid var(--line);
  flex-shrink: 0;
}
.head h3 {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: var(--ink);
}
.body {
  flex: 1;
  min-height: 0;
  padding: 16px 18px;
  overflow-y: auto;
}
/* flush：内容自行布局（如左右分栏），面板内边距交给内容 */
.body.flush {
  padding: 0;
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
  flex-shrink: 0;
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
