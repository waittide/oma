<script setup lang="ts">
withDefaults(
  defineProps<{
    label: string;
    /** 气泡水平锚定方式，避免在容器边缘被裁剪 */
    align?: 'center' | 'start' | 'end';
    /** true 时外层占满 flex 剩余宽度，保持内部文本的省略号约束 */
    block?: boolean;
  }>(),
  { align: 'center', block: false },
);
</script>

<template>
  <span class="o-tip" :class="[`align-${align}`, { block }]">
    <slot />
    <span class="bubble" role="tooltip">{{ label }}</span>
  </span>
</template>

<style scoped>
.o-tip {
  position: relative;
  display: inline-flex;
}
.o-tip.block {
  display: flex;
  flex: 1;
  min-width: 0;
}
.bubble {
  position: absolute;
  bottom: calc(100% + 7px);
  z-index: 95;
  padding: 3px 9px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--crust);
  color: var(--text);
  font-size: 11px;
  white-space: nowrap;
  pointer-events: none;
  opacity: 0;
  box-shadow: 0 4px 14px var(--shadow);
  transform: translateY(3px);
  transition:
    opacity 0.12s ease,
    transform 0.12s ease;
}
.align-center .bubble {
  left: 50%;
  transform: translateX(-50%) translateY(3px);
}
.align-start .bubble {
  left: 0;
}
.align-end .bubble {
  right: 0;
}
.o-tip:hover .bubble,
.o-tip:focus-within .bubble {
  opacity: 1;
  transform: translateY(0);
}
.align-center:hover .bubble,
.align-center:focus-within .bubble {
  transform: translateX(-50%) translateY(0);
}
</style>
