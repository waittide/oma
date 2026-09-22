<script setup lang="ts">
import { computed } from 'vue';
import { parseDiffLines } from '../lib/patchDiff';

const props = defineProps<{ text: string }>();

const lines = computed(() => parseDiffLines(props.text));
</script>

<template>
  <div class="diff-view">
    <!-- 空行保留占位，否则相邻差异行会挤在一起看不出分组 -->
    <div v-for="(line, i) in lines" :key="i" class="diff-line" :class="line.kind">{{ line.text || ' ' }}</div>
  </div>
</template>

<style scoped>
.diff-view {
  display: flex;
  flex-direction: column;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.65;
}
/* 每条线都占同样的左侧色条宽度：有无标记的行文字才不会左右错位 */
.diff-line {
  padding: 0 4px;
  border-left: 2px solid transparent;
  white-space: pre-wrap;
  word-break: break-word;
}
/* 文件头：`--- a/x` / `+++ b/x`，给足对比但不上色 */
.diff-line.file {
  color: var(--text-secondary);
  font-weight: 600;
}
.diff-line.meta {
  color: var(--faint);
}
.diff-line.hunk {
  color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  border-left-color: var(--accent);
}
.diff-line.add {
  color: var(--success);
  background: color-mix(in srgb, var(--success) 12%, transparent);
  border-left-color: var(--success);
}
.diff-line.del {
  color: var(--danger);
  background: color-mix(in srgb, var(--danger) 12%, transparent);
  border-left-color: var(--danger);
}
.diff-line.ctx {
  color: var(--text-tertiary);
}
</style>
