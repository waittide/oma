<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { LuCheck, LuMessageCircleQuestion, LuX } from 'vue-icons-plus/lu';
import * as chat from '../stores/chat';
import { useTranslations } from '../composables/i18n';
import type { AskAnswer } from '../types';

const { t } = useTranslations('ask');

const req = chat.pendingAsk;

/** 每题已选标签；多选为数组，单选至多一个 */
const picked = ref<string[][]>([]);
/** 每题的自定义输入 */
const custom = ref<string[]>([]);
/** 当前题目下标 */
const index = ref(0);
/** 正在输入自定义答案的题目下标；null = 未开启 */
const customFor = ref<number | null>(null);
const customInput = ref<HTMLInputElement | null>(null);

const questions = computed(() => req.value?.questions ?? []);
const current = computed(() => questions.value[index.value] ?? null);
const isMulti = computed(() => !!current.value?.is_multi);

// 新提问到达时重置本地作答状态
watch(
  () => req.value?.request_id,
  () => {
    const list = questions.value;
    picked.value = list.map(() => []);
    custom.value = list.map(() => '');
    index.value = 0;
    customFor.value = null;
  },
  { immediate: true },
);

/** 「其他」是否已作答：以自定义文本为准，与当前是否在编辑无关 */
function otherChecked(i: number): boolean {
  return (custom.value[i]?.trim().length ?? 0) > 0;
}

/** 切题时结束编辑态，避免上一题的输入框状态带到下一题 */
function goTo(i: number) {
  index.value = i;
  customFor.value = null;
}

function toggle(label: string) {
  const i = index.value;
  const multi = !!questions.value[i]?.is_multi;
  const list = picked.value[i] ?? [];
  if (multi) {
    picked.value[i] = list.includes(label) ? list.filter((x) => x !== label) : [...list, label];
  } else {
    // 单选：点选项即放弃自定义输入，两者互斥
    picked.value[i] = list.includes(label) ? [] : [label];
    custom.value[i] = '';
    customFor.value = null;
  }
}

function openCustom() {
  const i = index.value;
  customFor.value = i;
  // 单选模式下自定义输入与预设选项互斥
  if (!isMulti.value) picked.value[i] = [];
  void nextTick(() => customInput.value?.focus());
}

/** 完成编辑但保留已输入内容，使其作为答案生效 */
function commitCustom() {
  customFor.value = null;
}

function clearCustom() {
  custom.value[index.value] = '';
  customFor.value = null;
}

function answered(i: number): boolean {
  return (picked.value[i]?.length ?? 0) > 0 || (custom.value[i]?.trim().length ?? 0) > 0;
}

const allAnswered = computed(() => questions.value.every((_, i) => answered(i)));

function toAnswers(): AskAnswer[] {
  return questions.value.map((_, i) => ({
    selected: [...(picked.value[i] ?? [])],
    custom_input: custom.value[i]?.trim() ?? '',
  }));
}

function submit() {
  chat.respondAsk(toAnswers());
}

function skip() {
  chat.respondAsk([], true);
}
</script>

<template>
  <Transition name="slide">
    <div v-if="req && current" class="ask-row">
      <div class="ask">
        <header class="ask-head">
          <LuMessageCircleQuestion :size="15" class="ask-icon" />
          <span class="ask-title">{{ t('askTitle') }}</span>
          <span v-if="questions.length > 1" class="ask-progress">
            {{ t('questionOf', { current: index + 1, total: questions.length }) }}
          </span>
          <span class="ask-mode">{{ isMulti ? t('multiHint') : t('singleHint') }}</span>
          <button type="button" class="ask-close" :aria-label="t('skip')" @click="skip">
            <LuX :size="14" />
          </button>
        </header>

        <p class="ask-question">{{ current.question }}</p>

        <div class="ask-options">
          <button
            v-for="(opt, oi) in current.options"
            :key="`${oi}-${opt.label}`"
            type="button"
            class="option"
            :class="{ on: (picked[index] ?? []).includes(opt.label) }"
            @click="toggle(opt.label)"
          >
            <span class="mark" :class="{ multi: isMulti }">
              <LuCheck v-if="(picked[index] ?? []).includes(opt.label)" :size="11" />
            </span>
            <span class="opt-main">
              <span class="opt-label">
                {{ opt.label }}
                <span v-if="current.recommended === oi" class="opt-reco">{{ t('recommended') }}</span>
              </span>
              <span v-if="opt.description" class="opt-desc">{{ opt.description }}</span>
            </span>
          </button>

          <!-- 其他（自定义输入）：与预设选项同列的横排 -->
          <div
            class="option custom"
            :class="{ on: otherChecked(index) || customFor === index }"
            @click="customFor === index ? undefined : openCustom()"
          >
            <button
              type="button"
              class="mark custom-mark"
              :class="{ multi: isMulti }"
              :aria-label="t('other')"
              @click.stop="otherChecked(index) ? clearCustom() : openCustom()"
            >
              <LuCheck v-if="otherChecked(index)" :size="11" />
            </button>
            <span class="opt-main">
              <span class="opt-label">{{ t('other') }}</span>
              <input
                v-if="customFor === index"
                ref="customInput"
                v-model="custom[index]"
                class="opt-input"
                :placeholder="t('otherPlaceholder')"
                @click.stop
                @keydown.enter.prevent="commitCustom"
                @keydown.esc="commitCustom"
              />
              <span v-else-if="otherChecked(index)" class="opt-desc custom-value">
                {{ custom[index] }}
              </span>
            </span>
          </div>
        </div>

        <footer class="ask-foot">
          <div class="ask-nav">
            <button
              v-if="questions.length > 1"
              type="button"
              class="nav-btn"
              :disabled="index === 0"
              @click="goTo(index - 1)"
            >
              {{ t('prev') }}
            </button>
            <button
              v-if="index < questions.length - 1"
              type="button"
              class="nav-btn"
              :disabled="!answered(index)"
              @click="goTo(index + 1)"
            >
              {{ t('next') }}
            </button>
          </div>
          <div class="ask-actions">
            <button type="button" class="skip-btn" @click="skip">{{ t('skip') }}</button>
            <button
              type="button"
              class="submit-btn"
              :disabled="!allAnswered"
              @click="submit"
            >
              {{ t('submit') }}
            </button>
          </div>
        </footer>
      </div>
    </div>
  </Transition>
</template>

<style scoped>
.ask-row {
  max-width: var(--chat-col);
  width: 100%;
  margin: 0 auto;
  padding: 0 14px 8px;
}
.ask {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 12px 14px;
  border: 1px solid color-mix(in srgb, var(--mauve) 40%, transparent);
  background: var(--surface);
  border-radius: 10px;
}
.ask-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.ask-icon {
  flex-shrink: 0;
  color: var(--mauve);
}
.ask-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--ink);
}
.ask-progress {
  font-size: 11.5px;
  color: var(--text-tertiary);
  font-variant-numeric: tabular-nums;
}
.ask-mode {
  padding: 1px 7px;
  border-radius: 99px;
  background: var(--surface-strong);
  color: var(--text-tertiary);
  font-size: 11px;
}
.ask-close {
  margin-left: auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
}
.ask-close:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.ask-question {
  margin: 0;
  font-size: 13.5px;
  line-height: 1.6;
  color: var(--ink);
  overflow-wrap: break-word;
}
.ask-options {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
/* 一个选项占一整行；多行文本在选项框内换行 */
.option {
  display: flex;
  align-items: flex-start;
  gap: 9px;
  width: 100%;
  padding: 8px 11px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--paper);
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  transition:
    border-color 0.12s ease,
    background-color 0.12s ease,
    color 0.12s ease;
}
.option:hover {
  border-color: var(--overlay0);
  color: var(--ink);
}
.option.on {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  color: var(--ink);
}
/* 选中标记：单选圆形、多选方形（用 border-radius 区分，无需额外图标） */
.mark {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  width: 15px;
  height: 15px;
  margin-top: 2px;
  border: 1.5px solid var(--control-border);
  border-radius: 99px;
  color: var(--base);
  transition:
    background-color 0.12s ease,
    border-color 0.12s ease;
}
.mark.multi {
  border-radius: 4px;
}
.option.on .mark {
  border-color: var(--accent);
  background: var(--accent);
}
.opt-main {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
}
.opt-label {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 500;
  /* 长标签在选项框内换行，不溢出右边界 */
  overflow-wrap: anywhere;
}
.opt-reco {
  flex-shrink: 0;
  padding: 0 6px;
  border-radius: 99px;
  background: color-mix(in srgb, var(--yellow) 22%, transparent);
  color: var(--yellow);
  font-size: 10.5px;
  font-weight: 500;
}
.opt-desc {
  font-size: 12px;
  line-height: 1.55;
  color: var(--text-tertiary);
  overflow-wrap: anywhere;
}
.custom {
  cursor: pointer;
}
.custom-mark {
  cursor: pointer;
  border-radius: 4px;
}
.custom-mark.multi {
  border-radius: 4px;
}
.custom-value {
  color: var(--ink);
}
.opt-input {
  width: 100%;
  margin-top: 2px;
  padding: 5px 8px;
  border: 1px solid var(--control-border);
  border-radius: 6px;
  background: var(--paper);
  color: var(--ink);
  font-family: inherit;
  font-size: 12.5px;
}
.opt-input:focus {
  outline: none;
  border-color: var(--accent);
}
.ask-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.ask-nav {
  display: flex;
  gap: 6px;
}
.nav-btn,
.skip-btn {
  border: none;
  background: transparent;
  color: var(--text-tertiary);
  font-family: inherit;
  font-size: 12.5px;
  padding: 5px 9px;
  border-radius: 6px;
  cursor: pointer;
}
.nav-btn:hover:not(:disabled),
.skip-btn:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.nav-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
.ask-actions {
  display: flex;
  gap: 6px;
}
.submit-btn {
  border: none;
  border-radius: 7px;
  padding: 6px 14px;
  background: var(--accent);
  color: var(--base);
  font-family: inherit;
  font-size: 12.5px;
  font-weight: 500;
  cursor: pointer;
}
.submit-btn:hover:not(:disabled) {
  filter: brightness(1.08);
}
.submit-btn:disabled {
  opacity: 0.45;
  cursor: not-allowed;
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
