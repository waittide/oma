<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { toast, UiButton, UiIconButton, UiSelect, UiTextarea, UiTooltip } from '@waittide/ui';
import {
  LuAlertTriangle,
  LuArrowDownToLine,
  LuArrowUp,
  LuBot,
  LuCheck,
  LuCopy,
  LuGitBranch,
  LuGripHorizontal,
  LuListTree,
  LuPlus,
  LuLoader,
  LuPaperclip,
  LuPencil,
  LuSquare,
  LuTrash2,
  LuX,
  LuZap,
} from 'vue-icons-plus/lu';
import MessageBlocks from './MessageBlocks.vue';
import MessageRail from './MessageRail.vue';
import { modelSelectorLabel, toModelSelectGroups } from '../lib/modelSelect';
import { copyText } from '../lib/clipboard';
import { ensureNotificationPermission } from '../lib/notify';
import { formatDuration } from '../lib/format';
import ContextGauge from './ContextGauge.vue';
import * as chat from '../stores/chat';
import { activeSession, activeSessionId, requestNewSession } from '../stores/sessions';
import * as layout from '../stores/layout';
import { useTranslations } from '../composables/i18n';
import type { TokenUsage } from '../types';

const props = defineProps<{ online: boolean }>();
const emit = defineEmits<{ needSettings: [] }>();

const { t } = useTranslations('chat');

/** 模型选择器的分组选项与当前展示名（provider 作分组标题）。 */
const modelGroups = computed(() => toModelSelectGroups(chat.modelCatalog.value));
const modelLabel = computed(() => modelSelectorLabel(chat.modelCatalog.value, chat.activeModel.value));
const { t: tc } = useTranslations('common');

// 草稿与分叉目标放在 store 里：历史树切换对话时需回填输入框
const draft = chat.draft;
const forkFrom = chat.forkFrom;
/** 已上传待发送的附件：ref 为 session_attachment:// 引用 */
const pendingUploads = ref<{ ref: string; name: string }[]>([]);
const fileInput = ref<HTMLInputElement | null>(null);
const uploading = ref(false);
const scrollEl = ref<HTMLElement | null>(null);
const stickBottom = ref(true);

/** 输入框固定高度（px）；null 表示保持默认自适应高度。 */
const boxEl = ref<HTMLElement | null>(null);
const boxHeight = ref<number | null>(readBoxHeight());

const BOX_H_KEY = 'oma.composer.height';
const BOX_H_MIN = 96;
const BOX_H_MAX = 560;

function clampBoxHeight(h: number): number {
  const max = Math.min(BOX_H_MAX, window.innerHeight * 0.7);
  return Math.round(Math.min(Math.max(h, BOX_H_MIN), max));
}

function readBoxHeight(): number | null {
  const stored = Number(localStorage.getItem(BOX_H_KEY));
  return Number.isFinite(stored) && stored > 0 ? clampBoxHeight(stored) : null;
}

function persistBoxHeight() {
  if (boxHeight.value === null) localStorage.removeItem(BOX_H_KEY);
  else localStorage.setItem(BOX_H_KEY, String(boxHeight.value));
}

/** 拖动输入框上边沿：向上拉高、向下压低。 */
function startResize(e: PointerEvent) {
  const el = boxEl.value;
  const handle = e.currentTarget as HTMLElement | null;
  if (!el || !handle) return;
  e.preventDefault();

  const startY = e.clientY;
  const startH = el.offsetHeight;
  handle.setPointerCapture(e.pointerId);

  const onMove = (ev: PointerEvent) => {
    boxHeight.value = clampBoxHeight(startH + startY - ev.clientY);
  };
  const onEnd = () => {
    handle.removeEventListener('pointermove', onMove);
    handle.removeEventListener('pointerup', onEnd);
    handle.removeEventListener('pointercancel', onEnd);
    persistBoxHeight();
  };
  handle.addEventListener('pointermove', onMove);
  handle.addEventListener('pointerup', onEnd);
  handle.addEventListener('pointercancel', onEnd);
}

/** 键盘支持：方向键微调高度。 */
function nudgeBoxHeight(delta: number) {
  boxHeight.value = clampBoxHeight((boxHeight.value ?? boxEl.value?.offsetHeight ?? BOX_H_MIN) + delta);
  persistBoxHeight();
}

watch(activeSessionId, () => {
  forkFrom.value = null;
  const id = activeSessionId.value;
  if (id && activeSession.value) {
    void chat.open(id, activeSession.value.workspace);
  } else {
    // 会话被取消选中（如删除）：断开连接并清空遗留状态，
    // 否则 WebSocket 与 sessionId 会悬空指向已不存在的会话
    chat.reset();
  }
});

watch(
  () => [chat.messages.value.length, chat.live.value.segments, chat.running.value],
  () => {
    if (stickBottom.value) void nextTick(() => scrollToBottom());
  },
  { deep: true },
);

// 历史树切换查看位置后回到最新一条，避免停在中间看不见变化
watch(
  () => chat.viewLeafId.value,
  () => {
    stickBottom.value = true;
    void nextTick(() => scrollToBottom());
  },
);

/** 是否处于历史树预览（非最新分支）：提醒用户可一键回到最新。 */
const previewing = computed(() => chat.viewLeafId.value !== null);

function scrollToBottom() {
  const el = scrollEl.value;
  if (el) el.scrollTop = el.scrollHeight;
}

/** 消息流可视高度与滚动位置：指示器据此判断当前落在哪一轮 */
const streamViewport = ref(0);
const streamScrollTop = ref(0);
/** 消息渲染/尺寸变化都会挪动锚点位置，用它触发重算 */
const layoutTick = ref(0);
/**
 * 平滑跳转的导航锁：滚动动画期间按位置判定会一路掠过中间几轮，高亮跟着乱闪，
 * 所以先把高亮钉在用户点选的那一项上，动画结束后再交还给位置判定。
 *
 * 必须做成响应式：锁定用普通变量的话，到期后没有任何依赖变化，计算属性不会重算，
 * 高亮会一直卡在被点的那一项上。
 */
const navLockId = ref<string | null>(null);
let navLockTimer: ReturnType<typeof setTimeout> | undefined;

function lockNav(id: string) {
  navLockId.value = id;
  clearTimeout(navLockTimer);
  navLockTimer = setTimeout(() => {
    navLockId.value = null;
  }, NAV_LOCK_MS);
}

function onScroll() {
  const el = scrollEl.value;
  if (!el) return;
  streamScrollTop.value = el.scrollTop;
  streamViewport.value = el.clientHeight;
  stickBottom.value = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
}

function onStreamResize() {
  const el = scrollEl.value;
  if (!el) return;
  streamViewport.value = el.clientHeight;
  layoutTick.value += 1;
}

async function pickFiles(ev: Event) {
  const input = ev.target as HTMLInputElement;
  const files = Array.from(input.files ?? []);
  input.value = '';
  if (files.length === 0) return;
  await uploadFiles(files);
}

/** 上传选中的文件并把引用挂到待发送列表。 */
async function uploadFiles(files: File[]) {
  if (files.length === 0) return;
  uploading.value = true;
  try {
    const refs = await chat.upload(files);
    refs.forEach((r, i) => pendingUploads.value.push({ ref: r, name: files[i]?.name ?? r }));
  } catch (e) {
    toast.error(t('uploadFailed', { message: (e as Error).message }));
  } finally {
    uploading.value = false;
  }
}

// ---- 拖拽上传：自实现落点提示，不借助浏览器默认行为 ----
const dragging = ref(false);
// 拖拽经过子元素时会连续触发 dragleave，用计数器避免指示器闪烁
let dragDepth = 0;

function onDragEnter(ev: DragEvent) {
  if (!ready.value || !hasFiles(ev)) return;
  ev.preventDefault();
  dragDepth += 1;
  dragging.value = true;
}

function onDragOver(ev: DragEvent) {
  if (!ready.value || !hasFiles(ev)) return;
  // 必须阻止默认行为，否则浏览器会直接打开被拖入的文件
  ev.preventDefault();
  if (ev.dataTransfer) ev.dataTransfer.dropEffect = 'copy';
}

function onDragLeave() {
  dragDepth = Math.max(0, dragDepth - 1);
  if (dragDepth === 0) dragging.value = false;
}

async function onDrop(ev: DragEvent) {
  dragDepth = 0;
  dragging.value = false;
  if (!ready.value) return;
  ev.preventDefault();
  const files = Array.from(ev.dataTransfer?.files ?? []);
  if (files.length > 0) await uploadFiles(files);
}

/** 仅接受携带文件的拖拽（拖动选中文字不应触发落点提示）。 */
function hasFiles(ev: DragEvent): boolean {
  return Array.from(ev.dataTransfer?.types ?? []).includes('Files');
}

function send() {
  // 申请系统通知权限必须处于用户手势中；借首次发送顺带申请，
  // 不阻塞发送：被拒时仅少一路系统通知，应用内 toast 不受影响。
  void ensureNotificationPermission();
  const text = draft.value;
  const attachments = pendingUploads.value.map((p) => p.ref);
  if (!text.trim() && attachments.length === 0) return;
  if (forkFrom.value !== null && attachments.length > 0) {
    toast.error(t('forkAttachmentUnsupported'));
    return;
  }

  const delivered =
    forkFrom.value !== null
      ? chat.forkAndRun(forkFrom.value, text)
      : chat.submit(text, attachments);
  if (!delivered) {
    // 未送达时保留草稿与附件，避免内容凭空消失
    toast.error(t('notConnected'));
    return;
  }
  if (forkFrom.value !== null) forkFrom.value = null;
  draft.value = '';
  pendingUploads.value = [];
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
  if (!chat.cancel()) {
    toast.error(t('notConnected'));
    return;
  }
  toast.info(t('cancelRequested'));
}

function userText(id: string): string {
  const m = chat.messages.value.find((x) => x.id === id);
  const b = m?.content.find((c) => c.type === 'text');
  return b && b.type === 'text' ? b.text : '';
}

/** 刚复制过的消息 id：用于把图标短暂换成对勾作为反馈。 */
const copiedId = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | null = null;

/** 复制单条用户消息的文本内容。 */
async function copyMessage(id: string) {
  const text = userText(id);
  if (!text) return;
  if (!(await copyText(text))) {
    toast.error(t('copyFailed'));
    return;
  }
  toast.success(t('copied'));
  copiedId.value = id;
  if (copiedTimer) clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => {
    copiedId.value = null;
  }, 1200);
}

const agentOptions = computed(() => chat.agents.value.map((a) => ({ value: a.id, label: a.name })));

// MCP 概览徽标已迁到 TopToolbar（工作区外壳），此处不再保留旧顶栏

/** 规范推理等级：与后端 REASONING_LEVELS 一致 */
const REASONING_LEVELS = ['minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra'] as const;

/** 当前选中模型是否支持思考：决定是否展示推理等级选择器 */
const supportsThinking = computed(() => {
  const wanted = chat.activeModel.value;
  if (!wanted) return false;
  const [pid, ...rest] = wanted.split('/');
  const mid = rest.join('/');
  const list = chat.modelCatalog.value[pid ?? ''] ?? [];
  return list.find((m) => m.id === mid)?.capabilities.includes('thinking') ?? false;
});

const reasoningOptions = computed(() =>
  REASONING_LEVELS.map((v) => ({ value: v, label: v })),
);

const isEmpty = computed(() => chat.messages.value.length === 0 && !chat.running.value);

/**
 * 消息上的模型标签（`provider/model` → 模型显示名）。
 *
 * 会话中途可切模型，因此模型跟着消息走（见后端 `ChatMessage.model`）；
 * 旧数据没有该字段，退回当前模型名（与 pi-web 一致）。
 */
function messageModelLabel(model?: string | null): string {
  const selector = model || chat.activeModel.value;
  if (!selector) return '';
  return modelSelectorLabel(chat.modelCatalog.value, selector) ?? selector;
}

/**
 * 本轮 token 开销行（`耗时 · N 输入 · N 输出 · N 缓存读 · N 缓存写`）。
 *
 * 与 pi-web 的 `formatUsage` 对齐；oma 的模型配置里没有价格字段，故不展示成本。
 * 单位是各语言里的词（中文「输入/输出」），数字按当前语言做千分位。
 */
function usageText(usage?: TokenUsage | null, elapsedMs?: number): string {
  const parts: string[] = [];
  if (elapsedMs) parts.push(t('usageElapsed', { time: formatDuration(elapsedMs) }));
  if (usage) {
    if (usage.input_tokens) parts.push(t('usageIn', { count: usage.input_tokens.toLocaleString() }));
    if (usage.output_tokens) parts.push(t('usageOut', { count: usage.output_tokens.toLocaleString() }));
    if (usage.cache_read_tokens)
      parts.push(t('usageCacheRead', { count: usage.cache_read_tokens.toLocaleString() }));
    if (usage.cache_write_tokens)
      parts.push(t('usageCacheWrite', { count: usage.cache_write_tokens.toLocaleString() }));
  }
  return parts.join(' · ');
}
/** 会话已建立且 WebSocket 在线时才允许提交指令 */
const ready = computed(() => !!activeSessionId.value && props.online && chat.connected.value);

/**
 * 每条助手消息的耗时（毫秒）：从本轮提问（用户消息）到这条回复落库。
 *
 * 服务端只在助手消息上盖「这次请求读完流」的时刻，没有轮次起始时间戳，因此整轮
 * 墙钟耗时只能按 `created_at` 差值推：一条含工具往返的轮次里，最后一条助手消息
 * 的差值即整轮耗时，中间几条是当时的累计；流式中的消息还没有 `created_at`，不显示。
 */
const turnElapsedMs = computed<Record<string, number>>(() => {
  const elapsed: Record<string, number> = {};
  let askedAt = 0;
  for (const m of chat.messages.value) {
    if (m.role === 'user' && !chat.isInternalMessage(m)) askedAt = m.created_at;
    else if (m.role === 'assistant' && askedAt > 0 && m.created_at > askedAt) {
      elapsed[m.id] = m.created_at - askedAt;
    }
  }
  return elapsed;
});

/**
 * 右侧竖线导航：每条用户消息对应一个点，悬停展示提示词。
 * 仅取文本块，附件、工具回执（内部消息）不参与。
 */
const railItems = computed(() =>
  chat.messages.value
    .filter((m) => m.role === 'user' && !chat.isInternalMessage(m))
    .map((m) => ({ id: m.id, text: userText(m.id) })),
);

/** 消息元素引用：导航点据此定位目标，避免依赖会随重构失效的属性选择器。 */
const msgEls: Record<string, HTMLElement | null> = {};
function setMsgEl(id: string, el: unknown) {
  const node = (el ?? null) as HTMLElement | null;
  if (node) msgEls[id] = node;
  else delete msgEls[id];
}

/** 跳转后锚点停在可视高度的这个比例处（与 `railActiveId` 用同一基准线） */
const ANCHOR_RATIO = 0.35;
/** 平滑滚动动画的时长上限：这段时间内高亮钉在被点的那一轮（对齐 pi-web） */
const NAV_LOCK_MS = 1000;

/** 某条消息相对消息流内容顶部的位置 */
function offsetOf(id: string): number | null {
  const el = msgEls[id];
  if (!el) return null;
  const stream = scrollEl.value;
  if (!stream) return el.offsetTop;
  // 用 rect 差值算，避免 offsetParent 变化时偏移基准搞错
  return el.getBoundingClientRect().top - stream.getBoundingClientRect().top + stream.scrollTop;
}

/**
 * 当前所在的一轮：基准线之前（含）的最后一条锚点。
 *
 * pi-web 用的是「离基准线最近」——只有两条锚点时，停在顶部反而会高亮第二条。改成
 * 「区间」判定后语义与阅读位置一致：滚到哪一轮，就高亮哪一轮；还没到第一轮时高亮
 * 第一轮；跳转把锚点对到基准线上，因此跳完命中的必然是被点的那一轮。
 *
 * 依赖 `layoutTick`——消息渲染、窗口缩放都会挪动锚点位置，而那时的 scrollTop
 * 可能没变，必须显式触发重算。
 */
const railActiveId = computed<string | null>(() => {
  void layoutTick.value;
  const items = railItems.value;
  if (items.length === 0) return null;
  if (navLockId.value) return navLockId.value;
  const line = streamScrollTop.value + streamViewport.value * ANCHOR_RATIO;
  let current: string | null = null;
  for (const item of items) {
    const offset = offsetOf(item.id);
    if (offset === null) continue;
    if (current === null) current = item.id;
    if (offset <= line) current = item.id;
    else break;
  }
  return current;
});

/** 点击/拖拽标记：把对应用户消息滚到基准线附近（而非顶部，保留上下文）。 */
function jumpToMessage(id: string) {
  const stream = scrollEl.value;
  const offset = offsetOf(id);
  if (!stream || offset === null) return;
  lockNav(id);
  stream.scrollTo({
    top: Math.max(0, offset - stream.clientHeight * ANCHOR_RATIO),
    behavior: 'smooth',
  });
}

onMounted(() => {
  const el = scrollEl.value;
  if (!el) return;
  streamViewport.value = el.clientHeight;
  const observer = new ResizeObserver(onStreamResize);
  observer.observe(el);
  onBeforeUnmount(() => observer.disconnect());
});

// 消息增删或换分支后锚点位置会变，重新判定当前一轮
watch(() => [chat.messages.value, activeSessionId.value], () => void nextTick(() => (layoutTick.value += 1)));

const hasProviders = computed(() => Object.keys(chat.modelCatalog.value).length > 0);
</script>

<template>
  <div class="chat">
    <div class="stream-wrap">
      <div ref="scrollEl" class="stream" @scroll.passive="onScroll">
        <div v-if="!activeSessionId" class="hero">
          <LuBot :size="42" class="hero-icon" />
          <h2>{{ t('welcomeTitle') }}</h2>
          <p>{{ t('welcomeBody') }}</p>
          <UiButton variant="solid" tone="accent" size="md" @click="requestNewSession">
            <template #prefix><LuPlus :size="14" /></template>
            {{ t('newSession') }}
          </UiButton>
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
          <UiButton variant="soft" tone="neutral" v-if="!hasProviders" @click="emit('needSettings')">{{ t('goSettings') }}</UiButton>
        </div>

        <template v-else>
          <article
            v-for="m in chat.messages.value.filter((x) => !chat.isInternalMessage(x))"
            :key="m.id"
            :ref="(el) => setMsgEl(m.id, el)"
            class="msg"
            :class="m.role"
          >
            <template v-if="m.role === 'user'">
              <div class="user-bubble">
                <MessageBlocks :blocks="m.content" :streaming="false" :results="chat.toolResults.value" />
              </div>
              <div class="user-actions">
                <UiButton
                  class="fork"
                  variant="ghost"
                  tone="neutral"
                  size="sm"
                  @click="copyMessage(m.id)"
                >
                  <LuCheck v-if="copiedId === m.id" :size="11" />
                  <LuCopy v-else :size="11" />
                  {{ t('copy') }}
                </UiButton>
                <UiButton
                  v-if="m.parent_id"
                  class="fork"
                  variant="ghost"
                  tone="neutral"
                  size="sm"
                  @click="startFork(m.parent_id, userText(m.id))"
                >
                  <LuPencil :size="11" /> {{ t('editResend') }}
                </UiButton>
                <UiButton
                  class="fork"
                  variant="ghost"
                  tone="neutral"
                  size="sm"
                  @click="chat.deleteMessage(m.id)"
                >
                  <LuTrash2 :size="11" /> {{ t('delete') }}
                </UiButton>
              </div>
            </template>
            <template v-else>
              <div v-if="messageModelLabel(m.model)" class="model-label">{{ messageModelLabel(m.model) }}</div>
              <MessageBlocks :blocks="m.content" :streaming="false" :results="chat.toolResults.value" />
              <div v-if="usageText(m.usage, turnElapsedMs[m.id])" class="turn-usage">
                {{ usageText(m.usage, turnElapsedMs[m.id]) }}
              </div>
            </template>
          </article>

          <article v-if="chat.running.value || chat.finalizing.value" class="msg assistant">
            <div v-if="chat.activeModel.value" class="model-label">{{ messageModelLabel(null) }}</div>
            <MessageBlocks :blocks="chat.renderBlocks.value" :streaming="true" />
            <div v-if="chat.running.value" class="live-row">
              <LuLoader :size="13" class="spin" />
            </div>
          </article>

          <!--
            排队中的输入：单独成区、紧跟在流式块下方。
            它们还没落库，不能混进正式消息流——否则会被排在正在流式的回复**上方**，
            看起来就像排队消息插到了上一条回复前面。
          -->
          <article v-for="m in chat.queuedMessages.value" :key="m.id" class="msg user queued">
            <div class="user-bubble">
              <MessageBlocks :blocks="m.content" :streaming="false" :results="chat.toolResults.value" />
            </div>
            <div class="user-actions queued-actions">
              <span class="queued-badge">
                <LuLoader :size="11" class="spin" />
                {{ t('queuedPending') }}
              </span>
            </div>
          </article>
        </template>
      </div>
      <MessageRail :items="railItems" :active-id="railActiveId" @jump="jumpToMessage" />
    </div>

    <footer class="composer">
      <div v-if="pendingUploads.length > 0" class="attach-row">
        <span v-for="(a, i) in pendingUploads" :key="a.ref" class="attach-chip">
          <LuPaperclip :size="11" />
          <span class="attach-name">{{ a.name }}</span>
          <UiIconButton
            class="chip-x"
            size="sm"
            :label="t('removeAttachment')"
            @click="pendingUploads.splice(i, 1)"
          >
            <LuX :size="11" />
          </UiIconButton>
        </span>
      </div>
      <div v-if="previewing" class="preview-banner">
        <LuListTree :size="12" />
        <span>{{ t('previewBanner') }}</span>
        <UiButton class="preview-back" variant="ghost" tone="neutral" size="sm" @click="chat.followCurrent()">
          {{ t('backToLatest') }}
        </UiButton>
      </div>
      <div v-if="forkFrom !== null" class="fork-banner">
        <LuGitBranch :size="12" />
        <span>{{ t('editResendBanner') }}</span>
        <UiButton class="fork-cancel" variant="ghost" tone="neutral" size="sm" @click="cancelFork">
          {{ tc('cancel') }}
        </UiButton>
      </div>
      <div
        ref="boxEl"
        class="box"
        :class="{ disabled: !ready, dragging }"
        :style="boxHeight ? { height: `${boxHeight}px` } : undefined"
        @dragenter="onDragEnter"
        @dragover="onDragOver"
        @dragleave="onDragLeave"
        @drop="onDrop"
      >
        <!-- 拖拽落点提示：自实现浮层，不依赖浏览器默认高亮 -->
        <div v-if="dragging" class="drop-hint" aria-hidden="true">
          <LuArrowDownToLine :size="18" />
          <span>{{ t('dropToAttach') }}</span>
        </div>
        <div
          class="resize-handle"
          role="separator"
          aria-orientation="horizontal"
          :aria-label="t('resizeHint')"
          tabindex="0"
          @pointerdown="startResize"
          @keydown.up.prevent="nudgeBoxHeight(24)"
          @keydown.down.prevent="nudgeBoxHeight(-24)"
        >
          <LuGripHorizontal :size="12" class="grip" />
        </div>
        <div class="composer-input" @keydown.enter.exact.prevent="send">
          <UiTextarea
            v-model="draft"
            :rows="2"
            :placeholder="activeSessionId ? t('placeholder') : t('placeholderNoSession')"
            :disabled="!ready"
          />
        </div>
        <!--
          隐藏的原生 file input 仅充当“文件选择器通道”（浏览器不允许
          自绘系统文件对话框）；可见的按钮、拖拽区与附件列表全部自实现。
        -->
        <input
          ref="fileInput"
          type="file"
          multiple
          class="file-input"
          @change="pickFiles"
        />
        <div class="c-toolbar">
          <div class="c-controls">
            <UiTooltip :content="t('attachHint')">
              <UiIconButton
                class="icon-btn"
                size="sm"
                :label="t('attach')"
                :disabled="!ready || uploading"
                @click="fileInput?.click()"
              >
                <LuPaperclip :size="13" />
              </UiIconButton>
            </UiTooltip>
            <span v-if="chat.queued.value > 0" class="queued-chip">
              {{ t('queuedCount', { count: chat.queued.value }) }}
            </span>
            <UiSelect
              :model-value="chat.activeAgent.value"
              :options="agentOptions"
              style="width: 118px"
              @update:model-value="(v) => chat.setAgent(String(v ?? ''))"
            />
            <UiSelect
              :model-value="chat.activeModel.value"
              :groups="modelGroups"
              style="width: 170px"
              @update:model-value="(v) => chat.setModel(String(v ?? ''))"
            >
              <template v-if="modelLabel" #value>{{ modelLabel }}</template>
            </UiSelect>
            <!-- 推理等级紧跟在模型选择右侧；仅支持思考的模型才展示 -->
            <UiSelect
              v-if="supportsThinking"
              :model-value="chat.reasoningLevel.value"
              :options="reasoningOptions"
              style="width: 110px"
              @update:model-value="(v) => chat.setReasoningLevel(String(v ?? ''))"
            />
            <!-- 上下文占用：位于推理等级右侧 -->
            <ContextGauge
              v-if="chat.contextUsage.value"
              :tokens="chat.contextUsage.value.tokens"
              :context-len="chat.contextUsage.value.contextLen"
            />
          </div>
          <div class="c-actions">
            <UiTooltip v-if="chat.running.value" :content="t('stopHint')">
              <UiIconButton class="icon-btn stop" size="sm" :label="t('stopHint')" @click="cancel">
                <LuSquare :size="13" />
              </UiIconButton>
            </UiTooltip>
            <UiTooltip :content="t('sendHint')">
              <UiIconButton
                class="icon-btn send"
                size="sm"
                :label="t('sendHint')"
                :disabled="(!draft.trim() && pendingUploads.length === 0) || !ready"
                @click="send"
              >
                <LuArrowUp :size="15" />
              </UiIconButton>
            </UiTooltip>
          </div>
        </div>
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
  /* 竖向收缩下限必须归零：默认的 auto 会让聊天列按内容高度撑开，
     输入框控件一旦换行就把整列顶出外壳（外壳 overflow: hidden，滚不回来） */
  min-height: 0;
  background: var(--paper);
  /* 消息与输入框共用同一列宽，保证左右边缘对齐 */
  --chat-col: 1120px;
}
.stream-wrap {
  position: relative;
  flex: 1;
  min-height: 0;
  display: flex;
}
.stream {
  flex: 1;
  overflow-y: auto;
  /* 左右对称：右侧为竖线导航让位，同时保持消息列与输入框列同轴居中；
     底部不预留内边距——输入框容器自身已有上边距，两边叠加会让消息
     到此的距离比流式图标上方的间距大一倍 */
  padding: 14px 24px 0;
  scrollbar-gutter: stable both-edges;
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
/*
 * 排队中的输入：压暗一点与已发送的消息区分，并始终展示「排队中」标签
 * （常态下 user-actions 是悬停才显示的，排队状态必须一眼可见）。
 */
.msg.user.queued .user-bubble {
  opacity: 0.72;
}
.queued-actions {
  opacity: 1;
}
.queued-badge {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 2px 7px;
  border-radius: 99px;
  background: var(--surface);
  color: var(--text-tertiary);
  font-size: 11px;
  white-space: nowrap;
}
.live-row {
  color: var(--accent);
  /* 与上方回复内容拉开距离；与下方到输入框的间距（消息底边距 + 输入框上边距）
     取值一致，图标上下留白相等 */
  margin-top: 16px;
}
/* 每轮的模型标签：弱色小字，压在消息内容上方（对齐 pi-web） */
.model-label {
  font-size: 11px;
  color: var(--overlay0);
  margin-bottom: 4px;
}
/* 本轮 token 开销：与 pi-web 的 `N in · N out · …` 行一致 */
.turn-usage {
  font-size: 11px;
  color: var(--overlay0);
  margin-top: 4px;
  font-variant-numeric: tabular-nums;
}
/* 加载动画：与完成态标签同一高度感，不把行高撑出多余的空白 */
.live-row svg {
  display: block;
}
.fork {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: auto;
  border: none;
  background: transparent;
  color: var(--overlay0);
  font-family: inherit;
  font-size: 11px;
  cursor: pointer;
  padding: 1px 5px;
  border-radius: 5px;
}
.fork :deep(.ui-button__label) {
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
.fork:hover {
  color: var(--accent);
  background: var(--surface-hover);
}
.composer {
  padding: 8px 24px 12px;
  max-width: calc(var(--chat-col) + 20px);
  width: 100%;
  margin: 0 auto;
}
.preview-banner {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 8px;
  font-size: 12px;
  color: var(--accent);
}
.preview-back {
  height: auto;
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  font-family: inherit;
  font-size: 11.5px;
  cursor: pointer;
  padding: 0;
  text-decoration: underline;
}
.preview-back:hover {
  background: transparent;
  color: var(--ink);
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
  height: auto;
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  font-size: 11.5px;
  cursor: pointer;
  padding: 0;
  text-decoration: underline;
}
.fork-cancel:hover {
  background: transparent;
  color: var(--ink);
}
.attach-row {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  margin-bottom: 8px;
}
.attach-chip {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  max-width: 220px;
  padding: 3px 6px 3px 8px;
  border-radius: 6px;
  background: var(--surface);
  color: var(--text-secondary);
  font-size: 11.5px;
}
.attach-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.attach-chip button {
  display: inline-flex;
  align-items: center;
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  cursor: pointer;
  padding: 0;
}
/* 附件胶囊里的移除按钮：压到与胶囊同高的小图标 */
.chip-x {
  width: 14px !important;
  height: 14px !important;
  min-width: 0 !important;
  padding: 0;
  color: var(--text-tertiary);
}
.chip-x:hover {
  background: transparent;
  color: var(--danger);
}
.attach-chip button:hover {
  color: var(--danger);
}
/* 原生文件选择器仅作触发入口，不参与布局 */
.file-input {
  display: none;
}
.queued-chip {
  flex-shrink: 0;
  padding: 2px 7px;
  border-radius: 99px;
  background: var(--surface);
  color: var(--text-tertiary);
  font-size: 11px;
  white-space: nowrap;
}
.box {
  position: relative;
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
.box.dragging {
  border-color: var(--accent);
}
/* 拖拽落点提示：覆盖整个输入区，虚线与图标均由 CSS + 图标组件绘制 */
.drop-hint {
  position: absolute;
  inset: 0;
  z-index: 2;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  border-radius: 12px;
  background: color-mix(in srgb, var(--accent) 12%, var(--paper));
  color: var(--accent);
  font-size: 12.5px;
  font-weight: 500;
  /* 覆盖层不参与命中测试：拖拽事件始终按未覆盖时的路径命中 .box，
     免得浮层自身引发额外的 dragenter/dragleave 抖动 */
  pointer-events: none;
  outline: 2px dashed color-mix(in srgb, var(--accent) 55%, transparent);
  outline-offset: -6px;
}
/* 上边沿拖拽热区：悬停时浮现把手图标 */
.resize-handle {
  position: absolute;
  top: 0;
  left: 0;
  right: 0;
  height: 9px;
  display: flex;
  align-items: center;
  justify-content: center;
  cursor: ns-resize;
  touch-action: none;
}
.grip {
  color: var(--overlay0);
  opacity: 0;
  transition: opacity 0.15s ease;
}
.box:hover .grip,
.resize-handle:focus-visible .grip {
  opacity: 0.7;
}
.resize-handle:focus-visible {
  outline: none;
}
.composer-input {
  flex: 1;
  min-width: 0;
}
/* 组合器里的输入区去掉组件库默认边框与内边距，还原为无框文字区 */
.composer-input :deep(textarea) {
  border: none;
  outline: none;
  background: transparent;
  box-shadow: none;
  color: var(--ink);
  font-family: inherit;
  font-size: 13px;
  line-height: 20px;
  padding: 12px 16px 2px;
  resize: none;
}
.composer-input :deep(textarea)::placeholder {
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
  width: 28px !important;
  height: 28px !important;
  min-width: 0 !important;
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
</style>
