<script setup lang="ts">
withDefaults(
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
</script>

<template>
  <div class="o-input" :class="{ disabled }">
    <!-- 前缀图标槽（如搜索放大镜）：留空则不占位 -->
    <span v-if="$slots.prefix" class="affix prefix"><slot name="prefix" /></span>
    <input
      :type="type"
      :value="modelValue"
      :placeholder="placeholder"
      :disabled="disabled"
      :autofocus="autofocus"
      @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
      @keydown.enter="emit('enter')"
      @blur="emit('blur')"
    />
    <!-- 后缀槽（如清空按钮）：内容与点击行为都由调用方决定，
         避免把点击热区写成只有图标本身那么大 -->
    <span v-if="$slots.suffix" class="affix suffix"><slot name="suffix" /></span>
  </div>
</template>

<style scoped>
.o-input {
  display: flex;
  align-items: center;
  background: var(--surface);
  border: 1px solid var(--control-border);
  border-radius: 8px;
  transition: border-color 0.15s ease;
}
.o-input:focus-within {
  border-color: var(--accent);
}
.o-input.disabled {
  opacity: 0.55;
}
.affix {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  color: var(--text-tertiary);
}
.prefix {
  padding-left: 9px;
}
.suffix {
  padding-right: 6px;
}
input {
  flex: 1;
  min-width: 0;
  border: none;
  outline: none;
  background: transparent;
  color: var(--ink);
  font-family: inherit;
  font-size: 13px;
  padding: 7px 10px;
}
/* 带前缀时压缩左侧内边距，避免图标与文字之间出现双倍间距 */
.prefix + input {
  padding-left: 6px;
}
input::placeholder {
  color: var(--overlay0);
}
</style>
