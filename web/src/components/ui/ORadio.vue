<script setup lang="ts" generic="T extends string">
withDefaults(
  defineProps<{
    modelValue: T;
    options: { value: T; label: string }[];
    disabled?: boolean;
  }>(),
  { disabled: false },
);

const emit = defineEmits<{ 'update:modelValue': [T] }>();
</script>

<template>
  <div class="o-segment" role="radiogroup">
    <button
      v-for="opt in options"
      :key="opt.value"
      type="button"
      role="radio"
      class="seg"
      :class="{ active: opt.value === modelValue }"
      :aria-checked="opt.value === modelValue"
      :disabled="disabled"
      @click="emit('update:modelValue', opt.value)"
    >
      {{ opt.label }}
    </button>
  </div>
</template>

<style scoped>
.o-segment {
  display: inline-flex;
  padding: 3px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 9px;
  gap: 2px;
}
.seg {
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  font-family: inherit;
  font-size: 12px;
  font-weight: 500;
  padding: 5px 12px;
  border-radius: 6px;
  cursor: pointer;
  transition:
    background-color 0.15s ease,
    color 0.15s ease;
}
.seg:hover:not(:disabled):not(.active) {
  color: var(--ink);
}
.seg.active {
  background: var(--accent);
  color: var(--base);
}
.seg:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
