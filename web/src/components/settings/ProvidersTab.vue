<script setup lang="ts">
import { Fa6Plus, Fa6Server, Fa6Trash } from 'vue-icons-plus/fa6';
import { computed, ref, watch } from 'vue';
import type { OmaConfigView, ProviderConfig } from '../../types';
import OuiSelect from '../oui/OuiSelect.vue';
import { uiConfirm } from '../../useDialogs';

const props = defineProps<{
  config: OmaConfigView;
}>();

const API_TYPES = ['anthropic', 'completion', 'response', 'google'] as const;
const apiTypeOptions = API_TYPES.map((t) => ({ value: t, label: t }));

const names = computed(() => Object.keys(props.config.providers));
const selected = ref('');
const newName = ref('');
const nameError = ref('');

const provider = computed(() => props.config.providers[selected.value] ?? null);

// 请求体 JSON 以文本编辑；解析成功才同步回 config，失败时展示错误并保留上次有效值
const bodyText = ref('');
const bodyError = ref('');

watch(
  selected,
  () => {
    if (provider.value) {
      bodyText.value = JSON.stringify(provider.value.body ?? {}, null, 2);
      bodyError.value = '';
    }
  },
  { immediate: true },
);

watch(bodyText, (txt) => {
  if (!provider.value) return;
  try {
    provider.value.body = JSON.parse(txt || '{}');
    bodyError.value = '';
  } catch (e) {
    bodyError.value = `无效的 JSON: ${e instanceof Error ? e.message : e}`;
  }
});

function addProvider() {
  const n = newName.value.trim();
  nameError.value = '';
  if (!n) {
    nameError.value = '请输入 Provider 名称';
    return;
  }
  if (!/^[A-Za-z0-9_-]+$/.test(n)) {
    nameError.value = '名称仅允许字母、数字、下划线和中划线';
    return;
  }
  if (props.config.providers[n]) {
    nameError.value = `Provider "${n}" 已存在`;
    return;
  }
  props.config.providers[n] = {
    api_type: 'completion',
    base_url: '',
    api_key: '',
    headers: {},
    body: {},
    models: [],
  };
  newName.value = '';
  selected.value = n;
}

async function deleteProvider(n: string) {
  const ok = await uiConfirm({
    title: '删除 Provider',
    message: `确定删除 Provider "${n}" 及其所有模型配置吗？`,
    confirmText: '删除',
    danger: true,
  });
  if (!ok) return;
  delete props.config.providers[n];
  selected.value = names.value[0] ?? '';
}
</script>

<template>
  <div class="providers-layout">
    <!-- 左侧 Provider 清单 -->
    <aside class="providers-list">
      <div class="providers-add">
        <input v-model="newName" class="field-input" placeholder="新 Provider 名称" @keyup.enter="addProvider" />
        <button class="btn-icon" v-tip="'添加 Provider'" @click="addProvider"><Fa6Plus /></button>
      </div>
      <div v-if="nameError" class="providers-name-error">{{ nameError }}</div>
      <button
        v-for="n in names"
        :key="n"
        class="settings-nav-item"
        :class="{ active: n === selected }"
        @click="selected = n"
      >
        <Fa6Server /> {{ n }}
        <Fa6Trash class="providers-del" @click.stop="deleteProvider(n)" />
      </button>
      <div v-if="!names.length" class="field-hint">尚无 Provider，添加后即可配置模型目录</div>
    </aside>

    <!-- 右侧编辑区 -->
    <section class="providers-editor">
      <div v-if="!provider" class="field-hint">选择或新建一个 Provider 进行配置</div>

      <div v-else class="settings-panel">
        <div class="provider-grid">
          <div class="form-field">
            <label class="field-label">API 协议类型</label>
            <OuiSelect
              :model-value="provider.api_type"
              :options="apiTypeOptions"
              variant="field"
              @update:model-value="(v) => (provider!.api_type = v as ProviderConfig['api_type'])"
            />
          </div>
          <div class="form-field">
            <label class="field-label">Base URL</label>
            <input v-model="provider.base_url" class="field-input" type="text" placeholder="https://api.openai.com/v1" />
          </div>
          <div class="form-field">
            <label class="field-label">API Key</label>
            <input v-model="provider.api_key" class="field-input" type="text" placeholder="sk-… 或 env:VAR_NAME" />
            <div class="field-hint">*** 表示服务端已存有密钥，保持不动则沿用旧值；支持 env: 前缀引用环境变量</div>
          </div>
        </div>

        <div class="form-field">
          <label class="field-label">模型清单 (留空则按请求 id 动态合成默认元数据)</label>
          <table class="model-table">
            <thead>
              <tr>
                <th>模型 ID *</th>
                <th>显示名称</th>
                <th class="col-ctx">上下文长度</th>
                <th class="col-flag">视觉</th>
                <th class="col-flag">思考</th>
                <th class="col-del"></th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="(m, i) in provider.models" :key="i">
                <td><input v-model="m.id" class="field-input" type="text" placeholder="gpt-4o" /></td>
                <td><input v-model="m.name" class="field-input" type="text" placeholder="缺省同 ID" /></td>
                <td><input v-model.number="m.context_len" class="field-input" type="number" /></td>
                <td class="col-flag"><input v-model="m.supports_vision" type="checkbox" /></td>
                <td class="col-flag"><input v-model="m.supports_thinking" type="checkbox" /></td>
                <td class="col-del">
                  <button class="btn-icon" v-tip="'删除模型'" @click="provider.models!.splice(i, 1)"><Fa6Trash /></button>
                </td>
              </tr>
            </tbody>
          </table>
          <button
            class="btn-default map-editor-add"
            @click="provider.models = provider.models || []; provider.models.push({ id: '', name: '', context_len: 128000, supports_vision: true, supports_thinking: true })"
          >
            <Fa6Plus  /> 添加模型
          </button>
        </div>

        <div class="form-field">
          <label class="field-label">自定义请求头</label>
          <StringMapEditor v-model="provider.headers" key-placeholder="Header 名" value-placeholder="Header 值" />
        </div>

        <div class="form-field">
          <label class="field-label">请求体预设 JSON (随每次请求深度合并)</label>
          <textarea v-model="bodyText" class="field-input body-json" rows="5" spellcheck="false"></textarea>
          <div v-if="bodyError" class="settings-error">{{ bodyError }}</div>
        </div>
      </div>
    </section>
  </div>
</template>
