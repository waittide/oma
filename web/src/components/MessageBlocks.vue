<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from 'vue';
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
import { UiButton } from '@waittide/ui';
import { prettyJson, renderMarkdown } from '../lib/format';
import { enhanceCodeBlocks } from '../lib/codeCopy';
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
  /** task 工具：子代理过程块（thinking / 正文 / 嵌套工具调用） */
  subagentBlocks?: Block[];
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
    } else if (b.type === 'subagent') {
      // 子代理过程：并入宿主 task 条目，在卡片内部渲染（不占顶层位置）
      const idx = toolIndex[b.tool_use_id];
      if (idx !== undefined) out[idx]!.subagentBlocks = b.blocks;
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
 * 子代理正在执行（宿主 task 尚未出结果）。
 *
 * 与思考块「active」同思路但判定不同：子代理是否有内容不看位置，
 * 只看宿主任务是否结束——即使期间夹了别的顶层块也不会误判。
 */
function subagentLive(it: Item): boolean {
  return props.streaming && !!it.subagentBlocks && !it.resultDone;
}

function isOpen(it: Item): boolean {
  return manual.value[it.key] ?? (!!it.active || subagentLive(it));
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
      <div v-else-if="it.kind === 'thinking'" class="fold" :class="{ open: isOpen(it) }">
        <UiButton variant="ghost" tone="neutral" block class="fold-head think" @click="toggleFold(it)">
          <LuBrain :size="13" />
          <span class="fold-title">{{ t('thinking') }}</span>
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

      <div v-else-if="it.kind === 'tool'" class="fold" :class="{ open: isOpen(it), error: it.resultDone && it.resultError }">
        <UiButton variant="ghost" tone="neutral" block class="fold-head" @click="toggleFold(it)">
          <component :is="toolIcon(it.toolName)" :size="13" class="tool-icon" />
          <span class="fold-title">{{ it.toolName }}</span>
          <span v-if="toolSubtitle(it)" class="fold-sep">·</span>
          <span v-if="toolSubtitle(it)" class="fold-sub">{{ toolSubtitle(it) }}</span>
          <span v-if="!it.resultDone" class="tstatus running">{{ t('running') }}</span>
          <span v-else-if="it.resultError" class="tstatus err">{{ t('failed') }}</span>
          <span v-else class="tstatus ok">{{ t('done') }}</span>
          <LuChevronRight :size="13" class="caret" />
        </UiButton>
        <div v-show="isOpen(it)" class="fold-body-wrap">
          <pre class="fold-body">{{ prettyJson(it.toolInput) }}</pre>
          <!--
            子代理过程嵌在 task 卡片内部：它属于这张卡片的执行细节，
            不是主 Agent 的同级输出。仅对 task 且有待显示内容时才有值。
          -->
          <div v-if="it.subagentBlocks" class="subagent">
            <div class="subagent-label">{{ t('subagent') }}</div>
            <MessageBlocks :blocks="it.subagentBlocks" :streaming="!it.resultDone" />
          </div>
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
/* 思考正文：与正文一样按 Markdown 渲染，但整体压一档、默认偏弱色 */
.think-body {
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

/* 子代理过程：嵌在 task 卡片内，用左侧竖线表达从属关系 */
.subagent {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 6px 0 0;
  padding-left: 10px;
  border-left: 2px solid var(--surface2);
}
.subagent-label {
  font-size: 11px;
  color: var(--overlay0);
  user-select: none;
}
</style>
