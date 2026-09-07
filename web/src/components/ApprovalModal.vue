<script setup lang="ts">
import { Fa6Ban, Fa6Check, Fa6ShieldHalved } from 'vue-icons-plus/fa6';
import type { ApprovalDecision, PermissionRequestedData } from '../types';

defineProps<{
  approval: PermissionRequestedData;
  remainingSeconds: number;
}>();

defineEmits<{
  (e: 'decision', decision: ApprovalDecision): void;
}>();
</script>

<template>
  <div class="modal-overlay">
    <div class="approval-card" role="alertdialog" aria-modal="true">
      <div class="approval-strip">
        <span class="dot"></span>
        <span>高危操作审批请求</span>
        <span class="approval-count">Human-in-the-Loop · {{ remainingSeconds }}s</span>
      </div>

      <div class="approval-body">
        <div class="approval-headline">
          Agent 正在尝试调用工具
          <strong>{{ approval.name }}</strong>
        </div>
        <div class="approval-command">{{ approval.summary }}</div>
        <div class="field-hint">
          多端协同遵循「先到先得」：任意客户端做出决断立即对全端生效；倒计时结束未响应将自动拒绝。
        </div>
      </div>

      <div class="approval-actions">
        <button class="btn-cancel" @click="$emit('decision', 'deny')">
          <Fa6Ban /> 拒绝
        </button>
        <button class="btn-default" @click="$emit('decision', 'allow_once')">
          <Fa6Check /> 仅本次允许
        </button>
        <button class="btn-primary" @click="$emit('decision', 'allow_session')">
          <Fa6ShieldHalved /> 本会话允许
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.approval-card {
  overflow: hidden;
  width: min(640px, 100%);
  border: 1px solid var(--warning);
  border-radius: 20px;
  background: var(--bg-card);
  box-shadow: var(--shadow-lg);
  animation: fade-in-up 0.16s var(--ease-in-out);
}

.approval-strip {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 16px;
  background: var(--warning-soft);
  color: var(--warning-strong);
  font-size: 13px;
  line-height: 18px;
}

.approval-strip .dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--warning);
  animation: pulse-dot 1s ease-in-out infinite;
}

.approval-count {
  margin-left: auto;
  font-variant-numeric: tabular-nums;
}

.approval-body {
  display: flex;
  flex-direction: column;
  gap: 8px;
  max-height: 336px;
  overflow-y: auto;
  padding: 14px 16px 0;
}

.approval-headline {
  color: var(--text-primary);
  font-size: 15px;
  line-height: 24px;
  font-weight: 500;
}

.approval-headline strong {
  color: var(--accent);
  font-family: var(--font-mono);
}

.approval-command {
  color: var(--text-secondary);
  font-family: var(--font-mono);
  font-size: 13px;
  line-height: 20px;
  word-break: break-all;
  background: var(--bg-code);
  border-radius: 10px;
  padding: 10px 12px;
  max-height: 200px;
  overflow-y: auto;
  white-space: pre-wrap;
}

.approval-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 14px 16px;
}

.approval-actions .btn-cancel:hover {
  background: var(--danger-soft);
  color: var(--danger);
}
</style>
