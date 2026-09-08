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

interface Row {
  msg: ChatMessage;
  depth: number;
  /** 位于当前激活分支上 */
  active: boolean;
  /** 当前激活叶子 */
  leaf: boolean;
  /** 分叉点：存在兄弟分支 */
  branch: boolean;
}

/** 展平消息树：跳过运行时内部消息（子节点挂到最近可见祖先），按先序排列。 */
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
  const walk = (parent: string | null, depth: number) => {
    const list = kids.get(parent);
    if (!list) return;
    for (const m of list) {
      out.push({
        msg: m,
        depth,
        active: activeIds.has(m.id),
        leaf: m.id === leafId,
        branch: (kids.get(nearestVisible(m))?.length ?? 0) > 1,
      });
      walk(m.id, depth + 1);
    }
  };
  walk(null, 0);
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
        :style="{ paddingLeft: `${6 + r.depth * 16}px` }"
        :disabled="chat.running.value"
        @click="pick(r.msg)"
      >
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
  gap: 1px;
  max-height: 60vh;
  overflow-y: auto;
}
.node {
  display: flex;
  align-items: center;
  gap: 7px;
  width: 100%;
  border: none;
  background: transparent;
  padding: 6px 10px;
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
