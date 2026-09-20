<script setup lang="ts">
import { computed } from 'vue';
import { UiTooltip } from '@waittide/ui';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{
  /** 提示侧 token 总量（含缓存） */
  tokens: number;
  /** 模型上下文窗口 */
  contextLen: number;
}>();

const { t } = useTranslations('chat');

/** 占用比例（0~1）；窗口未知时视为 0，不展示误导性的百分比 */
const ratio = computed(() => {
  if (!Number.isFinite(props.contextLen) || props.contextLen <= 0) return null;
  return Math.min(1, Math.max(0, props.tokens / props.contextLen));
});

const percent = computed(() => (ratio.value === null ? null : ratio.value * 100));
const percentText = computed(() =>
  percent.value === null ? '?' : `${percent.value.toFixed(1)}%`,
);

/**
 * 占用分档：阈值对齐 oh-my-pi（50 / 70 / 90）。
 * 70% 恰好是 oma 的压缩触发点（`context_len * 0.7`），故该档起即已进入压缩区。
 */
const level = computed<'normal' | 'warning' | 'pending' | 'danger'>(() => {
  const p = percent.value;
  if (p === null) return 'normal';
  if (p >= 90) return 'danger';
  if (p >= 70) return 'pending';
  if (p >= 50) return 'warning';
  return 'normal';
});

/** 10 格进度条：与参考实现的 contextGauge 一致 */
const filled = computed(() =>
  ratio.value === null ? 0 : Math.round(ratio.value * 10),
);

/** 悬停明细：仅展示数值，分档含义由进度条颜色表达 */
const tip = computed(() => {
  if (percent.value === null) {
    return t('contextUnknown', { tokens: props.tokens.toLocaleString() });
  }
  return t('contextTip', {
    tokens: props.tokens.toLocaleString(),
    total: props.contextLen.toLocaleString(),
    percent: percent.value.toFixed(1),
  });
});
</script>

<template>
  <UiTooltip :content="tip" placement="bottom">
    <span class="ctx" :class="level" role="status">
      <span class="bar" aria-hidden="true">
        <span v-for="i in 10" :key="i" class="cell" :class="{ on: i <= filled }" />
      </span>
      <span class="pct">{{ percentText }}</span>
    </span>
  </UiTooltip>
</template>

<style scoped>
.ctx {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  flex-shrink: 0;
  /* 高度与 OSelect/OModelSelect 保持一致（均 32px） */
  height: 32px;
  padding: 0 8px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface);
  color: var(--text-tertiary);
  font-size: 11px;
  font-variant-numeric: tabular-nums;
  cursor: default;
  white-space: nowrap;
}
.bar {
  display: inline-flex;
  align-items: center;
  /* 固定轨道宽度：外层宽度由内容决定，flex:1 拿不到富余空间；
     固定尺寸同时避免百分比文字变长变短时进度条跟着抽动 */
  width: 72px;
  /* 格子两端对齐均分铺开，轨道随宽度拉开而非挤在左侧 */
  justify-content: space-between;
}
.cell {
  width: 5px;
  height: 12px;
  flex-shrink: 0;
  border-radius: 1px;
  background: var(--surface2);
}
/* 分档配色：绿 → 黄 → 橙 → 红（对齐参考实现的四级） */
.ctx.normal .cell.on {
  background: var(--green);
}
.ctx.warning .cell.on {
  background: var(--yellow);
}
.ctx.pending .cell.on {
  background: var(--peach);
}
.ctx.danger .cell.on {
  background: var(--red);
}
.ctx.warning .pct {
  color: var(--yellow);
}
.ctx.pending .pct {
  color: var(--peach);
}
.ctx.danger .pct {
  color: var(--red);
}
.ctx.danger {
  border-color: color-mix(in srgb, var(--red) 40%, transparent);
}
.pct {
  /* 定宽右对齐：'9.9%' 到 '100.0%' 字数不同，不定宽会造成数字左右跳动 */
  min-width: 34px;
  text-align: right;
}
</style>
