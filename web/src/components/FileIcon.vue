<script setup lang="ts">
import { computed } from 'vue';
import { fileKind } from '../lib/fileKinds';

/**
 * 文件 / 目录图标。
 *
 * Catppuccin 图标当作 alpha 蒙版，颜色由 `fileKind()` 按类型给出的调色板令牌决定，
 * 因此换主题、换调色板都会自动跟随。
 */
const props = withDefaults(defineProps<{ name: string; isDir?: boolean; open?: boolean; size?: number }>(), {
  size: 13,
});

const kind = computed(() =>
  props.isDir
    ? { icon: props.open ? '_folder_open' : '_folder', color: 'var(--subtext0, currentColor)' }
    : fileKind(props.name),
);
</script>

<template>
  <span
    class="file-icon"
    aria-hidden="true"
    :style="{
      '--file-icon': `url(/icons/catppuccin/${kind.icon}.svg)`,
      '--file-icon-color': kind.color,
      width: `${size}px`,
      height: `${size}px`,
    }"
  />
</template>

<style scoped>
.file-icon {
  display: inline-block;
  flex-shrink: 0;
  background-color: var(--file-icon-color);
  mask: center / contain no-repeat var(--file-icon);
  -webkit-mask: center / contain no-repeat var(--file-icon);
}
</style>
