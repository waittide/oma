<script setup lang="ts">
const props = defineProps<{
  modelValue: string;
  options: Array<{ label: string; value: string }>;
  disabled?: boolean;
}>();

const emit = defineEmits<{
  (e: 'update:modelValue', v: string): void;
}>();

function pick(value: string) {
  if (!props.disabled && value !== props.modelValue) emit('update:modelValue', value);
}
</script>

<template>
  <div class="oui-radio-group" role="radiogroup" :aria-disabled="disabled">
    <button
      v-for="o in options"
      :key="o.value"
      type="button"
      class="oui-radio"
      role="radio"
      :aria-checked="modelValue === o.value"
      :class="{ checked: modelValue === o.value, disabled }"
      :disabled="disabled"
      @click="pick(o.value)"
    >
      <span class="oui-radio-dot" />
      <span class="oui-radio-label">{{ o.label }}</span>
    </button>
  </div>
</template>

<style scoped>
.oui-radio-group {
  display: flex;
  gap: 18px;
  font-size: 13px;
}

.oui-radio {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 0;
  border: none;
  background: transparent;
  color: var(--text-primary);
  font-family: inherit;
  font-size: 13px;
  cursor: pointer;
}

.oui-radio-dot {
  flex-shrink: 0;
  width: 16px;
  height: 16px;
  border: 1px solid var(--border-strong);
  border-radius: 50%;
  background: var(--bg-input);
  position: relative;
  transition: border-color var(--dur-fast) ease;
}

.oui-radio-dot::before {
  content: '';
  position: absolute;
  inset: 3px;
  border-radius: 50%;
  background: var(--accent);
  transform: scale(0);
  transition: transform var(--dur-fast) var(--ease-in-out);
}

.oui-radio:hover .oui-radio-dot {
  border-color: var(--accent);
}

.oui-radio.checked .oui-radio-dot {
  border-color: var(--accent);
}

.oui-radio.checked .oui-radio-dot::before {
  transform: scale(1);
}

.oui-radio.disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
</style>
