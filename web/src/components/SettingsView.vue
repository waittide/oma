<script setup lang="ts">
import { onMounted, reactive, ref } from 'vue';
import { toast } from 'vue-sonner';
import {
  LuCheck,
  LuMonitor,
  LuMoon,
  LuPalette,
  LuServer,
  LuSun,
} from 'vue-icons-plus/lu';
import OButton from './ui/OButton.vue';
import OCheckbox from './ui/OCheckbox.vue';
import OInput from './ui/OInput.vue';
import OModal from './ui/OModal.vue';
import ORadio from './ui/ORadio.vue';
import OSelect from './ui/OSelect.vue';
import { ACCENTS, config, configReady, loadConfig, saveConfig, saveTheme, theme } from '../stores/theme';
import type { OmaConfig, Theme } from '../types';

const emit = defineEmits<{ back: [] }>();

const mode = ref<Theme['mode']>(theme.value.mode);
const flavor = ref<Theme['dark_flavor']>(theme.value.dark_flavor);
const accent = ref<string>(theme.value.accent);
const savingTheme = ref(false);

const modeOptions: { value: Theme['mode']; label: string }[] = [
  { value: 'light', label: '浅色' },
  { value: 'dark', label: '深色' },
  { value: 'system', label: '跟随系统' },
];
const flavorOptions: { value: Theme['dark_flavor']; label: string }[] = [
  { value: 'frappe', label: 'Frappé' },
  { value: 'macchiato', label: 'Macchiato' },
  { value: 'mocha', label: 'Mocha' },
];

async function applyTheme() {
  savingTheme.value = true;
  try {
    const next: Theme = {
      mode: mode.value,
      dark_flavor: flavor.value,
      accent: accent.value,
    };
    await saveTheme(next);
    toast.success('主题已保存至服务端');
  } catch (e) {
    toast.error(`保存失败：${(e as Error).message}`);
  } finally {
    savingTheme.value = false;
  }
}

// ---------- 默认参数 / Provider ----------
const defaults = reactive({ model: '', agent: '', approval: 'normal' as OmaConfig['default_approval_mode'] });
const showKey = ref<Record<string, boolean>>({});
const savingDefaults = ref(false);

onMounted(async () => {
  if (!configReady.value) await loadConfig();
  if (config.value) {
    defaults.model = config.value.default_model;
    defaults.agent = config.value.default_agent;
    defaults.approval = config.value.default_approval_mode;
  }
});

async function saveDefaults() {
  if (!config.value) return;
  savingDefaults.value = true;
  try {
    await saveConfig({ ...config.value, ...defaults });
    toast.success('默认配置已保存');
  } catch (e) {
    toast.error(`保存失败：${(e as Error).message}`);
  } finally {
    savingDefaults.value = false;
  }
}

const showProvider = ref(false);
const providerId = ref('');

function addProvider() {
  providerId.value = '';
  showProvider.value = true;
}

function confirmProvider() {
  if (!config.value) return;
  const id = providerId.value.trim();
  if (!/^[a-zA-Z][a-zA-Z0-9_]*$/.test(id)) {
    toast.error('名称需以字母开头，仅含字母/数字/下划线');
    return;
  }
  if (config.value.providers[id]) {
    toast.error('该 Provider 已存在');
    return;
  }
  config.value.providers[id] = {
    api_type: 'anthropic',
    base_url: 'https://api.anthropic.com',
    api_key: '',
    headers: {},
    body: {},
    models: [],
  };
  showProvider.value = false;
  toast.success('已新增，填写后请点击「保存全部」');
}

async function saveProviders() {
  if (!config.value) return;
  try {
    await saveConfig(config.value);
    toast.success('Provider 配置已保存');
  } catch (e) {
    toast.error(`保存失败：${(e as Error).message}`);
  }
}

function removeProvider(id: string) {
  if (!config.value) return;
  delete config.value.providers[id];
}

const approvalOptions: { value: OmaConfig['default_approval_mode']; label: string }[] = [
  { value: 'normal', label: 'Normal（危险操作审批）' },
  { value: 'strict', label: 'Strict（全部审批）' },
  { value: 'auto', label: 'Auto（自动放行）' },
];
</script>

<template>
  <div class="settings">
    <header class="head">
      <div>
        <h2>设置</h2>
        <p>主题与模型配置由后端统一管理，保存后立即生效。</p>
      </div>
      <OButton variant="soft" @click="emit('back')">返回会话</OButton>
    </header>

    <div class="body">
      <!-- 主题 -->
      <section class="card">
        <h3><LuPalette :size="15" /> 主题</h3>

        <div class="row">
          <span class="k">显示模式</span>
          <div class="v">
            <ORadio v-model="mode" :options="modeOptions" />
          </div>
        </div>

        <div v-if="mode !== 'light'" class="row">
          <span class="k">深色系 flavor</span>
          <div class="v">
            <OSelect v-model="flavor" :options="flavorOptions" width="160px" />
          </div>
        </div>

        <div class="row">
          <span class="k">强调色</span>
          <div class="v swatches">
            <button
              v-for="a in ACCENTS"
              :key="a"
              type="button"
              class="swatch"
              :class="{ active: a === accent }"
              :title="a"
              @click="accent = a"
            >
              <span class="chip" :style="{ background: `var(--${a === 'green' ? 'green-color' : a})` }" />
              <span class="sw-name">{{ a }}</span>
              <LuCheck v-if="a === accent" :size="11" class="sw-check" />
            </button>
          </div>
        </div>

        <div class="row">
          <span class="k">预览</span>
          <div class="v preview">
            <span class="pv-bg">Base</span>
            <span class="pv-mantle">Mantle</span>
            <span class="pv-surface">Surface</span>
            <span class="pv-accent">Accent</span>
            <LuSun v-if="mode === 'light'" :size="14" class="pv-icon" />
            <LuMonitor v-else-if="mode === 'system'" :size="14" class="pv-icon" />
            <LuMoon v-else :size="14" class="pv-icon" />
          </div>
        </div>

        <div class="row end">
          <OButton variant="primary" :loading="savingTheme" @click="applyTheme">保存主题</OButton>
        </div>
      </section>

      <!-- 默认参数 -->
      <section v-if="config" class="card">
        <h3><LuServer :size="15" /> 默认参数</h3>
        <div class="row">
          <span class="k">默认模型</span>
          <div class="v"><OInput v-model="defaults.model" placeholder="provider/model-id" /></div>
        </div>
        <div class="row">
          <span class="k">默认 Agent</span>
          <div class="v"><OInput v-model="defaults.agent" placeholder="task" /></div>
        </div>
        <div class="row">
          <span class="k">默认审批模式</span>
          <div class="v">
            <OSelect v-model="defaults.approval" :options="approvalOptions" width="240px" />
          </div>
        </div>
        <div class="row end">
          <OButton variant="primary" :loading="savingDefaults" @click="saveDefaults">保存默认</OButton>
        </div>
      </section>

      <!-- Providers -->
      <section v-if="config" class="card">
        <div class="card-head">
          <h3><LuServer :size="15" /> Providers（{{ Object.keys(config.providers).length }}）</h3>
          <div class="ch-actions">
            <OButton size="sm" variant="soft" @click="addProvider">新增</OButton>
            <OButton size="sm" variant="primary" @click="saveProviders">保存全部</OButton>
          </div>
        </div>

        <div v-for="(p, id) in config.providers" :key="id" class="provider">
          <div class="pv-head">
            <span class="pv-id">{{ id }}</span>
            <OCheckbox
              :model-value="showKey[id] === true"
              label="显示密钥"
              @update:model-value="showKey[id] = $event"
            />
            <OButton size="sm" variant="danger" @click="removeProvider(String(id))">删除</OButton>
          </div>
          <div class="grid">
            <label>api_type</label>
            <OInput v-model="p.api_type" />
            <label>base_url</label>
            <OInput v-model="p.base_url" />
            <label>api_key</label>
            <OInput v-model="p.api_key" :type="showKey[id] ? 'text' : 'password'" />
            <label>models</label>
            <div class="tags">
              <span v-for="m in p.models ?? []" :key="m.id" class="tag">
                {{ m.name || m.id }}
                <button type="button" class="tag-x" @click="p.models = (p.models ?? []).filter((x) => x.id !== m.id)">
                  ×
                </button>
              </span>
              <span v-if="(p.models ?? []).length === 0" class="muted">未声明（按 id 动态合成）</span>
            </div>
          </div>
        </div>

        <p v-if="Object.keys(config.providers).length === 0" class="muted empty">
          尚无 Provider。点击右上角「新增」开始配置。
        </p>
      </section>

      <p v-if="!config" class="muted empty">
        无法读取服务端配置，请确认 Daemon 正在运行且 Token 有效。
      </p>
    </div>

    <OModal :open="showProvider" title="新增 Provider" width="400px" @close="showProvider = false">
      <div class="grid one">
        <label>名称</label>
        <OInput v-model="providerId" placeholder="my_anthropic" autofocus @enter="confirmProvider" />
      </div>
      <template #footer>
        <OButton variant="ghost" @click="showProvider = false">取消</OButton>
        <OButton variant="primary" @click="confirmProvider">新增</OButton>
      </template>
    </OModal>
  </div>
</template>

<style scoped>
.settings {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-width: 0;
  background: var(--paper);
  overflow: hidden;
}
.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  padding: 18px 26px;
  border-bottom: 1px solid var(--line);
  background: var(--surface);
}
.head h2 {
  margin: 0;
  font-size: 17px;
}
.head p {
  margin: 4px 0 0;
  font-size: 12.5px;
  color: var(--text-tertiary);
}
.body {
  flex: 1;
  overflow-y: auto;
  padding: 20px 26px 40px;
  display: flex;
  flex-direction: column;
  gap: 16px;
  max-width: 860px;
}
.card {
  border: 1px solid var(--line);
  border-radius: 14px;
  background: var(--surface);
  padding: 16px 18px 18px;
}
.grid {
  display: grid;
  grid-template-columns: 90px 1fr;
  gap: 8px 12px;
  align-items: center;
}
.card h3 {
  display: flex;
  align-items: center;
  gap: 7px;
  margin: 0 0 14px;
  font-size: 14px;
}
.grid.one {
  grid-template-columns: 60px 1fr;
}
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.ch-actions {
  display: flex;
  gap: 8px;
  margin-bottom: 12px;
}
.row {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 8px 0;
}
.row.end {
  justify-content: flex-end;
  padding-top: 12px;
}
.k {
  width: 108px;
  flex-shrink: 0;
  font-size: 12.5px;
  color: var(--text-tertiary);
}
.v {
  flex: 1;
  min-width: 0;
}
.swatches {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.swatch {
  position: relative;
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 9px 4px 5px;
  border: 1px solid var(--line);
  border-radius: 99px;
  background: var(--paper);
  color: var(--text-tertiary);
  font-family: inherit;
  font-size: 11.5px;
  cursor: pointer;
  transition: border-color 0.15s ease;
}
.swatch:hover {
  border-color: var(--overlay0);
  color: var(--ink);
}
.swatch.active {
  border-color: var(--accent);
  color: var(--ink);
  background: var(--surface-active);
}
.chip {
  width: 13px;
  height: 13px;
  border-radius: 99px;
  flex-shrink: 0;
}
.sw-check {
  color: var(--accent);
}
.preview {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11.5px;
}
.preview span {
  padding: 4px 10px;
  border-radius: 7px;
}
.pv-bg {
  background: var(--base);
  color: var(--text);
  border: 1px solid var(--line);
}
.pv-mantle {
  background: var(--mantle);
  color: var(--text);
  border: 1px solid var(--line);
}
.pv-surface {
  background: var(--surface0);
  color: var(--text);
}
.pv-accent {
  background: var(--accent);
  color: var(--base);
  font-weight: 600;
}
.pv-icon {
  color: var(--text-tertiary);
  margin-left: auto;
}
.grid {
  display: grid;
  grid-template-columns: 90px 1fr;
  gap: 8px 12px;
  align-items: center;
}
.grid label {
  font-size: 12px;
  color: var(--overlay0);
  font-family: var(--font-mono);
}
.provider {
  border: 1px solid var(--line);
  border-radius: 11px;
  background: var(--paper);
  padding: 12px 14px;
  margin-top: 10px;
}
.pv-head {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 10px;
}
.pv-id {
  flex: 1;
  font-family: var(--font-mono);
  font-size: 13px;
  font-weight: 600;
  color: var(--accent);
}
.tags {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.tag {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 2px 8px;
  border-radius: 99px;
  background: var(--surface-strong);
  font-size: 11.5px;
  color: var(--text-secondary);
}
.tag-x {
  border: none;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
  font-size: 12px;
  padding: 0 2px;
}
.tag-x:hover {
  color: var(--danger);
}
.muted {
  font-size: 12.5px;
  color: var(--overlay0);
}
.empty {
  padding: 10px 0;
  margin: 6px 0 0;
}
</style>
