<script setup lang="ts">
import { Fa6Gear, Fa6Xmark } from 'vue-icons-plus/fa6';
import { ref } from 'vue';
import { getToken, setToken } from '../api';

const props = defineProps<{
  workspace: string;
}>();

const emit = defineEmits<{
  (e: 'close'): void;
  (e: 'update-workspace', ws: string): void;
}>();

const inputToken = ref(getToken());
const inputWorkspace = ref(props.workspace);

function handleSave() {
  setToken(inputToken.value.trim());
  emit('update-workspace', inputWorkspace.value.trim());
  emit('close');
}
</script>

<template>
  <div class="modal-overlay" @click.self="$emit('close')">
    <div class="modal-dialog">
      <div class="modal-header">
        <span><Fa6Gear style="vertical-align: -2px;" /> 系统设置与接入配置</span>
        <button class="btn-icon" @click="$emit('close')"><Fa6Xmark /></button>
      </div>

      <div class="modal-body">
        <div>
          <label style="display: block; font-size: 12px; margin-bottom: 6px; color: var(--text-secondary);">
            Daemon 访问 Bearer Token:
          </label>
          <input
            v-model="inputToken"
            type="password"
            placeholder="请输入 ~/.local/share/oma/auth.token 中的 Token"
            style="width: 100%; background: var(--bg-input); border: 1px solid var(--border-default); border-radius: 6px; padding: 8px 10px; color: var(--text-primary); font-family: var(--font-mono); font-size: 13px;"
          />
        </div>

        <div>
          <label style="display: block; font-size: 12px; margin-bottom: 6px; color: var(--text-secondary);">
            工作区绝对路径 (Workspace):
          </label>
          <input
            v-model="inputWorkspace"
            type="text"
            placeholder="/absolute/path/to/workspace"
            style="width: 100%; background: var(--bg-input); border: 1px solid var(--border-default); border-radius: 6px; padding: 8px 10px; color: var(--text-primary); font-family: var(--font-mono); font-size: 13px;"
          />
        </div>

        <div style="font-size: 12px; color: var(--text-muted); line-height: 1.4;">
          * Token 会自动保存于浏览器 localStorage。修改后页面将使用新 Token 重新与 Daemon 进行握手。
        </div>
      </div>

      <div class="modal-footer">
        <button class="btn-default" @click="$emit('close')">取消</button>
        <button class="btn-primary" @click="handleSave">保存并应用</button>
      </div>
    </div>
  </div>
</template>
