<script setup lang="ts">
import type { ConnectionStatus } from '../useWebSocket';
import type { Ready } from '../types';

defineProps<{
  status: ConnectionStatus;
  ready: Ready | null;
  workspace: string;
}>();

defineEmits<{
  (e: 'open-files'): void;
  (e: 'open-settings'): void;
}>();
</script>

<template>
  <header class="top-header">
    <div class="header-left">
      <span
        :class="['badge', status === 'connected' ? 'badge-connected' : 'badge-connecting']"
      >
        <span class="dot">●</span>
        {{ status === 'connected' ? '已连接 Daemon' : status === 'connecting' ? '正在连接...' : '未连接' }}
      </span>

      <span v-if="workspace" class="badge-pill">
        📁 {{ workspace }}
      </span>

      <span v-if="ready?.active_model" class="badge-pill">
        🤖 {{ ready.active_model }}
      </span>

      <span v-if="ready?.active_agent" class="badge-pill">
        🎭 {{ ready.active_agent }}
      </span>
    </div>

    <div class="header-right">
      <button class="btn-icon" title="查看工作区文件" @click="$emit('open-files')">
        🗂️
      </button>
      <button class="btn-icon" title="系统设置" @click="$emit('open-settings')">
        ⚙️
      </button>
    </div>
  </header>
</template>
