<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { LuMessageCircleQuestion, LuX } from 'vue-icons-plus/lu';
import { UiButton, UiCheckbox, UiChoiceGroup, UiIconButton, UiInput } from '@waittide/ui';
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
const customInputRef = ref<InstanceType<typeof UiInput> | null>(null);

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

/** 预设选项 → UiChoiceGroup 选项；推荐项以角标呈现 */
const choiceOptions = computed(() =>
  (current.value?.options ?? []).map((option, optionIndex) => ({
    label: option.label,
    value: option.label,
    description: option.description,
    badge: current.value?.recommended === optionIndex ? t('recommended') : undefined,
  })),
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

/** 选择变化：单选时放弃自定义输入（两者互斥），多选则保留 */
function onChoice(list: (string | number)[]) {
  const i = index.value;
  picked.value[i] = list.map((value) => String(value));
  if (!isMulti.value) {
    custom.value[i] = '';
    customFor.value = null;
  }
}

function openCustom() {
  const i = index.value;
  customFor.value = i;
  // 单选模式下自定义输入与预设选项互斥
  if (!isMulti.value) picked.value[i] = [];
  void nextTick(() => customInputRef.value?.focus());
}

/** 完成编辑但保留已输入内容，使其作为答案生效 */
function commitCustom() {
  customFor.value = null;
}

function clearCustom() {
  custom.value[index.value] = '';
  customFor.value = null;
}

function setCustom(value: string) {
  custom.value[index.value] = value;
}

/** 「其他」勾选切换：勾上进入编辑，取消则清空 */
function toggleCustom(checked: boolean) {
  if (checked) openCustom();
  else clearCustom();
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
          <UiIconButton class="ask-close" size="sm" :label="t('skip')" @click="skip">
            <LuX :size="14" />
          </UiIconButton>
        </header>

        <p class="ask-question">{{ current.question }}</p>

        <div class="ask-options">
          <UiChoiceGroup
            :model-value="picked[index] ?? []"
            :options="choiceOptions"
            :multiple="isMulti"
            @update:model-value="onChoice"
          />

          <!-- 其他（自定义输入）：勾选后展开输入框，与预设选项同列 -->
          <div class="option custom" :class="{ on: otherChecked(index) || customFor === index }">
            <UiCheckbox
              :model-value="otherChecked(index) || customFor === index"
              :label="t('other')"
              @update:model-value="toggleCustom"
            />
            <span v-if="customFor === index" class="opt-input-wrap" @keydown.esc="commitCustom">
              <UiInput
                ref="customInputRef"
                :model-value="custom[index]"
                :placeholder="t('otherPlaceholder')"
                @update:model-value="setCustom"
                @enter="commitCustom"
              />
            </span>
            <span v-else-if="otherChecked(index)" class="opt-desc custom-value">{{ custom[index] }}</span>
          </div>
        </div>

        <footer class="ask-foot">
          <div class="ask-nav">
            <UiButton
              v-if="questions.length > 1"
              variant="ghost"
              size="sm"
              :disabled="index === 0"
              @click="goTo(index - 1)"
            >
              {{ t('prev') }}
            </UiButton>
            <UiButton
              v-if="index < questions.length - 1"
              variant="ghost"
              size="sm"
              :disabled="!answered(index)"
              @click="goTo(index + 1)"
            >
              {{ t('next') }}
            </UiButton>
          </div>
          <div class="ask-actions">
            <UiButton variant="ghost" size="sm" @click="skip">{{ t('skip') }}</UiButton>
            <UiButton variant="solid" tone="accent" size="sm" :disabled="!allAnswered" @click="submit">
              {{ t('submit') }}
            </UiButton>
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
/* 自定义输入与预设选项同列，选中态沿用强调色描边 */
.option.custom {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px 11px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--paper);
  transition:
    border-color 0.12s ease,
    background-color 0.12s ease;
}
.option.custom.on {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
.opt-input-wrap {
  display: block;
}
.opt-desc {
  font-size: 12px;
  line-height: 1.55;
  color: var(--text-tertiary);
  overflow-wrap: anywhere;
}
.custom-value {
  color: var(--ink);
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
.ask-actions {
  display: flex;
  gap: 6px;
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
