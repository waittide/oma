<script setup lang="ts">
import { Fa6Bolt, Fa6CircleStop, Fa6Lock, Fa6MasksTheater, Fa6PaperPlane, Fa6Robot, Fa6ShieldHalved } from 'vue-icons-plus/fa6';
import { ref, computed } from 'vue';
import type { ApprovalMode, Ready } from '../types';
import OuiSelect, { type OuiSelectOption } from './oui/OuiSelect.vue';

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

const modelOptions = computed<OuiSelectOption[]>(() => {
  const list: OuiSelectOption[] = [];
  for (const [providerId, models] of Object.entries(props.ready?.providers ?? {})) {
    for (const m of models) {
      const sel = `${providerId}/${m.id}`;
      list.push({ value: sel, label: sel, description: m.name !== m.id ? m.name : undefined });
    }
  }
  // 当前激活模型不在目录中 (如 provider 未配 models 清单) 时仍可选中展示
  if (props.ready?.active_model && !list.some((o) => o.value === props.ready?.active_model)) {
    list.unshift({ value: props.ready.active_model, label: props.ready.active_model });
  }
  return list;
});

const agentOptions = computed<OuiSelectOption[]>(() =>
  (props.ready?.agents ?? []).map((a) => ({ value: a.id, label: a.name, description: a.description })),
);

const APPROVAL_DESC: Record<ApprovalMode, string> = {
  normal: '常规工具免批，危险操作请求审批',
  strict: '所有工具调用均需审批',
  auto: '全部自动放行',
};

const approvalOptions: OuiSelectOption[] = (['normal', 'strict', 'auto'] as ApprovalMode[]).map(
  (m) => ({ value: m, label: m, description: APPROVAL_DESC[m] }),
);

const approvalIcon = computed(() => {
  switch (props.ready?.approval_mode) {
    case 'strict':
      return Fa6Lock;
    case 'auto':
      return Fa6Bolt;
    default:
      return Fa6ShieldHalved;
  }
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
    <div class="input-inner">
      <div class="composer-card">
        <textarea
          v-model="inputContent"
          class="chat-input"
          placeholder="给 Oma 发送消息或任务指令…"
          rows="1"
          @keydown="handleKeyDown"
        ></textarea>

        <div class="composer-row">
          <div class="input-controls">
            <!-- 模型选择 -->
            <OuiSelect
              :model-value="ready?.active_model"
              :options="modelOptions"
              placeholder="默认模型"
              v-tip="'切换激活模型'"
              @update:model-value="(v) => $emit('change-model', v)"
            >
              <template #icon><span class="control-icon"><Fa6Robot /></span></template>
            </OuiSelect>

            <!-- Agent 模板选择 -->
            <OuiSelect
              :model-value="ready?.active_agent"
              :options="agentOptions"
              placeholder="task"
              v-tip="'切换预设 Agent'"
              @update:model-value="(v) => $emit('change-agent', v)"
            >
              <template #icon><span class="control-icon"><Fa6MasksTheater /></span></template>
            </OuiSelect>

            <!-- 权限审批模式 -->
            <OuiSelect
              :model-value="ready?.approval_mode"
              :options="approvalOptions"
              v-tip="'切换审批防护模式'"
              @update:model-value="(v) => $emit('change-approval-mode', v as ApprovalMode)"
            >
              <template #icon>
                <span class="control-icon">
                  <component :is="approvalIcon" />
                </span>
              </template>
            </OuiSelect>
          </div>

          <div class="trailing">
            <!-- 正在执行时展示 Cancel 中断按钮 -->
            <button v-if="isBusy" class="btn-cancel" @click="$emit('cancel')">
              <Fa6CircleStop /> 停止
            </button>
            <button
              class="btn-send"
              v-tip="isBusy ? '加入指令队列' : '发送 (Enter)'"
              :disabled="!inputContent.trim()"
              @click="handleSend"
            >
              <Fa6PaperPlane />
            </button>
          </div>
        </div>
      </div>

      <div class="input-hint">Enter 发送 · Shift+Enter 换行{{ isBusy ? ' · 执行中的新指令将进入队列' : '' }}</div>
    </div>
  </div>
</template>

<style scoped>
.trailing {
  flex: none;
  display: flex;
  align-items: center;
  gap: 12px;
  margin-left: auto;
}
</style>
