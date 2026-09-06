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
    <div class="drawer-header">
      <span style="display: inline-flex; align-items: center; gap: 8px;"><Fa6FolderTree /> 工作区文件</span>
      <button class="btn-icon" @click="$emit('close')"><Fa6Xmark /></button>
    </div>

    <div class="drawer-body">
      <template v-if="tree">
        <div
          v-for="child in tree.children || []"
          :key="child.path"
          class="tree-node"
          :class="{ selected: selectedFile === child.path }"
          @click="handleFileClick(child)"
        >
          <Fa6Folder v-if="child.is_dir" />
          <Fa6RegFileLines v-else />
          <span>{{ child.name }}</span>
        </div>
      </template>
      <div v-else class="sidebar-empty">加载文件树中…</div>
    </div>

    <!-- 文件预览抽屉 -->
    <div v-if="selectedFile" class="file-preview">
      <div class="file-preview-header">
        <span class="path">{{ selectedFile }}</span>
        <button v-tip="'关闭预览'" @click="selectedFile = null"><Fa6Xmark /></button>
      </div>
      <pre>{{ fileContent }}</pre>
    </div>
  </div>
</template>
