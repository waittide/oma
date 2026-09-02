<script setup lang="ts">
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
  <div class="message-row">
    <div :class="['message-card', message.role]">
      <div class="message-header">
        <span :class="['role-tag', message.role]">
          {{ message.role }}
        </span>
        <span>{{ formatTime(message.created_at) }}</span>
        <div style="flex: 1"></div>
        <button
          v-if="message.role === 'user'"
          class="btn-icon"
          style="width: 20px; height: 20px; font-size: 10px;"
          title="以此节点为基准分叉重新生成"
          @click="$emit('fork-message', message.id)"
        >
          🌱
        </button>
      </div>

      <!-- 思维链 Thinking 折叠展示 -->
      <div v-if="thinkingBlocks.length > 0" class="thinking-box">
        <div class="thinking-header" @click="isThinkingOpen = !isThinkingOpen">
          <span>🧠 深度思考过程 ({{ thinkingBlocks.reduce((acc, b) => acc + b.thinking.length, 0) }} 字符)</span>
          <span>{{ isThinkingOpen ? '▲ 折叠' : '▼ 展开' }}</span>
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
          <span class="tool-name-badge">🔧 {{ tu.name }}</span>
          <span class="tool-status-badge success">已执行</span>
        </div>
        <div class="tool-body">
          <div style="color: var(--text-muted); margin-bottom: 4px;">// 输入参数 (Arguments):</div>
          <pre>{{ JSON.stringify(tu.input, null, 2) }}</pre>
        </div>
      </div>

      <!-- 工具结果 ToolResult (如果是 User 角色包裹的执行结果) -->
      <div
        v-for="tr in message.content.filter((b): b is Extract<Block, { type: 'tool_result' }> => b.type === 'tool_result')"
        :key="tr.tool_use_id"
        class="tool-call-card"
      >
        <div class="tool-call-header">
          <span class="tool-name-badge">📥 工具结果回传</span>
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
</template>
