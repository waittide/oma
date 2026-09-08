<script setup lang="ts">
import { nextTick, onBeforeUnmount, ref, watch } from 'vue';

const props = withDefaults(
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

/** 气泡与触发器的间距 */
const GAP = 7;
/** 气泡与视口边缘的最小留白 */
const EDGE = 8;

const root = ref<HTMLElement | null>(null);
const bubble = ref<HTMLElement | null>(null);
const visible = ref(false);
const position = ref({ top: '0px', left: '0px' });

/**
 * 气泡 Teleport 到 body 后用 fixed 定位：彻底摆脱祖先 overflow/transform 的裁剪，
 * 并在视口边缘自动翻转与夹取。
 */
function updatePosition() {
  const trigger = root.value?.getBoundingClientRect();
  const tip = bubble.value?.getBoundingClientRect();
  if (!trigger || !tip) return;

  const vw = document.documentElement.clientWidth;
  const vh = document.documentElement.clientHeight;

  // 触发器滚出视口后隐藏，避免气泡滞留在边缘
  if (trigger.bottom < 0 || trigger.top > vh || trigger.right < 0 || trigger.left > vw) {
    hide();
    return;
  }

  // 垂直：优先 props.placement，空间不足且反向更宽裕时翻转
  const spaceAbove = trigger.top;
  const spaceBelow = vh - trigger.bottom;
  let place = props.placement;
  if (place === 'top' && spaceAbove < tip.height + GAP && spaceBelow > spaceAbove) place = 'bottom';
  else if (place === 'bottom' && spaceBelow < tip.height + GAP && spaceAbove > spaceBelow) place = 'top';

  // 水平：按 align 锚定后夹取到视口内
  let left =
    props.align === 'start'
      ? trigger.left
      : props.align === 'end'
        ? trigger.right - tip.width
        : trigger.left + trigger.width / 2 - tip.width / 2;

  const top = place === 'top' ? trigger.top - tip.height - GAP : trigger.bottom + GAP;
  left = Math.min(Math.max(EDGE, left), Math.max(EDGE, vw - tip.width - EDGE));

  position.value = {
    top: `${Math.round(Math.min(Math.max(EDGE, top), Math.max(EDGE, vh - tip.height - EDGE)))}px`,
    left: `${Math.round(left)}px`,
  };
}

function show() {
  visible.value = true;
  void nextTick(updatePosition);
}

function hide() {
  visible.value = false;
}

/** 焦点在包装元素内部移动时保持显示，避免按钮内部元素切换导致闪烁 */
function onFocusOut(e: FocusEvent) {
  const next = e.relatedTarget as Node | null;
  if (next && root.value?.contains(next)) return;
  hide();
}

function onViewportChange() {
  if (visible.value) updatePosition();
}

watch(visible, (v) => {
  if (v) {
    window.addEventListener('resize', onViewportChange);
    // capture 捕获 .tree/.stream 等局部滚动，保持气泡贴合触发器
    window.addEventListener('scroll', onViewportChange, true);
  } else {
    window.removeEventListener('resize', onViewportChange);
    window.removeEventListener('scroll', onViewportChange, true);
  }
});

onBeforeUnmount(() => {
  window.removeEventListener('resize', onViewportChange);
  window.removeEventListener('scroll', onViewportChange, true);
});
</script>

<template>
  <span
    ref="root"
    class="o-tip"
    :class="{ block }"
    @mouseenter="show"
    @mouseleave="hide"
    @focusin="show"
    @focusout="onFocusOut"
  >
    <slot />
  </span>
  <Teleport to="body">
    <Transition name="o-tip">
      <span v-if="visible" ref="bubble" class="bubble" role="tooltip" :style="position">
        {{ label }}
      </span>
    </Transition>
  </Teleport>
</template>

<style scoped>
.o-tip {
  display: inline-flex;
}
.o-tip.block {
  display: flex;
  flex: 1;
  min-width: 0;
}
.bubble {
  position: fixed;
  z-index: 96;
  padding: 3px 9px;
  border: 1px solid var(--line);
  border-radius: 6px;
  background: var(--crust);
  color: var(--text);
  font-size: 11px;
  line-height: 1.45;
  max-width: min(360px, calc(100vw - 16px));
  white-space: normal;
  overflow-wrap: anywhere;
  pointer-events: none;
  box-shadow: 0 4px 14px var(--shadow);
}
.o-tip-enter-active,
.o-tip-leave-active {
  transition: opacity 0.12s ease;
}
.o-tip-enter-from,
.o-tip-leave-to {
  opacity: 0;
}
</style>
