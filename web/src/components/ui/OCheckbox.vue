<script setup lang="ts">
import { LuCheck } from 'vue-icons-plus/lu';

withDefaults(
  defineProps<{
    modelValue: boolean;
    label?: string;
    disabled?: boolean;
  }>(),
  { label: undefined, disabled: false },
);

const emit = defineEmits<{ 'update:modelValue': [boolean] }>();

</script>

<template>
  <button
    type="button"
    class="o-checkbox"
    :class="{ checked: modelValue, disabled }"
    :disabled="disabled"
    @click="emit('update:modelValue', !modelValue)"
  >
    <span class="box">
      <LuCheck v-if="modelValue" :size="12" />
    </span>
    <span v-if="label" class="label">{{ label }}</span>
  </button>
</template>

<style scoped>
.o-checkbox {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  border: none;
  background: transparent;
  padding: 0;
  font-family: inherit;
  font-size: 13px;
  color: var(--text-secondary);
  cursor: pointer;
  user-select: none;
}
.o-checkbox.disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.box {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 16px;
  height: 16px;
  border: 1.5px solid var(--control-border);
  border-radius: 5px;
  color: var(--base);
  transition:
    background-color 0.15s ease,
    border-color 0.15s ease;
}
.checked .box {
  background: var(--accent);
  border-color: var(--accent);
}
.o-checkbox:hover:not(.disabled) .box {
  border-color: var(--accent);
}
.label {
  color: inherit;
}
.checked .label {
  color: var(--ink);
}
</style>
