<script setup lang="ts">
import { computed } from 'vue';
import { UiIconButton, UiTooltip } from '@waittide/ui';
import { LuPanelLeft, LuPanelRight, LuPlug } from 'vue-icons-plus/lu';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';
import * as layout from '../stores/layout';
import * as chat from '../stores/chat';
import { sessionUsage, usageLine } from '../lib/sessionUsage';

const { t } = useTranslations('chat');

const wsPath = computed(() => activeSession.value?.workspace ?? '');

/**
 * 整会话 token 合计（`in · out · cache R · cache W`）。
 *
 * 与 pi-web 顶栏的 token/成本统计对齐；oma 的模型配置没有价格字段，故不含成本。
 */
const usage = computed(() => usageLine(sessionUsage(chat.messages.value)));

/** MCP 概览：已发现的服务端与它们提供的工具总数（悬停看逐个明细） */
const mcpToolTotal = computed(() => chat.mcpServers.value.reduce((sum, s) => sum + s.tool_count, 0));
const mcpTip = computed(() =>
  chat.mcpServers.value.map((s) => `${s.name} (${s.tool_count})`).join('\n'),
);
</script>

<template>
  <header class="toolbar">
    <div class="tb-left">
      <UiTooltip :content="t('toggleSidebar')" align="start" placement="bottom">
        <UiIconButton
          class="icon-ghost"
          size="sm"
          :label="t('toggleSidebar')"
          @click="layout.toggleSidebar()"
        >
          <LuPanelLeft :size="14" />
        </UiIconButton>
      </UiTooltip>
      <span class="session-title">{{ activeSession?.title ?? t('noSession') }}</span>
      <UiTooltip v-if="wsPath" :content="wsPath" align="start" placement="bottom">
        <span class="ws-path">{{ wsPath }}</span>
      </UiTooltip>
    </div>

    <div class="tb-right">
      <span v-if="usage" class="usage" :title="t('sessionUsage')">{{ usage }}</span>

      <UiTooltip v-if="chat.mcpServers.value.length" :content="mcpTip" align="end" placement="bottom">
        <span class="mcp-chip">
          <LuPlug :size="11" />
          {{ mcpToolTotal }}
        </span>
      </UiTooltip>

      <UiTooltip :content="chat.connected.value ? t('connected') : t('disconnected')" align="end" placement="bottom">
        <span class="dot" :class="chat.connected.value ? 'ok' : 'off'" />
      </UiTooltip>
      <UiTooltip :content="t('toggleRightPanel')" align="end" placement="bottom">
        <UiIconButton
          class="icon-ghost"
          size="sm"
          :label="t('toggleRightPanel')"
          :class="{ on: layout.rightOpen.value }"
          @click="layout.toggleRight()"
        >
          <LuPanelRight :size="14" />
        </UiIconButton>
      </UiTooltip>
    </div>
  </header>
</template>

<style scoped>
.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  height: 40px;
  flex-shrink: 0;
  padding: 0 10px;
  border-bottom: 1px solid var(--line);
  background: var(--paper);
}
.tb-left,
.tb-right {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.session-title {
  font-size: 13px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.ws-path {
  font-size: 12px;
  color: var(--muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
/* 整会话 token 合计：等宽数字，避免流式时宽度跳动 */
.usage {
  font-size: 11px;
  color: var(--muted);
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 320px;
}
.icon-ghost {
  width: 26px !important;
  height: 26px !important;
}
.icon-ghost.on {
  color: var(--accent);
}
/* MCP 概览：胶囊 + 工具总数，悬停展开服务端明细 */
.mcp-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 22px;
  padding: 0 8px;
  border: 1px solid var(--line);
  border-radius: 99px;
  background: var(--surface);
  color: var(--muted);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
  white-space: nowrap;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}
.dot.ok {
  background: var(--success);
}
.dot.off {
  background: var(--danger);
}
</style>
