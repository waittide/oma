<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import {
  LuAlertTriangle,
  LuArrowUp,
  LuBot,
  LuCheck,
  LuGitBranch,
  LuListTree,
  LuPlus,
  LuLoader,
  LuPencil,
  LuPlug,
  LuSquare,
  LuTrash2,
  LuX,
  LuZap,
} from 'vue-icons-plus/lu';
import OModelSelect from './ui/OModelSelect.vue';
import OTooltip from './ui/OTooltip.vue';
import OSelect from './ui/OSelect.vue';
import MessageBlocks from './MessageBlocks.vue';
import OButton from './ui/OButton.vue';
import HistoryTree from './HistoryTree.vue';
import type { ApprovalMode } from '../types';
import * as chat from '../stores/chat';
import { activeSession, activeSessionId, requestNewSession } from '../stores/sessions';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ online: boolean }>();
const emit = defineEmits<{ needSettings: [] }>();

const { t } = useTranslations('chat');
const { t: tc } = useTranslations('common');
const { t: ta } = useTranslations('approval');
const treeOpen = ref(false);

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
  () => [chat.messages.value.length, chat.live.value.segments, chat.running.value],
  () => {
    if (stickBottom.value) void nextTick(() => scrollToBottom());
  },
  { deep: true },
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

const agentOptions = computed(() => chat.agents.value.map((a) => ({ value: a.id, label: a.name })));

const mcpToolTotal = computed(() =>
  chat.mcpServers.value.reduce((sum, s) => sum + s.tool_count, 0)
);
const mcpTip = computed(() =>
  chat.mcpServers.value
    .map((s) => `${s.name} (${s.tool_count})`)
    .join('  ·  ')
);

const approvalOptions = computed<{ value: ApprovalMode; label: string }[]>(() => [
  { value: 'normal', label: ta('normal') },
  { value: 'strict', label: ta('strict') },
  { value: 'auto', label: ta('auto') },
]);

const isEmpty = computed(() => chat.messages.value.length === 0 && !chat.running.value);

const hasProviders = computed(() => Object.keys(chat.providers.value).length > 0);
</script>

<template>
  <div class="chat">
    <header class="top">
      <div class="top-left">
        <span class="session-title">{{ activeSession?.title ?? t('noSession') }}</span>
        <OTooltip :label="activeSession?.workspace ?? ''" align="start" block placement="bottom">
          <span class="ws-path">{{ activeSession?.workspace }}</span>
        </OTooltip>
      </div>
      <div v-if="activeSessionId" class="top-right">
        <OTooltip :label="chat.connected.value ? t('connected') : t('disconnected')" align="end" placement="bottom">
          <span class="dot" :class="chat.connected.value ? 'ok' : 'off'" />
        </OTooltip>
        <OTooltip
          v-if="chat.mcpServers.value.length"
          :label="mcpTip"
          align="end"
          placement="bottom"
        >
          <span class="mcp-chip">
            <LuPlug :size="11" />
            {{ mcpToolTotal }}
          </span>
        </OTooltip>
        <OTooltip :label="t('historyTree')" align="end" placement="bottom">
          <button type="button" class="icon-ghost" :aria-label="t('historyTree')" @click="treeOpen = true">
            <LuListTree :size="14" />
          </button>
        </OTooltip>
      </div>
    </header>

    <div ref="scrollEl" class="stream" @scroll.passive="onScroll">
      <div v-if="!activeSessionId" class="hero">
        <LuBot :size="42" class="hero-icon" />
        <h2>{{ t('welcomeTitle') }}</h2>
        <p>{{ t('welcomeBody') }}</p>
        <OButton variant="primary" size="md" @click="requestNewSession">
          <template #icon><LuPlus :size="14" /></template>
          {{ t('newSession') }}
        </OButton>
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
          <template v-if="m.role === 'user'">
            <div class="user-bubble">
              <MessageBlocks :blocks="m.content" :streaming="false" :results="chat.toolResults.value" />
            </div>
            <div class="user-actions">
              <button
                v-if="m.parent_id"
                type="button"
                class="fork"
                :aria-label="t('editResendHint')"
                @click="startFork(m.parent_id, userText(m.id))"
              >
                <LuPencil :size="11" /> {{ t('editResend') }}
              </button>
              <button
                type="button"
                class="fork"
                :aria-label="t('deleteHint')"
                @click="chat.deleteMessage(m.id)"
              >
                <LuTrash2 :size="11" /> {{ t('delete') }}
              </button>
            </div>
          </template>
          <template v-else>
            <MessageBlocks :blocks="m.content" :streaming="false" :results="chat.toolResults.value" />
          </template>
        </article>

        <article v-if="chat.running.value || chat.finalizing.value" class="msg assistant">
          <MessageBlocks :blocks="chat.renderBlocks.value" :streaming="true" />
          <div v-if="chat.running.value" class="live-row">
            <LuLoader :size="13" class="spin" />
          </div>
        </article>
      </template>
    </div>

    <Transition name="slide">
      <div v-if="chat.pendingApproval.value" class="approval-row">
        <div class="approval">
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
          rows="2"
          :placeholder="activeSessionId ? t('placeholder') : t('placeholderNoSession')"
          :disabled="!activeSessionId || !props.online"
          @keydown.enter.exact.prevent="send"
        />
        <div class="c-toolbar">
          <div class="c-controls">
            <OSelect
              v-model="chat.approvalMode.value"
              :options="approvalOptions"
              width="96px"
              @update:model-value="chat.setApprovalMode"
            />
            <OSelect
              v-model="chat.activeAgent.value"
              :options="agentOptions"
              width="118px"
              @update:model-value="chat.setAgent"
            />
            <OModelSelect
              v-model="chat.activeModel.value"
              :groups="chat.providers.value"
              width="170px"
              @update:model-value="chat.setModel"
            />
          </div>
          <div class="c-actions">
            <OTooltip v-if="chat.running.value" :label="t('stopHint')">
              <button type="button" class="icon-btn stop" :aria-label="t('stopHint')" @click="cancel">
                <LuSquare :size="13" />
              </button>
            </OTooltip>
            <OTooltip :label="t('sendHint')">
              <button
                type="button"
                class="icon-btn send"
                :aria-label="t('sendHint')"
                :disabled="!draft.trim() || !activeSessionId || !props.online"
                @click="send"
              >
                <LuArrowUp :size="15" />
              </button>
            </OTooltip>
          </div>
        </div>
      </div>
    </footer>
    <HistoryTree :open="treeOpen" @close="treeOpen = false" />
  </div>
</template>

<style scoped>
.chat {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
  background: var(--paper);
  /* 消息、审批条、输入框共用同一列宽，保证左右边缘对齐 */
  --chat-col: 1120px;
}
.top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  height: 48px;
  padding: 0 16px;
  border-bottom: 1px solid var(--line);
  background: var(--paper);
  flex-shrink: 0;
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
.mcp-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 22px;
  padding: 0 8px;
  border: 1px solid var(--line);
  border-radius: 99px;
  background: var(--surface);
  color: var(--text-tertiary);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
}
.icon-ghost {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text-tertiary);
  cursor: pointer;
  transition:
    background-color 0.15s ease,
    color 0.15s ease;
}
.icon-ghost:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.dot.off {
  background: var(--overlay0);
}
.stream {
  flex: 1;
  overflow-y: auto;
  padding: 14px 0 8px;
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
  flex-direction: column;
  padding: 8px 14px;
  max-width: var(--chat-col);
  margin: 0 auto;
}
.msg.user {
  align-items: flex-end;
}
.user-bubble {
  background: var(--surface-strong);
  border-radius: 10px;
  padding: 8px 12px;
  max-width: min(82%, 64ch);
  overflow-wrap: break-word;
}
.user-actions {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 4px;
  opacity: 0;
  transition: opacity 0.15s ease;
}
.msg.user:hover .user-actions,
.msg.user:focus-within .user-actions {
  opacity: 1;
}
.live-row {
  color: var(--accent);
  padding-bottom: 4px;
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
.approval-row {
  max-width: var(--chat-col);
  width: 100%;
  margin: 0 auto;
  padding: 0 14px 8px;
}
.approval {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 10px;
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
  flex-wrap: wrap;
  gap: 6px;
  flex-shrink: 0;
}
.composer {
  padding: 8px 14px 12px;
  max-width: var(--chat-col);
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
  flex-direction: column;
  min-height: 96px;
  background: var(--paper);
  border: 1px solid var(--control-border);
  border-radius: 12px;
  box-shadow: var(--shadow-raised);
  transition: border-color 0.15s ease, background-color 0.15s ease;
}
.box:focus-within {
  border-color: color-mix(in srgb, var(--accent) 55%, transparent);
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
  font-size: 13px;
  line-height: 20px;
  max-height: 180px;
  padding: 12px 16px 2px;
}
textarea::placeholder {
  color: var(--overlay0);
}
.c-toolbar {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 44px;
  padding: 0 7px 4px;
}
.c-controls {
  display: flex;
  align-items: center;
  gap: 4px;
  flex: 1;
  min-width: 0;
}
.c-actions {
  display: flex;
  gap: 6px;
  align-items: center;
}
.icon-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  border-radius: 6px;
  background: var(--accent);
  color: var(--base);
  cursor: pointer;
  transition:
    background-color 0.15s ease,
    color 0.15s ease,
    opacity 0.15s ease;
}
.icon-btn:hover:not(:disabled) {
  filter: brightness(1.08);
}
.icon-btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
}
.icon-btn.stop {
  background: var(--danger-soft);
  color: var(--danger);
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
