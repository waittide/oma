<script setup lang="ts" generic="T extends string">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { LuCheck, LuChevronDown } from 'vue-icons-plus/lu';
import { useTranslations } from '../../composables/i18n';

export interface SelectOption<V extends string = string> {
  value: V;
  label: string;
  hint?: string;
}

const props = withDefaults(
  defineProps<{
    modelValue: T;
    options: SelectOption<T>[];
    placeholder?: string;
    width?: string;
    align?: 'start' | 'end';
  }>(),
  { placeholder: undefined, width: '100%', align: 'start' },
);

const emit = defineEmits<{ 'update:modelValue': [T] }>();

const { t } = useTranslations('select');

const root = ref<HTMLElement | null>(null);
const open = ref(false);
const popup = ref<HTMLElement | null>(null);

const current = computed(() => props.options.find((o) => o.value === props.modelValue) ?? null);

function toggle() {
  open.value = !open.value;
}

function pick(value: T) {
  emit('update:modelValue', value);
  open.value = false;
}

function onDocClick(e: MouseEvent) {
  if (open.value && root.value && !root.value.contains(e.target as Node)) open.value = false;
}

function onDocKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') open.value = false;
}

onMounted(() => {
  document.addEventListener('mousedown', onDocClick);
  document.addEventListener('keydown', onDocKeydown);
});
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocClick);
  document.removeEventListener('keydown', onDocKeydown);
});

watch(open, async (v) => {
  if (v && popup.value) popup.value.scrollTop = 0;
});
</script>

<template>
  <div ref="root" class="o-select" :style="{ width }">
    <button type="button" class="trigger" @click="toggle">
      <span class="label" :class="{ empty: !current }">
        {{ current?.label ?? placeholder ?? t('placeholder') }}
      </span>
      <LuChevronDown :size="14" class="chevron" :class="{ open }" />
    </button>

    <Teleport to="body">
      <div
        v-if="open"
        ref="popup"
        class="menu"
        :class="`align-${align}`"
        :style="{
          top: `${(root?.getBoundingClientRect().bottom ?? 0) + 6}px`,
          left:
            align === 'end'
              ? `${root?.getBoundingClientRect().right ?? 0}px`
              : `${root?.getBoundingClientRect().left ?? 0}px`,
        }"
      >
        <button
          v-for="opt in options"
          :key="opt.value"
          type="button"
          class="item"
          :class="{ active: opt.value === modelValue }"
          @click="pick(opt.value)"
        >
          <span class="item-label">{{ opt.label }}</span>
          <span v-if="opt.hint" class="item-hint">{{ opt.hint }}</span>
          <LuCheck v-if="opt.value === modelValue" :size="13" class="check" />
        </button>
        <div v-if="options.length === 0" class="empty-menu">{{ t('empty') }}</div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.o-select {
  position: relative;
}
.trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  height: 32px;
  padding: 0 10px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--surface);
  color: var(--ink);
  font-family: inherit;
  font-size: 13px;
  cursor: pointer;
  transition: border-color 0.15s ease;
}
.trigger:hover {
  border-color: var(--overlay0);
}
.label {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.label.empty {
  color: var(--overlay0);
}
.chevron {
  flex-shrink: 0;
  color: var(--text-tertiary);
  transition: transform 0.15s ease;
}
.chevron.open {
  transform: rotate(180deg);
}
.menu {
  position: fixed;
  z-index: 80;
  min-width: 180px;
  max-height: 320px;
  overflow-y: auto;
  padding: 4px;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 10px;
  box-shadow: 0 8px 28px var(--shadow);
}
.align-end {
  transform: translateX(-100%);
}
.item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 9px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}
.item:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.item.active {
  color: var(--accent);
  background: var(--surface-active);
}
.item-label {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.item-hint {
  font-size: 11px;
  color: var(--overlay0);
}
.check {
  flex-shrink: 0;
}
.empty-menu {
  padding: 10px;
  font-size: 12px;
  color: var(--overlay0);
  text-align: center;
}
</style>
