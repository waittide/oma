<script setup lang="ts">
import { Fa6FolderOpen, Fa6FolderTree, Fa6Gear, Fa6MasksTheater, Fa6Robot } from 'vue-icons-plus/fa6';
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
        <span class="dot"></span>
        {{ status === 'connected' ? '已连接 Daemon' : status === 'connecting' ? '正在连接…' : '未连接' }}
      </span>

      <span v-if="workspace" class="badge-pill" v-tip="workspace">
        <Fa6FolderOpen /> {{ workspace }}
      </span>

      <span v-if="ready?.active_model" class="badge-pill">
        <Fa6Robot /> {{ ready.active_model }}
      </span>

      <span v-if="ready?.active_agent" class="badge-pill">
        <Fa6MasksTheater /> {{ ready.active_agent }}
      </span>
    </div>

    <div class="header-right">
      <button class="btn-icon" v-tip="'查看工作区文件'" @click="$emit('open-files')">
        <Fa6FolderTree />
      </button>
      <button class="btn-icon" v-tip="'系统设置'" @click="$emit('open-settings')">
        <Fa6Gear />
      </button>
    </div>
  </header>
</template>
