<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import { useTranslations } from '../composables/i18n';

export interface RailItem {
  /** 目标消息 id；点击后滚动到该消息 */
  id: string;
  /** 用户提示词原文；悬停时在点左侧展示，最多三行 */
  text: string;
}

const props = defineProps<{ items: RailItem[] }>();
const emit = defineEmits<{ jump: [id: string] }>();

const { t } = useTranslations('chat');

/** 气泡与点的水平间距 */
const GAP = 10;
/** 气泡与视口边缘的最小留白 */
const EDGE = 8;

const hoverId = ref<string | null>(null);
const tipEl = ref<HTMLElement | null>(null);
const tipPos = ref({ top: '0px', left: '0px' });
/** 每个点的 DOM 引用：气泡需要按点的实时位置定位 */
const dotEls: Record<string, HTMLElement | null> = {};

function setDotEl(id: string, el: unknown) {
  const node = (el ?? null) as HTMLElement | null;
  if (node) dotEls[id] = node;
  else delete dotEls[id];
}

/** 提示词去掉多余空白：气泡按行截断，空行会白占行数 */
const hoverText = computed(() => {
  const raw = props.items.find((i) => i.id === hoverId.value)?.text ?? '';
  return raw.replace(/\n{2,}/g, '\n').trim();
});

/** 气泡固定定位在点的左侧并垂直居中，超出视口时夹取。 */
function updateTip() {
  const dot = hoverId.value ? dotEls[hoverId.value] : null;
  const tip = tipEl.value;
  if (!dot || !tip) return;
  const rect = dot.getBoundingClientRect();
  const vh = document.documentElement.clientHeight;
  // 点靠近视口上下边缘时气泡夹取到边界，仍尽量与点同高
  const centered = rect.top + rect.height / 2 - tip.offsetHeight / 2;
  tipPos.value = {
    top: `${Math.round(Math.min(Math.max(EDGE, centered), Math.max(EDGE, vh - tip.offsetHeight - EDGE)))}px`,
    left: `${Math.round(Math.max(EDGE, rect.left - tip.offsetWidth - GAP))}px`,
  };
}

function enter(id: string) {
  hoverId.value = id;
  void nextTick(updateTip);
}

function leave() {
  hoverId.value = null;
}

/** 消息流滚动或视口变化时让气泡跟随其点 */
function onViewportChange() {
  if (hoverId.value) updateTip();
}

onMounted(() => {
  window.addEventListener('resize', onViewportChange);
  // capture 捕获消息流自身的滚动，气泡才不会与点错位
  window.addEventListener('scroll', onViewportChange, true);
});

onBeforeUnmount(() => {
  window.removeEventListener('resize', onViewportChange);
  window.removeEventListener('scroll', onViewportChange, true);
});
</script>

<template>
  <!-- 竖线在可视区域垂直居中；消息过多时内部滚动，点间距保持恒定 -->
  <nav v-if="items.length" class="rail" :aria-label="t('messageRail')">
    <div class="rail-scroll">
      <div class="dot-col">
        <span class="line" />
        <button
          v-for="it in items"
          :key="it.id"
          :ref="(el) => setDotEl(it.id, el)"
          type="button"
          class="dot"
          :aria-label="it.text"
          @mouseenter="enter(it.id)"
          @mouseleave="leave"
          @focus="enter(it.id)"
          @blur="leave"
          @click="emit('jump', it.id)"
        />
      </div>
    </div>
  </nav>
  <Teleport to="body">
    <Transition name="rail-tip">
      <div v-if="hoverId" ref="tipEl" class="tip" :style="tipPos">{{ hoverText }}</div>
    </Transition>
  </Teleport>
</template>

<style scoped>
/* 只占右侧一窄列，鼠标离开后不遮挡消息；整列在消息流可视高度内垂直居中 */
.rail {
  position: absolute;
  top: 0;
  bottom: 0;
  right: 6px;
  display: flex;
  align-items: center;
  /* 仅点与竖线自身可交互：空白处不拦截消息流上的操作 */
  pointer-events: none;
}
/* 隐藏滚动条，但它仍可滚动：消息很多时点可滑到任意一条而不挤压消息宽度 */
.rail-scroll {
  max-height: 100%;
  overflow-y: auto;
  padding: 2px 6px;
  scrollbar-width: none;
  pointer-events: auto;
}
.rail-scroll::-webkit-scrollbar {
  width: 0;
}
.dot-col {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
}
/* 竖线贯穿所有点，作为位置参考而不是装饰；悬停整段时高亮 */
.line {
  position: absolute;
  top: 7px;
  bottom: 7px;
  left: 50%;
  width: 2px;
  transform: translateX(-50%);
  border-radius: 99px;
  background: color-mix(in srgb, var(--overlay0) 42%, transparent);
  transition: background-color 0.15s ease;
}
.rail-scroll:hover .line {
  background: color-mix(in srgb, var(--accent) 70%, transparent);
}
/* 点自带较大点击热区，视觉尺寸由 ::before 控制 */
.dot {
  display: grid;
  place-items: center;
  width: 14px;
  height: 14px;
  padding: 0;
  border: none;
  background: transparent;
  cursor: pointer;
}
.dot::before {
  content: '';
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--overlay0);
  transition:
    transform 0.12s ease,
    background-color 0.12s ease;
}
.dot:hover::before,
.dot:focus-visible::before {
  transform: scale(1.7);
  background: var(--accent);
}
.tip {
  position: fixed;
  z-index: 96;
  /* 最多三行：行高已知，直接按行数限制高度而非用 em 估算 */
  --tip-line: 18px;
  max-height: calc(var(--tip-line) * 3);
  width: max-content;
  max-width: min(280px, calc(100vw - 24px));
  overflow: hidden;
  padding: 7px 10px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--crust);
  color: var(--text);
  font-size: 12px;
  line-height: 1.5;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
  pointer-events: none;
  box-shadow: 0 6px 18px var(--shadow);
}
.rail-tip-enter-active,
.rail-tip-leave-active {
  transition: opacity 0.12s ease;
}
.rail-tip-enter-from,
.rail-tip-leave-to {
  opacity: 0;
}
</style>
