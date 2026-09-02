<script setup lang="ts">
import { ref, computed } from 'vue';
import type { ApprovalMode, Ready } from '../types';

const props = defineProps<{
  ready: Ready | null;
  isBusy: boolean;
}>();

const emit = defineEmits<{
  (e: 'send', content: string): void;
  (e: 'cancel'): void;
  (e: 'change-model', model: string): void;
  (e: 'change-agent', agent: string): void;
  (e: 'change-approval-mode', mode: ApprovalMode): void;
}>();

const inputContent = ref('');

const availableModels = computed(() => {
  if (!props.ready?.providers) return [];
  const list: string[] = [];
  for (const [providerId, models] of Object.entries(props.ready.providers)) {
    for (const m of models) {
      list.push(`${providerId}/${m.id}`);
    }
  }
  return list;
});

function handleKeyDown(event: KeyboardEvent) {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    handleSend();
  }
}

function handleSend() {
  const text = inputContent.value.trim();
  if (!text) return;
  emit('send', text);
  inputContent.value = '';
}
</script>

<template>
  <div class="input-container">
    <div class="input-controls">
      <div class="controls-group">
        <!-- 模型选择 -->
        <select
          class="select-control"
          :value="ready?.active_model"
          title="切换激活模型"
          @change="(e) => $emit('change-model', (e.target as HTMLSelectElement).value)"
        >
          <option v-for="m in availableModels" :key="m" :value="m">
            {{ m }}
          </option>
          <option v-if="availableModels.length === 0" :value="ready?.active_model">
            {{ ready?.active_model || '默认模型' }}
          </option>
        </select>

        <!-- Agent 模板选择 -->
        <select
          class="select-control"
          :value="ready?.active_agent"
          title="切换预设 Agent"
          @change="(e) => $emit('change-agent', (e.target as HTMLSelectElement).value)"
        >
          <option v-for="a in ready?.agents" :key="a.id" :value="a.id">
            🎭 {{ a.name }}
          </option>
          <option v-if="!ready?.agents?.length" :value="ready?.active_agent">
            🎭 {{ ready?.active_agent || 'task' }}
          </option>
        </select>

        <!-- 权限审批模式 -->
        <select
          class="select-control"
          :value="ready?.approval_mode"
          title="切换审批防护模式"
          @change="(e) => $emit('change-approval-mode', (e.target as HTMLSelectElement).value as ApprovalMode)"
        >
          <option value="normal">🛡️ Normal 模式 (危险弹窗)</option>
          <option value="strict">🔒 Strict 模式 (全部弹窗)</option>
          <option value="auto">⚡ Auto 模式 (全免审批)</option>
        </select>
      </div>

      <div class="controls-group">
        <!-- 正在执行时展示级联 Cancel 中断按钮 -->
        <button v-if="isBusy" class="btn-cancel" @click="$emit('cancel')">
          ⏹️ 停止生成 (Cancel)
        </button>
      </div>
    </div>

    <div class="textarea-wrapper">
      <textarea
        v-model="inputContent"
        class="chat-input"
        placeholder="给 Oma 发送消息或任务指令... (Enter 发送，Shift+Enter 换行)"
        rows="2"
        @keydown="handleKeyDown"
      ></textarea>

      <button
        class="btn-send"
        :disabled="!inputContent.trim()"
        @click="handleSend"
      >
        发送
      </button>
    </div>
  </div>
</template>
