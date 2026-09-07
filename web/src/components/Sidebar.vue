<script setup lang="ts">
import {
  Fa6Bolt,
  Fa6ChevronDown,
  Fa6FolderOpen,
  Fa6Moon,
  Fa6Plus,
  Fa6RegFolderOpen,
  Fa6Sun,
  Fa6Trash,
} from 'vue-icons-plus/fa6';
import { computed, ref } from 'vue';
import type { SessionRecord } from '../types';
import { isDark, toggleMode } from '../theme';

const props = defineProps<{
  sessions: SessionRecord[];
  currentSessionId: string;
  workspace: string;
}>();

const emit = defineEmits<{
  (e: 'select-session', id: string): void;
  (e: 'create-session', workspace?: string): void;
  (e: 'delete-session', id: string): void;
}>();

// ===== 按工作区分组 =====
interface SessionGroup {
  workspace: string;
  basename: string;
  sessions: SessionRecord[];
}

const groups = computed<SessionGroup[]>(() => {
  const map = new Map<string, SessionRecord[]>();
  for (const s of props.sessions) {
    const list = map.get(s.workspace) ?? [];
    list.push(s);
    map.set(s.workspace, list);
  }
  return [...map.entries()]
    .map(([workspace, list]) => ({
      workspace,
      basename: workspace.split('/').filter(Boolean).pop() || workspace || '默认工作区',
      sessions: list.sort((a, b) => b.updated_at - a.updated_at),
    }))
    .sort((a, b) => a.workspace.localeCompare(b.workspace));
});

// ===== 分组折叠状态 (localStorage 持久化) =====
const COLLAPSED_KEY = 'oma_sidebar_collapsed';

function loadCollapsed(): Set<string> {
  try {
    const raw = JSON.parse(localStorage.getItem(COLLAPSED_KEY) || '[]');
    return new Set(Array.isArray(raw) ? raw.filter((x) => typeof x === 'string') : []);
  } catch {
    return new Set();
  }
}

const collapsed = ref<Set<string>>(loadCollapsed());

function persistCollapsed() {
  localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...collapsed.value]));
}

function toggleGroup(workspace: string) {
  const next = new Set(collapsed.value);
  if (next.has(workspace)) next.delete(workspace);
  else next.add(workspace);
  collapsed.value = next;
  persistCollapsed();
}

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

    <button class="session-new" @click="emit('create-session')">
      <Fa6Plus /> 新建会话
    </button>

    <div class="session-list">
      <section v-for="g in groups" :key="g.workspace" class="session-group">
        <div
          class="group-header"
          role="button"
          tabindex="0"
          :aria-expanded="!collapsed.has(g.workspace)"
          @click="toggleGroup(g.workspace)"
          @keydown.enter.prevent="toggleGroup(g.workspace)"
          @keydown.space.prevent="toggleGroup(g.workspace)"
        >
          <Fa6ChevronDown class="group-chevron" :class="{ collapsed: collapsed.has(g.workspace) }" />
          <Fa6FolderOpen class="group-icon" />
          <span class="group-name" v-tip="g.workspace">{{ g.basename }}</span>
          <span class="group-count">{{ g.sessions.length }}</span>
          <button
            class="group-add"
            v-tip="`在 ${g.basename} 新建会话`"
            @click.stop="emit('create-session', g.workspace)"
          >
            <Fa6Plus />
          </button>
        </div>

        <div v-show="!collapsed.has(g.workspace)" class="group-sessions">
          <div
            v-for="s in g.sessions"
            :key="s.session_id"
            :class="['session-item', s.session_id === currentSessionId ? 'active' : '']"
            @click="emit('select-session', s.session_id)"
          >
            <div class="session-item-main">
              <div class="session-title">{{ s.title }}</div>
              <div class="session-meta">{{ relativeTime(s.updated_at) }} · {{ s.active_model }}</div>
            </div>
            <button
              class="btn-delete-session"
              v-tip="'删除会话'"
              @click.stop="emit('delete-session', s.session_id)"
            >
              <Fa6Trash />
            </button>
          </div>
        </div>
      </section>

      <div v-if="groups.length === 0" class="sidebar-empty">
        <div class="empty-hint-icon"><Fa6RegFolderOpen /></div>
        <div>暂无会话</div>
      </div>
    </div>

    <div class="sidebar-footer">
      <div class="workspace-badge" v-tip="workspace">
        {{ workspace || '默认工作区' }}
      </div>
      <button class="btn-icon" v-tip="isDark ? '切换到浅色主题' : '切换到深色主题'" @click="toggleMode">
        <Fa6Sun v-if="isDark" />
        <Fa6Moon v-else />
      </button>
    </div>
  </aside>
</template>
