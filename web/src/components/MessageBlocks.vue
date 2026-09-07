<script setup lang="ts">
import { computed } from 'vue';
import {
  LuBrain,
  LuChevronDown,
  LuFileCode2,
  LuTerminalSquare,
} from 'vue-icons-plus/lu';
import type { Block } from '../types';
import { prettyJson, renderMarkdown } from '../lib/format';

const props = defineProps<{ blocks: Block[]; streaming: boolean }>();

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
      out.push({ kind: 'tool', key: b.id, toolName: b.name, toolInput: b.input });
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
</script>

<template>
  <div class="blocks">
    <template v-for="it in items" :key="it.key">
      <div v-if="it.kind === 'text' && it.text" class="md" v-html="renderMarkdown(it.text)" />

      <details v-else-if="it.kind === 'thinking'" class="fold think">
        <summary>
          <LuBrain :size="13" />
          <span>思考过程</span>
          <LuChevronDown :size="13" class="caret" />
        </summary>
        <pre class="think-body">{{ it.thinking }}</pre>
      </details>

      <img v-else-if="it.kind === 'image' && it.imageSrc" class="att" :src="it.imageSrc" alt="attachment" />

      <details v-else-if="it.kind === 'tool'" class="fold tool">
        <summary>
          <LuTerminalSquare v-if="it.toolName === 'shell'" :size="13" />
          <LuFileCode2 v-else :size="13" />
          <span class="tname">{{ it.toolName }}</span>
          <span v-if="!it.resultDone" class="tstatus running">执行中…</span>
          <span v-else-if="it.resultError" class="tstatus err">失败</span>
          <span v-else class="tstatus ok">完成</span>
          <LuChevronDown :size="13" class="caret" />
        </summary>
        <pre class="tjson">{{ prettyJson(it.toolInput) }}</pre>
        <pre v-if="it.resultDone" class="tres" :class="{ err: it.resultError }">{{ it.resultContent }}</pre>
      </details>
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
  border: 1px solid var(--line);
  border-radius: 10px;
  overflow-x: auto;
  font-size: 12.5px;
  line-height: 1.6;
}
.md :deep(code) {
  font-family: var(--font-mono);
  font-size: 0.92em;
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
.fold {
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  overflow: hidden;
}
.fold summary {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 7px 11px;
  font-size: 12.5px;
  color: var(--text-secondary);
  cursor: pointer;
  list-style: none;
  user-select: none;
}
.fold summary::-webkit-details-marker {
  display: none;
}
.fold summary:hover {
  background: var(--surface-hover);
}
.caret {
  margin-left: auto;
  color: var(--overlay0);
  transition: transform 0.15s ease;
}
.fold[open] .caret {
  transform: rotate(180deg);
}
.think summary {
  color: var(--mauve);
}
.think-body {
  margin: 0;
  padding: 10px 13px;
  border-top: 1px solid var(--line);
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.65;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--text-tertiary);
  max-height: 260px;
  overflow-y: auto;
}
.tname {
  font-weight: 600;
  font-family: var(--font-mono);
}
.tstatus {
  font-size: 11px;
  padding: 1px 7px;
  border-radius: 99px;
}
.tstatus.ok {
  color: var(--success);
  background: var(--success-soft);
}
.tstatus.err {
  color: var(--danger);
  background: var(--danger-soft);
}
.tstatus.running {
  color: var(--warning);
  background: var(--warning-soft);
}
.tjson,
.tres {
  margin: 0;
  padding: 9px 13px;
  border-top: 1px solid var(--line);
  font-family: var(--font-mono);
  font-size: 11.5px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  color: var(--text-tertiary);
  max-height: 220px;
  overflow-y: auto;
}
.tres.err {
  color: var(--danger);
}
.att {
  max-width: 320px;
  border-radius: 10px;
  border: 1px solid var(--line);
}
</style>
