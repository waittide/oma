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

/** 左侧 chevron 槽 + 每层列宽（px） */
const GUTTER = 20;
const COL = 20;

interface Guide {
  /** 引导线 x 坐标 */
  x: number;
  pos: 'top' | 'mid' | 'bottom' | 'single';
  /** 是否向右侧画出横向连接（分支首尾行） */
  stub: boolean;
}

interface Row {
  msg: ChatMessage;
  /** 层级：主线第一层，自父节点分叉出的子节点整体下移一层 */
  depth: number;
  /** 引导线段：仅激活分支绘制 */
  guides: Guide[];
  /** 位于当前激活分支上（圆点亮色 + 引导线） */
  active: boolean;
  /** 当前激活叶子（左侧 chevron 标记） */
  leaf: boolean;
}

/**
 * 展平消息树：跳过运行时内部消息（子节点挂到最近可见祖先），按先序排列。
 * 分层规则：父节点只有一个子节点时子节点延续同层；父节点分叉时其全部子节点下移一层。
 * 引导线：每个分叉子节点的子树在父列上画一条竖线，首行 ┌、末行 └，仅激活分支可见。
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
  const walk = (list: ChatMessage[], depth: number, parentDepth: number, isRoot: boolean) => {
    list.forEach((m) => {
      const start = out.length;
      const forkChild = !isRoot && depth !== parentDepth;
      out.push({
        msg: m,
        depth,
        guides: [],
        active: activeIds.has(m.id),
        leaf: m.id === leafId,
      });
      const childList = kids.get(m.id) ?? [];
      walk(childList, childList.length > 1 ? depth + 1 : depth, depth, false);

      // 分叉子节点的整棵子树：在父列上绘制引导线，仅激活分支可见
      if (forkChild) {
        const end = out.length - 1;
        const x = GUTTER + (depth - 1) * COL;
        for (let j = start; j <= end; j++) {
          if (!out[j]!.active) continue;
          const pos = start === end ? 'single' : j === start ? 'top' : j === end ? 'bottom' : 'mid';
          out[j]!.guides.push({ x, pos, stub: pos !== 'mid' });
        }
      }
    });
  };
  walk(kids.get(null) ?? [], 0, 0, true);
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
        :class="{ leaf: r.leaf, off: !r.active }"
        :style="{ paddingLeft: `${GUTTER + r.depth * COL + (r.active ? 14 : 0)}px` }"
        :disabled="chat.running.value"
        @click="pick(r.msg)"
      >
        <LuChevronRight v-if="r.leaf" :size="12" class="chev" />
        <span
          v-for="(g, gi) in r.guides"
          :key="gi"
          class="gline"
          :class="[g.pos, { stub: g.stub }]"
          :style="{ left: `${g.x}px` }"
        />
        <span
          v-if="r.active"
          class="bullet"
          :style="{ left: `${GUTTER + r.depth * COL - 3.5}px` }"
        />
        <span class="role" :class="r.msg.role">{{ r.msg.role }}:</span>
        <span class="n-text">{{ snippet(r.msg) }}</span>
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
  align-items: baseline;
  gap: 6px;
  width: 100%;
  border: none;
  background: transparent;
  padding: 3px 8px 3px 0;
  border-radius: 6px;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.6;
  color: var(--text);
  cursor: pointer;
  text-align: left;
  transition:
    background-color 0.12s ease,
    box-shadow 0.12s ease;
}
.node:hover:not(:disabled) {
  background: var(--surface-hover);
  box-shadow: inset 0 0 0 1px var(--control-border);
}
.node.off {
  color: var(--overlay1);
}
.node:disabled {
  cursor: default;
  opacity: 0.55;
}
/* 激活叶子：左侧 chevron 标记 */
.chev {
  position: absolute;
  left: 2px;
  top: 50%;
  transform: translateY(-50%);
  color: var(--peach);
}
/* 激活分支圆点（peach），非激活分支不显示 */
.bullet {
  position: absolute;
  top: 50%;
  width: 7px;
  height: 7px;
  border-radius: 99px;
  transform: translateY(-50%);
  background: var(--peach);
  pointer-events: none;
}
/* 引导线：父列竖线，分支首行 ┌、末行 └，仅激活分支绘制 */
.gline {
  position: absolute;
  width: 1.5px;
  background: var(--surface2);
  pointer-events: none;
}
.gline.top {
  top: 50%;
  bottom: 0;
}
.gline.bottom {
  top: 0;
  bottom: 50%;
}
.gline.mid,
.gline.single {
  top: 0;
  bottom: 0;
}
.gline.stub::after {
  content: '';
  position: absolute;
  top: 50%;
  left: 0;
  width: 20px;
  height: 1.5px;
  margin-top: -0.75px;
  background: var(--surface2);
}
.role {
  flex-shrink: 0;
}
.role.user {
  color: var(--lavender);
}
.role.assistant {
  color: var(--green-color);
}
.node.off .role {
  color: var(--overlay1);
}
.n-text {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.empty {
  margin: 0;
  padding: 12px 2px;
  font-size: 12.5px;
  color: var(--text-tertiary);
  text-align: center;
}
</style>
