<script setup lang="ts">
withDefaults(
  defineProps<{
    label: string;
    /** 气泡水平锚定方式，避免在容器边缘被裁剪 */
    align?: 'center' | 'start' | 'end';
    /** 气泡垂直弹出方向：顶栏元素用 bottom 避免被视口上缘裁剪 */
    placement?: 'top' | 'bottom';
    /** true 时外层占满 flex 剩余宽度，保持内部文本的省略号约束 */
    block?: boolean;
  }>(),
  { align: 'center', placement: 'top', block: false },
);
</script>

<template>
  <span class="o-tip" :class="[`align-${align}`, `placement-${placement}`, { block }]">
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
  transition:
    opacity 0.12s ease,
    transform 0.12s ease;
}
/* 垂直锚定 + 入场位移方向 */
.placement-top .bubble {
  bottom: calc(100% + 7px);
  transform: translateY(3px);
}
.placement-bottom .bubble {
  top: calc(100% + 7px);
  transform: translateY(-3px);
}
/* 水平锚定 */
.align-center .bubble {
  left: 50%;
}
.align-start .bubble {
  left: 0;
}
.align-end .bubble {
  right: 0;
}
/* 居中锚定需叠加水平位移 */
.align-center.placement-top .bubble {
  transform: translateX(-50%) translateY(3px);
}
.align-center.placement-bottom .bubble {
  transform: translateX(-50%) translateY(-3px);
}
/* 悬停/聚焦显示 */
.o-tip:hover .bubble,
.o-tip:focus-within .bubble {
  opacity: 1;
}
.placement-top:hover .bubble,
.placement-top:focus-within .bubble {
  transform: translateY(0);
}
.placement-bottom:hover .bubble,
.placement-bottom:focus-within .bubble {
  transform: translateY(0);
}
.align-center.placement-top:hover .bubble,
.align-center.placement-top:focus-within .bubble {
  transform: translateX(-50%) translateY(0);
}
.align-center.placement-bottom:hover .bubble,
.align-center.placement-bottom:focus-within .bubble {
  transform: translateX(-50%) translateY(0);
}
</style>
