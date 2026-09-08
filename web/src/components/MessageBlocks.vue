<script setup lang="ts">
import { computed, ref } from 'vue';
import {
  LuBot,
  LuBrain,
  LuChevronRight,
  LuFileDiff,
  LuFileText,
  LuGlobe,
  LuSearch,
  LuTerminalSquare,
  LuWrench,
} from 'vue-icons-plus/lu';
import type { Block } from '../types';
import { prettyJson, renderMarkdown } from '../lib/format';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{
  blocks: Block[];
  streaming: boolean;
  /** 跨消息的 tool_use_id → 结果映射（重载后完成态） */
  results?: Record<string, { content: string; is_error: boolean }>;
}>();

const { t } = useTranslations('blocks');

interface Item {
  kind: 'text' | 'thinking' | 'tool' | 'image';
  key: string;
  text?: string;
  thinking?: string;
  imageSrc?: string;
  toolName?: string;
  toolInput?: unknown;
  resultContent?: string;
  resultError?: boolean;
  resultDone?: boolean;
}

function imageSrc(data: string): string | undefined {
  if (data.startsWith('data:') || data.startsWith('http')) return data;
  return undefined;
}

/** 把 tool_use 与其 tool_result 合并成单个条目；tool_result 不单独渲染。 */
const items = computed<Item[]>(() => {
  const out: Item[] = [];
  const toolIndex: Record<string, number> = {};
  props.blocks.forEach((b, i) => {
    if (b.type === 'text') out.push({ kind: 'text', key: `t${i}`, text: b.text });
    else if (b.type === 'thinking') out.push({ kind: 'thinking', key: `h${i}`, thinking: b.thinking });
    else if (b.type === 'image') out.push({ kind: 'image', key: `i${i}`, imageSrc: imageSrc(b.data) });
    else if (b.type === 'tool_use') {
      toolIndex[b.id] = out.length;
      const carried = props.results?.[b.id];
      out.push({
        kind: 'tool',
        key: b.id,
        toolName: b.name,
        toolInput: b.input,
        resultContent: carried?.content,
        resultError: carried?.is_error,
        resultDone: carried !== undefined,
      });
    } else {
      const idx = toolIndex[b.tool_use_id];
      if (idx !== undefined) {
        const target = out[idx]!;
        target.resultContent = b.content;
        target.resultError = b.is_error;
        target.resultDone = true;
      }
    }
  });
  return out;
});

// ---------- 自绘折叠（替代原生 details/summary，保证跨浏览器一致） ----------
const openKeys = ref<Record<string, boolean>>({});
function toggleFold(key: string) {
  openKeys.value[key] = !openKeys.value[key];
}
function isOpen(key: string): boolean {
  return !!openKeys.value[key];
}

// ---------- 工具展示元数据：图标 + 副标题（参照 opencode 的 Title · Subtitle 形态） ----------

function toolIcon(name?: string) {
  switch (name) {
    case 'shell':
      return LuTerminalSquare;
    case 'read':
    case 'write':
    case 'edit':
      return LuFileDiff;
    case 'grep':
      return LuSearch;
    case 'web_fetch':
    case 'web_search':
      return LuGlobe;
    case 'task':
      return LuBot;
    default:
      return LuWrench;
  }
}

function toolSubtitle(it: Item): string {
  const input = (it.toolInput ?? {}) as Record<string, unknown>;
  const raw = input.command ?? input.file_path ?? input.path ?? input.pattern ?? input.query ?? input.prompt ?? '';
  const text = String(raw).replace(/\s+/g, ' ').trim();
  return text.length > 64 ? `${text.slice(0, 64)}…` : text;
}
</script>

<template>
  <div class="blocks">
    <template v-for="it in items" :key="it.key">
      <div v-if="it.kind === 'text' && it.text" class="md" v-html="renderMarkdown(it.text)" />

      <div v-else-if="it.kind === 'thinking'" class="fold" :class="{ open: isOpen(it.key) }">
        <button type="button" class="fold-head think" @click="toggleFold(it.key)">
          <LuBrain :size="13" />
          <span class="fold-title">{{ t('thinking') }}</span>
          <LuChevronRight :size="13" class="caret" />
        </button>
        <pre v-show="isOpen(it.key)" class="fold-body">{{ it.thinking }}</pre>
      </div>

      <img v-else-if="it.kind === 'image' && it.imageSrc" class="att" :src="it.imageSrc" alt="attachment" />

      <div
        v-else-if="it.kind === 'tool'"
        class="fold tool"
        :class="{ open: isOpen(it.key), error: it.resultDone && it.resultError }"
      >
        <button type="button" class="fold-head" @click="toggleFold(it.key)">
          <component :is="toolIcon(it.toolName)" :size="13" class="tool-icon" />
          <span class="fold-title">{{ it.toolName }}</span>
          <span v-if="toolSubtitle(it)" class="fold-sep">·</span>
          <span v-if="toolSubtitle(it)" class="fold-sub">{{ toolSubtitle(it) }}</span>
          <span v-if="!it.resultDone" class="tstatus running">{{ t('running') }}</span>
          <span v-else-if="it.resultError" class="tstatus err">{{ t('failed') }}</span>
          <span v-else class="tstatus ok">{{ t('done') }}</span>
          <LuChevronRight :size="13" class="caret" />
        </button>
        <div v-show="isOpen(it.key)" class="fold-body-wrap">
          <pre class="fold-body">{{ prettyJson(it.toolInput) }}</pre>
          <pre v-if="it.resultDone" class="fold-body result" :class="{ err: it.resultError }">{{ it.resultContent }}</pre>
        </div>
      </div>
    </template>
  </div>
</template>

<style scoped>
.blocks {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.md :deep(p) {
  margin: 0 0 8px;
  line-height: 1.7;
}
.md :deep(p:last-child) {
  margin-bottom: 0;
}
.md :deep(pre) {
  margin: 8px 0;
  padding: 12px 14px;
  background: var(--surface);
  border-radius: 8px;
  overflow-x: auto;
  font-size: 12.5px;
  line-height: 1.6;
}
.md :deep(code) {
  font-family: var(--font-mono);
  font-size: 12.5px;
  background: var(--surface);
  border-radius: 4px;
  padding: 1px 5px;
}
.md :deep(pre code) {
  background: transparent;
  padding: 0;
}
.md :deep(a) {
  color: var(--accent);
}
.md :deep(blockquote) {
  margin: 8px 0;
  padding: 2px 12px;
  border-left: 3px solid var(--surface2);
  color: var(--text-tertiary);
}
.md :deep(table) {
  border-collapse: collapse;
  font-size: 12.5px;
}
.md :deep(th),
.md :deep(td) {
  border: 1px solid var(--line);
  padding: 5px 10px;
}

/* 折叠行：无边框扁平形态（参照 opencode basic-tool） */
.fold {
  display: flex;
  flex-direction: column;
  padding: 2px 0;
}
.fold.error {
  border-left: 2px solid var(--danger);
  padding-left: 10px;
}
.fold-head {
  display: flex;
  align-items: center;
  gap: 6px;
  min-height: 20px;
  width: 100%;
  border: none;
  background: transparent;
  padding: 2px 0;
  font-family: inherit;
  color: var(--text-secondary);
  cursor: pointer;
  text-align: left;
  user-select: none;
}
.fold-head:hover .fold-title {
  color: var(--ink);
}
.fold.think .fold-head {
  color: var(--mauve);
}
.tool-icon {
  color: var(--overlay1);
  flex-shrink: 0;
}
.fold-title {
  font-size: 12.5px;
  font-weight: 500;
  font-family: var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.fold-sep {
  font-size: 11px;
  color: var(--overlay1);
}
.fold-sub {
  font-size: 12.5px;
  color: var(--overlay1);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  min-width: 0;
  flex: 1;
}
.caret {
  margin-left: auto;
  color: var(--overlay0);
  flex-shrink: 0;
  transition: transform 0.15s ease;
}
.fold.open .caret {
  transform: rotate(90deg);
}
.tstatus {
  flex-shrink: 0;
  font-size: 11px;
  font-variant-numeric: tabular-nums;
}
.tstatus.ok {
  color: var(--success);
}
.tstatus.err {
  color: var(--danger);
}
.tstatus.running {
  color: var(--warning);
}
.fold-body-wrap {
  display: flex;
  flex-direction: column;
  padding-left: 22px;
}
.fold-body {
  margin: 4px 0 0;
  padding: 8px 11px;
  background: var(--surface);
  border-radius: 6px;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--text-tertiary);
  max-height: 240px;
  overflow-y: auto;
}
.fold-body.result {
  color: var(--text-secondary);
}
.fold-body.result.err {
  color: var(--danger);
}
.att {
  max-width: 320px;
  border-radius: 8px;
}
</style>
