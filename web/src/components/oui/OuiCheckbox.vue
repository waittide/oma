<script setup lang="ts">
import { Fa6Check } from 'vue-icons-plus/fa6';

const props = defineProps<{
  modelValue: boolean;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  (e: 'update:modelValue', v: boolean): void;
}>();

function toggle() {
  if (!props.disabled) emit('update:modelValue', !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    class="oui-checkbox"
    role="checkbox"
    :aria-checked="modelValue"
    :class="{ checked: modelValue, disabled }"
    :disabled="disabled"
    @click="toggle"
  >
    <Fa6Check v-if="modelValue" />
  </button>
</template>

<style scoped>
.oui-checkbox {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  padding: 0;
  border: 1px solid var(--border-strong);
  border-radius: 5px;
  background: var(--bg-input);
  color: var(--accent-contrast);
  font-size: 9px;
  cursor: pointer;
  vertical-align: middle;
  transition:
    background-color var(--dur-fast) ease,
    border-color var(--dur-fast) ease;
}

.oui-checkbox:hover {
  border-color: var(--accent);
}

.oui-checkbox.checked {
  background: var(--accent);
  border-color: var(--accent);
}

.oui-checkbox.disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
</style>
