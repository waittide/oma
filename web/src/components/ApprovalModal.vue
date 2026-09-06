<script setup lang="ts">
import { Fa6Ban, Fa6Check, Fa6HourglassHalf, Fa6ShieldHalved, Fa6TriangleExclamation } from 'vue-icons-plus/fa6';
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
    <div class="modal-dialog approval">
      <div class="modal-header">
        <span style="display: inline-flex; align-items: center; gap: 8px;">
          <Fa6TriangleExclamation /> 高危操作审批请求
        </span>
        <span class="badge badge-connecting" style="font-variant-numeric: tabular-nums;">
          <Fa6HourglassHalf /> {{ remainingSeconds }}s
        </span>
      </div>

      <div class="modal-body">
        <div>
          Agent 正在尝试调用工具
          <strong style="color: var(--accent); font-family: var(--font-mono); font-size: 15px;">
            {{ approval.name }}
          </strong>
        </div>

        <div class="field-label">调用参数与指令摘要</div>

        <pre class="approval-summary">{{ approval.summary }}</pre>

        <div class="field-hint">
          多端协同遵循「先到先得」机制：任意客户端做出决断将立即对全端生效；倒计时结束未响应将自动按拒绝处理。
        </div>
      </div>

      <div class="modal-footer">
        <button class="btn-danger" @click="$emit('decision', 'deny')">
          <Fa6Ban  /> 拒绝
        </button>
        <button class="btn-default" @click="$emit('decision', 'allow_once')">
          <Fa6Check  /> 仅本次允许
        </button>
        <button class="btn-primary" @click="$emit('decision', 'allow_session')">
          <Fa6ShieldHalved  /> 本会话永久允许
        </button>
      </div>
    </div>
  </div>
</template>
