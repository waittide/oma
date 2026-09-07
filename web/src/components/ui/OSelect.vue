<script setup lang="ts" generic="T extends string">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
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

const popupStyle = ref<{ top: string; left: string; width: string }>({ top: '0', left: '0', width: '0' });

/** 弹层与触发器等宽，按视口夹取位置，避免右侧溢出。 */
function updatePosition() {
  const rect = root.value?.getBoundingClientRect();
  if (!rect) return;
  const vw = document.documentElement.clientWidth;
  const width = rect.width;
  const rawLeft = props.align === 'end' ? rect.right - width : rect.left;
  const left = Math.min(Math.max(8, rawLeft), Math.max(8, vw - width - 8));
  popupStyle.value = {
    top: `${rect.bottom + 6}px`,
    left: `${left}px`,
    width: `${width}px`,
  };
}

function toggle() {
  open.value = !open.value;
}

function pick(value: T) {
  emit('update:modelValue', value);
  open.value = false;
}

function onDocClick(e: MouseEvent) {
  if (!open.value) return;
  const t = e.target as Node;
  // 弹层 Teleport 到 body，点击弹层内部不属于 root，需显式排除
  if (root.value?.contains(t) || popup.value?.contains(t)) return;
  open.value = false;
}

function onDocKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') open.value = false;
}

onMounted(() => {
  document.addEventListener('mousedown', onDocClick);
  document.addEventListener('keydown', onDocKeydown);
  window.addEventListener('resize', updatePosition);
});
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocClick);
  document.removeEventListener('keydown', onDocKeydown);
  window.removeEventListener('resize', updatePosition);
});

watch(open, async (v) => {
  if (v) {
    await nextTick();
    updatePosition();
    if (popup.value) popup.value.scrollTop = 0;
  }
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
        :style="popupStyle"
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
  z-index: 95;
  max-height: 320px;
  overflow-y: auto;
  padding: 4px;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 10px;
  box-shadow: 0 8px 28px var(--shadow);
}
/* align-end 的右对齐由 updatePosition 计算，无需 transform */
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
  white-space: nowrap;
}
.item:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.item {
  white-space: nowrap;
}
.item.active {
  color: var(--accent);
  background: var(--surface-active);
}
.item-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.item-hint {
  font-size: 11px;
  color: var(--overlay0);
  flex-shrink: 0;
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
