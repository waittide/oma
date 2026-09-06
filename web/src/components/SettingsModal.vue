<script setup lang="ts">
import { Fa6Cubes, Fa6Gear, Fa6Plug, Fa6Server, Fa6Sliders, Fa6Xmark } from 'vue-icons-plus/fa6';
import { onMounted, ref } from 'vue';
import type { OmaConfigView } from '../types';
import { fetchConfig, getToken, setToken, updateConfig } from '../api';
import GeneralTab from './settings/GeneralTab.vue';
import McpTab from './settings/McpTab.vue';
import ProvidersTab from './settings/ProvidersTab.vue';

const props = defineProps<{
  workspace: string;
}>();

const emit = defineEmits<{
  (e: 'close'): void;
  (e: 'update-workspace', ws: string): void;
}>();

type TabId = 'connection' | 'general' | 'providers' | 'mcp';

const activeTab = ref<TabId>('general');
const inputToken = ref(getToken());
const inputWorkspace = ref(props.workspace);

const config = ref<OmaConfigView | null>(null);
const loadError = ref('');
const saveError = ref('');
const saveSuccess = ref('');
const saving = ref(false);

onMounted(async () => {
  try {
    config.value = await fetchConfig();
  } catch (e) {
    loadError.value = `加载服务端配置失败: ${e}`;
  }
});

function handleSaveConnection() {
  setToken(inputToken.value.trim());
  emit('update-workspace', inputWorkspace.value.trim());
  emit('close');
}

async function handleSaveConfig() {
  if (!config.value || saving.value) return;
  saving.value = true;
  saveError.value = '';
  saveSuccess.value = '';
  try {
    await updateConfig(config.value);
    saveSuccess.value = '配置已保存并热更新至 Daemon';
  } catch (e) {
    saveError.value = `保存失败: ${e}`;
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="modal-overlay" @click.self="$emit('close')">
    <div class="modal-dialog settings-dialog">
      <div class="modal-header">
        <span><Fa6Gear style="vertical-align: -2px;" /> 系统设置</span>
        <button class="btn-icon" @click="$emit('close')"><Fa6Xmark /></button>
      </div>

      <div class="settings-layout">
        <nav class="settings-nav">
          <button
            class="settings-nav-item"
            :class="{ active: activeTab === 'general' }"
            @click="activeTab = 'general'"
          >
            <Fa6Sliders /> 默认偏好
          </button>
          <button
            class="settings-nav-item"
            :class="{ active: activeTab === 'providers' }"
            @click="activeTab = 'providers'"
          >
            <Fa6Server /> Providers
          </button>
          <button
            class="settings-nav-item"
            :class="{ active: activeTab === 'mcp' }"
            @click="activeTab = 'mcp'"
          >
            <Fa6Cubes /> MCP Servers
          </button>
          <button
            class="settings-nav-item"
            :class="{ active: activeTab === 'connection' }"
            @click="activeTab = 'connection'"
          >
            <Fa6Plug /> 接入连接
          </button>
        </nav>

        <div class="settings-content">
          <!-- 接入连接 (浏览器本地) -->
          <div v-if="activeTab === 'connection'" class="settings-panel">
            <div class="form-field">
              <label class="field-label">Daemon 访问 Bearer Token</label>
              <input
                v-model="inputToken"
                class="field-input"
                type="password"
                placeholder="请输入 ~/.local/share/oma/auth.token 中的 Token"
              />
              <div class="field-hint">Token 自动保存于浏览器 localStorage，修改后将以新 Token 重新与 Daemon 握手</div>
            </div>

            <div class="form-field">
              <label class="field-label">工作区绝对路径 (Workspace)</label>
              <input
                v-model="inputWorkspace"
                class="field-input"
                type="text"
                placeholder="/absolute/path/to/workspace"
              />
            </div>
          </div>

          <!-- 服务端持久化配置 -->
          <template v-else>
            <div v-if="loadError" class="settings-error">{{ loadError }}</div>
            <template v-else-if="config">
              <GeneralTab v-if="activeTab === 'general'" :config="config" />
              <ProvidersTab v-else-if="activeTab === 'providers'" :config="config" />
              <McpTab v-else :config="config" />
            </template>
            <div v-else class="field-hint">正在加载服务端配置…</div>
          </template>
        </div>
      </div>

      <div class="modal-footer">
        <span v-if="activeTab !== 'connection' && saveSuccess" class="settings-success">{{ saveSuccess }}</span>
        <span v-if="activeTab !== 'connection' && saveError" class="settings-error">{{ saveError }}</span>
        <button class="btn-default" @click="$emit('close')">关闭</button>
        <button v-if="activeTab === 'connection'" class="btn-primary" @click="handleSaveConnection">
          保存并应用
        </button>
        <button v-else class="btn-primary" :disabled="!config || saving" @click="handleSaveConfig">
          {{ saving ? '保存中…' : '保存到 Daemon' }}
        </button>
      </div>
    </div>
  </div>
</template>
