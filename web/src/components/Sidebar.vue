<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import {
  LuChevronRight,
  LuFolder,
  LuMessageSquare,
  LuPencil,
  LuPlus,
  LuSettings,
  LuSparkles,
  LuTrash2,
} from 'vue-icons-plus/lu';
import OButton from './ui/OButton.vue';
import OInput from './ui/OInput.vue';
import OModal from './ui/OModal.vue';
import * as store from '../stores/sessions';
import { activeSessionId } from '../stores/sessions';

const emit = defineEmits<{ openSettings: []; back: [] }>();

const showNew = ref(false);
const newWorkspace = ref(localStorage.getItem('oma.lastWorkspace') ?? '');
const newTitle = ref('');
const renaming = ref<string | null>(null);
const renameValue = ref('');
const confirmDelete = ref<string | null>(null);

onMounted(() => store.refresh());

const groups = store.groups;

function select(id: string) {
  activeSessionId.value = id;
  emit('back');
}

function startRename(id: string, title: string) {
  renaming.value = id;
  renameValue.value = title;
}

function commitRename() {
  if (renaming.value && renameValue.value.trim()) {
    void store.rename(renaming.value, renameValue.value.trim());
  }
  renaming.value = null;
}

async function createSession() {
  const ws = newWorkspace.value.trim();
  if (!ws) return;
  const rec = await store.create(ws, newTitle.value.trim() || 'New Session');
  if (rec) {
    localStorage.setItem('oma.lastWorkspace', ws);
    showNew.value = false;
    newTitle.value = '';
    select(rec.session_id);
  }
}

const deleteTarget = computed(
  () => store.sessions.value.find((s) => s.session_id === confirmDelete.value) ?? null,
);
</script>

<template>
  <aside class="sidebar">
    <header class="brand">
      <span class="logo"><LuSparkles :size="16" /></span>
      <span class="brand-name">Oma</span>
      <OButton size="sm" variant="soft" title="新建会话" @click="showNew = true">
        <template #icon><LuPlus :size="14" /></template>
        新建会话
      </OButton>
    </header>

    <nav class="tree">
      <div v-if="groups.length === 0 && !store.loading.value" class="empty">
        暂无会话，点击右上角新建
      </div>

      <section v-for="g in groups" :key="g.workspace" class="group">
        <button
          type="button"
          class="group-head"
          :title="g.workspace"
          @click="store.toggleGroup(g.workspace)"
        >
          <LuChevronRight
            :size="13"
            class="caret"
            :class="{ expanded: !store.collapsed.value[g.workspace] }"
          />
          <LuFolder :size="13" class="g-icon" />
          <span class="g-label">{{ g.label }}</span>
          <span class="g-count">{{ g.items.length }}</span>
        </button>

        <div v-if="!store.collapsed.value[g.workspace]" class="group-body">
          <div
            v-for="s in g.items"
            :key="s.session_id"
            class="session"
            :class="{ active: s.session_id === activeSessionId }"
            @click="select(s.session_id)"
          >
            <template v-if="renaming === s.session_id">
              <OInput
                v-model="renameValue"
                autofocus
                class="rename-input"
                @enter="commitRename"
                @blur="commitRename"
              />
            </template>
            <template v-else>
              <LuMessageSquare :size="13" class="s-icon" />
              <span class="s-title" :title="s.title">{{ s.title }}</span>
              <span class="s-actions">
                <button
                  type="button"
                  class="mini"
                  title="重命名"
                  @click.stop="startRename(s.session_id, s.title)"
                >
                  <LuPencil :size="12" />
                </button>
                <button
                  type="button"
                  class="mini danger"
                  title="删除"
                  @click.stop="confirmDelete = s.session_id"
                >
                  <LuTrash2 :size="12" />
                </button>
              </span>
            </template>
          </div>
        </div>
      </section>
    </nav>

    <footer class="foot">
      <button type="button" class="foot-item" @click="emit('openSettings')">
        <LuSettings :size="14" /> 设置 · 主题
      </button>
    </footer>

    <OModal :open="showNew" title="新建会话" width="480px" @close="showNew = false">
      <div class="form">
        <label>工作区路径</label>
        <OInput v-model="newWorkspace" placeholder="/home/me/project" autofocus />
        <label>会话标题（可选）</label>
        <OInput v-model="newTitle" placeholder="New Session" @enter="createSession" />
      </div>
      <template #footer>
        <OButton variant="ghost" @click="showNew = false">取消</OButton>
        <OButton variant="primary" :disabled="!newWorkspace.trim()" @click="createSession">
          创建
        </OButton>
      </template>
    </OModal>

    <OModal :open="confirmDelete !== null" title="删除会话" width="380px" @close="confirmDelete = null">
      <p class="confirm-text">
        确定删除「{{ deleteTarget?.title }}」？该会话的全部消息将被永久移除。
      </p>
      <template #footer>
        <OButton variant="ghost" @click="confirmDelete = null">取消</OButton>
        <OButton
          variant="danger"
          @click="confirmDelete && store.remove(confirmDelete); confirmDelete = null"
        >
          删除
        </OButton>
      </template>
    </OModal>
  </aside>
</template>

<style scoped>
.sidebar {
  display: flex;
  flex-direction: column;
  width: 280px;
  flex-shrink: 0;
  background: var(--sidebar);
  border-right: 1px solid var(--line);
}
.brand {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 14px 14px 12px;
}
.logo {
  display: inline-flex;
  width: 26px;
  height: 26px;
  align-items: center;
  justify-content: center;
  border-radius: 8px;
  background: var(--accent);
  color: var(--base);
}
.brand-name {
  font-weight: 700;
  font-size: 15px;
  flex: 1;
}
.tree {
  flex: 1;
  overflow-y: auto;
  padding: 4px 8px 12px;
}
.empty {
  padding: 24px 12px;
  font-size: 12.5px;
  color: var(--overlay0);
  text-align: center;
}
.group {
  margin-bottom: 4px;
}
.group-head {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 6px 8px;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 12.5px;
  font-weight: 600;
  cursor: pointer;
  text-align: left;
}
.group-head:hover {
  background: var(--surface-hover);
}
.caret {
  flex-shrink: 0;
  color: var(--overlay0);
  transition: transform 0.15s ease;
}
.caret.expanded {
  transform: rotate(90deg);
}
.g-icon {
  flex-shrink: 0;
  color: var(--overlay1);
}
.g-label {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.g-count {
  font-size: 11px;
  color: var(--overlay0);
  background: var(--surface-strong);
  border-radius: 99px;
  padding: 1px 7px;
}
.group-body {
  padding: 1px 0 4px;
}
.session {
  display: flex;
  align-items: center;
  gap: 7px;
  padding: 6px 8px 6px 26px;
  border-radius: 8px;
  cursor: pointer;
  color: var(--text-tertiary);
  font-size: 13px;
  transition: background-color 0.12s ease;
}
.session:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.session.active {
  background: var(--surface-active);
  color: var(--ink);
}
.s-icon {
  flex-shrink: 0;
  color: var(--overlay1);
}
.session.active .s-icon {
  color: var(--accent);
}
.s-title {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.s-actions {
  display: none;
  gap: 2px;
}
.session:hover .s-actions {
  display: inline-flex;
}
.rename-input {
  flex: 1;
}
.rename-input :deep(input) {
  padding: 3px 7px;
  height: 24px;
}
.mini {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
}
.mini:hover {
  background: var(--surface-strong);
  color: var(--ink);
}
.mini.danger:hover {
  color: var(--danger);
}
.foot {
  padding: 10px;
  border-top: 1px solid var(--line);
}
.foot-item {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 8px 10px;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 13px;
  cursor: pointer;
}
.foot-item:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.form {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.form label {
  margin-top: 8px;
  font-size: 12px;
  color: var(--text-tertiary);
}
.confirm-text {
  margin: 4px 0 0;
  font-size: 13px;
  color: var(--text-secondary);
  line-height: 1.6;
}
</style>
