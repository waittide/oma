<script setup lang="ts">
import { Fa6CircleInfo } from 'vue-icons-plus/fa6';
import { computed } from 'vue';
import type { ApprovalMode, OmaConfigView } from '../../types';
import OuiCombobox from '../oui/OuiCombobox.vue';
import OuiSelect, { type OuiSelectOption } from '../oui/OuiSelect.vue';

const props = defineProps<{
  config: OmaConfigView;
}>();

// 从已配置 providers 汇总 "provider/model" 候选项，默认模型仍可自由输入
const modelOptions = computed(() =>
  Object.entries(props.config.providers).flatMap(([pid, p]) =>
    (p.models ?? []).map((m) => `${pid}/${m.id}`),
  ),
);

const agentOptions: OuiSelectOption[] = ['task', 'plan', 'explore', 'review', 'build'].map((a) => ({
  value: a,
  label: a,
}));

const approvalOptions: OuiSelectOption[] = [
  { value: 'normal', label: 'normal', description: '常规工具免批，危险操作请求审批' },
  { value: 'strict', label: 'strict', description: '所有工具调用均需审批' },
  { value: 'auto', label: 'auto', description: '全部自动放行' },
];
</script>

<template>
  <div class="settings-panel">
    <div class="form-field">
      <label class="field-label">默认模型 (provider/model)</label>
      <OuiCombobox
        v-model="config.default_model"
        :options="modelOptions"
        placeholder="例如 deepseek/deepseek-chat"
      />
      <div class="field-hint">新建会话缺省采用的模型；需与 Providers 配置中的 "名称/模型 ID" 对应</div>
    </div>

    <div class="form-field">
      <label class="field-label">默认 Agent 模板</label>
      <OuiSelect v-model="config.default_agent" :options="agentOptions" variant="field" />
      <div class="field-hint">会话启动时挂载的 Agent 角色模板 (项目级 .oma/agents/ 可覆盖)</div>
    </div>

    <div class="form-field">
      <label class="field-label">默认审批模式</label>
      <OuiSelect
        :model-value="config.default_approval_mode"
        :options="approvalOptions"
        variant="field"
        @update:model-value="(v) => (config.default_approval_mode = v as ApprovalMode)"
      />
      <div class="field-hint">新建会话的缺省权限仲裁策略；会话内可随时切换</div>
    </div>

    <div class="form-field">
      <label class="field-label">Daemon 监听地址</label>
      <input :value="config.server.listen_addr" class="field-input" type="text" readonly />
      <div class="field-hint">
        <Fa6CircleInfo />
        修改需通过 <code>oma daemon --addr</code> 命令行参数指定并重启生效
      </div>
    </div>
  </div>
</template>
