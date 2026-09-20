<script setup lang="ts">
import { ref, watch } from 'vue';
import { toast, UiButton, UiIconButton, UiModal, UiTooltip } from '@waittide/ui';
import { LuCopy, LuCheck } from 'vue-icons-plus/lu';
import { api } from '../api';
import { copyText } from '../lib/clipboard';
import { useTranslations } from '../composables/i18n';
import * as chat from '../stores/chat';
import { activeSession } from '../stores/sessions';

/**
 * 系统提示词面板（对齐 pi-web 的 System）。
 *
 * 提示词是服务端用内置常量拼出来的，配置目录里没有对应文件，因此必须回源取，
 * 不能在客户端拼一份「看起来像」的文本。
 */
const open = defineModel<boolean>('open', { default: false });
const { t } = useTranslations('chat');

const loading = ref(false);
const failed = ref('');
const model = ref('');
const workspace = ref('');
const prompt = ref('');
const copied = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | undefined;

async function load() {
  const ws = activeSession.value?.workspace;
  if (!ws) return;
  loading.value = true;
  failed.value = '';
  try {
    const resp = await api.systemPrompt(ws, chat.activeModel.value);
    workspace.value = resp.workspace;
    model.value = resp.model;
    prompt.value = resp.prompt;
  } catch (e) {
    failed.value = e instanceof Error ? e.message : String(e);
  } finally {
    loading.value = false;
  }
}

async function copy() {
  if (!prompt.value) return;
  await copyText(prompt.value);
  copied.value = true;
  if (copyTimer) clearTimeout(copyTimer);
  copyTimer = setTimeout(() => {
    copied.value = false;
  }, 1200);
}

// 每次打开都重新取：工作区与模型都可能在上次打开之后变过
watch(open, (v) => {
  if (v) void load();
});
</script>

<template>
  <UiModal v-model:open="open" :title="t('systemPrompt')" size="lg">
    <div class="meta">
      <span class="chip">{{ model || t('noModel') }}</span>
      <span class="ws">{{ workspace }}</span>
      <span class="spacer" />
      <UiTooltip :content="t('copy')" align="end">
        <UiIconButton size="sm" :label="t('copy')" :disabled="!prompt" @click="copy">
          <LuCheck v-if="copied" :size="13" />
          <LuCopy v-else :size="13" />
        </UiIconButton>
      </UiTooltip>
    </div>

    <div v-if="loading" class="hint">{{ t('systemPromptLoading') }}</div>
    <div v-else-if="failed" class="hint err">{{ failed }}</div>
    <pre v-else class="prompt">{{ prompt }}</pre>

    <template #footer>
      <UiButton variant="soft" tone="neutral" @click="open = false">{{ t('close') }}</UiButton>
    </template>
  </UiModal>
</template>

<style scoped>
.meta {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 10px;
  min-width: 0;
}
.chip {
  flex-shrink: 0;
  padding: 2px 8px;
  border-radius: 99px;
  background: var(--surface);
  font-size: 11.5px;
}
.ws {
  font-size: 11.5px;
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.spacer {
  flex: 1;
}
.hint {
  font-size: 12.5px;
  color: var(--muted);
  padding: 12px 0;
}
.hint.err {
  color: var(--danger);
}
.prompt {
  margin: 0;
  padding: 12px 14px;
  max-height: 52vh;
  overflow: auto;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 8px;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.6;
  white-space: pre-wrap;
  word-break: break-word;
  overscroll-behavior: contain;
}
</style>
