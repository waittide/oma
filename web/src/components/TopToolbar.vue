<script setup lang="ts">
import { computed } from 'vue';
import { UiIconButton, UiTooltip } from '@waittide/ui';
import { LuListTree, LuPanelLeft, LuPanelRight } from 'vue-icons-plus/lu';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';
import * as layout from '../stores/layout';
import * as chat from '../stores/chat';

const { t } = useTranslations('chat');

const wsPath = computed(() => activeSession.value?.workspace ?? '');
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
