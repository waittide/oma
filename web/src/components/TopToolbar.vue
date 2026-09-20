<script setup lang="ts">
import { computed, ref } from 'vue';
import { toast, UiIconButton, UiTooltip } from '@waittide/ui';
import { LuDownload, LuListTree, LuPanelLeft, LuPanelRight, LuTerminal } from 'vue-icons-plus/lu';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';
import * as layout from '../stores/layout';
import * as chat from '../stores/chat';
import SystemPanel from './SystemPanel.vue';
import { downloadText, sessionToMarkdown, sessionUsage, usageLine } from '../lib/sessionExport';

const { t } = useTranslations('chat');

const wsPath = computed(() => activeSession.value?.workspace ?? '');
const systemOpen = ref(false);

/**
 * 整会话 token 合计（`in · out · cache R · cache W`）。
 *
 * 与 pi-web 顶栏的 token/成本统计对齐；oma 的模型配置没有价格字段，故不含成本。
 */
const usage = computed(() => usageLine(sessionUsage(chat.messages.value)));
const hasMessages = computed(() => chat.messages.value.length > 0);

/** 导出当前会话为 Markdown（纯前端，不落新接口）。 */
function exportSession() {
  const session = activeSession.value;
  if (!session) return;
  const title = session.title || session.session_id;
  const markdown = sessionToMarkdown(chat.messages.value, {
    title,
    workspace: session.workspace ?? '',
  });
  // 文件名去掉路径分隔符等不安全字符，避免下载被浏览器拒绝
  const safe = title.replace(/[\\/:*?"<>|]/g, '_').slice(0, 60) || 'session';
  downloadText(`${safe}.md`, markdown);
  toast.success(t('exported'));
}
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

      <UiTooltip :content="t('exportSession')" align="end" placement="bottom">
        <UiIconButton
          class="icon-ghost"
          size="sm"
          :label="t('exportSession')"
          :disabled="!hasMessages"
          @click="exportSession"
        >
          <LuDownload :size="14" />
        </UiIconButton>
      </UiTooltip>

      <UiTooltip :content="t('systemPrompt')" align="end" placement="bottom">
        <UiIconButton
          class="icon-ghost"
          size="sm"
          :label="t('systemPrompt')"
          :disabled="!activeSession"
          @click="systemOpen = true"
        >
          <LuTerminal :size="14" />
        </UiIconButton>
      </UiTooltip>

      <UiTooltip :content="chat.connected.value ? t('connected') : t('disconnected')" align="end" placement="bottom">
        <span class="dot" :class="chat.connected.value ? 'ok' : 'off'" />
      </UiTooltip>
      <UiTooltip :content="t('historyTree')" align="end" placement="bottom">
        <UiIconButton
          class="icon-ghost"
          size="sm"
          :label="t('historyTree')"
          :disabled="!activeSession"
          @click="layout.setTreeOpen(true)"
        >
          <LuListTree :size="14" />
        </UiIconButton>
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

    <SystemPanel v-model:open="systemOpen" />
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
