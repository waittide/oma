<script setup lang="ts">
import { Fa6Check, Fa6ChevronDown } from 'vue-icons-plus/fa6';
import { computed, onBeforeUnmount, ref, watch } from 'vue';
export interface OuiSelectOption {
  value: string;
  label: string;
  description?: string;
}

const props = defineProps<{
  modelValue: string | undefined | null;
  options: OuiSelectOption[];
  placeholder?: string;
  variant?: 'pill' | 'field';
}>();

const emit = defineEmits<{
  (e: 'update:modelValue', v: string): void;
}>();

const open = ref(false);
const root = ref<HTMLElement | null>(null);

const current = computed(() => props.options.find((o) => o.value === props.modelValue));
const displayLabel = computed(() => current.value?.label ?? props.modelValue ?? props.placeholder ?? '');

function toggle() {
  open.value = !open.value;
}

function pick(o: OuiSelectOption) {
  emit('update:modelValue', o.value);
  open.value = false;
}

function onDocPointer(e: PointerEvent) {
  if (root.value && !root.value.contains(e.target as Node)) open.value = false;
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') open.value = false;
}

watch(open, (v) => {
  if (v) {
    document.addEventListener('pointerdown', onDocPointer);
    document.addEventListener('keydown', onKeydown);
  } else {
    document.removeEventListener('pointerdown', onDocPointer);
    document.removeEventListener('keydown', onKeydown);
  }
});

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onDocPointer);
  document.removeEventListener('keydown', onKeydown);
});
</script>

<template>
  <div
    ref="root"
    class="oui-select"
    :class="[variant === 'field' ? 'oui-select--field' : 'oui-select--pill', { open }]"
  >
    <button type="button" class="oui-select-trigger" :aria-expanded="open" @click="toggle">
      <slot name="icon" />
      <span class="oui-select-value">{{ displayLabel }}</span>
      <Fa6ChevronDown class="oui-select-arrow" />
    </button>

    <Transition name="oui-pop">
      <div v-if="open" class="oui-select-menu" role="listbox">
        <div
          v-for="o in options"
          :key="o.value"
          class="oui-select-option"
          :class="{ selected: o.value === modelValue }"
          role="option"
          :aria-selected="o.value === modelValue"
          @click="pick(o)"
        >
          <span class="oui-option-label">{{ o.label }}</span>
          <span v-if="o.description" class="oui-option-desc">{{ o.description }}</span>
          <Fa6Check v-if="o.value === modelValue" class="oui-option-check" />
        </div>
        <div v-if="!options.length" class="oui-select-empty">无可用选项</div>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.oui-select {
  position: relative;
  display: inline-block;
  min-width: 0;
}

.oui-select--pill .oui-select-trigger {
  background: var(--bg-card);
  border: 1px solid var(--border-subtle);
  color: var(--text-primary);
  border-radius: 999px;
  padding: 5px 10px 5px 12px;
  font-size: 12px;
  font-family: var(--font-mono);
  max-width: 260px;
}

.oui-select--field {
  width: 100%;
}

.oui-select--field .oui-select-trigger {
  width: 100%;
  background: var(--bg-input);
  border: 1px solid var(--border-default);
  color: var(--text-primary);
  border-radius: 9px;
  padding: 8px 11px;
  font-size: 13px;
  font-family: var(--font-mono);
}

.oui-select-trigger {
  display: inline-flex;
  align-items: center;
  gap: 7px;
  cursor: pointer;
  transition: border-color 0.15s ease;
  width: 100%;
}

.oui-select-trigger:hover {
  border-color: var(--border-default);
}

.oui-select--field .oui-select-trigger:hover,
.oui-select--field.open .oui-select-trigger {
  border-color: var(--accent);
}

.oui-select--pill.open .oui-select-trigger {
  border-color: var(--accent);
}

.oui-select-value {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  flex: 1;
  text-align: left;
}

.oui-select-arrow {
  color: var(--text-muted);
  font-size: 9px;
  transition: transform 0.18s ease;
  flex-shrink: 0;
}

.oui-select.open .oui-select-arrow {
  transform: rotate(180deg);
}

.oui-select-menu {
  position: absolute;
  top: calc(100% + 6px);
  left: 0;
  min-width: 100%;
  max-height: 260px;
  overflow-y: auto;
  background: var(--bg-card);
  border: 1px solid var(--border-default);
  border-radius: 12px;
  box-shadow: var(--shadow-lg);
  padding: 4px;
  z-index: 1200;
}

.oui-select--pill .oui-select-menu {
  left: auto;
  right: 0;
  top: auto;
  bottom: calc(100% + 6px);
}

.oui-select-option {
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 7px 10px;
  border-radius: 8px;
  cursor: pointer;
  color: var(--text-secondary);
  font-family: var(--font-mono);
  transition: background-color 0.12s ease;
}

.oui-select-option:hover {
  background: var(--bg-card-hover);
  color: var(--text-primary);
}

.oui-select-option.selected {
  background: var(--accent-soft);
  color: var(--accent);
}

.oui-option-label {
  font-size: 12px;
  white-space: nowrap;
}

.oui-option-desc {
  font-family: var(--font-sans);
  font-size: 11px;
  color: var(--text-muted);
  white-space: normal;
}

.oui-select-option.selected .oui-option-desc {
  color: var(--text-secondary);
}

.oui-select-empty {
  padding: 10px;
  font-size: 12px;
  color: var(--text-muted);
  text-align: center;
}

.oui-option-check {
  flex-shrink: 0;
  color: var(--accent);
  font-size: 11px;
  align-self: center;
}

.oui-pop-enter-active,
.oui-pop-leave-active {
  transition: opacity 0.14s ease, transform 0.14s ease;
}

.oui-pop-enter-from,
.oui-pop-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}
</style>
