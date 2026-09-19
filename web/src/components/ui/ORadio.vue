<script setup lang="ts" generic="T extends string">
import { computed } from 'vue';
import { UiSegmented } from '@waittide/ui';

/**
 * oma 分段单选 → `UiSegmented` 适配层。
 *
 * ORadio 在 oma 里本就是「分段按钮组」的形态（而非圆点单选），
 * 对应组件库的 UiSegmented。
 */
const props = withDefaults(
  defineProps<{
    modelValue: T;
    options: { value: T; label: string }[];
    disabled?: boolean;
  }>(),
  { disabled: false },
);

const emit = defineEmits<{ 'update:modelValue': [T] }>();

const uiOptions = computed(() =>
  props.options.map((option) => ({ label: option.label, value: option.value })),
);
</script>

<template>
  <UiSegmented
    :model-value="modelValue"
    :options="uiOptions"
    :disabled="disabled"
    @update:model-value="emit('update:modelValue', $event as T)"
  />
</template>
