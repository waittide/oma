<script setup lang="ts">
import { Fa6Bolt, Fa6Moon, Fa6Plus, Fa6Sun, Fa6Trash } from 'vue-icons-plus/fa6';
import type { SessionRecord } from '../types';
import { theme, toggleTheme } from '../useTheme';

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

function relativeTime(ts: number): string {
  const diff = Date.now() - ts;
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return '刚刚';
  if (mins < 60) return `${mins} 分钟前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} 天前`;
  return new Date(ts).toLocaleDateString();
}
</script>

<template>
  <aside class="sidebar">
    <div class="sidebar-header">
      <div class="brand-title">
        <span class="logo"><Fa6Bolt /></span>
        <span>Oma</span>
      </div>
    </div>

    <button class="session-new" @click="$emit('create-session')">
      <Fa6Plus /> 新建会话
    </button>

    <div class="sidebar-section-label">会话</div>

    <div class="session-list">
      <div
        v-for="s in sessions"
        :key="s.session_id"
        :class="['session-item', s.session_id === currentSessionId ? 'active' : '']"
        @click="$emit('select-session', s.session_id)"
      >
        <div class="session-item-main">
          <div class="session-title">
            {{ s.title }}
          </div>
          <div class="session-meta">{{ relativeTime(s.updated_at) }} · {{ s.active_model }}</div>
        </div>
        <button
          class="btn-delete-session"
          v-tip="'删除会话'"
          @click.stop="$emit('delete-session', s.session_id)"
        >
          <Fa6Trash />
        </button>
      </div>

      <div v-if="sessions.length === 0" class="sidebar-empty">
        <div class="empty-hint-icon">…</div>
        <div>暂无会话</div>
      </div>
    </div>

    <div class="sidebar-footer">
      <div class="workspace-badge" v-tip="workspace">
        {{ workspace || '默认工作区' }}
      </div>
      <button class="btn-icon" v-tip="theme === 'dark' ? '切换到浅色主题' : '切换到深色主题'" @click="toggleTheme">
        <Fa6Sun v-if="theme === 'dark'" />
        <Fa6Moon v-else />
      </button>
    </div>
  </aside>
</template>
