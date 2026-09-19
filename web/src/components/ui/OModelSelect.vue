<script setup lang="ts">
import { computed } from 'vue';
import { UiSelect, type UiSelectGroup } from '@waittide/ui';
import { useTranslations } from '../../composables/i18n';
import type { ModelInfo } from '../../types';

/**
 * 模型选择器 → `UiSelect` 分组模式的适配层。
 *
 * provider 作为分组标题，模型为组内选项，`modelValue` 仍为 `provider/model`
 * 选择器（与后端 find_model 一致）；未在清单中找到时回退显示模型名部分。
 */
const props = withDefaults(
  defineProps<{
    modelValue: string;
    /** provider id → 模型清单 */
    groups: Record<string, ModelInfo[]>;
    placeholder?: string;
    width?: string;
  }>(),
  { placeholder: undefined, width: '180px' },
);

const emit = defineEmits<{ 'update:modelValue': [string] }>();

const { t } = useTranslations('modelSelect');

/** "provider/model" → [provider, model]；模型 id 本身可含 '/'，仅在首个 '/' 处切分。 */
function splitSelector(value: string): [string, string] {
  const i = value.indexOf('/');
  return i === -1 ? ['', value] : [value.slice(0, i), value.slice(i + 1)];
}

const currentLabel = computed(() => {
  if (!props.modelValue) return null;
  const [provider, modelId] = splitSelector(props.modelValue);
  const model = (props.groups[provider] ?? []).find((item) => item.id === modelId);
  // 查不到元数据时也只显示模型部分，绝不露出 "provider/" 前缀
  return model ? model.name || model.id : modelId || provider || props.modelValue;
});

const uiGroups = computed<UiSelectGroup[]>(() =>
  Object.entries(props.groups).map(([provider, models]) => ({
    label: provider,
    options: models.map((model) => ({
      label: model.name || model.id,
      value: `${provider}/${model.id}`,
      description: `${Math.round(model.context_len / 1024)}K`,
    })),
  })),
);
</script>

<template>
  <div class="o-model-select" :style="{ width }">
    <UiSelect
      :model-value="modelValue"
      :groups="uiGroups"
      :placeholder="placeholder ?? t('placeholder')"
      @update:model-value="emit('update:modelValue', $event == null ? '' : String($event))"
    >
      <template #value>{{ currentLabel ?? placeholder ?? t('placeholder') }}</template>
    </UiSelect>
  </div>
</template>

<style scoped>
.o-model-select {
  display: block;
}
</style>
