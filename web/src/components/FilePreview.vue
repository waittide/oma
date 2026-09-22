<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { UiButton, UiIconButton, UiTooltip } from '@waittide/ui';
import { LuArrowLeft, LuCheck, LuCopy, LuRefreshCw } from 'vue-icons-plus/lu';
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import diff from 'highlight.js/lib/languages/diff';
import dockerfile from 'highlight.js/lib/languages/dockerfile';
import go from 'highlight.js/lib/languages/go';
import graphql from 'highlight.js/lib/languages/graphql';
import ini from 'highlight.js/lib/languages/ini';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import kotlin from 'highlight.js/lib/languages/kotlin';
import less from 'highlight.js/lib/languages/less';
import makefile from 'highlight.js/lib/languages/makefile';
import markdown from 'highlight.js/lib/languages/markdown';
import python from 'highlight.js/lib/languages/python';
import ruby from 'highlight.js/lib/languages/ruby';
import rust from 'highlight.js/lib/languages/rust';
import scss from 'highlight.js/lib/languages/scss';
import sql from 'highlight.js/lib/languages/sql';
import swift from 'highlight.js/lib/languages/swift';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';
import { api } from '../api';
import { copyText } from '../lib/clipboard';
import { renderMarkdown } from '../lib/format';
import { fileLanguage } from '../lib/fileKinds';
import { useTranslations } from '../composables/i18n';
import { activeSession } from '../stores/sessions';

// 按需注册：完整包会把 ~200 种语言都打进产物，这里只挑面板里真会遇到的
const LANGUAGES = {
  bash,
  c,
  cpp,
  csharp,
  css,
  diff,
  dockerfile,
  go,
  graphql,
  ini,
  java,
  javascript,
  json,
  kotlin,
  less,
  makefile,
  markdown,
  python,
  ruby,
  rust,
  scss,
  sql,
  swift,
  typescript,
  xml,
  yaml,
};
for (const [id, definition] of Object.entries(LANGUAGES)) hljs.registerLanguage(id, definition);

/** 超过这个行数就退化成纯文本：高亮要重建全部 token 元素，上千行的文件会拖住面板 */
const HIGHLIGHT_MAX_LINES = 1000;

/**
 * 工作区文件预览。
 *
 * Markdown 按渲染后的样子展示（和聊天里的正文一致），其余文件按语法高亮 + 行号展示：
 * 面板窄，等宽 + 行号更容易对照 `grep` 输出里的 `path:line:`。语言由扩展名判定，
 * 判定不出或文件过长时退回纯文本——两种视图都用同一套行号网格。
 */
const props = defineProps<{ path: string }>();
const emit = defineEmits<{ back: [] }>();

const { t } = useTranslations('panel');

const loading = ref(false);
const error = ref('');
const content = ref('');
/** 服务端只回了文件开头（超过预览上限），要在标题栏说明 */
const truncated = ref(false);
const copied = ref(false);
let copyTimer: ReturnType<typeof setTimeout> | undefined;

const name = computed(() => props.path.split('/').pop() ?? props.path);
const isMarkdown = computed(() => /\.(md|markdown)$/i.test(props.path));
const lines = computed(() => content.value.split('\n'));
/** 高亮后的 HTML；空串表示走纯文本渲染（未知语言、超长文件或高亮失败） */
const highlighted = computed(() => {
  if (isMarkdown.value || !content.value || lines.value.length > HIGHLIGHT_MAX_LINES) return '';
  const language = fileLanguage(props.path);
  if (language === 'plaintext') return '';
  try {
    return hljs.highlight(content.value, { language, ignoreIllegals: true }).value;
  } catch {
    return '';
  }
});

async function load() {
  const workspace = activeSession.value?.workspace;
  if (!workspace || !props.path) return;
  loading.value = true;
  error.value = '';
  try {
    const resp = await api.workspaceFile(workspace, props.path);
    content.value = resp.content;
    truncated.value = !!resp.truncated;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
    content.value = '';
    truncated.value = false;
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
      <span v-if="truncated" class="trunc">{{ t('truncated') }}</span>
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
      <!-- 高亮分支：行号列 sticky 固定，代码列横向滚动 -->
      <div v-if="highlighted" class="hl">
        <div class="ln-col" aria-hidden="true">
          <span v-for="(_, i) in lines" :key="i">{{ i + 1 }}</span>
        </div>
        <pre class="hl-code"><code class="hljs" v-html="highlighted" /></pre>
      </div>
      <!-- 纯文本回退：没有对应语言（或文件过长）时不假装高亮 -->
      <div v-else>
        <div v-for="(line, i) in lines" :key="i" class="code-row">
          <span class="ln">{{ i + 1 }}</span>
          <span class="lt">{{ line || ' ' }}</span>
        </div>
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
/* 只回了开头一段的提示：标题旁的小胶囊，不抢文件名 */
.trunc {
  flex-shrink: 0;
  padding: 1px 6px;
  border-radius: 99px;
  font-size: 10.5px;
  color: var(--warning);
  background: color-mix(in srgb, var(--warning) 14%, transparent);
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
/*
 * 高亮视图：行号列与代码列各自成列，靠同一 line-height 对齐。
 * 行号列 sticky 在左侧，横向滚动代码时不会被卷走。
 */
.hl {
  display: flex;
  align-items: flex-start;
  min-width: max-content;
}
.ln-col {
  position: sticky;
  left: 0;
  z-index: 1;
  display: flex;
  flex-direction: column;
  flex-shrink: 0;
  width: 44px;
  padding-right: 10px;
  text-align: right;
  color: var(--overlay0);
  background: var(--surface);
  user-select: none;
}
.hl-code {
  margin: 0;
  padding-right: 12px;
  font-family: inherit;
  font-size: inherit;
  line-height: inherit;
  tab-size: 4;
}
.hl-code code {
  font-family: inherit;
}
/*
 * 高亮主题：token 颜色全部取调色板令牌（缺省回退到 catppuccin 的对应色），
 * 因此换主题 / 换调色板时高亮跟着走，不需要为明暗各准备一份主题样式表。
 */
.hl :deep(.hljs-comment),
.hl :deep(.hljs-quote) {
  color: var(--overlay1, #7f849c);
  font-style: italic;
}
.hl :deep(.hljs-keyword),
.hl :deep(.hljs-selector-tag),
.hl :deep(.hljs-literal),
.hl :deep(.hljs-section),
.hl :deep(.hljs-doctag),
.hl :deep(.hljs-name),
.hl :deep(.hljs-strong) {
  color: var(--mauve, #cba6f7);
}
.hl :deep(.hljs-string),
.hl :deep(.hljs-regexp),
.hl :deep(.hljs-addition),
.hl :deep(.hljs-attribute) {
  color: var(--green, #a6e3a1);
}
.hl :deep(.hljs-number),
.hl :deep(.hljs-symbol),
.hl :deep(.hljs-bullet),
.hl :deep(.hljs-variable),
.hl :deep(.hljs-template-variable) {
  color: var(--peach, #fab387);
}
.hl :deep(.hljs-title),
.hl :deep(.hljs-built_in),
.hl :deep(.hljs-type) {
  color: var(--blue, #89b4fa);
}
.hl :deep(.hljs-attr),
.hl :deep(.hljs-property),
.hl :deep(.hljs-params),
.hl :deep(.hljs-selector-class),
.hl :deep(.hljs-selector-id) {
  color: var(--yellow, #f9e2af);
}
.hl :deep(.hljs-meta),
.hl :deep(.hljs-tag) {
  color: var(--sapphire, #74c7ec);
}
.hl :deep(.hljs-deletion) {
  color: var(--red, #f38ba8);
}
.hl :deep(.hljs-link),
.hl :deep(.hljs-emphasis) {
  color: var(--sky, #89dceb);
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
