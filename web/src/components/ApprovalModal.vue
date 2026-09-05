<script setup lang="ts">
import { Fa6TriangleExclamation, Fa6HourglassHalf, Fa6Ban, Fa6Check, Fa6ShieldHalved } from 'vue-icons-plus/fa6';
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
    <div class="modal-dialog" style="border-color: var(--warning);">
      <div class="modal-header" style="background: rgba(210, 153, 34, 0.1); color: var(--warning);">
        <span><Fa6TriangleExclamation style="vertical-align: -2px;" /> 高危操作审批请求 (Human-in-the-Loop)</span>
        <span class="badge badge-connecting"><Fa6HourglassHalf style="vertical-align: -2px;" /> 倒计时: {{ remainingSeconds }}s</span>
      </div>

      <div class="modal-body">
        <div>
          Agent 正在尝试调用工具：
          <strong style="color: var(--accent); font-family: var(--font-mono); font-size: 15px;">
            {{ approval.name }}
          </strong>
        </div>

        <div style="font-size: 12px; color: var(--text-secondary);">
          调用参数与指令摘要：
        </div>

        <pre style="background: var(--bg-base); padding: 10px; border-radius: 6px; font-family: var(--font-mono); font-size: 12px; max-height: 200px; overflow-y: auto; border: 1px solid var(--border-default); white-space: pre-wrap;">{{ approval.summary }}</pre>

        <div style="font-size: 12px; color: var(--text-muted); line-height: 1.4;">
          * 多端协同遵循「先到先得」机制：任意客户端做出决断将立即对全端生效；若倒计时 120 秒超时未响应将自动按 Deny（拒绝）处理。
        </div>
      </div>

      <div class="modal-footer">
        <button class="btn-danger" @click="$emit('decision', 'deny')">
          <Fa6Ban style="vertical-align: -2px;" /> 拒绝 (Deny)
        </button>
        <button class="btn-default" @click="$emit('decision', 'allow_once')">
          <Fa6Check style="vertical-align: -2px;" /> 仅本次允许
        </button>
        <button class="btn-primary" @click="$emit('decision', 'allow_session')">
          <Fa6ShieldHalved style="vertical-align: -2px;" /> 本会话永久允许
        </button>
      </div>
    </div>
  </div>
</template>
