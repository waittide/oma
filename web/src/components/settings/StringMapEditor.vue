<script setup lang="ts">
import { Fa6Plus, Fa6Trash } from 'vue-icons-plus/fa6';
import { ref, watch } from 'vue';

const props = defineProps<{
  modelValue?: Record<string, string>;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}>();

const emit = defineEmits<{
  (e: 'update:modelValue', v: Record<string, string>): void;
}>();

interface Row {
  k: string;
  v: string;
}

function toRows(map: Record<string, string> | undefined): Row[] {
  return Object.entries(map ?? {}).map(([k, v]) => ({ k, v }));
}

const rows = ref<Row[]>(toRows(props.modelValue));

watch(
  () => props.modelValue,
  (m) => {
    // 外部整体替换 (如切换编辑对象) 时同步行；自身回写产生的等值变更跳过
    if (JSON.stringify(m ?? {}) !== JSON.stringify(fromRows(rows.value))) {
      rows.value = toRows(m);
    }
  },
);

function fromRows(list: Row[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of list) {
    if (r.k.trim()) out[r.k.trim()] = r.v;
  }
  return out;
}

function sync() {
  emit('update:modelValue', fromRows(rows.value));
}
</script>

<template>
  <div class="map-editor">
    <div v-for="(row, i) in rows" :key="i" class="map-editor-row">
      <input v-model="row.k" class="field-input" :placeholder="keyPlaceholder || 'Key'" @input="sync" @blur="sync" />
      <input v-model="row.v" class="field-input" :placeholder="valuePlaceholder || 'Value'" @input="sync" @blur="sync" />
      <button class="btn-icon map-editor-del" title="删除该项" @click="rows.splice(i, 1); sync()">
        <Fa6Trash />
      </button>
    </div>
    <button class="btn-default map-editor-add" @click="rows.push({ k: '', v: '' }); sync()">
      <Fa6Plus style="vertical-align: -2px;" /> 添加条目
    </button>
  </div>
</template>
