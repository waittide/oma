<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { UiIconButton, UiTooltip } from '@waittide/ui';
import {
  LuFileText,
  LuGitCompare,
  LuListTree,
  LuPanelRight,
  LuRefreshCw,
  LuTerminal,
} from 'vue-icons-plus/lu';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';
import * as layout from '../stores/layout';
import { api } from '../api';
import type { FileNode } from '../types';
import FileTree from './FileTree.vue';
import FilePreview from './FilePreview.vue';
import GitChanges from './GitChanges.vue';
import HistoryTree from './HistoryTree.vue';

/**
 * 右侧工作面板。
 *
 * 「文件」「变更」「历史树」已接入真实内容（工作区文件树 / 预览、Git status + 逐文件
 * diff、会话分支树）；内置终端待 P9（需要后端 pty + WS）。
 */
const { t } = useTranslations('panel');

const tabs = computed(() => [
  { id: 'files' as const, label: t('files'), icon: LuFileText },
  { id: 'changes' as const, label: t('changes'), icon: LuGitCompare },
  { id: 'tree' as const, label: t('tree'), icon: LuListTree },
  { id: 'terminal' as const, label: t('terminal'), icon: LuTerminal },
]);

const workspace = computed(() => activeSession.value?.workspace ?? '');

// ---------- 文件树 ----------
const tree = ref<FileNode[]>([]);
const treeLoading = ref(false);
const treeError = ref('');
/** 选中的文件：非空时展示预览，否则展示树 */
const selectedPath = ref('');

async function loadTree() {
  const ws = workspace.value;
  if (!ws) {
    tree.value = [];
    return;
  }
  treeLoading.value = true;
  treeError.value = '';
  try {
    const root = await api.workspaceTree(ws);
    tree.value = root.children ?? [];
  } catch (e) {
    treeError.value = e instanceof Error ? e.message : String(e);
    tree.value = [];
  } finally {
    treeLoading.value = false;
  }
}

// 换工作区要重取；换会话但工作区相同时不必（树与 git 都只看工作区）
watch(workspace, () => {
  selectedPath.value = '';
  void loadTree();
}, { immediate: true });
</script>

<template>
  <aside class="right-panel">
    <div class="tabs">
      <button
        v-for="tab in tabs"
        :key="tab.id"
        type="button"
        class="tab"
        :class="{ active: layout.rightTab.value === tab.id }"
        @click="layout.setRightTab(tab.id)"
      >
        <component :is="tab.icon" :size="13" />
        <span>{{ tab.label }}</span>
      </button>
      <UiTooltip
        v-if="layout.rightTab.value === 'files'"
        :content="t('refresh')"
        align="end"
        placement="bottom"
      >
        <UiIconButton class="tab-action" size="sm" :label="t('refresh')" :disabled="!activeSession" @click="loadTree">
          <LuRefreshCw :size="13" />
        </UiIconButton>
      </UiTooltip>
      <UiTooltip :content="t('close')" align="end" placement="bottom">
        <UiIconButton class="tab-close" size="sm" :label="t('close')" @click="layout.toggleRight()">
          <LuPanelRight :size="13" />
        </UiIconButton>
      </UiTooltip>
    </div>

    <div class="body">
      <div v-if="!activeSession" class="empty">{{ t('noSession') }}</div>

      <template v-else-if="layout.rightTab.value === 'files'">
        <FilePreview v-if="selectedPath" :path="selectedPath" @back="selectedPath = ''" />
        <div v-else-if="treeLoading" class="empty">{{ t('loading') }}</div>
        <div v-else-if="treeError" class="empty err">{{ treeError }}</div>
        <div v-else-if="tree.length === 0" class="empty">{{ t('emptyWorkspace') }}</div>
        <FileTree
          v-else
          class="tree-root"
          :nodes="tree"
          :depth="0"
          :selected="selectedPath"
          :reveal-prefix="selectedPath"
          @select="(n) => (selectedPath = n.path)"
        />
      </template>

      <GitChanges v-else-if="layout.rightTab.value === 'changes'" :workspace="workspace" />

      <HistoryTree v-else-if="layout.rightTab.value === 'tree'" />

      <div v-else class="empty">
        <p class="hint">{{ t('terminalHint') }}</p>
      </div>
    </div>
  </aside>
</template>

<style scoped>
.right-panel {
  display: flex;
  flex-direction: column;
  min-width: 0;
  height: 100%;
  border-left: 1px solid var(--line);
  background: var(--surface);
}
.tabs {
  display: flex;
  align-items: center;
  gap: 2px;
  height: 40px;
  flex-shrink: 0;
  padding: 0 6px;
  border-bottom: 1px solid var(--line);
  /* 窄面板下四个标签会超出宽度：让标签条自己横向滚动，动作按钮仍钉在右侧 */
  overflow-x: auto;
  scrollbar-width: none;
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 28px;
  padding: 0 9px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--muted);
  font: inherit;
  font-size: 12.5px;
  flex-shrink: 0;
  cursor: pointer;
}
.tab:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.tab.active {
  background: var(--surface-active);
  color: var(--ink);
}
/*
 * 动作按钮靠右：`.tabs` 的 flex 项是 Tooltip 的包裹元素而不是按钮本身，
 * auto 外边距必须加在包裹元素上——加在 `.tab-action` 上不参与父级布局，
 * 两个按钮会紧跟在标签后面、右侧留出一大片空白。
 */
.tabs :deep(.ui-tooltip-anchor) {
  margin-left: auto;
}
/* 第二个动作紧随第一个，不重复撑开 */
.tabs :deep(.ui-tooltip-anchor + .ui-tooltip-anchor) {
  margin-left: 0;
}
.tab-close {
  width: 26px !important;
  height: 26px !important;
}
.tab-action {
  width: 26px !important;
  height: 26px !important;
}
.body {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
/* 树自己滚，预览标题栏需要固定在顶部 */
.body:has(.tree-root) {
  overflow: auto;
}
.empty {
  padding: 16px;
  color: var(--muted);
  font-size: 13px;
}
.empty.err {
  color: var(--danger);
}
.hint {
  margin: 0 0 8px;
}
.path {
  display: block;
  color: var(--text-secondary);
  font-size: 12px;
  word-break: break-all;
}
</style>
