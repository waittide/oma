<script setup lang="ts">
import { UiModal } from '@waittide/ui';

/**
 * oma 弹窗 → `UiModal` 适配层：保留 `title/width/flush/floatingClose/footer` 语义。
 */
withDefaults(
  defineProps<{
    open: boolean;
    title?: string;
    width?: string;
    /** true 时 body 不带内边距与滚动，由内容自行布局（如左右分栏） */
    flush?: boolean;
    /** true 时不渲染标题栏，关闭按钮悬浮于面板右上角 */
    floatingClose?: boolean;
  }>(),
  { title: '', width: '440px', flush: false, floatingClose: false },
);

const emit = defineEmits<{ close: [] }>();
</script>

<template>
  <UiModal
    :open="open"
    :title="title"
    :width="width"
    :flush="flush"
    :floating-close="floatingClose"
    @close="emit('close')"
  >
    <slot />
    <template v-if="$slots.footer" #footer><slot name="footer" /></template>
  </UiModal>
</template>
