<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import { useTranslations } from '../composables/i18n';

export interface RailItem {
  /** 目标消息 id；点击后滚动到该消息 */
  id: string;
  /** 用户提示词原文；悬停时在点左侧展示，最多三行 */
  text: string;
}

/**
 * 对话记录指示器。
 *
 * 每条用户消息一个标记，自上而下按固定间距铺开：`gap = min(MAX_GAP,
 * 可用高度 / (n-1))`——数量少时聚在上方不强行铺满，数量多时自然收紧、正好占满整
 * 条竖线，因此标记的位置与对话长度是对应的。当前所在的一轮由 `activeId` 高亮。
 *
 * 这里**始终显示**（只要有消息就渲染，不等滚动条出现）。
 */
const props = defineProps<{
  items: RailItem[];
  /** 当前视口所在一轮的消息 id */
  activeId?: string | null;
}>();
const emit = defineEmits<{ jump: [id: string] }>();

const { t } = useTranslations('chat');

/** 竖线两端留白 */
const PAD = 10;
/** 相邻标记的最大间距：数量少时聚在上方 */
const MAX_GAP = 50;
/** 单个标记的点击热区高度（比圆点大，便于点中） */
const HIT = 16;
/** 气泡与点的水平间距 */
const GAP = 10;
/** 气泡与视口边缘的最小留白 */
const EDGE = 8;

const railEl = ref<HTMLElement | null>(null);
const railHeight = ref(0);
const hoverId = ref<string | null>(null);
/** 拖拽中：按住后滑动可连续跳转 */
const dragging = ref(false);

const gap = computed(() => {
  const count = props.items.length;
  if (count <= 1) return 0;
  return Math.min(MAX_GAP, Math.max(0, (railHeight.value - PAD * 2) / (count - 1)));
});

/** 第 i 个标记的中心 y（相对竖线容器） */
function centerOf(index: number): number {
  return PAD + index * gap.value;
}

/** 指针 y → 最近的标记下标 */
function indexAt(clientY: number): number {
  const box = railEl.value?.getBoundingClientRect();
  if (!box) return 0;
  const y = clientY - box.top;
  if (gap.value <= 0) return 0;
  return Math.min(props.items.length - 1, Math.max(0, Math.round((y - PAD) / gap.value)));
}

// 竖线高度变化（换会话、窗口缩放）时重排：`ResizeObserver` 比监听 window 更准
let observer: ResizeObserver | undefined;
function observeRail(el: HTMLElement | null) {
  railEl.value = el;
  observer?.disconnect();
  if (!el) return;
  railHeight.value = el.clientHeight;
  observer = new ResizeObserver(() => {
    railHeight.value = el.clientHeight;
  });
  observer.observe(el);
}

onBeforeUnmount(() => observer?.disconnect());

function jumpTo(index: number) {
  const item = props.items[index];
  if (item) emit('jump', item.id);
}

function onPointerDown(ev: PointerEvent) {
  if (ev.button !== 0) return;
  dragging.value = true;
  jumpTo(indexAt(ev.clientY));
  // 指针捕获让拖拽时的事件继续落在本元素上；合成事件或部分环境会抛错，
  // 失败也不能影响这次点击跳转本身
  try {
    (ev.currentTarget as HTMLElement).setPointerCapture(ev.pointerId);
  } catch {
    // 忽略：没有捕获时拖拽仍可在轨道内继续
  }
}

function onPointerMove(ev: PointerEvent) {
  if (!dragging.value) {
    // 未拖拽时也让悬停的点高亮，拖拽中则连续跳转
    const index = indexAt(ev.clientY);
    hoverId.value = props.items[index]?.id ?? null;
    return;
  }
  const index = indexAt(ev.clientY);
  if (props.items[index]?.id !== hoverId.value) {
    hoverId.value = props.items[index]?.id ?? null;
    jumpTo(index);
  }
}

function endDrag(ev: PointerEvent) {
  if (!dragging.value) return;
  dragging.value = false;
  try {
    (ev.currentTarget as HTMLElement).releasePointerCapture?.(ev.pointerId);
  } catch {
    // 忽略：未捕获时释放会抛错，不影响后续交互
  }
}

function onPointerLeave() {
  if (!dragging.value) hoverId.value = null;
}

/** 悬停/聚焦到某个标记时高亮它并展示气泡 */
function enter(id: string) {
  if (dragging.value) return;
  hoverId.value = id;
  void nextTick(updateTip);
}

const tipEl = ref<HTMLElement | null>(null);
const tipPos = ref({ top: '0px', left: '0px' });

/** 气泡固定定位在标记的左侧并垂直居中，超出视口时夹取。 */
function updateTip() {
  const marker = hoverId.value ? markerEls[hoverId.value] : null;
  const tip = tipEl.value;
  if (!marker || !tip) return;
  const rect = marker.getBoundingClientRect();
  const vh = document.documentElement.clientHeight;
  const centered = rect.top + rect.height / 2 - tip.offsetHeight / 2;
  tipPos.value = {
    top: `${Math.round(Math.min(Math.max(EDGE, centered), Math.max(EDGE, vh - tip.offsetHeight - EDGE)))}px`,
    left: `${Math.round(Math.max(EDGE, rect.left - tip.offsetWidth - GAP))}px`,
  };
}

/** 提示词去掉多余空白：气泡按行截断，空行会白占行数 */
const hoverText = computed(() => {
  const raw = props.items.find((i) => i.id === hoverId.value)?.text ?? '';
  return raw.replace(/\n{2,}/g, '\n').trim();
});

/** 每个标记的 DOM 引用：气泡需要按标记的实时位置定位 */
const markerEls: Record<string, HTMLElement | null> = {};
function setMarkerEl(id: string, el: unknown) {
  const raw = el && typeof el === 'object' && '$el' in el ? (el as { $el: HTMLElement }).$el : el;
  const node = (raw ?? null) as HTMLElement | null;
  if (node) markerEls[id] = node;
  else delete markerEls[id];
}

/** 消息流滚动或视口变化时让气泡跟随其标记 */
function onViewportChange() {
  if (hoverId.value) updateTip();
}

onMounted(() => {
  window.addEventListener('resize', onViewportChange);
  // capture 捕获消息流自身的滚动，气泡才不会与标记错位
  window.addEventListener('scroll', onViewportChange, true);
});

onBeforeUnmount(() => {
  window.removeEventListener('resize', onViewportChange);
  window.removeEventListener('scroll', onViewportChange, true);
});
</script>

<template>
  <nav
    v-if="items.length"
    :ref="(el) => observeRail(el as HTMLElement | null)"
    class="rail"
    :aria-label="t('messageRail')"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="endDrag"
    @pointercancel="endDrag"
    @pointerleave="onPointerLeave"
  >
    <span class="line" :style="{ top: `${PAD}px`, bottom: `${PAD}px` }" />
    <button
      v-for="(it, i) in items"
      :key="it.id"
      :ref="(el) => setMarkerEl(it.id, el)"
      type="button"
      class="marker"
      :class="{ active: it.id === activeId, on: it.id === hoverId }"
      :style="{ top: `${centerOf(i) - HIT / 2}px`, height: `${HIT}px` }"
      :aria-label="it.text"
      @mouseenter="enter(it.id)"
      @focus="enter(it.id)"
      @blur="hoverId = null"
    />
  </nav>
  <Teleport to="body">
    <Transition name="rail-tip">
      <div v-if="hoverId && hoverText" ref="tipEl" class="tip" :style="tipPos">{{ hoverText }}</div>
    </Transition>
  </Teleport>
</template>

<style scoped>
/* 覆盖消息流右侧一整列：标记的纵向位置与对话长度对应，不再内部滚动 */
.rail {
  position: absolute;
  top: 0;
  bottom: 0;
  right: 4px;
  width: 20px;
  pointer-events: auto;
  touch-action: none;
}
/* 竖线贯穿整条轨道，作为位置参考而不是装饰 */
.line {
  position: absolute;
  left: 50%;
  width: 2px;
  transform: translateX(-50%);
  border-radius: 99px;
  background: color-mix(in srgb, var(--overlay0) 34%, transparent);
}
/* 标记：整条热区负责命中，圆点由 ::before 绘制（活跃/悬停时放大并上色） */
.marker {
  position: absolute;
  left: 0;
  width: 100%;
  display: grid;
  place-items: center;
  padding: 0;
  border: none;
  background: transparent;
  cursor: pointer;
}
.marker::before {
  content: '';
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--overlay0);
  transition:
    transform 0.12s ease,
    background-color 0.12s ease;
}
.marker:hover::before,
.marker.on::before {
  transform: scale(1.5);
  background: var(--accent);
}
.marker.active::before {
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
