<script setup lang="ts">
import { computed } from 'vue';
import { useTranslations } from '../composables/i18n';
import * as chat from '../stores/chat';
import { activeSession } from '../stores/sessions';

/** 底部状态栏：模型 / 推理等级 / 上下文占用 / 队列深度（对齐 pi-web 的底栏信息）。 */
const { t } = useTranslations('status');

const modelLabel = computed(() => {
  const wanted = chat.activeModel.value;
  if (!wanted) return '';
  const [pid, ...rest] = wanted.split('/');
  const mid = rest.join('/');
  const found = (chat.modelCatalog.value[pid ?? ''] ?? []).find((m) => m.id === mid);
  return found?.name ?? mid;
});

const contextLabel = computed(() => {
  const usage = chat.contextUsage.value;
  if (!usage) return '';
  const pct = usage.contextLen > 0 ? Math.round((usage.tokens / usage.contextLen) * 100) : 0;
  return `${formatTokens(usage.tokens)} / ${formatTokens(usage.contextLen)} · ${pct}%`;
});

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}
</script>

<template>
  <footer class="status-bar">
    <span v-if="modelLabel" class="item">{{ modelLabel }}</span>
    <span v-if="chat.reasoningLevel.value" class="item">{{ chat.reasoningLevel.value }}</span>
    <span v-if="contextLabel" class="item">{{ contextLabel }}</span>
    <span v-if="chat.queued.value > 0" class="item">
      {{ t('queued', { count: chat.queued.value }) }}
    </span>
    <span class="spacer" />
    <span class="item dim">{{ activeSession?.workspace ?? '' }}</span>
  </footer>
</template>

<style scoped>
.status-bar {
  display: flex;
  align-items: center;
  gap: 14px;
  height: 26px;
  flex-shrink: 0;
  padding: 0 12px;
  border-top: 1px solid var(--line);
  background: var(--paper);
  color: var(--muted);
  font-size: 11.5px;
  white-space: nowrap;
  overflow: hidden;
}
.item {
  overflow: hidden;
  text-overflow: ellipsis;
}
.item.dim {
  color: var(--faint);
}
.spacer {
  flex: 1;
}
</style>
