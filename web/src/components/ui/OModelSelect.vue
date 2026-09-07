<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { LuCheck, LuChevronDown } from 'vue-icons-plus/lu';
import { useTranslations } from '../../composables/i18n';
import type { ModelInfo } from '../../types';

/**
 * 模型选择器：弹层左栏为提供商分组、右栏为该提供商的模型清单。
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
const modelPane = ref<HTMLElement | null>(null);
const activeProvider = ref('');

const popupStyle = ref<{ top: string; left: string; width: string }>({ top: '0', left: '0', width: '0' });

/** 弹层至少容纳双栏；左缘对齐触发器并按视口夹取，右侧溢出时右对齐触发器。 */
function updatePosition() {
  const rect = root.value?.getBoundingClientRect();
  if (!rect) return;
  const vw = document.documentElement.clientWidth;
  const width = Math.max(rect.width, 478);
  const rawLeft = rect.left + width > vw - 8 ? rect.right - width : rect.left;
  const left = Math.min(Math.max(8, rawLeft), Math.max(8, vw - width - 8));
  popupStyle.value = {
    top: `${rect.bottom + 6}px`,
    left: `${left}px`,
    width: `${width}px`,
  };
}

const providerIds = computed(() => Object.keys(props.groups));

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

const activeModels = computed(() => props.groups[activeProvider.value] ?? []);

function modelName(m: ModelInfo): string {
  return m.name || m.id;
}

function hintOf(m: ModelInfo): string {
  return `${Math.round(m.context_len / 1024)}K`;
}

function toggle() {
  open.value = !open.value;
}

function pickProvider(id: string) {
  activeProvider.value = id;
}

function pick(model: ModelInfo) {
  emit('update:modelValue', `${activeProvider.value}/${model.id}`);
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
  window.addEventListener('resize', updatePosition);
});
onBeforeUnmount(() => {
  document.removeEventListener('mousedown', onDocClick);
  document.removeEventListener('keydown', onDocKeydown);
  window.removeEventListener('resize', updatePosition);
});

watch(open, async (v) => {
  if (!v) return;
  const [p] = splitSelector(props.modelValue);
  activeProvider.value = props.groups[p] ? p : (providerIds.value[0] ?? '');
  await nextTick();
  updatePosition();
  if (modelPane.value) modelPane.value.scrollTop = 0;
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
        <template v-if="providerIds.length > 0">
          <div class="prov-col">
            <button
              v-for="id in providerIds"
              :key="id"
              type="button"
              class="prov"
              :class="{ active: id === activeProvider }"
              @click="pickProvider(id)"
            >
              <span class="prov-name">{{ id }}</span>
              <span class="prov-count">{{ (groups[id] ?? []).length }}</span>
            </button>
          </div>
          <div ref="modelPane" class="model-col">
            <button
              v-for="m in activeModels"
              :key="m.id"
              type="button"
              class="item"
              :class="{ active: activeProvider === curSel[0] && m.id === curSel[1] }"
              @click="pick(m)"
            >
              <span class="item-label">{{ modelName(m) }}</span>
              <span class="item-hint">{{ hintOf(m) }}</span>
              <LuCheck v-if="activeProvider === curSel[0] && m.id === curSel[1]" :size="13" class="check" />
            </button>
            <div v-if="activeModels.length === 0" class="empty-menu">{{ t('empty') }}</div>
          </div>
        </template>
        <div v-else class="empty-menu full">{{ t('empty') }}</div>
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
  z-index: 80;
  display: flex;
  height: 328px;
  padding: 4px;
  background: var(--surface-strong);
  border: 1px solid var(--line);
  border-radius: 10px;
  box-shadow: 0 8px 28px var(--shadow);
}
.prov-col {
  display: flex;
  flex-direction: column;
  gap: 1px;
  width: 170px;
  flex-shrink: 0;
  padding-right: 4px;
  border-right: 1px solid var(--line);
  overflow-y: auto;
}
.prov {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 6px;
  width: 100%;
  padding: 7px 9px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 12.5px;
  text-align: left;
  cursor: pointer;
  white-space: nowrap;
}
.prov:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.prov.active {
  background: var(--surface-active);
  color: var(--accent);
  font-weight: 600;
}
.prov-name {
  overflow: hidden;
  text-overflow: ellipsis;
}
.prov-count {
  flex-shrink: 0;
  font-size: 10.5px;
  color: var(--overlay0);
  background: var(--surface);
  border-radius: 99px;
  padding: 0 6px;
}
.model-col {
  flex: 1;
  min-width: 0;
  padding-left: 4px;
  overflow-y: auto;
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
.empty-menu.full {
  position: absolute;
  inset: 0;
  display: grid;
  place-items: center;
}
</style>
