<script setup lang="ts">
import { Fa6ChevronDown } from 'vue-icons-plus/fa6';
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue';

const props = defineProps<{
  modelValue: string;
  options: string[];
  placeholder?: string;
}>();

const emit = defineEmits<{
  (e: 'update:modelValue', v: string): void;
}>();

const root = ref<HTMLElement | null>(null);
const input = ref<HTMLInputElement | null>(null);
const open = ref(false);
const activeIndex = ref(-1);

const filtered = computed(() => {
  const q = props.modelValue.trim().toLowerCase();
  if (!q) return props.options;
  return props.options.filter((o) => o.toLowerCase().includes(q));
});

function onInput(v: string) {
  emit('update:modelValue', v);
  open.value = true;
  activeIndex.value = -1;
}

function pick(o: string) {
  emit('update:modelValue', o);
  open.value = false;
}

function onFocus() {
  open.value = true;
}

function onKeydown(e: KeyboardEvent) {
  if (!open.value && e.key === 'ArrowDown') {
    open.value = true;
    return;
  }
  if (!open.value) return;
  if (e.key === 'Escape') {
    open.value = false;
  } else if (e.key === 'ArrowDown') {
    e.preventDefault();
    activeIndex.value = Math.min(activeIndex.value + 1, filtered.value.length - 1);
    scrollActive();
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    activeIndex.value = Math.max(activeIndex.value - 1, 0);
    scrollActive();
  } else if (e.key === 'Enter' && activeIndex.value >= 0) {
    e.preventDefault();
    pick(filtered.value[activeIndex.value]);
  }
}

function scrollActive() {
  nextTick(() => {
    root.value?.querySelector('.oui-combo-option.active')?.scrollIntoView({ block: 'nearest' });
  });
}

function onDocPointer(e: PointerEvent) {
  if (root.value && !root.value.contains(e.target as Node)) open.value = false;
}

watch(open, (v) => {
  if (v) document.addEventListener('pointerdown', onDocPointer);
  else document.removeEventListener('pointerdown', onDocPointer);
});

onBeforeUnmount(() => document.removeEventListener('pointerdown', onDocPointer));
</script>

<template>
  <div ref="root" class="oui-combo">
    <input
      ref="input"
      class="field-input"
      type="text"
      :value="modelValue"
      :placeholder="placeholder"
      role="combobox"
      autocomplete="off"
      :aria-expanded="open"
      @input="onInput(($event.target as HTMLInputElement).value)"
      @focus="onFocus"
      @keydown="onKeydown"
    />
    <Fa6ChevronDown class="oui-combo-arrow" :class="{ up: open }" />

    <Transition name="oui-pop">
      <div v-if="open && filtered.length" class="oui-combo-menu" role="listbox">
        <div
          v-for="(o, i) in filtered"
          :key="o"
          class="oui-combo-option"
          :class="{ active: i === activeIndex, selected: o === modelValue }"
          role="option"
          :aria-selected="o === modelValue"
          @mousedown.prevent="pick(o)"
        >
          {{ o }}
        </div>
      </div>
    </Transition>
  </div>
</template>

<style scoped>
.oui-combo {
  position: relative;
}

.oui-combo-arrow {
  position: absolute;
  right: 11px;
  top: 50%;
  transform: translateY(-50%);
  color: var(--text-muted);
  font-size: 9px;
  pointer-events: none;
  transition: transform 0.18s ease;
}

.oui-combo-arrow.up {
  transform: translateY(-50%) rotate(180deg);
}

.oui-combo-menu {
  position: absolute;
  top: calc(100% + 4px);
  left: 0;
  right: 0;
  max-height: 220px;
  overflow-y: auto;
  background: var(--bg-card);
  border: 1px solid var(--border-default);
  border-radius: 10px;
  box-shadow: var(--shadow-lg);
  padding: 4px;
  z-index: 1200;
}

.oui-combo-option {
  padding: 6px 10px;
  border-radius: 7px;
  cursor: pointer;
  font-family: var(--font-mono);
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.oui-combo-option:hover,
.oui-combo-option.active {
  background: var(--bg-card-hover);
  color: var(--text-primary);
}

.oui-combo-option.selected {
  color: var(--accent);
  background: var(--accent-soft);
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
