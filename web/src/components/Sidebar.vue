<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import {
  LuChevronRight,
  LuFolder,
  LuLoader,
  LuMessageSquare,
  LuPencil,
  LuPlus,
  LuSettings,
  LuSparkles,
  LuTrash2,
} from 'vue-icons-plus/lu';
import OButton from './ui/OButton.vue';
import OInput from './ui/OInput.vue';
import OTooltip from './ui/OTooltip.vue';
import OModal from './ui/OModal.vue';
import * as store from '../stores/sessions';
import { activeSessionId } from '../stores/sessions';
import * as chat from '../stores/chat';
import type { SessionRecord } from '../types';
import { useTranslations } from '../composables/i18n';

const emit = defineEmits<{ openSettings: [] }>();

const { t } = useTranslations('sidebar');
const { t: tc } = useTranslations('common');

const showNew = ref(false);
const newWorkspace = ref(localStorage.getItem('oma.lastWorkspace') ?? '');
const newTitle = ref('');
const renaming = ref<string | null>(null);
const renameValue = ref('');
const confirmDelete = ref<string | null>(null);

onMounted(() => store.refresh());

// 空态 CTA 等外部入口请求新建会话时打开弹窗
watch(store.newSessionRequested, (v) => {
  if (v) {
    showNew.value = true;
    store.newSessionRequested.value = false;
  }
});

const groups = store.groups;

function select(id: string) {
  activeSessionId.value = id;
}

/** 当前会话正在执行轮次时，侧栏条目显示加载动画 */
function isRunning(s: SessionRecord): boolean {
  return chat.running.value && s.session_id === activeSessionId.value;
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

/** 从工作区分组头部新建该工作区下的会话：预填路径并打开弹窗 */
function openNewFor(workspace: string) {
  newWorkspace.value = workspace;
  newTitle.value = '';
  showNew.value = true;
}

async function createSession() {
  const ws = newWorkspace.value.trim();
  if (!ws) return;
  const rec = await store.create(ws, newTitle.value.trim() || t('defaultTitle'));
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
    </header>
    <div class="brand-actions">
      <OButton variant="soft" class="new-ws" @click="showNew = true">
        <template #icon><LuPlus :size="14" /></template>
        {{ t('newSession') }}
      </OButton>
    </div>

    <nav class="tree">
      <div v-if="groups.length === 0 && !store.loading.value" class="empty">
        {{ t('empty') }}
      </div>

      <section v-for="g in groups" :key="g.workspace" class="group">
        <OTooltip :label="g.workspace" align="start" block>
          <div class="group-head">
            <button type="button" class="gh-toggle" @click="store.toggleGroup(g.workspace)">
              <LuChevronRight
                :size="14"
                class="caret"
                :class="{ expanded: !store.collapsed.value[g.workspace] }"
              />
              <LuFolder :size="14" class="g-icon" />
              <span class="g-label">{{ g.label }}</span>
              <span class="g-count">{{ g.items.length }}</span>
            </button>
            <OTooltip :label="t('addSessionHere')" align="end">
              <button type="button" class="gh-add" :aria-label="t('addSessionHere')" @click="openNewFor(g.workspace)">
                <LuPlus :size="14" />
              </button>
            </OTooltip>
          </div>
        </OTooltip>

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
              <LuLoader v-if="isRunning(s)" :size="13" class="s-icon spin" />
              <LuMessageSquare v-else :size="13" class="s-icon" />
              <OTooltip :label="s.title" align="start" block>
                <span class="s-title">{{ s.title }}</span>
              </OTooltip>
              <span class="s-actions">
                <OTooltip :label="t('rename')" align="end">
                  <button
                    type="button"
                    class="mini"
                    :aria-label="t('rename')"
                    @click.stop="startRename(s.session_id, s.title)"
                  >
                    <LuPencil :size="12" />
                  </button>
                </OTooltip>
                <OTooltip :label="t('deleteAria')" align="end">
                  <button
                    type="button"
                    class="mini danger"
                    :aria-label="t('deleteAria')"
                    @click.stop="confirmDelete = s.session_id"
                  >
                    <LuTrash2 :size="12" />
                  </button>
                </OTooltip>
              </span>
            </template>
          </div>
        </div>
      </section>
    </nav>

    <footer class="foot">
      <button type="button" class="foot-item" @click="emit('openSettings')">
        <LuSettings :size="14" /> {{ t('settings') }}
      </button>
    </footer>

    <OModal :open="showNew" :title="t('newSession')" width="480px" @close="showNew = false">
      <div class="form">
        <label>{{ t('workspacePath') }}</label>
        <OInput v-model="newWorkspace" :placeholder="t('workspacePlaceholder')" autofocus />
        <label>{{ t('sessionTitle') }}</label>
        <OInput v-model="newTitle" :placeholder="t('titlePlaceholder')" @enter="createSession" />
      </div>
      <template #footer>
        <OButton variant="ghost" @click="showNew = false">{{ tc('cancel') }}</OButton>
        <OButton variant="primary" :disabled="!newWorkspace.trim()" @click="createSession">
          {{ tc('create') }}
        </OButton>
      </template>
    </OModal>

    <OModal :open="confirmDelete !== null" :title="t('deleteSession')" width="380px" @close="confirmDelete = null">
      <p class="confirm-text">
        {{ t('deleteConfirm', { title: deleteTarget?.title ?? '' }) }}
      </p>
      <template #footer>
        <OButton variant="ghost" @click="confirmDelete = null">{{ tc('cancel') }}</OButton>
        <OButton
          variant="danger"
          @click="confirmDelete && store.remove(confirmDelete); confirmDelete = null"
        >
          {{ tc('delete') }}
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
}
.brand-actions {
  padding: 0 12px 10px;
}
.new-ws {
  width: 100%;
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
  gap: 2px;
  width: 100%;
  padding: 2px 4px;
  border-radius: 8px;
  transition: background-color 0.12s ease;
}
.group-head:hover {
  background: var(--surface-hover);
}
.gh-toggle {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: 1;
  min-width: 0;
  padding: 10px 4px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 13.5px;
  font-weight: 600;
  cursor: pointer;
  text-align: left;
}
.gh-add {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  flex-shrink: 0;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
  opacity: 0;
  transition:
    opacity 0.12s ease,
    color 0.12s ease,
    background-color 0.12s ease;
}
.group-head:hover .gh-add,
.gh-add:focus-visible {
  opacity: 1;
}
.gh-add:hover {
  color: var(--accent);
  background: var(--surface-active);
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
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
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
  display: inline-flex;
  gap: 2px;
  opacity: 0;
  pointer-events: none;
  transition: opacity 0.12s ease;
}
.session:hover .s-actions,
.session:focus-within .s-actions {
  opacity: 1;
  pointer-events: auto;
}
.s-icon.spin {
  color: var(--accent);
  animation: spin 0.9s linear infinite;
}
@keyframes spin {
  to {
    transform: rotate(360deg);
  }
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
  border-radius: 6px;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
  border: none;
  flex-shrink: 0;
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
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
