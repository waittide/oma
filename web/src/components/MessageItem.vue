<script setup lang="ts">
import {
  Fa6Brain,
  Fa6ChevronDown,
  Fa6Download,
  Fa6ScrewdriverWrench,
  Fa6Seedling,
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

const thinkingChars = computed(() => thinkingBlocks.value.reduce((acc, b) => acc + b.thinking.length, 0));

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
    <div class="message-main">
      <!-- 用户消息: 右对齐气泡 (仅文本)；工具结果以独立卡片跟随 -->
      <template v-if="message.role === 'user'">
        <div v-if="textBlocks.length" class="message-card user">
          <div v-for="(txt, idx) in textBlocks" :key="idx" class="message-body">{{ txt.text }}</div>
        </div>

        <div v-for="tr in toolResultBlocks" :key="tr.tool_use_id" class="tool-call-card">
          <div class="tool-call-header">
            <span class="flow-icon"><Fa6Download /></span>
            <span class="tool-name-badge neutral">工具结果</span>
            <span class="flow-dot"></span>
            <span class="flow-summary">{{ tr.is_error ? '执行报错' : '成功返回' }}</span>
            <span :class="['tool-status-badge', tr.is_error ? 'error' : 'success']">
              {{ tr.is_error ? 'error' : 'ok' }}
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

        <div class="msg-actions">
          <button class="btn-fork" v-tip="'以此节点为基准分叉重新生成'" @click="$emit('fork-message', message.id)">
            <Fa6Seedling /> 分叉
          </button>
        </div>
      </template>

      <!-- 助手消息: 无卡片平面流 -->
      <template v-else>
        <div v-if="thinkingBlocks.length" class="thinking-box">
          <button
            type="button"
            class="flow-row"
            :class="{ open: isThinkingOpen }"
            @click="isThinkingOpen = !isThinkingOpen"
          >
            <span class="flow-icon"><Fa6Brain /></span>
            <span class="flow-title">深度思考</span>
            <span class="flow-dot"></span>
            <span class="flow-summary">{{ thinkingChars }} 字符</span>
            <Fa6ChevronDown class="chevron" />
          </button>
          <div v-if="isThinkingOpen" class="flow-body">
            <div v-for="(th, idx) in thinkingBlocks" :key="idx">{{ th.thinking }}</div>
          </div>
        </div>

        <div v-for="(txt, idx) in textBlocks" :key="idx" class="message-body">{{ txt.text }}</div>

        <div v-for="tu in toolUseBlocks" :key="tu.id" class="tool-call-card">
          <div class="tool-call-header">
            <span class="flow-icon"><Fa6ScrewdriverWrench /></span>
            <span class="tool-name-badge">{{ tu.name }}</span>
            <span class="flow-dot"></span>
            <span class="flow-summary">{{ JSON.stringify(tu.input) }}</span>
            <span class="tool-status-badge success">已执行</span>
          </div>
          <div class="tool-body">
            <pre>{{ JSON.stringify(tu.input, null, 2) }}</pre>
          </div>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.btn-fork {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  background: transparent;
  border: 0.5px solid var(--border-strong);
  color: var(--text-secondary);
  font-size: 12px;
  font-family: inherit;
  height: 26px;
  padding: 0 10px;
  border-radius: 13px;
  cursor: pointer;
  transition: all var(--dur-fast) ease;
}

.btn-fork:hover {
  color: var(--text-primary);
  background: var(--wash-hover);
}

.tool-name-badge.neutral {
  color: var(--text-secondary);
  font-weight: 400;
}
</style>
