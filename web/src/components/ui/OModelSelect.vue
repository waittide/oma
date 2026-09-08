<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { LuCheck, LuChevronDown } from 'vue-icons-plus/lu';
import { useTranslations } from '../../composables/i18n';
import type { ModelInfo } from '../../types';

/**
 * 模型选择器：单栏分组弹层，provider 作为分组标题，模型为其下条目。
 * modelValue 为 "provider/model" 选择器（与后端 find_model 一致）。
 */
const props = withDefaults(
  defineProps<{
    modelValue: string;
    /** provider id → 模型清单 */
    groups: Record<string, ModelInfo[]>;
    placeholder?: string;
    width?: string;
  }>(),
  { placeholder: undefined, width: '180px' },
);

const emit = defineEmits<{ 'update:modelValue': [string] }>();

const { t } = useTranslations('modelSelect');

const root = ref<HTMLElement | null>(null);
const open = ref(false);
const popup = ref<HTMLElement | null>(null);

const popupStyle = ref<{ top: string; left: string; width: string }>({ top: '0', left: '0', width: '0' });

/** 弹层窄于 min 260 时取 260；下方放不下且上方更宽裕时向上展开；左右按视口夹取。 */
function updatePosition() {
  const rect = root.value?.getBoundingClientRect();
  if (!rect) return;
  const vw = document.documentElement.clientWidth;
  const vh = document.documentElement.clientHeight;
  const width = Math.min(Math.max(rect.width, 260), vw - 16);
  const rawLeft = rect.left + width > vw - 8 ? rect.right - width : rect.left;
  const left = Math.min(Math.max(8, rawLeft), Math.max(8, vw - width - 8));
  const h = popup.value?.offsetHeight ?? 0;
  const spaceBelow = vh - rect.bottom;
  const spaceAbove = rect.top;
  const below = h === 0 || h + 12 <= spaceBelow || spaceBelow >= spaceAbove;
  const top = below ? rect.bottom + 6 : Math.max(8, rect.top - h - 6);
  popupStyle.value = {
    top: `${top}px`,
    left: `${left}px`,
    width: `${width}px`,
  };
}

/** "provider/model" → [provider, model]；模型 id 本身可含 '/'，仅在首个 '/' 处切分。 */
function splitSelector(v: string): [string, string] {
  const i = v.indexOf('/');
  return i === -1 ? ['', v] : [v.slice(0, i), v.slice(i + 1)];
}

const curSel = computed(() => splitSelector(props.modelValue));

const currentLabel = computed(() => {
  if (!props.modelValue) return null;
  const [p, m] = splitSelector(props.modelValue);
  const model = (props.groups[p] ?? []).find((x) => x.id === m);
  return model ? model.name || model.id : props.modelValue;
});

const providerEntries = computed(() => Object.entries(props.groups));

function isSel(pid: string, m: ModelInfo): boolean {
  return curSel.value[0] === pid && curSel.value[1] === m.id;
}

function modelName(m: ModelInfo): string {
  return m.name || m.id;
}

function hintOf(m: ModelInfo): string {
  return `${Math.round(m.context_len / 1024)}K`;
}

function toggle() {
  open.value = !open.value;
}

function pick(pid: string, model: ModelInfo) {
  emit('update:modelValue', `${pid}/${model.id}`);
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
  // 容器内部滚动（capture 捕获 .stream 等局部滚动）时保持弹层贴合触发器
  window.addEventListener('scroll', updatePosition, true);
});
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocClick);
  document.removeEventListener('keydown', onDocKeydown);
  window.removeEventListener('resize', updatePosition);
  window.removeEventListener('scroll', updatePosition, true);
});

watch(open, async (v) => {
  if (!v) return;
  await nextTick();
  updatePosition();
  if (popup.value) popup.value.scrollTop = 0;
});
</script>

<template>
  <div ref="root" class="o-model-select" :style="{ width }">
    <button type="button" class="trigger" @click="toggle">
      <span class="label" :class="{ empty: !currentLabel }">
        {{ currentLabel ?? placeholder ?? t('placeholder') }}
      </span>
      <LuChevronDown :size="14" class="chevron" :class="{ open }" />
    </button>

    <Teleport to="body">
      <div v-if="open" ref="popup" class="menu" :style="popupStyle">
        <section v-for="[pid, models] in providerEntries" :key="pid" class="group">
          <header class="group-head">
            <span class="gh-name">{{ pid }}</span>
            <span class="gh-count">{{ models.length }}</span>
          </header>
          <button
            v-for="m in models"
            :key="m.id"
            type="button"
            class="item"
            :class="{ active: isSel(pid, m) }"
            @click="pick(pid, m)"
          >
            <span class="item-label">{{ modelName(m) }}</span>
            <span class="item-hint">{{ hintOf(m) }}</span>
            <LuCheck v-if="isSel(pid, m)" :size="13" class="check" />
          </button>
        </section>
        <div v-if="providerEntries.length === 0" class="empty-menu">{{ t('empty') }}</div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.o-model-select {
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
  max-height: 360px;
  padding: 4px;
  overflow-y: auto;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 10px;
  box-shadow: 0 8px 28px var(--shadow);
}
.group + .group {
  margin-top: 2px;
}
.group-head {
  position: sticky;
  top: -4px;
  z-index: 1;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  padding: 6px 9px 3px;
  background: var(--surface-strong);
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.4px;
  text-transform: uppercase;
  color: var(--overlay1);
  user-select: none;
}
.gh-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.gh-count {
  flex-shrink: 0;
  font-variant-numeric: tabular-nums;
  background: var(--surface);
  border-radius: 99px;
  padding: 0 6px;
  line-height: 14px;
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
  flex-shrink: 0;
  font-size: 11px;
  font-variant-numeric: tabular-nums;
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
