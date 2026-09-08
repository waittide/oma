<script setup lang="ts">
import { computed } from 'vue';
import { LuBot, LuGitBranch, LuUser } from 'vue-icons-plus/lu';
import OModal from './ui/OModal.vue';
import * as chat from '../stores/chat';
import type { ChatMessage } from '../types';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('historyTree');

/** 每层缩进列宽（px） */
const INDENT = 18;

interface Row {
  msg: ChatMessage;
  /** 层级：主线第一层，自父节点分叉出的子节点整体下移一层 */
  depth: number;
  /** 各祖先层的竖线是否延续到本行（该层祖先还有后续兄弟） */
  lines: boolean[];
  /** 是否为父节点的最后一个子节点（└ 形连接） */
  last: boolean;
  /** 是否有父节点（根节点不画连接线） */
  hasParent: boolean;
  /** 分叉子节点：绘制肘形连接（├ / └）；否则为同层延续（竖线） */
  forkChild: boolean;
  /** 位于当前激活分支上（圆点亮色） */
  active: boolean;
  /** 当前激活叶子 */
  leaf: boolean;
  /** 分叉点：存在兄弟分支 */
  branch: boolean;
}

/**
 * 展平消息树：跳过运行时内部消息（子节点挂到最近可见祖先），按先序排列。
 * 分层规则：父节点只有一个子节点时子节点延续同层（线性对话不逐层缩进）；
 * 父节点分叉（多个子节点）时，其全部子节点整体下移一层。
 */
const rows = computed<Row[]>(() => {
  const byId = new Map(chat.tree.value.map((m) => [m.id, m]));

  const nearestVisible = (m: ChatMessage): string | null => {
    let p = m.parent_id;
    while (p && byId.has(p)) {
      const pm = byId.get(p)!;
      if (!chat.isInternalMessage(pm)) return p;
      p = pm.parent_id;
    }
    return null;
  };

  const kids = new Map<string | null, ChatMessage[]>();
  const visible = chat.tree.value.filter((m) => !chat.isInternalMessage(m));
  for (const m of visible) {
    const key = nearestVisible(m);
    const list = kids.get(key);
    if (list) list.push(m);
    else kids.set(key, [m]);
  }
  for (const list of kids.values()) list.sort((a, b) => a.created_at - b.created_at);

  const activeIds = new Set(chat.messages.value.map((m) => m.id));
  const leafId = chat.messages.value[chat.messages.value.length - 1]?.id ?? null;

  const out: Row[] = [];
  /** cols[col] = 该列当前最近祖先是否为其父的最后一个子节点 */
  const walk = (
    list: ChatMessage[],
    depth: number,
    parentDepth: number,
    cols: Map<number, boolean>,
    isRoot: boolean,
  ) => {
    list.forEach((m, idx) => {
      const last = idx === list.length - 1;
      const lines: boolean[] = [];
      for (let l = 0; l < depth; l++) lines.push(cols.get(l) === false);
      out.push({
        msg: m,
        depth,
        lines,
        last,
        hasParent: !isRoot,
        forkChild: !isRoot && depth !== parentDepth,
        active: activeIds.has(m.id),
        leaf: m.id === leafId,
        branch: list.length > 1,
      });
      const next = new Map(cols);
      next.set(depth, last);
      // 父节点分叉（多个子节点）时子节点整体下移一层，否则延续同层
      const childList = kids.get(m.id) ?? [];
      walk(childList, childList.length > 1 ? depth + 1 : depth, depth, next, false);
    });
  };
  walk(kids.get(null) ?? [], 0, 0, new Map(), true);
  return out;
});

function snippet(m: ChatMessage): string {
  const text = m.content.find((b) => b.type === 'text' && b.text.trim());
  if (text && text.type === 'text') return text.text.trim();
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
        :class="{ user: r.msg.role === 'user', active: r.active, leaf: r.leaf, off: !r.active }"
        :style="{ paddingLeft: `${16 + r.depth * INDENT}px` }"
        :disabled="chat.running.value"
        @click="pick(r.msg)"
      >
        <template v-for="(on, li) in r.lines" :key="`v${li}`">
          <span v-if="on" class="vline" :style="{ left: `${li * INDENT + 8.25}px` }" />
        </template>
        <span
          v-if="r.hasParent && !r.forkChild"
          class="vline"
          :style="{ left: `${r.depth * INDENT + 8.25}px` }"
        />
        <template v-if="r.forkChild">
          <span
            class="stem"
            :class="{ last: r.last }"
            :style="{ left: `${r.depth * INDENT + 8.25}px` }"
          />
          <span class="stub" :style="{ left: `${r.depth * INDENT + 9}px` }" />
        </template>
        <span
          class="dot"
          :class="{ on: r.active }"
          :style="{ left: `${r.depth * INDENT + 5.5}px` }"
        />
        <LuUser v-if="r.msg.role === 'user'" :size="12" class="n-icon" />
        <LuBot v-else :size="12" class="n-icon bot" />
        <span class="n-text">{{ snippet(r.msg) }}</span>
        <LuGitBranch v-if="r.branch" :size="11" class="b-icon" />
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
  gap: 7px;
  width: 100%;
  border: none;
  background: transparent;
  padding: 5px 10px 5px 0;
  border-radius: 7px;
  font-family: inherit;
  font-size: 12.5px;
  color: var(--ink);
  cursor: pointer;
  text-align: left;
  transition: background-color 0.12s ease;
}
.node:hover:not(:disabled) {
  background: var(--surface-hover);
}
.node.active {
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
.node.active.leaf {
  background: color-mix(in srgb, var(--accent) 18%, transparent);
}
.node.off {
  color: var(--text-tertiary);
}
.node:disabled {
  cursor: default;
  opacity: 0.55;
}
/* 树形连接线：祖先层竖线 + 分叉子节点肘形（├ / └），统一暗色结构 */
.vline,
.stem {
  position: absolute;
  top: 0;
  bottom: 0;
  width: 1.5px;
  background: var(--surface2);
  pointer-events: none;
}
.stem.last {
  bottom: 50%;
}
.stub {
  position: absolute;
  top: 50%;
  width: 7px;
  height: 1.5px;
  margin-top: -0.75px;
  background: var(--surface2);
  pointer-events: none;
}
/* 层级圆点：激活分支亮色，其余暗色；第一层同样有点 */
.dot {
  position: absolute;
  top: 50%;
  width: 7px;
  height: 7px;
  border-radius: 99px;
  transform: translateY(-50%);
  background: var(--surface2);
  pointer-events: none;
  transition:
    background-color 0.12s ease,
    box-shadow 0.12s ease;
}
.dot.on {
  background: var(--accent);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 22%, transparent);
}
.n-icon {
  flex-shrink: 0;
  color: var(--text-tertiary);
}
.n-icon.bot {
  color: var(--accent);
}
.n-text {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.b-icon {
  flex-shrink: 0;
  color: var(--overlay0);
}
.empty {
  margin: 0;
  padding: 12px 2px;
  font-size: 12.5px;
  color: var(--text-tertiary);
  text-align: center;
}
</style>
