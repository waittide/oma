<script setup lang="ts">
import { computed } from 'vue';
import OModal from './ui/OModal.vue';
import * as chat from '../stores/chat';
import type { ChatMessage } from '../types';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('historyTree');

/** 每个缩进层占 3 个等宽字符（参照 oh-my-pi tree-selector） */
const LEVEL_CHARS = 3;

interface Gutter {
  /** 该 gutter 所在的层号 */
  position: number;
  /** true 绘制 │，false 绘制空格（该层分支已结束） */
  show: boolean;
}

interface Row {
  msg: ChatMessage;
  /** 显示层号（虚拟根子节点已折算） */
  indent: number;
  /** 是否绘制连接符（父节点有多个子节点） */
  connector: boolean;
  /** 是否为本层最后一个兄弟（└─ 而非 ├─） */
  last: boolean;
  /** 各祖先分叉点在该行前缀中的竖线 */
  gutters: Gutter[];
  /** 位于当前激活分支上（圆点亮色） */
  active: boolean;
  /** 当前激活叶子（左侧 chevron 标记） */
  leaf: boolean;
}

/** 仅含工具调用、无文本的 assistant 消息不单独成行（当前叶子除外） */
function hasText(m: ChatMessage): boolean {
  return m.content.some((b) => b.type === 'text' && b.text.trim());
}

/**
 * 展平消息树：跳过运行时内部消息（子节点挂到最近可见祖先），按先序排列。
 * 分层与引导线规则对齐 oh-my-pi tree-selector：
 * - 仅父节点分叉（多子）时子节点 indent+1，线性对话保持同层；
 * - 含激活叶子的子树在同级中优先排列；
 * - 祖先分叉点以 gutter 向下传递，非末位兄弟继续画 │。
 */
const rows = computed<Row[]>(() => {
  const activeIds = new Set(chat.messages.value.map((m) => m.id));
  const leafId = chat.messages.value[chat.messages.value.length - 1]?.id ?? null;

  const hidden = (m: ChatMessage): boolean =>
    chat.isInternalMessage(m) || (m.role === 'assistant' && !hasText(m) && m.id !== leafId);

  const byId = new Map(chat.tree.value.map((m) => [m.id, m]));

  const nearestVisible = (m: ChatMessage): string | null => {
    let p = m.parent_id;
    while (p && byId.has(p)) {
      const pm = byId.get(p)!;
      if (!hidden(pm)) return p;
      p = pm.parent_id;
    }
    return null;
  };

  const kids = new Map<string | null, ChatMessage[]>();
  for (const m of chat.tree.value) {
    if (hidden(m)) continue;
    const key = nearestVisible(m);
    const list = kids.get(key);
    if (list) list.push(m);
    else kids.set(key, [m]);
  }
  for (const list of kids.values()) list.sort((a, b) => a.created_at - b.created_at);

  // 子树是否包含激活节点（同级中激活分支优先展示）
  const activeCache = new Map<string, boolean>();
  const containsActive = (m: ChatMessage): boolean => {
    const cached = activeCache.get(m.id);
    if (cached !== undefined) return cached;
    let result = activeIds.has(m.id);
    if (!result) result = (kids.get(m.id) ?? []).some(containsActive);
    activeCache.set(m.id, result);
    return result;
  };

  const roots = kids.get(null) ?? [];
  const multipleRoots = roots.length > 1;

  const out: Row[] = [];
  const walk = (
    m: ChatMessage,
    indent: number,
    connector: boolean,
    last: boolean,
    gutters: Gutter[],
    virtualRootChild: boolean,
  ) => {
    const displayIndent = multipleRoots ? Math.max(0, indent - 1) : indent;
    out.push({
      msg: m,
      indent: displayIndent,
      connector: connector && !virtualRootChild,
      last,
      gutters,
      active: activeIds.has(m.id),
      leaf: m.id === leafId,
    });

    const children = kids.get(m.id) ?? [];
    if (children.length === 0) return;
    const ordered = [...children.filter(containsActive), ...children.filter((c) => !containsActive(c))];
    const multiple = children.length > 1;
    const childIndent = multiple || virtualRootChild ? indent + 1 : indent;
    const connectorPosition = Math.max(0, displayIndent - 1);
    const childGutters =
      connector && !virtualRootChild ? [...gutters, { position: connectorPosition, show: !last }] : gutters;
    ordered.forEach((child, i) =>
      walk(child, childIndent, multiple, i === ordered.length - 1, childGutters, false),
    );
  };
  roots.forEach((root, i) =>
    walk(root, multipleRoots ? 1 : 0, multipleRoots, i === roots.length - 1, [], multipleRoots),
  );
  return out;
});

/** 逐字符构建前缀：祖先层竖线 + 本层连接符，每层固定 3 字符 */
function prefix(r: Row): string {
  const chars: string[] = [];
  for (let level = 0; level < r.indent; level++) {
    const gutter = r.gutters.find((g) => g.position === level);
    if (gutter) {
      chars.push(gutter.show ? '│' : ' ', ' ', ' ');
    } else if (r.connector && level === r.indent - 1) {
      chars.push(r.last ? '└' : '├', '─', ' ');
    } else {
      chars.push(' ', ' ', ' ');
    }
  }
  return chars.join('');
}

/** 单行摘要：折叠换行与连续空白，避免 pre 行高被撑破 */
function snippet(m: ChatMessage): string {
  const normalize = (s: string) => s.replace(/\s+/g, ' ').trim();
  const text = m.content.find((b) => b.type === 'text' && b.text.trim());
  if (text && text.type === 'text') return normalize(text.text);
  const tool = m.content.find((b) => b.type === 'tool_use');
  if (tool && tool.type === 'tool_use') return t('toolCall', { name: tool.name });
  if (m.content.some((b) => b.type === 'image')) return t('imageBlock');
  return t('emptySnippet');
}

/** 轮次进行中禁止切换分支，避免撕裂运行状态 */
function pick(m: ChatMessage) {
  if (chat.running.value) return;
  chat.switchBranch(m.id);
  emit('close');
}
</script>

<template>
  <OModal :open="props.open" :title="t('title')" width="560px" @close="emit('close')">
    <div class="tree">
      <button
        v-for="r in rows"
        :key="r.msg.id"
        type="button"
        class="node"
        :class="{ selected: r.leaf, off: !r.active }"
        :disabled="chat.running.value"
        @click="pick(r.msg)"
      >
        <span class="cursor">{{ r.leaf ? '› ' : '  ' }}</span>
        <span class="prefix">{{ prefix(r) }}</span>
        <span class="bullet">{{ r.active ? '● ' : '' }}</span>
        <span class="role" :class="r.msg.role">{{ r.msg.role }}:&nbsp;</span>
        <span class="text">{{ snippet(r.msg) }}</span>
      </button>
      <p v-if="rows.length === 0" class="empty">{{ t('empty') }}</p>
    </div>
  </OModal>
</template>

<style scoped>
.tree {
  display: flex;
  flex-direction: column;
  max-height: 60vh;
  overflow-y: auto;
}
.node {
  display: flex;
  align-items: center;
  width: 100%;
  border: none;
  background: transparent;
  padding: 2px 8px 2px 0;
  border-radius: 6px;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.75;
  white-space: pre;
  color: var(--text);
  cursor: pointer;
  text-align: left;
  transition: background-color 0.12s ease;
}
.node:hover:not(:disabled) {
  background: var(--surface-hover);
}
/* 选中（激活叶子）行：主题化高亮底色 */
.node.selected {
  background: var(--surface-active);
  font-weight: 600;
}
.node:disabled {
  cursor: default;
  opacity: 0.6;
}
/* 光标与激活圆点：主题强调色 */
.cursor,
.bullet {
  color: var(--accent);
  flex-shrink: 0;
}
/* 引导线与连接符：暗色结构 */
.prefix {
  color: var(--overlay0);
  flex-shrink: 0;
}
.role {
  flex-shrink: 0;
}
.role.user {
  color: var(--accent);
}
.role.assistant {
  color: var(--success);
}
.text {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
}
.empty {
  margin: 0;
  padding: 12px 2px;
  font-size: 12.5px;
  color: var(--text-tertiary);
  text-align: center;
}
</style>
