<script setup lang="ts">
import { Fa6Bolt, Fa6Plus, Fa6RegCommentDots, Fa6Trash } from 'vue-icons-plus/fa6';
import type { SessionRecord } from '../types';

defineProps<{
  sessions: SessionRecord[];
  currentSessionId: string;
  workspace: string;
}>();

defineEmits<{
  (e: 'select-session', id: string): void;
  (e: 'create-session'): void;
  (e: 'delete-session', id: string): void;
}>();
</script>

<template>
  <aside class="sidebar">
    <div class="sidebar-header">
      <div class="brand-title">
        <span class="logo"><Fa6Bolt /></span>
        <span>Oma Agent</span>
      </div>
      <div class="sidebar-actions">
        <button class="btn-icon" title="新建会话" @click="$emit('create-session')">
          <Fa6Plus />
        </button>
      </div>
    </div>

    <div class="session-list">
      <div
        v-for="s in sessions"
        :key="s.session_id"
        :class="['session-item', s.session_id === currentSessionId ? 'active' : '']"
        @click="$emit('select-session', s.session_id)"
      >
        <div class="session-title" :title="s.title">
          <Fa6RegCommentDots style="vertical-align: -2px; margin-right: 4px;" /> {{ s.title }}
        </div>
        <button
          class="btn-delete-session"
          title="删除会话"
          @click.stop="$emit('delete-session', s.session_id)"
        >
          <Fa6Trash />
        </button>
      </div>

      <div v-if="sessions.length === 0" style="padding: 16px; color: var(--text-muted); font-size: 12px; text-align: center;">
        暂无会话，请点击右上角 <Fa6Plus style="vertical-align: -2px;" /> 创建
      </div>
    </div>

    <div class="sidebar-footer">
      <div class="workspace-badge" :title="workspace">
        {{ workspace || '默认工作区' }}
      </div>
    </div>
  </aside>
</template>
