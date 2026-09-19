<script setup lang="ts" generic="T extends string">
import { computed } from 'vue';
import { UiSelect } from '@waittide/ui';

export interface SelectOption<V extends string = string> {
  value: V;
  label: string;
  hint?: string;
}

/**
 * oma 单选框 → `UiSelect` 适配层：`hint` 映射为组件库选项的 `description`。
 * `width` 由外层容器承载；`align` 由组件库浮层自身定位，这里仅为兼容旧调用保留。
 */
const props = withDefaults(
  defineProps<{
    modelValue: T;
    options: SelectOption<T>[];
    placeholder?: string;
    width?: string;
    align?: 'start' | 'end';
    disabled?: boolean;
  }>(),
  { placeholder: undefined, width: '100%', align: 'start', disabled: false },
);

const emit = defineEmits<{ 'update:modelValue': [T] }>();

const uiOptions = computed(() =>
  props.options.map((option) => ({
    label: option.label,
    value: option.value,
    description: option.hint,
  })),
);
</script>

<template>
  <div class="o-select-host" :style="{ width }">
    <UiSelect
      :model-value="modelValue"
      :options="uiOptions"
      :placeholder="placeholder"
      :disabled="disabled"
      @update:model-value="emit('update:modelValue', $event as T)"
    />
  </div>
</template>

<style scoped>
.o-select-host {
  display: block;
}
</style>
