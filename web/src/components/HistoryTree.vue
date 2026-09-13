<script setup lang="ts">
import { computed } from 'vue';
import { LuChevronRight } from 'vue-icons-plus/lu';
import OModal from './ui/OModal.vue';
import * as chat from '../stores/chat';
import type { ChatMessage } from '../types';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('historyTree');

/** 每个缩进层占 3 个等宽字符（参照 oh-my-pi tree-selector） */
const LEVEL_CHARS = 3;
/** 光标槽宽度（字符） */
const CURSOR_CHARS = 2;

interface Gutter {
  /** 该 gutter 所在的层号 */
  position: number;
  /** true 绘制竖线，false 表示该层分支已结束 */
  show: boolean;
}

interface Row {
  msg: ChatMessage;
  /** 显示层号（虚拟根子节点已折算） */
  indent: number;
  /** 是否绘制肘形连接（父节点有多个子节点） */
  connector: boolean;
  /** 是否为本层最后一个兄弟（└ 而非 ├） */
  last: boolean;
  /** 需要贯穿本行的竖线层号 */
  vlines: number[];
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
 * - 祖先分叉点以 gutter 向下传递，非末位兄弟继续画竖线。
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
      vlines: gutters.filter((g) => g.show).map((g) => g.position),
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

/** 单行摘要：折叠换行与连续空白，避免撑破单行行高 */
function snippet(m: ChatMessage): string {
  const normalize = (s: string) => s.replace(/\s+/g, ' ').trim();
  const text = m.content.find((b) => b.type === 'text' && b.text.trim());
  if (text && text.type === 'text') return normalize(text.text);
  const tool = m.content.find((b) => b.type === 'tool_use');
  if (tool && tool.type === 'tool_use') return t('toolCall', { name: tool.name });
  if (m.content.some((b) => b.type === 'image')) return t('imageBlock');
  return t('emptySnippet');
}

/** 回填输入框用的原文：保留换行，只去首尾空白 */
function snippetFull(m: ChatMessage): string {
  const text = m.content.find((b) => b.type === 'text');
  return text && text.type === 'text' ? text.text.trim() : '';
}

/** 轮次进行中禁止切换分支，避免撕裂运行状态 */
const busy = computed(() => chat.running.value);

/**
 * 点击节点切换当前对话。
 *
 * - 点用户消息 U：把 U 的文本放回输入框、视图截断到 **U 之前**（即 U 的父节点），
 *   再发送就是重发这条消息 —— 等价于气泡上的「编辑重发」；
 * - 点助手回复 A：视图截断到 A，输入框内容保持不动，分叉目标指向 A，
 *   再输入发送就从 A 处长出新分支。
 *
 * 两种都不写入服务端当前叶子：服务端已按 parent 自行落位，提前写入反而会
 * 把一条普通发送挂到旧分支上。
 */
function pick(r: Row) {
  if (busy.value) return;
  if (r.msg.role === 'user') {
    // 分叉目标是该消息的父节点：新消息才会与它成为兄弟分支
    const parentId = chat.truncatePointFor(r.msg.id);
    chat.showAt(parentId);
    chat.startForkFrom(parentId, snippetFull(r.msg));
  } else {
    chat.showAt(r.msg.id);
    chat.startForkFrom(r.msg.id);
  }
  emit('close');
}
</script>

<template>
  <OModal :open="props.open" :title="t('title')" width="760px" @close="emit('close')">
    <div class="tree">
      <button
        v-for="r in rows"
        :key="r.msg.id"
        type="button"
        class="node"
        :class="{ selected: r.leaf, off: !r.active }"
        :style="{ paddingLeft: `${CURSOR_CHARS + r.indent * LEVEL_CHARS}ch` }"
        :disabled="busy"
        :title="snippet(r.msg)"
        @click="pick(r)"
      >
        <!-- 光标槽 -->
        <span v-if="r.leaf" class="cursor"><LuChevronRight :size="12" /></span>
        <!-- 祖先层竖线：贯穿整行，跨行无缝 -->
        <span
          v-for="level in r.vlines"
          :key="`v${level}`"
          class="vline"
          :style="{ left: `${CURSOR_CHARS + level * LEVEL_CHARS}ch` }"
        />
        <!-- 本层连接：末位兄弟为圆角转角（└），其余为贯通竖线 + 水平分支（├） -->
        <template v-if="r.connector && !r.vlines.includes(r.indent - 1)">
          <span
            v-if="r.last"
            class="elbow"
            :style="{ left: `${CURSOR_CHARS + (r.indent - 1) * LEVEL_CHARS}ch` }"
          />
          <template v-else>
            <span
              class="vline"
              :style="{ left: `${CURSOR_CHARS + (r.indent - 1) * LEVEL_CHARS}ch` }"
            />
            <span
              class="stub"
              :style="{ left: `${CURSOR_CHARS + (r.indent - 1) * LEVEL_CHARS}ch` }"
            />
          </template>
        </template>
        <!-- 激活分支圆点：无论是否激活都占活 2ch 槽位，
             否则未选中分支会比选中分支少缩进一个圆点的宽度，两者无法左对齐 -->
        <span class="bullet"><i v-if="r.active" /></span>
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
  position: relative;
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
/* 光标槽：固定 2ch，图标居中，不影响文字网格 */
.cursor {
  position: absolute;
  left: 0;
  width: 2ch;
  display: flex;
  align-items: center;
  color: var(--accent);
}
/* 圆点槽：固定 2ch，避免 ● 字形回退导致网格偏移；未激活时留空占位 */
.bullet {
  display: inline-flex;
  align-items: center;
  width: 2ch;
  flex-shrink: 0;
}
.bullet i {
  width: 7px;
  height: 7px;
  border-radius: 99px;
  background: var(--accent);
}
/* 引导线统一用 border 绘制：横竖同为边框宽度，粗细一致；
   上下各溢出 1px 消除行间缝隙。 */
.vline,
.elbow {
  position: absolute;
  width: 0;
  border-left: 1.5px solid var(--overlay0);
  pointer-events: none;
}
.vline {
  top: -1px;
  bottom: -1px;
}
/* └ 末位转角：border-left 为上半段竖线，border-bottom 为横线，
   border-bottom-left-radius 形成圆角 */
.elbow {
  top: -1px;
  height: calc(50% + 0.75px);
  width: 3ch;
  border-bottom: 1.5px solid var(--overlay0);
  border-bottom-left-radius: 7px;
}
/* ├ 非末位：竖线整条贯通，横线在行中心分支（避免圆角使竖线偏移产生断口） */
.stub {
  position: absolute;
  top: calc(50% - 0.75px);
  width: 3ch;
  height: 0;
  border-bottom: 1.5px solid var(--overlay0);
  pointer-events: none;
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
