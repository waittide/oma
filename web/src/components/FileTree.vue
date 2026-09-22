<script setup lang="ts">
import { ref } from 'vue';
import { LuChevronRight } from 'vue-icons-plus/lu';
import type { FileNode } from '../types';
import FileIcon from './FileIcon.vue';

/**
 * 工作区文件树（递归组件）。
 *
 * 展开状态放在节点自身：默认全收起——工作区动辄上千条，全展开既无意义也拖慢渲染。
 * 目录内容随 `GET /api/workspace/tree` 一次性带回（深度 4、上限 2000 项），
 * 因此展开不再发请求。
 */
defineOptions({ name: 'FileTree' });

const props = defineProps<{
  nodes: FileNode[];
  depth: number;
  /** 当前选中的文件路径（用于高亮） */
  selected?: string;
  /** 上一次可见的路径前缀，用于把祖先目录默认展开 */
  revealPrefix?: string;
}>();

const emit = defineEmits<{ select: [node: FileNode] }>();

const open = ref<Record<string, boolean>>({});

function isOpen(node: FileNode): boolean {
  if (node.path in open.value) return open.value[node.path]!;
  // 点击预览时展开对应目录链，否则用户会以为树里没有这个文件
  return !!props.revealPrefix && node.is_dir && props.revealPrefix.startsWith(node.path);
}

function toggle(node: FileNode) {
  if (!node.is_dir) {
    emit('select', node);
    return;
  }
  open.value[node.path] = !isOpen(node);
}
</script>

<template>
  <ul class="tree" :style="{ '--depth': depth }">
    <li v-for="node in nodes" :key="node.path">
      <button
        type="button"
        class="row"
        :class="{ dir: node.is_dir, active: selected === node.path }"
        :title="node.path"
        @click="toggle(node)"
      >
        <span class="caret-slot">
          <LuChevronRight v-if="node.is_dir && node.children?.length" :size="12" class="caret" :class="{ open: isOpen(node) }" />
        </span>
        <FileIcon :name="node.name" :is-dir="node.is_dir" :open="isOpen(node)" />
        <span class="name">{{ node.name }}</span>
      </button>
      <FileTree
        v-if="node.is_dir && isOpen(node) && node.children?.length"
        :nodes="node.children"
        :depth="depth + 1"
        :selected="selected"
        :reveal-prefix="revealPrefix"
        @select="(n) => emit('select', n)"
      />
    </li>
  </ul>
</template>

<style scoped>
.tree {
  list-style: none;
  margin: 0;
  padding: 0;
}
.row {
  display: flex;
  align-items: center;
  gap: 4px;
  width: 100%;
  height: 22px;
  /* 每层缩进 12px，让层级在窄面板里也能一眼看清 */
  padding: 0 6px 0 calc(4px + var(--depth) * 12px);
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font: inherit;
  font-size: 12.5px;
  text-align: left;
  cursor: pointer;
}
.row:hover {
  background: var(--surface-hover);
}
.row.active {
  background: var(--surface-active);
  color: var(--ink);
}
.row.dir .name {
  color: var(--ink);
}
.caret-slot {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 12px;
  flex-shrink: 0;
}
.caret {
  color: var(--overlay0);
  transition: transform 0.12s ease;
}
.caret.open {
  transform: rotate(90deg);
}
.name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
