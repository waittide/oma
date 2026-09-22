<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue';
import {
  LuBot,
  LuBrain,
  LuChevronRight,
  LuFileDiff,
  LuGlobe,
  LuSearch,
  LuTerminalSquare,
  LuWrench,
} from 'vue-icons-plus/lu';
import type { Block } from '../types';
import { UiButton } from '@waittide/ui';
import { prettyJson, renderMarkdown } from '../lib/format';
import { enhanceCodeBlocks } from '../lib/codeCopy';
import { imageSrc } from '../lib/attachments';
import { currentSessionId, toolDurations } from '../stores/chat';
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
// manual 记录用户显式开合；未操作过的块按 active 自动展开/折叠
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

/**
 * 折叠块是否需要自动展开（流式中的思考块）。
 */
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

/**
 * 补丁里的文件头（`*** Update File: <path>` / `*** Add File:` / `*** Delete File:`）。
 *
 * 带补丁文本的工具（oh-my-pi 的 apply_patch 形式，`edit` 即其一）入参就是一段补丁原文，
 * 文件头里的路径正是被改文件，可直接当副标题用。
 */
const PATCH_FILES_RE = /^\*\*\* (?:Update|Add|Delete) File: (.+)$/gm;

/** 工具入参的展示文本：补丁原文原样展示，其余按 JSON 美化。 */
function toolCode(input: unknown): string {
  if (input && typeof input === 'object') {
    const patch = (input as Record<string, unknown>).input;
    // 补丁里全是换行，序列化成 JSON 只会得到一坨 `\n` 转义符
    if (typeof patch === 'string') return patch;
  }
  return prettyJson(input);
}

function toolSubtitle(it: Item): string {
  const input = (it.toolInput ?? {}) as Record<string, unknown>;
  const patch = typeof input.input === 'string' ? input.input : '';
  const files = [...patch.matchAll(PATCH_FILES_RE)].map((m) => m[1] ?? '');
  const raw =
    input.command ??
    input.file_path ??
    input.path ??
    // 一个 envelope 可落在多个文件上：只报首个路径会误导，后面跟上其余文件数
    (files.length > 1 ? `${files[0]} +${files.length - 1}` : files[0]) ??
    input.pattern ??
    input.query ??
    input.prompt ??
    '';
  const text = String(raw).replace(/\s+/g, ' ').trim();
  return text.length > 64 ? `${text.slice(0, 64)}…` : text;
}

/** 工具执行耗时（秒）；服务端不记录，仅本次连接内采集到的调用有值 */
function toolDuration(it: Item): string {
  const seconds = toolDurations.value[it.key];
  if (seconds === undefined) return '';
  return `${seconds}s`;
}

/**
 * 去掉 Markdown 标记，用于折叠态的一行预览。
 *
 * 折叠时只需要“像内容的一句话”，保留符号反而徒增噪声。
 */
function stripMarkdown(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`([^`]*)`/g, '$1')
    .replace(/!\[[^\]]*\]\([^)]*\)/g, ' ')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/^\s{0,3}#{1,6}\s+/gm, '')
    .replace(/^\s{0,3}>\s?/gm, '')
    .replace(/^\s*[-*+]\s+/gm, '')
    .replace(/[*_~]/g, '')
    .replace(/\s+/g, ' ')
    .trim();
}

/** 思考块的折叠态预览（与 pi-web 一致：单行、去标记、截断） */
function thinkingPreview(it: Item, max = 96): string {
  const text = stripMarkdown(it.thinking ?? '');
  return text.length > max ? `${text.slice(0, max)}…` : text;
}

// ---------- 代码块右上角复制按钮 ----------
// v-html 注入的 DOM 里挂不了组件，因此在每次渲染后由脚本补按钮（幂等）
const blocksRoot = ref<HTMLElement | null>(null);

function syncCodeCopy() {
  enhanceCodeBlocks(blocksRoot.value);
}

// 内容或流式状态一变就补：新增的代码块要跟上，已处理过的会自行跳过
watch(
  () => [props.blocks, props.streaming, manual.value] as const,
  () => void nextTick(syncCodeCopy),
  { deep: true },
);

onMounted(() => void nextTick(syncCodeCopy));
</script>

<template>
  <div ref="blocksRoot" class="blocks">
    <template v-for="it in items" :key="it.key">
      <div v-if="it.kind === 'text' && it.text" class="md" v-html="renderMarkdown(it.text)" />
      <div v-else-if="it.kind === 'thinking'" class="fold think" :class="{ open: isOpen(it) }">
        <UiButton variant="ghost" tone="neutral" block class="fold-head think" @click="toggleFold(it)">
          <LuBrain :size="13" class="think-icon" />
          <span v-if="isOpen(it)" class="fold-title">{{ t('thinking') }}</span>
          <span v-else class="fold-preview">{{ thinkingPreview(it) || t('thinking') }}</span>
          <LuChevronRight :size="13" class="caret" />
        </UiButton>
        <div
          v-show="isOpen(it)"
          :ref="(el) => setBodyEl(it.key, el)"
          class="fold-body think-body md"
          @scroll.passive="onFoldScroll(it.key)"
          v-html="renderMarkdown(it.thinking ?? '')"
        />
      </div>

      <img v-else-if="it.kind === 'image' && it.imageSrc" class="att" :src="it.imageSrc" alt="attachment" />

      <div v-else-if="it.kind === 'tool'" class="fold tool" :class="{ open: isOpen(it), error: it.resultDone && it.resultError }">
        <UiButton variant="ghost" tone="neutral" block class="fold-head tool-head" @click="toggleFold(it)">
          <component :is="toolIcon(it.toolName)" :size="13" class="tool-icon" />
          <span class="tool-name">{{ it.toolName }}</span>
          <span v-if="toolSubtitle(it)" class="fold-sub">{{ toolSubtitle(it) }}</span>
          <span v-if="!it.resultDone" class="tstatus running">{{ t('running') }}</span>
          <span v-else-if="toolDuration(it)" class="tool-dur">{{ toolDuration(it) }}</span>
          <LuChevronRight :size="13" class="caret" />
        </UiButton>
        <div v-show="isOpen(it)" class="fold-body-wrap">
          <pre class="fold-body">{{ toolCode(it.toolInput) }}</pre>
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

/* 代码块 + 右上角复制按钮：pre 自身仍负责横向滚动，
   按钮挂在外层包裹上才不会随代码横向滚动而跑掉 */
.blocks :deep(.code-block) {
  position: relative;
}
.blocks :deep(.code-copy) {
  position: absolute;
  top: 6px;
  right: 6px;
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  padding: 0;
  border: 1px solid transparent;
  border-radius: 6px;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
  /* 默认淡出，鼠标进入代码块才显现，不干扰阅读 */
  opacity: 0;
  transition:
    opacity 0.15s ease,
    color 0.12s ease,
    background-color 0.12s ease,
    border-color 0.12s ease;
}
.blocks :deep(.code-block:hover .code-copy),
.blocks :deep(.code-copy:focus-visible) {
  opacity: 1;
}
.blocks :deep(.code-copy:hover) {
  background: var(--surface-hover);
  border-color: var(--line);
  color: var(--ink);
}
.blocks :deep(.code-copy.copied) {
  opacity: 1;
  color: var(--success);
}

/* 折叠行：无边框扁平形态（参照 opencode basic-tool）。
   自身不留纵向内边距——容器的 8px 间距就是它到相邻块的全部距离，
   连续的工具调用/思考才会与正文保持同一行距（对齐 pi-web 的块间距） */
.fold {
  display: flex;
  flex-direction: column;
}
/*
 * 工具调用失败：整块换成危险色底 + 描边（对齐 pi-web 的 `isError` 卡片），
 * 不再用左侧一条竖线——竖线既占掉了内容的起点，又和消息列左侧对不齐。
 */
.fold.error {
  /* 负数左右外边距抵消内边距：底色有呼吸感，内容仍与正文左对齐；
     纵向不留内边距，失败卡片的行距与普通折叠行一致 */
  margin: 0 -7px;
  padding: 0 6px;
  border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
  border-radius: 6px;
  background: color-mix(in srgb, var(--danger) 7%, transparent);
}
/* 折叠头：纯文本行，压到 20px 高。
   `.blocks` 前缀是为了压过组件库的 `.ui-button--md[data-v-*]`（同为 0-2-0，而库的
   样式表在开发模式下注入得更晚，同特异性时它会赢） */
.blocks .fold-head {
  display: flex;
  align-items: center;
  gap: 6px;
  height: auto;
  min-height: 20px;
  width: 100%;
  border: none;
  background: transparent;
  padding: 0;
  font-family: inherit;
  color: var(--text-secondary);
  cursor: pointer;
  text-align: left;
  user-select: none;
}
/* 组件库按钮的默认内边距/悬停底色不适用于折叠头，这里还原为纯文本行 */
.fold-head:hover {
  background: transparent;
}
.fold-head :deep(.ui-button__label) {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex: 1;
  min-width: 0;
}
.fold-head:hover .fold-title {
  color: var(--ink);
}
.fold.think .fold-head {
  color: var(--mauve);
}
/* 折叠态的思考预览：一行灰字，与 pi-web 的紧凑折叠条一致 */
.fold-preview {
  flex: 1;
  min-width: 0;
  font-size: 12px;
  color: var(--overlay1);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.think-icon {
  flex-shrink: 0;
}
.tool-name {
  font-size: 12.5px;
  font-weight: 600;
  font-family: var(--font-mono);
  color: var(--text-secondary);
  flex-shrink: 0;
}
.fold.tool.error .tool-name {
  color: var(--danger);
}
/* 工具耗时：紧贴折叠箭头，等宽数字避免行内跳动 */
.tool-dur {
  margin-left: auto;
  flex-shrink: 0;
  font-size: 11px;
  color: var(--overlay1);
  font-variant-numeric: tabular-nums;
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
  color: var(--overlay0);
  flex-shrink: 0;
  transition: transform 0.15s ease;
}
.fold.open .caret {
  transform: rotate(90deg);
}
.tstatus {
  /* 推到最右，紧贴折叠箭头：无副标题的工具（如 ask）才与有副标题的一致；
     auto 只能留在这里，若箭头也带 auto 会把空白平分，反而回不去最右 */
  margin-left: auto;
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
  /* 不再左侧缩进：输入/结果块与正文左对齐，否则整块看着比模型回复凹进去 */
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
/* 思考正文：与正文一样按 Markdown 渲染，但整体压一档、默认偏弱色。
   不套底板也不留左右内边距——思考内容要与模型回复左对齐 */
.think-body {
  padding: 4px 0 0;
  background: transparent;
  font-family: var(--font-sans);
  font-size: 12.5px;
  line-height: 1.65;
  /* 折叠体默认 pre-wrap；Markdown 渲染后的 HTML 若沿用会在块级标签之间
     留下源码换行，多出空白行 */
  white-space: normal;
}
.think-body :deep(p) {
  margin: 0 0 8px;
}
.think-body :deep(p:last-child) {
  margin-bottom: 0;
}
/* 思考内容里的代码块/行内代码沿用正文样式，不再叠一层内边距 */
.think-body :deep(pre) {
  margin: 6px 0 8px;
  padding: 0;
  background: transparent;
  font-size: 11.5px;
  overflow-x: auto;
}
.think-body :deep(pre code) {
  padding: 0;
  background: transparent;
}
.think-body :deep(code) {
  font-size: 11.5px;
  background: color-mix(in srgb, var(--surface2) 55%, transparent);
}
.think-body :deep(ul),
.think-body :deep(ol) {
  margin: 0 0 8px;
  padding-left: 20px;
}
.think-body :deep(li) {
  margin: 2px 0;
}
.think-body :deep(h1),
.think-body :deep(h2),
.think-body :deep(h3),
.think-body :deep(h4),
.think-body :deep(h5),
.think-body :deep(h6) {
  margin: 10px 0 6px;
  font-size: 12.5px;
  font-weight: 600;
}
.think-body :deep(blockquote) {
  margin: 6px 0 8px;
  padding: 0 10px;
  border-left: 2px solid var(--surface2);
}
.think-body :deep(table) {
  border-collapse: collapse;
  font-size: 11.5px;
  margin: 0 0 8px;
}
.think-body :deep(th),
.think-body :deep(td) {
  border: 1px solid var(--line);
  padding: 3px 8px;
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
