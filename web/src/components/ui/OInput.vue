<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { UiInput } from '@waittide/ui';

/**
 * oma 输入框 → `UiInput` 适配层。
 *
 * `UiInput` 已内建 `#prefix` / `#suffix` 槽与 search/password 的图标，
 * 因此这里只做事件名与自动聚焦的桥接。
 */
const props = withDefaults(
  defineProps<{
    modelValue: string;
    placeholder?: string;
    type?: 'text' | 'password';
    disabled?: boolean;
    autofocus?: boolean;
  }>(),
  { placeholder: '', type: 'text', disabled: false, autofocus: false },
);

const emit = defineEmits<{ 'update:modelValue': [string]; enter: []; blur: [] }>();

const inputRef = ref<InstanceType<typeof UiInput> | null>(null);

onMounted(() => {
  if (props.autofocus) inputRef.value?.focus();
});
</script>

<template>
  <UiInput
    ref="inputRef"
    :model-value="modelValue"
    :type="type"
    :placeholder="placeholder"
    :disabled="disabled"
    @update:model-value="emit('update:modelValue', $event)"
    @enter="emit('enter')"
    @blur="emit('blur')"
  >
    <template v-if="$slots.prefix" #prefix><slot name="prefix" /></template>
    <template v-if="$slots.suffix" #suffix><slot name="suffix" /></template>
  </UiInput>
</template>
