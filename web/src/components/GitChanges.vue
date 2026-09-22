<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { UiButton, UiIconButton, UiSegmented, UiTooltip } from '@waittide/ui';
import {
  LuArrowLeft,
  LuChevronDown,
  LuChevronRight,
  LuList,
  LuListTree,
  LuRefreshCw,
} from 'vue-icons-plus/lu';
import { api } from '../api';
import type { GitFileChange } from '../types';
import { useTranslations } from '../composables/i18n';
import * as layout from '../stores/layout';
import { workspaceRevision } from '../stores/workspaceSync';
import { buildChangeRows, changeRowIndent } from '../lib/changesTree';
import { changeStatus, type ChangeStatus } from '../lib/gitStatus';

/**
 * Git 变更标签页：当前分支 + 变更文件清单，点开看相对 HEAD 的 diff。
 *
 * 只读展示：暂存/提交这类写操作留给用户在终端里做——这里的职责是「看一眼」，
 * 多一条写路径就多一份把用户仓库改坏的风险。
 *
 * 展示形式可切换：**列表**（扁平清单、显示完整路径）或**树**（按目录归组、目录可折叠）。
 * 偏好落在 `layout` store 里全局持久化；树只由前端对已有路径分组，后端接口不变。
 *
 * 状态不用徽标，直接染文件名：绿=创建、黄=修改（含改名）、红=删除。
 * 具体状态仍以 tooltip 文字给出，颜色不承担全部信息。
 *
 * 数据是磁盘快照，服务端不推送：窗口重新获得焦点、或 agent 用写类工具改过文件时，
 * 订阅 `workspaceRevision` 静默重取（见 `refreshQuiet`）。
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

/** 树视图里已折叠的目录（按相对路径记）；取不到就整体展开 */
const collapsedDirs = ref<Set<string>>(new Set());

const viewOptions = computed(() => [
  { value: 'list', label: t('viewList'), icon: LuList },
  { value: 'tree', label: t('viewTree'), icon: LuListTree },
]);

async function load() {
  const ws = props.workspace;
  if (!ws) {
    files.value = [];
    return;
  }
  loading.value = true;
  error.value = '';
  // 手动/首次读取是「重新开始看」：关掉正在看的 diff，回到清单
  selected.value = '';
  diff.value = '';
  // 重新读取后目录结构与上次可能完全不同，折叠状态一并重置为全展开
  collapsedDirs.value = new Set();
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

/**
 * 静默刷新：后台发现工作区变了时更新清单，但不打断正在阅读的 diff。
 *
 * 不能直接调 `load()`——它会清掉 `selected`，把用户从 diff 里踢回清单。
 * 只有该文件确实已不在变更中（被提交或撤回）时才关掉 diff，否则那内容已经是错的。
 * 失败时不动界面：后台刷新出错不该把用户正在看的清单换成一条报错，手动刷新仍在。
 */
async function refreshQuiet() {
  const ws = props.workspace;
  if (!ws) return;
  try {
    const status = await api.gitStatus(ws);
    branch.value = status.branch;
    files.value = status.files;
    error.value = '';
    if (selected.value && !status.files.some((f) => f.path === selected.value)) {
      selected.value = '';
      diff.value = '';
    }
  } catch {
    // 忽略：保留界面上已有的内容
  }
}

/**
 * 语义状态 → 文件名颜色类。
 *
 * 五态收成三色：未跟踪与新增同为「创建」（绿），改名归入「修改」（黄）。
 */
const STATUS_TONE: Record<ChangeStatus, string> = {
  untracked: 'created',
  added:     'created',
  modified:  'modified',
  renamed:   'modified',
  deleted:   'deleted',
};

/** 语义状态 → 文案键：颜色之外仍把具体状态写进 tooltip，供悬停与无障碍读取 */
const STATUS_KEY: Record<ChangeStatus, string> = {
  untracked: 'gitUntracked',
  added:     'gitAdded',
  modified:  'gitModified',
  renamed:   'gitRenamed',
  deleted:   'gitDeleted',
};

function statusTone(file: GitFileChange): string {
  return STATUS_TONE[changeStatus(file)];
}

function statusText(file: GitFileChange): string {
  return t(STATUS_KEY[changeStatus(file)]);
}

/** 行的 tooltip：完整路径 + 状态。树里只显示末段名，路径得靠它补全。 */
function rowTitle(file: GitFileChange): string {
  return `${file.path} · ${statusText(file)}`;
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

/** 变更视图的切换：`UiSegmented` 抛出 string | number，这里收敛回字面量类型 */
function onViewChange(value: string | number) {
  layout.setChangesView(value === 'tree' ? 'tree' : 'list');
}

function toggleDir(path: string) {
  const next = new Set(collapsedDirs.value);
  if (next.has(path)) next.delete(path);
  else next.add(path);
  collapsedDirs.value = next;
}

/** 树视图的待渲染行（构建逻辑在 lib 中，见 changesTree.check.ts） */
const rows = computed(() => buildChangeRows(files.value, collapsedDirs.value));

watch(() => props.workspace, load, { immediate: true });

// 工作区可能已变化（窗口重新获得焦点、agent 用写类工具改过文件）时静默重取
watch(workspaceRevision, () => void refreshQuiet());
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
      <UiSegmented
        v-if="!selected && files.length > 0"
        class="view-toggle"
        size="sm"
        :model-value="layout.changesView.value"
        :options="viewOptions"
        :label="t('viewLabel')"
        @update:model-value="onViewChange"
      />
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

      <!-- 列表：扁平清单，路径完整显示 -->
      <ul v-else-if="layout.changesView.value === 'list'" class="list">
        <li v-for="file in files" :key="file.path">
          <button type="button" class="row" :title="rowTitle(file)" @click="openDiff(file.path)">
            <span class="path" :class="statusTone(file)">{{ file.path }}</span>
          </button>
        </li>
      </ul>

      <!-- 树：按目录归组、目录可折叠；树靠层级表达位置，只显示末段名称 -->
      <div v-else class="tree">
        <template v-for="row in rows" :key="row.path">
          <button
            v-if="row.kind === 'dir'"
            type="button"
            class="row dir"
            :style="{ paddingLeft: `${changeRowIndent(row)}px` }"
            :title="row.path"
            :aria-expanded="!row.collapsed"
            @click="toggleDir(row.path)"
          >
            <span class="chev">
              <component :is="row.collapsed ? LuChevronRight : LuChevronDown" :size="12" />
            </span>
            <span class="name">{{ row.name }}</span>
            <span class="count">{{ row.count }}</span>
          </button>
          <button
            v-else
            type="button"
            class="row"
            :style="{ paddingLeft: `${changeRowIndent(row)}px` }"
            :title="rowTitle(row.file)"
            @click="openDiff(row.file.path)"
          >
            <!-- 与目录行的展开箭头同宽，保证同级名称左对齐 -->
            <span class="chev" />
            <span class="name" :class="statusTone(row.file)">{{ row.name }}</span>
          </button>
        </template>
      </div>
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
.path {
  /* 列表行里唯一的子元素：撑满后超长路径才会出省略号，而不是溢出到行外 */
  flex: 1;
  min-width: 0;
  font-family: var(--font-mono);
  font-size: 11.5px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
/* 状态直接染文件名：绿=创建、黄=修改（含改名）、红=删除。
   颜色只落在名字上，目录与计数保持中性，避免整屏花掉 */
.path.created,
.name.created {
  color: var(--success);
}
.path.modified,
.name.modified {
  color: var(--warning);
}
.path.deleted,
.name.deleted {
  color: var(--danger);
}
/* 树：目录行与文件行同构，层级只靠内联 padding-left 拉开 */
.tree {
  display: flex;
  flex-direction: column;
  padding: 4px 0;
}
.tree .row {
  gap: 6px;
}
/* 箭头槽：文件行放一个等宽占位，同级名称才能左对齐 */
.chev {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  flex-shrink: 0;
  color: var(--muted);
}
.tree .name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-mono);
  font-size: 11.5px;
}
/* 目录的变更文件数：等宽数字，折叠与否一目了然 */
.count {
  flex-shrink: 0;
  font-size: 10.5px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
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
