<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { UiButton, UiIconButton, UiTooltip } from '@waittide/ui';
import { LuArrowLeft, LuRefreshCw } from 'vue-icons-plus/lu';
import { api } from '../api';
import type { GitFileChange } from '../types';
import { useTranslations } from '../composables/i18n';

/**
 * Git 变更标签页：当前分支 + 变更文件清单，点开看相对 HEAD 的 diff。
 *
 * 只读展示：暂存/提交这类写操作留给用户在终端里做——这里的职责是「看一眼」，
 * 多一条写路径就多一份把用户仓库改坏的风险。
 */
const props = defineProps<{ workspace: string }>();
const { t } = useTranslations('panel');

const loading = ref(false);
const error = ref('');
const branch = ref('');
const files = ref<GitFileChange[]>([]);
const selected = ref('');

const diffLoading = ref(false);
const diffError = ref('');
const diff = ref('');

async function load() {
  const ws = props.workspace;
  if (!ws) {
    files.value = [];
    return;
  }
  loading.value = true;
  error.value = '';
  selected.value = '';
  diff.value = '';
  try {
    const status = await api.gitStatus(ws);
    branch.value = status.branch;
    files.value = status.files;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
    files.value = [];
  } finally {
    loading.value = false;
  }
}

async function openDiff(path: string) {
  selected.value = path;
  diffLoading.value = true;
  diffError.value = '';
  diff.value = '';
  try {
    const resp = await api.gitDiff(props.workspace, path);
    diff.value = resp.diff;
  } catch (e) {
    diffError.value = e instanceof Error ? e.message : String(e);
  } finally {
    diffLoading.value = false;
  }
}

/** porcelain 的两列状态合成一个可读标签：优先显示「未跟踪/新增/删除/改名」，否则「已修改」。 */
function statusLabel(file: GitFileChange): string {
  if (file.untracked) return t('gitUntracked');
  const codes = `${file.index}${file.worktree}`;
  if (codes.includes('A')) return t('gitAdded');
  if (codes.includes('D')) return t('gitDeleted');
  if (codes.includes('R')) return t('gitRenamed');
  if (codes.includes('M')) return t('gitModified');
  return codes.trim() || '·';
}

/** diff 行着色：新增/删除/文件头，其余按上下文。 */
function diffLineClass(line: string): string {
  if (line.startsWith('+++') || line.startsWith('---')) return 'meta';
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('diff ') || line.startsWith('index ')) return 'meta';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return 'ctx';
}

const diffLines = computed(() => (diff.value ? diff.value.replace(/\n$/, '').split('\n') : []));

watch(() => props.workspace, load, { immediate: true });
</script>

<template>
  <div class="changes">
    <div class="bar">
      <UiTooltip v-if="selected" :content="t('backToList')" align="start">
        <UiIconButton size="sm" :label="t('backToList')" @click="selected = ''">
          <LuArrowLeft :size="13" />
        </UiIconButton>
      </UiTooltip>
      <span v-if="selected" class="name" :title="selected">{{ selected }}</span>
      <span v-else-if="branch" class="branch">{{ branch }}</span>
      <span v-else class="branch">{{ t('changes') }}</span>
      <span class="spacer" />
      <UiTooltip :content="t('reload')" align="end">
        <UiIconButton size="sm" :label="t('reload')" @click="selected ? openDiff(selected) : load()">
          <LuRefreshCw :size="13" />
        </UiIconButton>
      </UiTooltip>
    </div>

    <!-- diff 视图 -->
    <template v-if="selected">
      <div v-if="diffLoading" class="hint">{{ t('loading') }}</div>
      <div v-else-if="diffError" class="hint err">{{ diffError }}</div>
      <div v-else-if="diffLines.length === 0" class="hint">{{ t('gitNoDiff') }}</div>
      <div v-else class="diff">
        <div v-for="(line, i) in diffLines" :key="i" class="diff-row" :class="diffLineClass(line)">
          {{ line || ' ' }}
        </div>
      </div>
    </template>

    <!-- 文件清单 -->
    <template v-else>
      <div v-if="loading" class="hint">{{ t('loading') }}</div>
      <div v-else-if="error" class="hint err">
        <p>{{ error }}</p>
        <UiButton variant="soft" tone="neutral" size="sm" @click="load">{{ t('reload') }}</UiButton>
      </div>
      <div v-else-if="files.length === 0" class="hint">{{ t('gitClean') }}</div>
      <ul v-else class="list">
        <li v-for="file in files" :key="file.path">
          <button type="button" class="row" :title="file.path" @click="openDiff(file.path)">
            <span class="badge" :class="{ untracked: file.untracked }">{{ statusLabel(file) }}</span>
            <span class="path">{{ file.path }}</span>
          </button>
        </li>
      </ul>
    </template>
  </div>
</template>

<style scoped>
.changes {
  display: flex;
  flex-direction: column;
  min-height: 0;
  height: 100%;
}
.bar {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
  padding: 6px 8px;
  border-bottom: 1px solid var(--line);
}
.branch {
  font-size: 12.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.name {
  font-family: var(--font-mono);
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.spacer {
  flex: 1;
}
.hint {
  padding: 14px;
  font-size: 12.5px;
  color: var(--muted);
}
.hint.err {
  color: var(--danger);
}
.list {
  list-style: none;
  margin: 0;
  padding: 4px 0;
}
.row {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 3px 10px;
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
.badge {
  flex-shrink: 0;
  min-width: 38px;
  padding: 1px 5px;
  border-radius: 4px;
  background: color-mix(in srgb, var(--warning) 18%, transparent);
  color: var(--warning);
  font-size: 10.5px;
  text-align: center;
}
.badge.untracked {
  background: color-mix(in srgb, var(--success) 18%, transparent);
  color: var(--success);
}
.path {
  font-family: var(--font-mono);
  font-size: 11.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.diff {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 6px 0;
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.55;
  overscroll-behavior: contain;
}
.diff-row {
  padding: 0 10px;
  white-space: pre;
}
/* 行底色沿用调色板的语义色，深浅两套主题下都能读 */
.diff-row.add {
  background: color-mix(in srgb, var(--success) 14%, transparent);
  color: var(--success);
}
.diff-row.del {
  background: color-mix(in srgb, var(--danger) 14%, transparent);
  color: var(--danger);
}
.diff-row.hunk {
  color: var(--accent);
}
.diff-row.meta {
  color: var(--overlay0);
}
.diff-row.ctx {
  color: var(--text-secondary);
}
</style>
