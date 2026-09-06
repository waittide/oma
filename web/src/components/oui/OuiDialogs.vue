<script setup lang="ts">
import { Fa6Check, Fa6TriangleExclamation, Fa6Xmark } from 'vue-icons-plus/fa6';
import { nextTick, onBeforeUnmount, ref, watch } from 'vue';
import { closeDialog, dialogRequest, dialogVisible, toasts } from '../../useDialogs';

const inputValue = ref('');
const input = ref<HTMLInputElement | null>(null);

watch(dialogVisible, (v) => {
  if (!v) {
    document.removeEventListener('keydown', onDocKeydown);
    return;
  }
  document.addEventListener('keydown', onDocKeydown);
  if (dialogRequest.value?.kind === 'prompt') {
    inputValue.value = dialogRequest.value.defaultValue ?? '';
    nextTick(() => input.value?.select());
  }
});

function onDocKeydown(e: KeyboardEvent) {
  if (e.key === 'Enter' && dialogRequest.value?.kind !== 'alert') {
    e.preventDefault();
    onConfirm();
  } else if (e.key === 'Escape') {
    onCancel();
  }
}

function onConfirm() {
  const req = dialogRequest.value;
  if (!req) return;
  if (req.kind === 'prompt') closeDialog(inputValue.value.trim() || null);
  else closeDialog(true);
}

function onCancel() {
  closeDialog(dialogRequest.value?.kind === 'prompt' ? null : false);
}

function transitionDone() {
  if (!dialogVisible.value) dialogRequest.value = null;
}

onBeforeUnmount(() => document.removeEventListener('keydown', onDocKeydown));
</script>

<template>
  <Teleport to="body">
    <Transition name="dlg" @after-leave="transitionDone">
      <div
        v-if="dialogVisible && dialogRequest"
        class="modal-overlay"
        @click.self="onCancel"
      >
        <div class="modal-dialog oui-dialog" :class="{ danger: dialogRequest.danger }" role="dialog" aria-modal="true">
          <div class="modal-header">
            <span style="display: inline-flex; align-items: center; gap: 8px;">
              <Fa6TriangleExclamation v-if="dialogRequest.danger" style="color: var(--danger);" />
              {{ dialogRequest.title }}
            </span>
            <button class="btn-icon" @click="onCancel"><Fa6Xmark /></button>
          </div>

          <div class="modal-body">
            <p class="oui-dialog-message">{{ dialogRequest.message }}</p>
            <input
              v-if="dialogRequest.kind === 'prompt'"
              ref="input"
              v-model="inputValue"
              class="field-input"
              type="text"
            />
          </div>

          <div class="modal-footer">
            <button v-if="dialogRequest.kind !== 'alert'" class="btn-default" @click="onCancel">取消</button>
            <button
              :class="dialogRequest.danger ? 'btn-danger' : 'btn-primary'"
              @click="onConfirm"
            >
              {{ dialogRequest.confirmText || (dialogRequest.kind === 'alert' ? '知道了' : '确定') }}
            </button>
          </div>
        </div>
      </div>
    </Transition>

    <!-- Toast 通知栈 -->
    <div class="toast-stack">
      <TransitionGroup name="toast">
        <div v-for="t in toasts" :key="t.id" class="toast" :class="t.type">
          <Fa6Check v-if="t.type === 'success'" />
          <Fa6TriangleExclamation v-else />
          {{ t.text }}
        </div>
      </TransitionGroup>
    </div>
  </Teleport>
</template>

<style scoped>
.oui-dialog {
  max-width: 420px;
}

.oui-dialog.danger {
  border-color: var(--danger);
}

.oui-dialog-message {
  font-size: 13px;
  color: var(--text-secondary);
  line-height: 1.6;
  white-space: pre-wrap;
}

.dlg-enter-active,
.dlg-leave-active {
  transition: opacity 0.16s ease;
}

.dlg-enter-from,
.dlg-leave-to {
  opacity: 0;
}

.dlg-enter-active .modal-dialog,
.dlg-leave-active .modal-dialog {
  transition: transform 0.16s ease;
}

.dlg-enter-from .modal-dialog,
.dlg-leave-to .modal-dialog {
  transform: scale(0.96) translateY(6px);
}

.toast-stack {
  position: fixed;
  top: 16px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 3100;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  pointer-events: none;
}

.toast {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  background: var(--bg-card);
  border: 1px solid var(--border-default);
  color: var(--text-primary);
  font-size: 13px;
  padding: 9px 16px;
  border-radius: 12px;
  box-shadow: var(--shadow-lg);
}

.toast.success {
  border-color: var(--success);
  color: var(--success);
}

.toast.error {
  border-color: var(--danger);
  color: var(--danger);
}

.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.2s ease, transform 0.2s ease;
}

.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateY(-10px);
}
</style>
