<script setup lang="ts">
import { Fa6FolderOpen, Fa6Robot, Fa6MasksTheater, Fa6FolderTree, Fa6Gear } from 'vue-icons-plus/fa6';
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
        <Fa6FolderOpen style="vertical-align: -2px;" /> {{ workspace }}
      </span>

      <span v-if="ready?.active_model" class="badge-pill">
        <Fa6Robot style="vertical-align: -2px;" /> {{ ready.active_model }}
      </span>

      <span v-if="ready?.active_agent" class="badge-pill">
        <Fa6MasksTheater style="vertical-align: -2px;" /> {{ ready.active_agent }}
      </span>
    </div>

    <div class="header-right">
      <button class="btn-icon" title="查看工作区文件" @click="$emit('open-files')">
        <Fa6FolderTree />
      </button>
      <button class="btn-icon" title="系统设置" @click="$emit('open-settings')">
        <Fa6Gear />
      </button>
    </div>
  </header>
</template>
