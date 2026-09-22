<script setup lang="ts">
import { watch } from 'vue';
import { LuChevronRight } from 'vue-icons-plus/lu';
import type { FileNode } from '../types';
import { useTranslations } from '../composables/i18n';
import { fileTreeOpen } from '../stores/layout';
import FileIcon from './FileIcon.vue';

/**
 * 工作区文件树（递归组件）。
 *
 * 展开状态存在 layout store 里（`fileTreeOpen`）：打开文件预览时本组件会被卸载，
 * 状态若留在组件内，返回时之前展开的目录就全折起来了。默认全收起——工作区动辄
 * 上千条，全展开既无意义也拖慢渲染。目录内容随 `GET /api/workspace/tree`
 * 一次性带回（深度 4、上限 2000 项），因此展开不再发请求。
 */
defineOptions({ name: 'FileTree' });

const props = defineProps<{
  nodes: FileNode[];
  depth: number;
  /** 当前选中的文件路径（用于高亮） */
  selected?: string;
  /** 上一次可见的路径前缀，用于把祖先目录默认展开 */
  revealPrefix?: string;
  /** 正在取子节点的目录路径集合（展开时按需下钻，这里只用于显示加载态） */
  loadingDirs?: Set<string>;
}>();

const emit = defineEmits<{
  select: [node: FileNode];
  /** 展开目录：由持有树数据的一方去取这一层的子节点 */
  expand: [node: FileNode];
}>();

const { t } = useTranslations('panel');

/** 展开状态直接引用 store（跨卸载保留） */
const open = fileTreeOpen;

function isOpen(node: FileNode): boolean {
  if (node.path in open.value) return open.value[node.path]!;
  // 点击预览时展开对应目录链，否则用户会以为树里没有这个文件
  return !!props.revealPrefix && node.is_dir && props.revealPrefix.startsWith(node.path);
}

// 展开（含上次会话遗留的展开态）都要把该目录的子节点取回来
watch(
  () => props.nodes.map((n) => isOpen(n)).join(','),
  () => {
    for (const node of props.nodes) if (node.is_dir && isOpen(node)) emit('expand', node);
  },
  { immediate: true },
);

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
          <LuChevronRight v-if="node.is_dir" :size="12" class="caret" :class="{ open: isOpen(node) }" />
        </span>
        <FileIcon :name="node.name" :is-dir="node.is_dir" :open="isOpen(node)" />
        <span class="name">{{ node.name }}</span>
      </button>
      <div v-if="node.is_dir && isOpen(node) && loadingDirs?.has(node.path)" class="tree-loading">
        {{ t('loading') }}
      </div>
      <FileTree
        v-if="node.is_dir && isOpen(node) && node.children?.length"
        :nodes="node.children"
        :depth="depth + 1"
        :selected="selected"
        :reveal-prefix="revealPrefix"
        :loading-dirs="loadingDirs"
        @select="(n) => emit('select', n)"
        @expand="(n) => emit('expand', n)"
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
/* 下钻中的提示：与行同缩进，避免加载时目录突然“空着” */
.tree-loading {
  padding: 2px 6px 2px calc(4px + (var(--depth) + 1) * 12px);
  font-size: 11.5px;
  color: var(--overlay0);
}
</style>
