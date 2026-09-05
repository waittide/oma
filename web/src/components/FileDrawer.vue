<script setup lang="ts">
import { Fa6FolderTree, Fa6Xmark, Fa6Folder, Fa6RegFileLines } from 'vue-icons-plus/fa6';
import { ref } from 'vue';
import type { FileNode } from '../types';
import { fetchWorkspaceFile } from '../api';

const props = defineProps<{
  tree: FileNode | null;
  workspace: string;
}>();

defineEmits<{
  (e: 'close'): void;
}>();

const selectedFile = ref<string | null>(null);
const fileContent = ref<string>('');

async function handleFileClick(node: FileNode) {
  if (node.is_dir) return;
  selectedFile.value = node.path;
  try {
    const res = await fetchWorkspaceFile(props.workspace, node.path);
    fileContent.value = res.content;
  } catch (e) {
    fileContent.value = `// 无法读取文件: ${e}`;
  }
}
</script>

<template>
  <div class="drawer-overlay">
    <div style="padding: 14px 16px; border-bottom: 1px solid var(--border-subtle); display: flex; align-items: center; justify-content: space-between;">
      <span style="font-weight: 600;"><Fa6FolderTree style="vertical-align: -2px;" /> 工作区文件</span>
      <button class="btn-icon" @click="$emit('close')"><Fa6Xmark /></button>
    </div>

    <div style="flex: 1; overflow-y: auto; padding: 8px;">
      <template v-if="tree">
        <!-- 递归节点渲染组件 -->
        <div
          v-for="child in tree.children || []"
          :key="child.path"
          class="tree-node"
          @click="handleFileClick(child)"
        >
          <span><Fa6Folder v-if="child.is_dir" style="vertical-align: -2px;" /><Fa6RegFileLines v-else style="vertical-align: -2px;" /></span>
          <span>{{ child.name }}</span>
        </div>
      </template>
      <div v-else style="padding: 16px; color: var(--text-muted); font-size: 12px;">
        加载文件树中...
      </div>
    </div>

    <!-- 文件预览抽屉 -->
    <div
      v-if="selectedFile"
      style="height: 45%; border-top: 1px solid var(--border-default); display: flex; flex-direction: column; background: var(--bg-card);"
    >
      <div style="padding: 8px 12px; background: #161c24; border-bottom: 1px solid var(--border-subtle); display: flex; justify-content: space-between; font-family: var(--font-mono); font-size: 11px;">
        <span style="color: var(--accent);">{{ selectedFile }}</span>
        <button style="background: none; border: none; color: var(--text-muted); cursor: pointer;" @click="selectedFile = null"><Fa6Xmark /></button>
      </div>
      <pre style="flex: 1; overflow: auto; padding: 10px; font-family: var(--font-mono); font-size: 12px; color: var(--text-secondary); line-height: 1.4;">{{ fileContent }}</pre>
    </div>
  </div>
</template>
