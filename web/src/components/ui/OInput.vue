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
input::placeholder {
  color: var(--overlay0);
}
</style>
