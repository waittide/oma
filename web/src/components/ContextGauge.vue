<script setup lang="ts">
import { computed } from 'vue';
import OTooltip from './ui/OTooltip.vue';
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
 * 占用分档：阈值对齐 oh-my-pi（50 提醒 / 70 注意 / 90 危险）。
 * 70% 恰好是 oma 的压缩触发点（`context_len * 0.7`），因此该档同时表示已进入压缩区。
 */
const level = computed<'normal' | 'warning' | 'pending' | 'danger'>(() => {
  const p = percent.value;
  if (p === null) return 'normal';
  if (p >= 90) return 'danger';
  if (p >= 70) return 'pending';
  if (p >= 50) return 'warning';
  return 'normal';
});

/** 已达压缩阈值：此后每轮请求都会裁剪历史 */
const compacting = computed(() => (percent.value ?? 0) >= 70);

/** 10 格进度条：与参考实现的 contextGauge 一致 */
const filled = computed(() =>
  ratio.value === null ? 0 : Math.round(ratio.value * 10),
);

const tip = computed(() => {
  if (percent.value === null) {
    return t('contextUnknown', { tokens: props.tokens.toLocaleString() });
  }
  const base = t('contextTip', {
    tokens: props.tokens.toLocaleString(),
    total: props.contextLen.toLocaleString(),
    percent: percent.value.toFixed(1),
  });
  // OTooltip 以普通文本渲染（空白折叠），故不用换行而用分句拼接
  return compacting.value ? `${base} — ${t('contextCompacting')}` : base;
});
</script>

<template>
  <OTooltip :label="tip" placement="bottom">
    <span class="ctx" :class="level" role="status">
      <span class="bar" aria-hidden="true">
        <span v-for="i in 10" :key="i" class="cell" :class="{ on: i <= filled }" />
      </span>
      <span class="pct">{{ percentText }}</span>
      <!-- 始终渲染以预留宽度：跨过阈值时避免工具栏因标签出现而抖动 -->
      <span class="tag" :class="{ off: !compacting }">{{ t('contextCompactingTag') }}</span>
    </span>
  </OTooltip>
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
  padding: 0 10px;
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
  gap: 2px;
}
.cell {
  width: 4px;
  height: 12px;
  border-radius: 1px;
  background: var(--surface2);
}
/* 分档配色：绿 → 黄 → 橙 → 红（对齐参考实现的四级） */
.ctx.normal .cell.on {
  background: var(--green-color);
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
.tag {
  padding: 0 5px;
  border-radius: 99px;
  background: var(--surface-strong);
  color: var(--text-tertiary);
  font-size: 10px;
}
/* 未达阈值时不可见但占位，保证整体宽度恒定 */
.tag.off {
  visibility: hidden;
}
</style>
