<script setup lang="ts">
import { ref } from 'vue';

/**
 * 面板拖拽分隔条。`vertical` 表示竖直分隔条（左右拖动改变相邻面板宽度）。
 * 拖动时只上报增量，由调用方决定夹取范围与落点。
 */
const props = withDefaults(
  defineProps<{
    /** 竖直分隔条（用于左右面板）；false 时为水平分隔条（用于上下分区） */
    vertical?: boolean;
    /** 拖动方向取反：右侧面板的分隔条位于其左侧，向左拖为变宽 */
    invert?: boolean;
  }>(),
  { vertical: true, invert: false },
);

const emit = defineEmits<{ (e: 'resize', delta: number): void; (e: 'resizeEnd'): void }>();

const dragging = ref(false);
let start = 0;

function onPointerDown(e: PointerEvent) {
  dragging.value = true;
  start = props.vertical ? e.clientX : e.clientY;
  (e.target as HTMLElement).setPointerCapture(e.pointerId);
}

function onPointerMove(e: PointerEvent) {
  if (!dragging.value) return;
  const current = props.vertical ? e.clientX : e.clientY;
  const delta = (current - start) * (props.invert ? -1 : 1);
  start = current;
  emit('resize', delta);
}

function onPointerUp(e: PointerEvent) {
  if (!dragging.value) return;
  dragging.value = false;
  (e.target as HTMLElement).releasePointerCapture(e.pointerId);
  emit('resizeEnd');
}
</script>

<template>
  <div
    class="resizer"
    :class="{ vertical, dragging }"
    role="separator"
    :aria-orientation="vertical ? 'vertical' : 'horizontal'"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
  />
</template>

<style scoped>
.resizer {
  flex-shrink: 0;
  background: transparent;
  transition: background var(--duration) var(--ease);
}
.resizer.vertical {
  width: 5px;
  margin: 0 -2px;
  cursor: col-resize;
}
.resizer:not(.vertical) {
  height: 5px;
  margin: -2px 0;
  cursor: row-resize;
}
.resizer:hover,
.resizer.dragging {
  background: color-mix(in srgb, var(--accent) 45%, transparent);
}
</style>
