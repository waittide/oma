<script setup lang="ts">
import { computed } from 'vue';
import { UiIconButton, UiTooltip } from '@waittide/ui';
import { LuFileText, LuGitCompare, LuTerminal, LuX } from 'vue-icons-plus/lu';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';
import * as layout from '../stores/layout';

/**
 * 右侧工作面板（工作区外壳的一部分）。
 *
 * 本批只落地「外壳」：标签页与占位态。文件树 / 预览、内置终端、Git 变更
 * 分别在 P4 / P9 / P5 期接入真实内容。
 */
const { t } = useTranslations('panel');

const tabs = computed(() => [
  { id: 'files' as const, label: t('files'), icon: LuFileText },
  { id: 'changes' as const, label: t('changes'), icon: LuGitCompare },
  { id: 'terminal' as const, label: t('terminal'), icon: LuTerminal },
]);
</script>

<template>
  <aside class="right-panel">
    <div class="tabs">
      <button
        v-for="tab in tabs"
        :key="tab.id"
        type="button"
        class="tab"
        :class="{ active: layout.rightTab.value === tab.id }"
        @click="layout.setRightTab(tab.id)"
      >
        <component :is="tab.icon" :size="13" />
        <span>{{ tab.label }}</span>
      </button>
      <UiTooltip :content="t('close')" align="end" placement="bottom">
        <UiIconButton class="tab-close" size="sm" :label="t('close')" @click="layout.toggleRight()">
          <LuX :size="13" />
        </UiIconButton>
      </UiTooltip>
    </div>

    <div class="body">
      <div v-if="!activeSession" class="empty">{{ t('noSession') }}</div>

      <template v-else>
        <div v-if="layout.rightTab.value === 'files'" class="empty">
          <p class="hint">{{ t('filesHint') }}</p>
          <code class="path">{{ activeSession.workspace }}</code>
        </div>
        <div v-else-if="layout.rightTab.value === 'changes'" class="empty">
          <p class="hint">{{ t('changesHint') }}</p>
        </div>
        <div v-else class="empty">
          <p class="hint">{{ t('terminalHint') }}</p>
        </div>
      </template>
    </div>
  </aside>
</template>

<style scoped>
.right-panel {
  display: flex;
  flex-direction: column;
  min-width: 0;
  height: 100%;
  border-left: 1px solid var(--line);
  background: var(--surface);
}
.tabs {
  display: flex;
  align-items: center;
  gap: 2px;
  height: 40px;
  flex-shrink: 0;
  padding: 0 6px;
  border-bottom: 1px solid var(--line);
}
.tab {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  height: 28px;
  padding: 0 9px;
  border: none;
  border-radius: var(--radius-sm);
  background: transparent;
  color: var(--muted);
  font: inherit;
  font-size: 12.5px;
  cursor: pointer;
}
.tab:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.tab.active {
  background: var(--surface-active);
  color: var(--ink);
}
.tab-close {
  margin-left: auto;
  width: 26px !important;
  height: 26px !important;
}
.body {
  flex: 1;
  min-height: 0;
  overflow: auto;
}
.empty {
  padding: 16px;
  color: var(--muted);
  font-size: 13px;
}
.hint {
  margin: 0 0 8px;
}
.path {
  display: block;
  color: var(--text-secondary);
  font-size: 12px;
  word-break: break-all;
}
</style>
