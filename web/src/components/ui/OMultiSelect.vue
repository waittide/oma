<script setup lang="ts" generic="T extends string = string">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { LuChevronDown, LuX } from 'vue-icons-plus/lu';
import { useTranslations } from '../../composables/i18n';

export interface MultiOption<T extends string = string> {
  value: T;
  label: string;
  hint?: string;
}

/**
 * 多选下拉。默认按 `string` 工作（工具白名单等自由文本场景），
 * 传入字面量联合类型（如 `ModelCapability`）时由泛型推断收窄，
 * 让 `modelValue` 与 `options` 的取值在编译期一致。
 */
const props = withDefaults(
  defineProps<{
    modelValue: T[];
    options: MultiOption<T>[];
    placeholder?: string;
    disabled?: boolean;
  }>(),
  { placeholder: undefined, disabled: false },
);

const emit = defineEmits<{ 'update:modelValue': [T[]] }>();

const { t } = useTranslations('select');

const root = ref<HTMLElement | null>(null);
const open = ref(false);
const popup = ref<HTMLElement | null>(null);
const popupStyle = ref<{ top: string; left: string; width: string }>({ top: '0', left: '0', width: '0' });

/** 选中项按原顺序保留；标签与下拉项共用同一份定义 */
const selected = computed(() =>
  props.modelValue
    .map((v) => props.options.find((o) => o.value === v) ?? { value: v, label: v })
);

/** 弹层与触发器等宽；下方放不下且上方更宽裕时向上展开，位置按视口夹取。 */
function updatePosition() {
  const rect = root.value?.getBoundingClientRect();
  if (!rect) return;
  const vw = document.documentElement.clientWidth;
  const vh = document.documentElement.clientHeight;
  const width = rect.width;
  const left = Math.min(Math.max(8, rect.left), Math.max(8, vw - width - 8));
  const h = popup.value?.offsetHeight ?? 0;
  const spaceBelow = vh - rect.bottom;
  const spaceAbove = rect.top;
  const below = h === 0 || h + 12 <= spaceBelow || spaceBelow >= spaceAbove;
  const top = below ? rect.bottom + 6 : Math.max(8, rect.top - h - 6);
  popupStyle.value = { top: `${top}px`, left: `${left}px`, width: `${width}px` };
}

function toggleValue(value: T) {
  const has = props.modelValue.includes(value);
  emit(
    'update:modelValue',
    has ? props.modelValue.filter((v) => v !== value) : [...props.modelValue, value],
  );
}

function remove(value: T, e: Event) {
  e.stopPropagation();
  emit('update:modelValue', props.modelValue.filter((v) => v !== value));
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
  // 容器内部滚动（capture 捕获弹窗内滚动）时保持弹层贴合触发器
  window.addEventListener('scroll', updatePosition, true);
});
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocClick);
  document.removeEventListener('keydown', onDocKeydown);
  window.removeEventListener('resize', updatePosition);
  window.removeEventListener('scroll', updatePosition, true);
});

watch(open, async (v) => {
  if (v) {
    await nextTick();
    updatePosition();
  }
});
</script>

<template>
  <div ref="root" class="o-multi" :class="{ disabled }">
    <button
      type="button"
      class="trigger"
      :disabled="disabled"
      @click="open = !open"
    >
      <span v-if="selected.length === 0" class="placeholder">
        {{ placeholder ?? t('placeholder') }}
      </span>
      <span v-else class="chips">
        <span v-for="s in selected" :key="s.value" class="chip">
          {{ s.label }}
          <span
            class="chip-x"
            role="button"
            :aria-label="s.label"
            @click="remove(s.value, $event)"
          >
            <LuX :size="10" />
          </span>
        </span>
      </span>
      <LuChevronDown :size="14" class="chevron" :class="{ open }" />
    </button>

    <Teleport to="body">
      <div v-if="open" ref="popup" class="menu" :style="popupStyle">
        <button
          v-for="opt in options"
          :key="opt.value"
          type="button"
          class="item"
          :class="{ active: modelValue.includes(opt.value) }"
          @click="toggleValue(opt.value)"
        >
          <span class="box" :class="{ on: modelValue.includes(opt.value) }" />
          <span class="item-label">{{ opt.label }}</span>
          <span v-if="opt.hint" class="item-hint">{{ opt.hint }}</span>
        </button>
        <div v-if="options.length === 0" class="empty-menu">{{ t('empty') }}</div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.o-multi {
  position: relative;
  width: 100%;
}
.trigger {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  min-height: 32px;
  padding: 5px 10px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--surface);
  color: var(--ink);
  font-family: inherit;
  font-size: 13px;
  cursor: pointer;
  text-align: left;
  transition: border-color 0.15s ease;
}
/* 无选中项时触发器仅一行高度，居中后提示文字自然垂直居中 */
.trigger:hover:not(:disabled) {
  border-color: var(--overlay0);
}
.trigger:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.placeholder {
  color: var(--overlay0);
  line-height: 1.4;
}
/* 标签随宽度换行，触发器随之增高；行内元素在交叉轴居中 */
.chips {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px;
  min-width: 0;
}
.chip {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  padding: 1px 4px 1px 7px;
  border-radius: 5px;
  background: var(--surface-hover);
  color: var(--ink);
  font-size: 11.5px;
  line-height: 18px;
}
.chip-x {
  display: inline-flex;
  align-items: center;
  color: var(--text-tertiary);
  cursor: pointer;
}
.chip-x:hover {
  color: var(--danger);
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
  max-height: 280px;
  overflow-y: auto;
  padding: 4px;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 10px;
  box-shadow: 0 8px 28px var(--shadow);
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
  white-space: nowrap;
}
.item:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.box {
  flex-shrink: 0;
  width: 13px;
  height: 13px;
  border: 1px solid var(--control-border);
  border-radius: 4px;
}
.box.on {
  border-color: var(--accent);
  background: var(--accent);
}
.item-label {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}
.item-hint {
  font-size: 11px;
  color: var(--overlay0);
  flex-shrink: 0;
}
.empty-menu {
  padding: 10px;
  font-size: 12px;
  color: var(--overlay0);
  text-align: center;
}
</style>
