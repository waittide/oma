<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import {
  LuAlertTriangle,
  LuBot,
  LuCheck,
  LuGitBranch,
  LuLoader,
  LuPencil,
  LuSend,
  LuSquare,
  LuUser,
  LuX,
  LuZap,
} from 'vue-icons-plus/lu';
import OButton from './ui/OButton.vue';
import OSelect from './ui/OSelect.vue';
import MessageBlocks from './MessageBlocks.vue';
import type { ApprovalMode, ChatMessage } from '../types';
import * as chat from '../stores/chat';
import { activeSession, activeSessionId } from '../stores/sessions';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ online: boolean }>();
const emit = defineEmits<{ needSettings: [] }>();

const { t } = useTranslations('chat');
const { t: tc } = useTranslations('common');
const { t: ta } = useTranslations('approval');

const draft = ref('');
/** 非空表示下一条发送将从该消息处分叉重跑（编辑重发）。 */
const forkFrom = ref<string | null>(null);
const scrollEl = ref<HTMLElement | null>(null);
const stickBottom = ref(true);

watch(activeSessionId, () => {
  forkFrom.value = null;
  const id = activeSessionId.value;
  if (id && activeSession.value) {
    void chat.open(id, activeSession.value.workspace);
  }
});

watch(
  () => [chat.messages.value.length, chat.live.value.text, chat.running.value],
  () => {
    if (stickBottom.value) void nextTick(() => scrollToBottom());
  },
);

function scrollToBottom() {
  const el = scrollEl.value;
  if (el) el.scrollTop = el.scrollHeight;
}

function onScroll() {
  const el = scrollEl.value;
  if (!el) return;
  stickBottom.value = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
}

function send() {
  const text = draft.value;
  if (!text.trim()) return;
  if (forkFrom.value !== null) {
    chat.forkAndRun(forkFrom.value, text);
    forkFrom.value = null;
  } else {
    chat.submit(text);
  }
  draft.value = '';
  stickBottom.value = true;
}

function startFork(messageId: string, text: string) {
  forkFrom.value = messageId;
  draft.value = text;
  stickBottom.value = false;
}

function cancelFork() {
  forkFrom.value = null;
}

function cancel() {
  chat.cancel();
  toast.info(t('cancelRequested'));
}

function userText(id: string): string {
  const m = chat.messages.value.find((x) => x.id === id);
  const b = m?.content.find((c) => c.type === 'text');
  return b && b.type === 'text' ? b.text : '';
}

const modelOptions = computed(() =>
  chat.modelList.value.map((m) => ({
    value: m.selector,
    label: m.info.name,
    hint: `${Math.round(m.info.context_len / 1024)}K`,
  })),
);

const agentOptions = computed(() => chat.agents.value.map((a) => ({ value: a.id, label: a.name })));

const approvalOptions = computed<{ value: ApprovalMode; label: string }[]>(() => [
  { value: 'normal', label: ta('normal') },
  { value: 'strict', label: ta('strict') },
  { value: 'auto', label: ta('auto') },
]);

function textOf(m: ChatMessage): string {
  const b = m.content.find((c) => c.type === 'text');
  return b && b.type === 'text' ? b.text : '';
}

function tipOf(startId: string): string {
  // 从分支起点向下沿唯一后继走到叶子（供 switch_branch 使用）
  const byParent: Record<string, ChatMessage[]> = {};
  for (const m of chat.tree.value) (byParent[m.parent_id ?? '__root__'] ??= []).push(m);
  let cur = startId;
  for (;;) {
    const kids = (byParent[cur] ?? []).sort((a, b) => a.created_at - b.created_at);
    if (kids.length === 0) return cur;
    cur = kids[kids.length - 1]!.id;
  }
}

/** 当前分支链上每个 user 节点的分支组（兄弟含自身），用于切换激活分支。 */
const branchGroups = computed(() => {
  const byParent: Record<string, ChatMessage[]> = {};
  for (const m of chat.tree.value) {
    if (m.role !== 'user') continue;
    (byParent[m.parent_id ?? '__root__'] ??= []).push(m);
  }
  const out: Record<string, { value: string; label: string }[]> = {};
  for (const m of chat.messages.value) {
    if (m.role !== 'user') continue;
    const sibs = byParent[m.parent_id ?? '__root__'] ?? [];
    if (sibs.length < 2) continue;
    out[m.id] = sibs.map((s, i) => ({ value: s.id, label: textOf(s).slice(0, 18) || t('branchN', { index: i + 1 }) }));
  }
  return out;
});

function switchBranch(startId: string) {
  chat.switchBranch(tipOf(startId));
  stickBottom.value = true;
}

const isEmpty = computed(() => chat.messages.value.length === 0 && !chat.running.value);
const hasProviders = computed(() => Object.keys(chat.providers.value).length > 0);
</script>

<template>
  <div class="chat">
    <header class="top">
      <div class="top-left">
        <span class="session-title">{{ activeSession?.title ?? t('noSession') }}</span>
        <span class="ws-path" :title="activeSession?.workspace">{{ activeSession?.workspace }}</span>
      </div>
      <div v-if="activeSessionId" class="top-right">
        <span class="dot" :class="chat.connected.value ? 'ok' : 'off'" />
        <OSelect
          v-model="chat.approvalMode.value"
          :options="approvalOptions"
          width="112px"
          @update:model-value="chat.setApprovalMode"
        />
        <OSelect
          v-model="chat.activeAgent.value"
          :options="agentOptions"
          width="120px"
          @update:model-value="chat.setAgent"
        />
        <OSelect
          v-model="chat.activeModel.value"
          :options="modelOptions"
          width="180px"
          @update:model-value="chat.setModel"
        />
      </div>
    </header>

    <div ref="scrollEl" class="stream" @scroll.passive="onScroll">
      <div v-if="!activeSessionId" class="hero">
        <LuBot :size="42" class="hero-icon" />
        <h2>{{ t('welcomeTitle') }}</h2>
        <p>{{ t('welcomeBody') }}</p>
      </div>

      <div v-else-if="!props.online" class="hero">
        <LuAlertTriangle :size="42" class="hero-icon warn" />
        <h2>{{ t('offlineTitle') }}</h2>
        <p>{{ t('offlineBody') }}</p>
      </div>

      <div v-else-if="isEmpty" class="hero">
        <LuZap :size="42" class="hero-icon" />
        <h2>{{ t('emptyTitle') }}</h2>
        <p>{{ t('emptyBody') }}</p>
        <OButton v-if="!hasProviders" variant="soft" @click="emit('needSettings')">{{ t('goSettings') }}</OButton>
      </div>

      <template v-else>
        <article
          v-for="m in chat.messages.value.filter((x) => !chat.isInternalMessage(x))"
          :key="m.id"
          class="msg"
          :class="m.role"
        >
          <div class="avatar">
            <LuUser v-if="m.role === 'user'" :size="14" />
            <LuBot v-else :size="14" />
          </div>
          <div class="bubble">
            <div class="meta">
              <span class="who">{{ m.role === 'user' ? t('you') : t('assistant') }}</span>
              <template v-if="m.role === 'user'">
                <button
                  v-if="m.parent_id"
                  type="button"
                  class="fork"
                  :title="t('editResendHint')"
                  @click="startFork(m.parent_id, userText(m.id))"
                >
                  <LuPencil :size="11" /> {{ t('editResend') }}
                </button>
                <OSelect
                  v-if="branchGroups[m.id]"
                  :model-value="m.id"
                  :options="branchGroups[m.id]!"
                  :placeholder="t('branch')"
                  width="128px"
                  @update:model-value="switchBranch"
                />
              </template>
            </div>
            <MessageBlocks :blocks="m.content" :streaming="false" :results="chat.toolResults.value" />
          </div>
        </article>

        <article v-if="chat.running.value" class="msg assistant">
          <div class="avatar live">
            <LuLoader :size="14" class="spin" />
          </div>
          <div class="bubble">
            <MessageBlocks :blocks="chat.renderBlocks.value" :streaming="true" />
          </div>
        </article>
      </template>
    </div>

    <Transition name="slide">
      <div v-if="chat.pendingApproval.value" class="approval">
        <LuAlertTriangle :size="15" class="warn" />
        <div class="ap-text">
          <strong>{{ chat.pendingApproval.value.name }}</strong>
          {{ t('approvalRequest') }}<code>{{ chat.pendingApproval.value.summary }}</code>
        </div>
        <div class="ap-actions">
          <OButton variant="primary" size="sm" @click="chat.respond('allow_once')">
            <template #icon><LuCheck :size="13" /></template>
            {{ t('allowOnce') }}
          </OButton>
          <OButton variant="soft" size="sm" @click="chat.respond('allow_session')">
            {{ t('allowSession') }}
          </OButton>
          <OButton variant="danger" size="sm" @click="chat.respond('deny')">
            <template #icon><LuX :size="13" /></template>
            {{ t('deny') }}
          </OButton>
        </div>
      </div>
    </Transition>

    <footer class="composer">
      <div v-if="forkFrom !== null" class="fork-banner">
        <LuGitBranch :size="12" />
        <span>{{ t('editResendBanner') }}</span>
        <button type="button" class="fork-cancel" @click="cancelFork">{{ tc('cancel') }}</button>
      </div>
      <div class="box" :class="{ disabled: !activeSessionId || !props.online }">
        <textarea
          v-model="draft"
          rows="3"
          :placeholder="activeSessionId ? t('placeholder') : t('placeholderNoSession')"
          :disabled="!activeSessionId || !props.online"
          @keydown.enter.exact.prevent="send"
        />
        <div class="c-actions">
          <OButton
            v-if="chat.running.value"
            variant="danger"
            size="sm"
            :title="t('stopHint')"
            @click="cancel"
          >
            <template #icon><LuSquare :size="12" /></template>
            {{ t('stop') }}
          </OButton>
          <OButton
            variant="primary"
            size="sm"
            :disabled="!draft.trim() || !activeSessionId || !props.online"
            :title="t('sendHint')"
            @click="send"
          >
            <template #icon><LuSend :size="13" /></template>
          </OButton>
        </div>
      </div>
      <div class="hint">
        <LuBot :size="11" />
        {{ chat.activeModel.value || t('noModel') }} · {{ chat.activeAgent.value || '—' }}
        <template v-if="chat.lastUsage.value">
          · {{ t('lastUsage', { input: chat.lastUsage.value.input_tokens, output: chat.lastUsage.value.output_tokens }) }}
        </template>
      </div>
    </footer>
  </div>
</template>

<style scoped>
.chat {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
  background: var(--paper);
}
.top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 18px;
  border-bottom: 1px solid var(--line);
  background: var(--surface);
}
.top-left {
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.session-title {
  font-size: 14px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ws-path {
  font-size: 11.5px;
  color: var(--overlay0);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--font-mono);
}
.top-right {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.dot {
  width: 8px;
  height: 8px;
  border-radius: 99px;
  flex-shrink: 0;
}
.dot.ok {
  background: var(--success);
}
.dot.off {
  background: var(--overlay0);
}
.stream {
  flex: 1;
  overflow-y: auto;
  padding: 22px 0 10px;
}
.hero {
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  color: var(--text-tertiary);
  padding: 0 32px;
}
.hero-icon {
  color: var(--accent);
}
.hero-icon.warn {
  color: var(--warning);
}
.hero h2 {
  margin: 0;
  font-size: 18px;
  color: var(--ink);
}
.hero p {
  margin: 0 0 8px;
  font-size: 13px;
  max-width: 420px;
  text-align: center;
  line-height: 1.7;
}
.msg {
  display: flex;
  gap: 10px;
  padding: 9px 22px;
  max-width: 920px;
  margin: 0 auto;
}
.avatar {
  flex-shrink: 0;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border-radius: 8px;
  background: var(--surface-strong);
  color: var(--text-tertiary);
  margin-top: 2px;
}
.msg.user .avatar {
  background: var(--surface-hover);
  color: var(--text-secondary);
}
.msg.assistant .avatar {
  background: var(--surface-active);
  color: var(--accent);
}
.avatar.live {
  color: var(--accent);
}
.bubble {
  flex: 1;
  min-width: 0;
}
.meta {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11.5px;
  color: var(--overlay0);
  margin-bottom: 4px;
}
.who {
  font-weight: 600;
}
.fork {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  border: none;
  background: transparent;
  color: var(--overlay0);
  font-family: inherit;
  font-size: 11px;
  cursor: pointer;
  padding: 1px 5px;
  border-radius: 5px;
}
.fork:hover {
  color: var(--accent);
  background: var(--surface-hover);
}
.approval {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 0 22px 10px;
  padding: 10px 14px;
  border: 1px solid color-mix(in srgb, var(--warning) 40%, transparent);
  background: var(--warning-soft);
  border-radius: 10px;
}
.approval .warn {
  color: var(--warning);
  flex-shrink: 0;
}
.ap-text {
  flex: 1;
  min-width: 0;
  font-size: 12.5px;
  color: var(--ink);
}
.ap-text code {
  display: block;
  margin-top: 2px;
  font-size: 11.5px;
  color: var(--text-tertiary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.ap-actions {
  display: flex;
  gap: 6px;
  flex-shrink: 0;
}
.composer {
  padding: 10px 22px 14px;
  max-width: 920px;
  width: 100%;
  margin: 0 auto;
}
.fork-banner {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 8px;
  font-size: 12px;
  color: var(--accent);
}
.fork-cancel {
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  font-size: 11.5px;
  cursor: pointer;
  text-decoration: underline;
}
.box {
  display: flex;
  align-items: flex-end;
  gap: 8px;
  padding: 8px 8px 8px 14px;
  background: var(--surface);
  border: 1px solid var(--control-border);
  border-radius: 14px;
  transition: border-color 0.15s ease;
}
.box:focus-within {
  border-color: var(--accent);
}
.box.disabled {
  opacity: 0.6;
}
textarea {
  flex: 1;
  resize: none;
  border: none;
  outline: none;
  background: transparent;
  color: var(--ink);
  font-family: inherit;
  font-size: 13.5px;
  line-height: 1.6;
  max-height: 180px;
  padding: 4px 0;
}
textarea::placeholder {
  color: var(--overlay0);
}
.c-actions {
  display: flex;
  gap: 6px;
  align-items: center;
}
.hint {
  display: flex;
  align-items: center;
  gap: 5px;
  margin-top: 6px;
  padding-left: 4px;
  font-size: 11px;
  color: var(--overlay0);
}
.spin {
  animation: rotate 0.9s linear infinite;
}
@keyframes rotate {
  to {
    transform: rotate(360deg);
  }
}
.slide-enter-active,
.slide-leave-active {
  transition: all 0.18s ease;
}
.slide-enter-from,
.slide-leave-to {
  opacity: 0;
  transform: translateY(6px);
}
</style>
