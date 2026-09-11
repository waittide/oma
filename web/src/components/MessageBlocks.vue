<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
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
import { imageSrc } from '../lib/attachments';
import { currentSessionId } from '../stores/chat';
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
  /** 思考块正在流式输出（位于末尾）：未手动操作时默认展开 */
  active?: boolean;
  text?: string;
  thinking?: string;
  imageSrc?: string;
  toolName?: string;
  toolInput?: unknown;
  /** 工具结果文本；命名与后端 ToolOutput.output / ToolCallFinished.output 对齐 */
  output?: string;
  resultError?: boolean;
  resultDone?: boolean;
}

/** 把 tool_use 与其 tool_result 合并成单个条目；tool_result 不单独渲染。 */
const items = computed<Item[]>(() => {
  const out: Item[] = [];
  const toolIndex: Record<string, number> = {};
  const live = props.streaming;
  props.blocks.forEach((b, i) => {
    if (b.type === 'text') out.push({ kind: 'text', key: `t${i}`, text: b.text });
    else if (b.type === 'thinking') {
      const it: Item = { kind: 'thinking', key: `h${i}`, thinking: b.thinking };
      // 仅流式轮次末尾、仍在增长中的思考块视为 active
      it.active = live && i === props.blocks.length - 1;
      out.push(it);
    }
    else if (b.type === 'image')
      out.push({ kind: 'image', key: `i${i}`, imageSrc: imageSrc(currentSessionId(), b.data) });
    else if (b.type === 'tool_use') {
      toolIndex[b.id] = out.length;
      const carried = props.results?.[b.id];
      out.push({
        kind: 'tool',
        key: b.id,
        toolName: b.name,
        toolInput: b.input,
        output: carried?.content,
        resultError: carried?.is_error,
        resultDone: carried !== undefined,
      });
    } else {
      const idx = toolIndex[b.tool_use_id];
      if (idx !== undefined) {
        const target = out[idx]!;
        target.output = b.content;
        target.resultError = b.is_error;
        target.resultDone = true;
      }
    }
  });
  return out;
});

// ---------- 自绘折叠（替代原生 details/summary，保证跨浏览器一致） ----------
// manual 记录用户显式开合；未操作过的思考块按 active 自动展开/折叠
const manual = ref<Record<string, boolean>>({});
function toggleFold(it: Item) {
  const open = !isOpen(it);
  manual.value[it.key] = open;
  // 重新展开正在输出的思考块时，先回到最新内容再继续跟随
  if (open && it.kind === 'thinking' && it.active) {
    following.value[it.key] = true;
    void nextTick(() => syncFollow());
  }
}
function isOpen(it: Item): boolean {
  return manual.value[it.key] ?? !!it.active;
}

// ---------- 思考内容跟随滚动：内容超出折叠体高度时贴底显示最新内容 ----------

/** 每个折叠体的滚动容器（流式思考正文） */
const bodyEls: Record<string, HTMLElement | null> = {};
function setBodyEl(key: string, el: unknown) {
  const node = (el ?? null) as HTMLElement | null;
  if (node) bodyEls[key] = node;
  else delete bodyEls[key];
}

/** 仅记录被显式上滚过的折叠体；未记录者视为跟随中 */
const following = ref<Record<string, boolean>>({});

function onFoldScroll(key: string) {
  const el = bodyEls[key];
  if (el) following.value[key] = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
}

/** 内容增长后，仅让「仍在输出且未手动上滚」的折叠体保持贴底。 */
function syncFollow() {
  for (const it of items.value) {
    if (it.kind !== 'thinking' || !it.active || !isOpen(it)) continue;
    if (following.value[it.key] === false) continue;
    const el = bodyEls[it.key];
    if (el) el.scrollTop = el.scrollHeight;
  }
}

/** 当前正在输出的思考块：其正文长度变化时触发跟随。 */
const activeThinking = computed(() => {
  const last = items.value[items.value.length - 1];
  return last?.kind === 'thinking' && last.active ? last : null;
});

watch(
  () => [activeThinking.value?.key ?? '', activeThinking.value?.thinking?.length ?? 0],
  () => void nextTick(syncFollow),
);

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
      <div v-else-if="it.kind === 'thinking'" class="fold" :class="{ open: isOpen(it) }">
        <button type="button" class="fold-head think" @click="toggleFold(it)">
          <LuBrain :size="13" />
          <span class="fold-title">{{ t('thinking') }}</span>
          <LuChevronRight :size="13" class="caret" />
        </button>
        <pre
          v-show="isOpen(it)"
          :ref="(el) => setBodyEl(it.key, el)"
          class="fold-body"
          @scroll.passive="onFoldScroll(it.key)"
        >{{ it.thinking }}</pre>
      </div>

      <img v-else-if="it.kind === 'image' && it.imageSrc" class="att" :src="it.imageSrc" alt="attachment" />

      <div v-else-if="it.kind === 'tool'" class="fold" :class="{ open: isOpen(it), error: it.resultDone && it.resultError }">
        <button type="button" class="fold-head" @click="toggleFold(it)">
          <component :is="toolIcon(it.toolName)" :size="13" class="tool-icon" />
          <span class="fold-title">{{ it.toolName }}</span>
          <span v-if="toolSubtitle(it)" class="fold-sep">·</span>
          <span v-if="toolSubtitle(it)" class="fold-sub">{{ toolSubtitle(it) }}</span>
          <span v-if="!it.resultDone" class="tstatus running">{{ t('running') }}</span>
          <span v-else-if="it.resultError" class="tstatus err">{{ t('failed') }}</span>
          <span v-else class="tstatus ok">{{ t('done') }}</span>
          <LuChevronRight :size="13" class="caret" />
        </button>
        <div v-show="isOpen(it)" class="fold-body-wrap">
          <pre class="fold-body">{{ prettyJson(it.toolInput) }}</pre>
          <pre v-if="it.resultDone" class="fold-body result" :class="{ err: it.resultError }">{{ it.output }}</pre>
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
.md {
  font-size: 14px;
  line-height: 1.6;
  overflow-wrap: break-word;
}
.md :deep(p) {
  margin: 0 0 12px;
}
.md :deep(p:last-child) {
  margin-bottom: 0;
}
.md :deep(h1) {
  margin: 28px 0 12px;
  font-size: 17px;
  font-weight: 600;
  line-height: 20px;
}
.md :deep(h2) {
  margin: 24px 0 10px;
  font-size: 15px;
  font-weight: 600;
  line-height: 20px;
}
.md :deep(h3),
.md :deep(h4),
.md :deep(h5),
.md :deep(h6) {
  margin: 20px 0 8px;
  font-size: 13px;
  font-weight: 500;
  line-height: 20px;
}
.md :deep(h4),
.md :deep(h5),
.md :deep(h6) {
  color: var(--text-tertiary);
}
.md :deep(h1:first-child),
.md :deep(h2:first-child),
.md :deep(h3:first-child) {
  margin-top: 0;
}
.md :deep(ul),
.md :deep(ol) {
  margin: 0 0 12px;
  padding-left: 22px;
}
.md :deep(li) {
  margin: 3px 0;
}
.md :deep(li > p) {
  margin-bottom: 4px;
}
.md :deep(hr) {
  border: none;
  border-top: 1px solid var(--line);
  margin: 16px 0;
}
.md :deep(pre) {
  margin: 8px 0 12px;
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
  padding: 2px 5px;
}
.md :deep(pre code) {
  background: transparent;
  padding: 0;
}
.md :deep(a) {
  color: var(--accent);
}
.md :deep(blockquote) {
  margin: 8px 0 12px;
  padding: 2px 12px;
  border-left: 3px solid var(--surface2);
  color: var(--text-tertiary);
}
.md :deep(table) {
  border-collapse: collapse;
  font-size: 12.5px;
  margin: 0 0 12px;
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
  /* 滚动到底时不再把滚动链传给外层消息流 */
  overscroll-behavior: contain;
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
