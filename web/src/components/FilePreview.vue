<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { UiButton, UiIconButton, UiTooltip } from '@waittide/ui';
import { LuArrowLeft, LuCheck, LuCopy, LuRefreshCw } from 'vue-icons-plus/lu';
import { api } from '../api';
import { copyText } from '../lib/clipboard';
import { renderMarkdown } from '../lib/format';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';

/**
 * 工作区文件预览。
 *
 * Markdown 按渲染后的样子展示（和聊天里的正文一致），其余文件按纯文本展示并带行号：
 * 面板窄，等宽 + 行号更容易对照 `grep` 输出里的 `path:line:`。
 */
const props = defineProps<{ path: string }>();
const emit = defineEmits<{ back: [] }>();

const { t } = useTranslations('panel');

const loading = ref(false);
const error = ref('');
const content = ref('');
const copied = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | undefined;

const name = computed(() => props.path.split('/').pop() ?? props.path);
const isMarkdown = computed(() => /\.(md|markdown)$/i.test(props.path));
const lines = computed(() => content.value.split('\n'));

async function load() {
  const workspace = activeSession.value?.workspace;
  if (!workspace || !props.path) return;
  loading.value = true;
  error.value = '';
  try {
    const resp = await api.workspaceFile(workspace, props.path);
    content.value = resp.content;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
    content.value = '';
  } finally {
    loading.value = false;
  }
}

async function copy() {
  await copyText(content.value);
  copied.value = true;
  if (copyTimer) clearTimeout(copyTimer);
  copyTimer = setTimeout(() => {
    copied.value = false;
  }, 1200);
}

watch(() => props.path, load, { immediate: true });
</script>

<template>
  <div class="preview">
    <div class="bar">
      <UiTooltip :content="t('backToTree')" align="start">
        <UiIconButton size="sm" :label="t('backToTree')" @click="emit('back')">
          <LuArrowLeft :size="13" />
        </UiIconButton>
      </UiTooltip>
      <span class="name" :title="path">{{ name }}</span>
      <span class="spacer" />
      <UiTooltip :content="t('reload')" align="end">
        <UiIconButton size="sm" :label="t('reload')" @click="load">
          <LuRefreshCw :size="13" />
        </UiIconButton>
      </UiTooltip>
      <UiTooltip :content="t('copyContent')" align="end">
        <UiIconButton size="sm" :label="t('copyContent')" :disabled="!content" @click="copy">
          <LuCheck v-if="copied" :size="13" />
          <LuCopy v-else :size="13" />
        </UiIconButton>
      </UiTooltip>
    </div>

    <div v-if="loading" class="hint">{{ t('loading') }}</div>
    <div v-else-if="error" class="hint err">
      <p>{{ error }}</p>
      <UiButton variant="soft" tone="neutral" size="sm" @click="load">{{ t('reload') }}</UiButton>
    </div>
    <div v-else-if="isMarkdown" class="md" v-html="renderMarkdown(content)" />
    <div v-else class="code">
      <div v-for="(line, i) in lines" :key="i" class="code-row">
        <span class="ln">{{ i + 1 }}</span>
        <span class="lt">{{ line || ' ' }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.preview {
  display: flex;
  flex-direction: column;
  min-height: 0;
  height: 100%;
}
.bar {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-shrink: 0;
  padding: 6px 8px;
  border-bottom: 1px solid var(--line);
}
.name {
  font-size: 12.5px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.spacer {
  flex: 1;
}
.hint {
  padding: 14px;
  font-size: 12.5px;
  color: var(--muted);
}
.hint.err {
  color: var(--danger);
}
/* 纯文本视图：行号固定宽度，等宽字体，横向不换行（代码靠右滚动看） */
.code {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 8px 0;
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.55;
  overscroll-behavior: contain;
}
.code-row {
  display: flex;
  gap: 10px;
  padding: 0 10px;
}
.code-row:hover {
  background: var(--surface-hover);
}
.ln {
  flex-shrink: 0;
  width: 34px;
  text-align: right;
  color: var(--overlay0);
  user-select: none;
}
.lt {
  white-space: pre;
}
.md {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 10px 12px;
  font-size: 13px;
  line-height: 1.65;
  overscroll-behavior: contain;
}
.md :deep(pre) {
  margin: 8px 0 12px;
  padding: 10px 12px;
  background: var(--surface);
  border-radius: 8px;
  overflow-x: auto;
  font-size: 12px;
}
.md :deep(code) {
  font-family: var(--font-mono);
  font-size: 12px;
  background: var(--surface);
  border-radius: 4px;
  padding: 1px 4px;
}
.md :deep(pre code) {
  background: transparent;
  padding: 0;
}
.md :deep(a) {
  color: var(--accent);
}
.md :deep(table) {
  border-collapse: collapse;
  font-size: 12px;
}
.md :deep(th),
.md :deep(td) {
  border: 1px solid var(--line);
  padding: 4px 8px;
}
</style>
