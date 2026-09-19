<script setup lang="ts" generic="T extends string = string">
import { computed } from 'vue';
import { UiMultiSelect } from '@waittide/ui';

export interface MultiOption<T extends string = string> {
  value: T;
  label: string;
  hint?: string;
}

/**
 * oma 多选下拉 → `UiMultiSelect` 适配层：`hint` 映射为选项的 `description`。
 * 保持泛型 `T extends string`，调用方的字面量联合类型仍能编译期收窄。
 */
const props = withDefaults(
  defineProps<{
    modelValue: T[];
    options: MultiOption<T>[];
    placeholder?: string;
    disabled?: boolean;
  }>(),
  { placeholder: undefined, disabled: false },
);

const emit = defineEmits<{ 'update:modelValue': [T[]] }>();

const uiOptions = computed(() =>
  props.options.map((option) => ({
    label: option.label,
    value: option.value,
    description: option.hint,
  })),
);
</script>

<template>
  <UiMultiSelect
    :model-value="modelValue"
    :options="uiOptions"
    :placeholder="placeholder"
    :disabled="disabled"
    @update:model-value="emit('update:modelValue', $event as T[])"
  />
</template>
