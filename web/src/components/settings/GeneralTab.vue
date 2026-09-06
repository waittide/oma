<script setup lang="ts">
import { Fa6CircleInfo } from 'vue-icons-plus/fa6';
import { computed } from 'vue';
import type { OmaConfigView } from '../../types';

const props = defineProps<{
  config: OmaConfigView;
}>();

// 从已配置 providers 汇总 "provider/model" 候选项，默认模型仍可自由输入
const modelOptions = computed(() =>
  Object.entries(props.config.providers).flatMap(([pid, p]) =>
    (p.models ?? []).map((m) => `${pid}/${m.id}`),
  ),
);

const agentOptions = ['task', 'plan', 'explore', 'review', 'build'];
</script>

<template>
  <div class="settings-panel">
    <div class="form-field">
      <label class="field-label">默认模型 (provider/model)</label>
      <input
        v-model="config.default_model"
        class="field-input"
        type="text"
        list="default-model-options"
        placeholder="例如 deepseek/deepseek-chat"
      />
      <datalist id="default-model-options">
        <option v-for="m in modelOptions" :key="m" :value="m" />
      </datalist>
      <div class="field-hint">新建会话缺省采用的模型；需与 Providers 配置中的 "名称/模型 ID" 对应</div>
    </div>

    <div class="form-field">
      <label class="field-label">默认 Agent 模板</label>
      <select v-model="config.default_agent" class="field-input">
        <option v-for="a in agentOptions" :key="a" :value="a">{{ a }}</option>
      </select>
      <div class="field-hint">会话启动时挂载的 Agent 角色模板 (项目级 .oma/agents/ 可覆盖)</div>
    </div>

    <div class="form-field">
      <label class="field-label">默认审批模式</label>
      <select v-model="config.default_approval_mode" class="field-input">
        <option value="normal">normal — 常规工具免批，危险操作请求审批</option>
        <option value="strict">strict — 所有工具调用均需审批</option>
        <option value="auto">auto — 全部自动放行</option>
      </select>
      <div class="field-hint">新建会话的缺省权限仲裁策略；会话内可随时切换</div>
    </div>

    <div class="form-field">
      <label class="field-label">Daemon 监听地址</label>
      <input :value="config.server.listen_addr" class="field-input" type="text" readonly />
      <div class="field-hint">
        <Fa6CircleInfo style="vertical-align: -2px;" />
        修改需通过 <code>oma daemon --addr</code> 命令行参数指定并重启生效
      </div>
    </div>

  </div>
</template>
