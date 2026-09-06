<script setup lang="ts">
import { Fa6NetworkWired, Fa6Plus, Fa6Terminal, Fa6Trash } from 'vue-icons-plus/fa6';
import { computed, ref, watch } from 'vue';
import type { McpServerConfig, OmaConfigView } from '../../types';
import StringMapEditor from './StringMapEditor.vue';
import { uiConfirm } from '../../useDialogs';

const props = defineProps<{
  config: OmaConfigView;
}>();

const names = computed(() => Object.keys(props.config.mcp_servers));
const selected = ref('');
const newName = ref('');
const nameError = ref('');

const server = computed<McpServerConfig | null>(
  () => props.config.mcp_servers[selected.value] ?? null,
);

// args 以每行一个参数编辑；解析成功才同步回 config
const argsText = ref('');
const savedArgs = ref<string[]>();

watch(server, (s) => {
  if (s && s.type === 'local') {
    savedArgs.value = s.args ?? [];
    argsText.value = (s.args ?? []).join('\n');
  }
});

watch(argsText, (txt) => {
  if (server.value?.type !== 'local' || !savedArgs.value) return;
  const next = txt.split('\n').map((l) => l.trim()).filter(Boolean);
  if (JSON.stringify(next) !== JSON.stringify(savedArgs.value)) {
    savedArgs.value = next;
    server.value.args = next;
  }
});

function addServer() {
  const n = newName.value.trim();
  nameError.value = '';
  if (!n) {
    nameError.value = '请输入 Server 名称';
    return;
  }
  if (!/^[A-Za-z0-9_-]+$/.test(n)) {
    nameError.value = '名称仅允许字母、数字、下划线和中划线';
    return;
  }
  if (props.config.mcp_servers[n]) {
    nameError.value = `MCP Server "${n}" 已存在`;
    return;
  }
  props.config.mcp_servers[n] = { type: 'local', command: '', args: [], env: {} };
  newName.value = '';
  selected.value = n;
}

async function deleteServer(n: string) {
  const ok = await uiConfirm({
    title: '删除 MCP Server',
    message: `确定删除 MCP Server "${n}" 吗？保存后其本地子进程将被终止。`,
    confirmText: '删除',
    danger: true,
  });
  if (!ok) return;
  delete props.config.mcp_servers[n];
  selected.value = names.value[0] ?? '';
}

function switchType(t: 'local' | 'remote') {
  if (!server.value || server.value.type === t) return;
  props.config.mcp_servers[selected.value] =
    t === 'local'
      ? { type: 'local', command: '', args: [], env: {} }
      : { type: 'remote', url: '', headers: {} };
}
</script>

<template>
  <div class="providers-layout">
    <!-- 左侧 MCP Server 清单 -->
    <aside class="providers-list">
      <div class="providers-add">
        <input v-model="newName" class="field-input" placeholder="新 Server 名称" @keyup.enter="addServer" />
        <button class="btn-icon" v-tip="'添加 MCP Server'" @click="addServer"><Fa6Plus /></button>
      </div>
      <div v-if="nameError" class="providers-name-error">{{ nameError }}</div>
      <button
        v-for="n in names"
        :key="n"
        class="settings-nav-item"
        :class="{ active: n === selected }"
        @click="selected = n"
      >
        <Fa6Terminal v-if="config.mcp_servers[n].type === 'local'" />
        <Fa6NetworkWired v-else />
        {{ n }}
        <Fa6Trash class="providers-del" @click.stop="deleteServer(n)" />
      </button>
      <div v-if="!names.length" class="field-hint">尚无 MCP Server</div>
    </aside>

    <!-- 右侧编辑区 -->
    <section class="providers-editor">
      <div v-if="!server" class="field-hint">选择或新建一个 MCP Server 进行配置</div>

      <div v-else class="settings-panel">
        <div class="form-field">
          <label class="field-label">连接类型</label>
          <div class="radio-row">
            <label><input type="radio" :checked="server.type === 'local'" @change="switchType('local')" /> 本地 Stdio (子进程)</label>
            <label><input type="radio" :checked="server.type === 'remote'" @change="switchType('remote')" /> 远程 Streamable HTTP</label>
          </div>
        </div>

        <template v-if="server.type === 'local'">
          <div class="form-field">
            <label class="field-label">启动命令</label>
            <input v-model="server.command" class="field-input" type="text" placeholder="npx / uvx" />
          </div>
          <div class="form-field">
            <label class="field-label">命令参数 (每行一个)</label>
            <textarea v-model="argsText" class="field-input body-json" rows="4" spellcheck="false" placeholder="-y&#10;@modelcontextprotocol/server-filesystem"></textarea>
          </div>
          <div class="form-field">
            <label class="field-label">环境变量</label>
            <StringMapEditor v-model="server.env" key-placeholder="变量名" value-placeholder="变量值" />
          </div>
        </template>

        <template v-else>
          <div class="form-field">
            <label class="field-label">服务 URL</label>
            <input v-model="server.url" class="field-input" type="text" placeholder="https://mcp.example.com/mcp" />
          </div>
          <div class="form-field">
            <label class="field-label">请求头</label>
            <StringMapEditor v-model="server.headers" key-placeholder="Header 名" value-placeholder="Header 值" />
          </div>
        </template>

        <div class="field-hint">保存后 Daemon 立即重建 MCP 连接池：新增/变更的服务器重新握手发现工具，被删除的本地服务器子进程随之终止</div>
      </div>
    </section>
  </div>
</template>
