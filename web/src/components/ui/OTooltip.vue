<script setup lang="ts">
import { UiTooltip } from '@waittide/ui';

/**
 * oma 气泡提示 → `UiTooltip` 适配层：`label` 映射到组件库的 `content`。
 */
withDefaults(
  defineProps<{
    label: string;
    align?: 'center' | 'start' | 'end';
    placement?: 'top' | 'bottom';
    block?: boolean;
  }>(),
  { align: 'center', placement: 'top', block: false },
);
</script>

<template>
  <UiTooltip :content="label" :align="align" :placement="placement" :block="block">
    <slot />
  </UiTooltip>
</template>
