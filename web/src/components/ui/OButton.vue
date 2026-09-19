<script setup lang="ts">
import { computed } from 'vue';
import { UiButton, type UiTone, type UiVariant } from '@waittide/ui';

/**
 * oma 按钮 → @waittide/ui 的 `UiButton` 适配层。
 *
 * 保留 oma 既有的 `variant` 词汇，映射到组件库的 `variant × tone` 组合；
 * `#icon` 槽对应组件库的 `#prefix`。调用点无需改动。
 */
const props = withDefaults(
  defineProps<{
    variant?: 'primary' | 'ghost' | 'danger' | 'soft';
    size?: 'sm' | 'md';
    loading?: boolean;
    disabled?: boolean;
    ariaLabel?: string;
  }>(),
  { variant: 'soft', size: 'md', loading: false, disabled: false, ariaLabel: undefined },
);

const emit = defineEmits<{ click: [MouseEvent] }>();

const mapped = computed<{ variant: UiVariant; tone: UiTone }>(() => {
  switch (props.variant) {
    case 'primary':
      return { variant: 'solid', tone: 'accent' };
    case 'danger':
      return { variant: 'solid', tone: 'danger' };
    case 'ghost':
      return { variant: 'ghost', tone: 'neutral' };
    default:
      return { variant: 'soft', tone: 'neutral' };
  }
});

/**
 * 透传属性用对象 v-bind 传入。
 *
 * `UiButton` 未声明 click 事件与 aria-label 属性类型，`v-bind="obj"` 可绕过
 * 模板严格属性检查，同时保留原生事件的 fallthrough 行为。
 */
const bindings = computed<Record<string, unknown>>(() => {
  const attrs: Record<string, unknown> = { onClick: (event: MouseEvent) => emit('click', event) };
  if (props.ariaLabel) attrs['aria-label'] = props.ariaLabel;
  return attrs;
});
</script>

<template>
  <UiButton
    v-bind="bindings"
    :variant="mapped.variant"
    :tone="mapped.tone"
    :size="size"
    :loading="loading"
    :disabled="disabled"
  >
    <template v-if="$slots.icon" #prefix><slot name="icon" /></template>
    <slot />
  </UiButton>
</template>
