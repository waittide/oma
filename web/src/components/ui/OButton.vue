<script setup lang="ts">
import { computed } from 'vue';
import { LuLoader } from 'vue-icons-plus/lu';

const props = withDefaults(
  defineProps<{
    variant?: 'primary' | 'ghost' | 'danger' | 'soft';
    size?: 'sm' | 'md';
    loading?: boolean;
    disabled?: boolean;
    ariaLabel?: string;
  }>(),
  { variant: 'soft', size: 'md', loading: false, disabled: false, ariaLabel: undefined },
);

const emit = defineEmits<{ click: [MouseEvent] }>();

const classes = computed(() => ['btn', `btn-${props.variant}`, `btn-${props.size}`]);

function onClick(e: MouseEvent) {
  if (props.disabled || props.loading) return;
  emit('click', e);
}
</script>

<template>
  <button
    :class="classes"
    :disabled="disabled || loading"
    :aria-label="ariaLabel"
    @click="onClick"
  >
    <LuLoader v-if="loading" class="spin" :size="size === 'sm' ? 13 : 15" />
    <slot v-else name="icon" />
    <span class="btn-label"><slot /></span>
  </button>
</template>

<style scoped>
.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  border: 1px solid transparent;
  border-radius: 8px;
  font-family: inherit;
  font-weight: 500;
  cursor: pointer;
  transition:
    background-color 0.15s ease,
    border-color 0.15s ease,
    color 0.15s ease,
    opacity 0.15s ease;
  white-space: nowrap;
  user-select: none;
}
.btn-md {
  height: 32px;
  padding: 0 12px;
  font-size: 13px;
}
.btn-sm {
  height: 26px;
  padding: 0 9px;
  font-size: 12px;
  border-radius: 6px;
}
.btn-primary {
  background: var(--accent);
  color: var(--base);
}
.btn-primary:hover:not(:disabled) {
  filter: brightness(1.08);
}
.btn-soft {
  background: var(--surface-strong);
  color: var(--text-secondary);
  border-color: var(--line);
}
.btn-soft:hover:not(:disabled) {
  color: var(--ink);
  background: var(--surface-hover);
}
.btn-ghost {
  background: transparent;
  color: var(--text-tertiary);
}
.btn-ghost:hover:not(:disabled) {
  color: var(--ink);
  background: var(--surface-hover);
}
.btn-danger {
  background: var(--danger-soft);
  color: var(--danger);
  border-color: color-mix(in srgb, var(--danger) 30%, transparent);
}
.btn-danger:hover:not(:disabled) {
  background: color-mix(in srgb, var(--danger) 26%, var(--surface));
}
.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.btn-label:empty {
  display: none;
}
.spin {
  animation: rotate 0.9s linear infinite;
}
@keyframes rotate {
  to {
    transform: rotate(360deg);
  }
}
</style>
