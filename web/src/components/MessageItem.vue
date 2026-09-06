<script setup lang="ts">
import {
  Fa6Brain,
  Fa6ChevronDown,
  Fa6Download,
  Fa6Robot,
  Fa6ScrewdriverWrench,
  Fa6Seedling,
  Fa6User,
} from 'vue-icons-plus/fa6';
import { ref, computed } from 'vue';
import type { ChatMessage, Block } from '../types';

const props = defineProps<{
  message: ChatMessage;
}>();

defineEmits<{
  (e: 'fork-message', messageId: string): void;
}>();

const isThinkingOpen = ref(false);

const thinkingBlocks = computed(() => {
  return props.message.content.filter((b): b is Extract<Block, { type: 'thinking' }> => b.type === 'thinking');
});

const textBlocks = computed(() => {
  return props.message.content.filter((b): b is Extract<Block, { type: 'text' }> => b.type === 'text');
});

const toolUseBlocks = computed(() => {
  return props.message.content.filter((b): b is Extract<Block, { type: 'tool_use' }> => b.type === 'tool_use');
});

const toolResultBlocks = computed(() => {
  return props.message.content.filter((b): b is Extract<Block, { type: 'tool_result' }> => b.type === 'tool_result');
});

function formatTime(timestamp: number): string {
  const d = new Date(timestamp);
  return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function isDiffOutput(text: string): boolean {
  return text.includes('Unified Diff:') || text.includes('@@');
}

function parseDiffLines(diffText: string): Array<{ type: 'add' | 'del' | 'info' | 'normal'; text: string }> {
  return diffText.split('\n').map((line) => {
    if (line.startsWith('+') && !line.startsWith('+++')) return { type: 'add', text: line };
    if (line.startsWith('-') && !line.startsWith('---')) return { type: 'del', text: line };
    if (line.startsWith('@') || line.startsWith('diff') || line.startsWith('original') || line.startsWith('modified')) {
      return { type: 'info', text: line };
    }
    return { type: 'normal', text: line };
  });
}
</script>

<template>
  <div :class="['message-row', message.role]">
    <div :class="['avatar', message.role]">
      <Fa6User v-if="message.role === 'user'" />
      <Fa6Robot v-else />
    </div>

    <div class="message-column">
      <div :class="['message-card', message.role]">
        <div class="message-header">
          <span :class="['role-tag', message.role]">
            {{ message.role === 'user' ? 'USER' : 'ASSISTANT' }}
          </span>
          <span>{{ formatTime(message.created_at) }}</span>
          <button
            v-if="message.role === 'user'"
            class="btn-fork"
            v-tip="'以此节点为基准分叉重新生成'"
            @click="$emit('fork-message', message.id)"
          >
            <Fa6Seedling /> 分叉
          </button>
        </div>

        <!-- 思维链 Thinking 折叠展示 -->
        <div v-if="thinkingBlocks.length > 0" class="thinking-box">
          <div class="thinking-header" :class="{ open: isThinkingOpen }" @click="isThinkingOpen = !isThinkingOpen">
            <span><Fa6Brain  /> 深度思考过程 ({{ thinkingBlocks.reduce((acc, b) => acc + b.thinking.length, 0) }} 字符)</span>
            <Fa6ChevronDown class="chevron" />
          </div>
          <div v-if="isThinkingOpen" class="thinking-content">
            <div v-for="(th, idx) in thinkingBlocks" :key="idx">
              {{ th.thinking }}
            </div>
          </div>
        </div>

        <!-- 文本内容 Text -->
        <div v-for="(txt, idx) in textBlocks" :key="idx" class="message-body">
          {{ txt.text }}
        </div>

        <!-- 工具调用展示 ToolUse -->
        <div v-for="tu in toolUseBlocks" :key="tu.id" class="tool-call-card">
          <div class="tool-call-header">
            <span class="tool-name-badge"><Fa6ScrewdriverWrench  /> {{ tu.name }}</span>
            <span class="tool-status-badge success">已执行</span>
          </div>
          <div class="tool-body">
            <div class="label">// 输入参数 (Arguments):</div>
            <pre>{{ JSON.stringify(tu.input, null, 2) }}</pre>
          </div>
        </div>

        <!-- 工具结果 ToolResult -->
        <div v-for="tr in toolResultBlocks" :key="tr.tool_use_id" class="tool-call-card">
          <div class="tool-call-header">
            <span class="tool-name-badge"><Fa6Download  /> 工具结果回传</span>
            <span :class="['tool-status-badge', tr.is_error ? 'error' : 'success']">
              {{ tr.is_error ? '执行报错' : '成功返回' }}
            </span>
          </div>
          <div class="tool-body">
            <div v-if="isDiffOutput(tr.content)" class="diff-view">
              <div
                v-for="(dl, i) in parseDiffLines(tr.content)"
                :key="i"
                :class="{
                  'diff-line-add': dl.type === 'add',
                  'diff-line-del': dl.type === 'del',
                  'diff-line-info': dl.type === 'info',
                }"
              >
                {{ dl.text }}
              </div>
            </div>
            <pre v-else>{{ tr.content }}</pre>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.btn-fork {
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  background: transparent;
  border: 1px solid var(--border-subtle);
  color: var(--text-muted);
  font-size: 11px;
  font-family: inherit;
  padding: 2px 9px;
  border-radius: 999px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.btn-fork:hover {
  color: var(--success);
  border-color: var(--success);
  background: var(--success-soft);
}
</style>
