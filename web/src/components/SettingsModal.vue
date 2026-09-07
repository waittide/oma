<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import {
  LuEye,
  LuEyeOff,
  LuLanguages,
  LuPalette,
  LuPlus,
  LuServer,
  LuSquareUserRound,
  LuTrash2,
} from 'vue-icons-plus/lu';
import OButton from './ui/OButton.vue';
import OCheckbox from './ui/OCheckbox.vue';
import OInput from './ui/OInput.vue';
import { api } from '../api';
import OModal from './ui/OModal.vue';
import ORadio from './ui/ORadio.vue';
import OSelect from './ui/OSelect.vue';
import OTooltip from './ui/OTooltip.vue';
import OModelSelect from './ui/OModelSelect.vue';
import { ACCENTS, config, loadConfig, saveConfig, saveTheme, theme } from '../stores/theme';
import { agents } from '../stores/chat';
import { LOCALES, settingStore, setLocale, type Locale } from '../stores/setting';
import type { AgentSummary, ModelInfo, OmaConfig, ProviderConfig, Theme } from '../types';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('settings');
const { t: tc } = useTranslations('common');

type SectionId = 'theme' | 'language' | 'defaults' | 'providers';
const section = ref<SectionId>('theme');

/** 参照 opencode 设置弹窗：导航按分组小标题聚类，底部展示应用版本。 */
const navGroups = computed(() => [
  {
    title: t('navSectionPersonal'),
    items: [
      { id: 'theme' as SectionId, label: t('navTheme'), icon: LuPalette },
      { id: 'language' as SectionId, label: t('navLanguage'), icon: LuLanguages },
    ],
  },
  {
    title: t('navSectionService'),
    items: [
      { id: 'defaults' as SectionId, label: t('navDefaults'), icon: LuSquareUserRound },
      { id: 'providers' as SectionId, label: t('navProviders'), icon: LuServer },
    ],
  },
]);

const version = ref('');

// ---------- 主题 ----------
const mode = ref<Theme['mode']>(theme.value.mode);
const flavor = ref<Theme['dark_flavor']>(theme.value.dark_flavor);
const accent = ref<string>(theme.value.accent);
const savingTheme = ref(false);

const modeOptions = computed<{ value: Theme['mode']; label: string }[]>(() => [
  { value: 'light', label: t('modeLight') },
  { value: 'dark', label: t('modeDark') },
  { value: 'system', label: t('modeSystem') },
]);
const flavorOptions: { value: Theme['dark_flavor']; label: string }[] = [
  { value: 'frappe', label: 'Frappé' },
  { value: 'macchiato', label: 'Macchiato' },
  { value: 'mocha', label: 'Mocha' },
];
const LIGHT_FLAVORS: { value: 'latte'; label: string }[] = [{ value: 'latte', label: 'Latte' }];

async function applyTheme() {
  savingTheme.value = true;
  try {
    const next: Theme = {
      mode: mode.value,
      dark_flavor: flavor.value,
      accent: accent.value,
    };
    await saveTheme(next);
    toast.success(t('themeSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  } finally {
    savingTheme.value = false;
  }
}

// ---------- 默认参数 ----------
const defaults = reactive({ model: '', agent: '', approval: 'normal' as OmaConfig['default_approval_mode'] });
const savingDefaults = ref(false);

watch(
  () => props.open,
  async (v) => {
    if (!v) return;
    if (!config.value) await loadConfig();
    if (config.value) {
      defaults.model = config.value.default_model;
      defaults.agent = config.value.default_agent;
      defaults.approval = config.value.default_approval_mode;
    }
    mode.value = theme.value.mode;
    flavor.value = theme.value.dark_flavor;
    accent.value = theme.value.accent;
    if (!version.value) {
      version.value = await api.status().then((s) => s.version).catch(() => '');
    }
  },
  { immediate: true },
);

async function saveDefaults() {
  if (!config.value) return;
  savingDefaults.value = true;
  try {
    await saveConfig({ ...config.value, ...defaults });
    toast.success(t('defaultsSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  } finally {
    savingDefaults.value = false;
  }
}

/** 默认参数：模型选择器按提供商分组；未开会话时回退到内置 agent 清单。 */
const providerGroups = computed<Record<string, ModelInfo[]>>(() => {
  if (!config.value) return {};
  return Object.fromEntries(
    Object.entries(config.value.providers).map(([id, p]) => [id, p.models ?? []]),
  );
});

const BUILTIN_AGENTS = ['task', 'plan', 'explore', 'review', 'build'];
const agentOptions = computed<{ value: string; label: string }[]>(() => {
  const list: AgentSummary[] = agents.value.length
    ? agents.value
    : BUILTIN_AGENTS.map((id) => ({ id, name: id, description: '' }));
  return list.map((a) => ({ value: a.id, label: a.name }));
});

// ---------- 模型提供商（草稿编辑，保存时统一校验与写回） ----------
interface ModelDraft {
  id: string;
  name: string;
  context_len: string;
  max_output: string;
  reasoning_effort: string;
  supports_thinking: boolean;
  supports_vision: boolean;
  input_types: string[];
}

interface ProviderDraft {
  /** 稳定标识：与可编辑的 name 解耦，供密钥显隐等局部状态作键 */
  uid: number;
  /** null = 本次新增，尚无服务端原键 */
  origId: string | null;
  name: string;
  api_type: string;
  base_url: string;
  api_key: string;
  headers: Record<string, string>;
  body: unknown;
  models: ModelDraft[];
}

const providerDrafts = ref<ProviderDraft[]>([]);
const savingProviders = ref(false);
let uidSeq = 0;
const keyRevealed = ref<Record<number, boolean>>({});
/** reveal 接口拉取的真实密钥缓存（provider 原键 → key） */
let revealedReal: Record<string, string> | null = null;

function toDraft(origId: string, p: ProviderConfig): ProviderDraft {
  return {
    uid: ++uidSeq,
    origId,
    name: origId,
    api_type: p.api_type,
    base_url: p.base_url,
    api_key: p.api_key,
    headers: { ...p.headers },
    body: p.body,
    models: (p.models ?? []).map((m) => ({
      id: m.id,
      name: m.name,
      context_len: String(m.context_len),
      max_output: m.max_output === undefined ? '' : String(m.max_output),
      reasoning_effort: m.reasoning_effort ?? '',
      supports_thinking: m.supports_thinking,
      supports_vision: m.supports_vision,
      input_types: [...(m.input_types ?? [])],
    })),
  };
}

watch(
  () => [props.open, section.value] as const,
  ([o, s]) => {
    if (o && s === 'providers') rebuildDrafts();
  },
);

const apiTypeOptions = ['anthropic', 'completion', 'response', 'google'].map((v) => ({ value: v, label: v }));
const effortOptions = computed(() => [
  { value: '', label: t('effortOff') },
  { value: 'low', label: 'low' },
  { value: 'medium', label: 'medium' },
  { value: 'high', label: 'high' },
]);

function rebuildDrafts() {
  keyRevealed.value = {};
  revealedReal = null;
  providerDrafts.value = config.value
    ? Object.entries(config.value.providers).map(([id, p]) => toDraft(id, p))
    : [];
}

/** 密钥显隐：展示真实密钥需经 reveal 接口获取；隐藏时若未被编辑则回填掩码。 */
async function toggleKeyVisibility(d: ProviderDraft) {
  const on = !keyRevealed.value[d.uid];
  if (on && d.api_key === '***') {
    if (!revealedReal) {
      const real = await api.getConfig({ reveal: true });
      revealedReal = Object.fromEntries(
        Object.entries(real.providers).map(([id, p]) => [id, p.api_key]),
      );
    }
    const orig = d.origId ? revealedReal[d.origId] : undefined;
    if (orig !== undefined) d.api_key = orig;
  } else if (!on && d.origId && revealedReal && d.api_key === revealedReal[d.origId]) {
    d.api_key = '***';
  }
  keyRevealed.value[d.uid] = on;
}

const INPUT_TYPES = ['text', 'image', 'video'] as const;
const INPUT_TYPE_LABELS: Record<string, string> = {
  text: 'inputText',
  image: 'inputImage',
  video: 'inputVideo',
};

function inputTypeLabel(ty: string): string {
  return t(INPUT_TYPE_LABELS[ty] ?? ty);
}

const showProvider = ref(false);
const providerId = ref('');

function addProvider() {
  providerId.value = '';
  showProvider.value = true;
}

function confirmProvider() {
  const id = providerId.value.trim();
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(id)) {
    toast.error(t('providerNameRule'));
    return;
  }
  if (providerDrafts.value.some((d) => d.name === id)) {
    toast.error(t('providerExists'));
    return;
  }
  providerDrafts.value.push({
    uid: ++uidSeq,
    origId: null,
    name: id,
    api_type: 'anthropic',
    base_url: 'https://api.anthropic.com',
    api_key: '',
    headers: {},
    body: {},
    models: [],
  });
  showProvider.value = false;
}

function removeDraft(index: number) {
  providerDrafts.value.splice(index, 1);
}

function addModel(d: ProviderDraft) {
  d.models.push({
    id: '',
    name: '',
    context_len: '128000',
    max_output: '',
    reasoning_effort: '',
    supports_thinking: true,
    supports_vision: true,
    input_types: ['text', 'image'],
  });
}

function toggleInputType(m: ModelDraft, type: string, on: boolean) {
  m.input_types = on
    ? [...m.input_types, type]
    : m.input_types.filter((x) => x !== type);
}

/** "provider/model" 选择器仅在首个 '/' 处切分（模型 id 可含 '/'）。 */
function splitSelector(v: string): [string, string] {
  const i = v.indexOf('/');
  return i === -1 ? ['', v] : [v.slice(0, i), v.slice(i + 1)];
}

async function saveProviders() {
  if (!config.value) return;
  // 名称校验：合法标识符且互不重复
  for (const d of providerDrafts.value) {
    // 选择器格式为 "provider/model"，名称不能含 '/'；连字符/点等其余字符均合法
    if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(d.name.trim())) {
      toast.error(t('providerNameRule'));
      return;
    }
  }
  const names = providerDrafts.value.map((d) => d.name.trim());
  if (new Set(names).size !== names.length) {
    toast.error(t('providerExists'));
    return;
  }

  // 重命名提供商时同步修正 default_model 的前缀
  const [defaultProvider] = splitSelector(config.value.default_model);
  const renamed = providerDrafts.value.find((d) => d.origId === defaultProvider && d.name.trim() !== d.origId);
  const default_model = renamed
    ? renamed.name.trim() + config.value.default_model.slice(defaultProvider.length)
    : config.value.default_model;

  savingProviders.value = true;
  try {
    // 脱敏值回传前先还原真实密钥：否则重命名后服务端按新键名找不到旧值，密钥会被清空
    if (providerDrafts.value.some((d) => d.api_key === '***')) {
      const real = await api.getConfig({ reveal: true });
      for (const d of providerDrafts.value) {
        if (d.api_key !== '***') continue;
        const orig = d.origId ? real.providers[d.origId]?.api_key : undefined;
        if (orig !== undefined) d.api_key = orig;
      }
    }

    const providers: Record<string, ProviderConfig> = {};
    for (const d of providerDrafts.value) {
      providers[d.name.trim()] = {
        api_type: d.api_type,
        base_url: d.base_url.trim(),
        api_key: d.api_key,
        headers: d.headers,
        body: d.body,
        models: d.models
          .filter((m) => m.id.trim())
          .map((m) => {
            const ctx = parseInt(m.context_len, 10);
            const mo = parseInt(m.max_output, 10);
            return {
              id: m.id.trim(),
              name: m.name.trim(),
              context_len: Number.isFinite(ctx) && ctx > 0 ? ctx : 128000,
              supports_vision: m.supports_vision,
              supports_thinking: m.supports_thinking,
              ...(Number.isFinite(mo) && mo > 0 ? { max_output: mo } : {}),
              ...(m.reasoning_effort ? { reasoning_effort: m.reasoning_effort } : {}),
              ...(m.input_types.length ? { input_types: [...m.input_types] } : {}),
            };
          }),
      };
    }
    await saveConfig({ ...config.value, providers, default_model });
    rebuildDrafts();
    toast.success(t('providersSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  } finally {
    savingProviders.value = false;
  }
}

const approvalOptions = computed<{ value: OmaConfig['default_approval_mode']; label: string }[]>(
  () => [
    { value: 'normal', label: t('approvalNormal') },
    { value: 'strict', label: t('approvalStrict') },
    { value: 'auto', label: t('approvalAuto') },
  ],
);

const localeOptions = computed(() =>
  LOCALES.map((l) => ({ value: l.value, label: l.label })),
);

function pickLocale(v: Locale) {
  setLocale(v);
}
</script>

<template>
  <OModal :open="open" width="min(calc(100vw - 32px), 980px)" flush floating-close @close="emit('close')">
    <div class="split">
      <nav class="nav">
        <div v-for="g in navGroups" :key="g.title" class="nav-group">
          <span class="nav-title">{{ g.title }}</span>
          <button
            v-for="s in g.items"
            :key="s.id"
            type="button"
            class="nav-item"
            :class="{ active: section === s.id }"
            @click="section = s.id"
          >
            <component :is="s.icon" :size="14" />
            <span>{{ s.label }}</span>
          </button>
        </div>
        <div class="nav-foot">
          <span class="nav-foot-name">Oma</span>
          <span v-if="version" class="nav-foot-ver">v{{ version }}</span>
        </div>
      </nav>

      <div class="content">
        <!-- 外观 -->
        <section v-if="section === 'theme'" class="pane">
          <h2 class="pane-title">{{ t('navTheme') }}</h2>
          <div class="list">
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('mode') }}</span>
              </div>
              <div class="srow-ctl">
                <ORadio v-model="mode" :options="modeOptions" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('themeLabel') }}</span>
                <span class="srow-desc">{{ t('themeDesc') }}</span>
              </div>
              <div class="srow-ctl">
                <!-- 浅色系当前仅 Latte，后续扩展时追加选项即可 -->
                <ORadio v-if="mode === 'light'" model-value="latte" :options="LIGHT_FLAVORS" />
                <ORadio v-else v-model="flavor" :options="flavorOptions" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('accent') }}</span>
              </div>
              <div class="srow-ctl">
                <div class="dots">
                  <OTooltip
                    v-for="(a, i) in ACCENTS"
                    :key="a"
                    :label="t(`accentNames.${a}`)"
                    :align="i % 7 === 0 ? 'start' : i % 7 === 6 ? 'end' : 'center'"
                  >
                    <button
                      type="button"
                      class="dot"
                      :class="{ active: a === accent }"
                      :style="{ '--dot': `var(--${a === 'green' ? 'green-color' : a})` }"
                      @click="accent = a"
                    />
                  </OTooltip>
                </div>
              </div>
            </div>
          </div>
          <div class="pane-actions">
            <OButton variant="primary" size="sm" :loading="savingTheme" @click="applyTheme">
              {{ t('saveTheme') }}
            </OButton>
          </div>
        </section>

        <!-- 语言 -->
        <section v-else-if="section === 'language'" class="pane">
          <h2 class="pane-title">{{ t('navLanguage') }}</h2>
          <div class="list">
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('languageLabel') }}</span>
              </div>
              <div class="srow-ctl">
                <OSelect
                  :model-value="settingStore.locale"
                  :options="localeOptions"
                  width="160px"
                  @update:model-value="pickLocale"
                />
              </div>
            </div>
          </div>
        </section>

        <!-- 默认参数 -->
        <section v-else-if="section === 'defaults' && config" class="pane">
          <h2 class="pane-title">{{ t('navDefaults') }}</h2>
          <div class="list">
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('defaultModel') }}</span>
              </div>
              <div class="srow-ctl wide">
                <OModelSelect v-model="defaults.model" :groups="providerGroups" width="300px" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('defaultAgent') }}</span>
              </div>
              <div class="srow-ctl">
                <OSelect v-model="defaults.agent" :options="agentOptions" width="200px" />
              </div>
            </div>
            <div class="srow">
              <div class="srow-main">
                <span class="srow-title">{{ t('defaultApproval') }}</span>
              </div>
              <div class="srow-ctl">
                <OSelect v-model="defaults.approval" :options="approvalOptions" width="200px" />
              </div>
            </div>
          </div>
          <div class="pane-actions">
            <OButton variant="primary" size="sm" :loading="savingDefaults" @click="saveDefaults">
              {{ t('saveDefaults') }}
            </OButton>
          </div>
        </section>

        <!-- 模型提供商 -->
        <section v-else-if="section === 'providers' && config" class="pane">
          <div class="pane-head">
            <h2 class="pane-title">{{ t('navProviders') }}</h2>
            <div class="pane-actions inline">
              <OButton size="sm" variant="soft" @click="addProvider">{{ t('add') }}</OButton>
              <OButton size="sm" variant="primary" :loading="savingProviders" @click="saveProviders">
                {{ t('saveAll') }}
              </OButton>
            </div>
          </div>

          <div v-for="(d, pi) in providerDrafts" :key="d.uid" class="prov">
            <div class="prov-head">
              <OInput v-model="d.name" class="prov-name" :placeholder="t('providerName')" />
              <button type="button" class="m-del" :title="tc('delete')" @click="removeDraft(pi)">
                <LuTrash2 :size="14" />
              </button>
            </div>

            <div class="fields">
              <div class="field">
                <label>{{ t('fApiType') }}</label>
                <OSelect v-model="d.api_type" :options="apiTypeOptions" />
              </div>
              <div class="field">
                <label>{{ t('fBaseUrl') }}</label>
                <OInput v-model="d.base_url" />
              </div>
              <div class="field span2">
                <label>{{ t('fApiKey') }}</label>
                <div class="key-row">
                  <OInput v-model="d.api_key" :type="keyRevealed[d.uid] ? 'text' : 'password'" />
                  <button
                    type="button"
                    class="m-del"
                    :title="keyRevealed[d.uid] ? t('hideKey') : t('showKey')"
                    @click="toggleKeyVisibility(d)"
                  >
                    <LuEyeOff v-if="keyRevealed[d.uid]" :size="14" />
                    <LuEye v-else :size="14" />
                  </button>
                </div>
              </div>
            </div>

            <div class="models-head">
              <span class="group-label">{{ t('fModels') }}</span>
              <OButton size="sm" variant="ghost" @click="addModel(d)">
                <template #icon><LuPlus :size="13" /></template>
                {{ t('addModel') }}
              </OButton>
            </div>
            <p v-if="d.models.length === 0" class="muted">{{ t('modelsNone') }}</p>

            <div v-for="(m, mi) in d.models" :key="mi" class="model">
              <div class="m-grid">
                <div class="field">
                  <label>{{ t('modelId') }}</label>
                  <div class="inline-field">
                    <OInput v-model="m.id" placeholder="model-id" />
                    <button type="button" class="m-del" :title="t('removeModel')" @click="d.models.splice(mi, 1)">
                      <LuTrash2 :size="14" />
                    </button>
                  </div>
                </div>
                <div class="field">
                  <label>{{ t('modelName') }}</label>
                  <OInput v-model="m.name" />
                </div>
                <div class="field">
                  <label>{{ t('contextLen') }}</label>
                  <OInput v-model="m.context_len" />
                </div>
                <div class="field">
                  <label>{{ t('maxOutput') }}</label>
                  <OInput v-model="m.max_output" :placeholder="t('unset')" />
                </div>
                <div class="field">
                  <label>{{ t('reasoningEffort') }}</label>
                  <OSelect v-model="m.reasoning_effort" :options="effortOptions" />
                </div>
                <div class="field">
                  <label>{{ t('capabilities') }}</label>
                  <div class="checks">
                    <OCheckbox v-model="m.supports_thinking" :label="t('supportsThinking')" />
                    <OCheckbox v-model="m.supports_vision" :label="t('supportsVision')" />
                  </div>
                </div>
                <div class="field span2">
                  <label>{{ t('inputTypes') }}</label>
                  <div class="checks">
                    <OCheckbox
                      v-for="ty in INPUT_TYPES"
                      :key="ty"
                      :model-value="m.input_types.includes(ty)"
                      :label="inputTypeLabel(ty)"
                      @update:model-value="toggleInputType(m, ty, $event)"
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>

          <p v-if="providerDrafts.length === 0" class="muted">{{ t('providersEmpty') }}</p>
        </section>

        <section v-else-if="section === 'defaults' || section === 'providers'" class="pane">
          <p class="muted">{{ t('configUnavailable') }}</p>
        </section>
      </div>
    </div>
  </OModal>

  <OModal :open="showProvider" :title="t('newProvider')" width="400px" @close="showProvider = false">
    <div class="grid one">
      <label>{{ t('providerName') }}</label>
      <OInput v-model="providerId" placeholder="my_anthropic" autofocus @enter="confirmProvider" />
    </div>
    <template #footer>
      <OButton variant="ghost" @click="showProvider = false">{{ tc('cancel') }}</OButton>
      <OButton variant="primary" @click="confirmProvider">{{ t('add') }}</OButton>
    </template>
  </OModal>
</template>

<style scoped>
.split {
  display: flex;
  height: min(640px, calc(100vh - 92px));
}
.nav {
  display: flex;
  flex-direction: column;
  gap: 14px;
  width: 188px;
  flex-shrink: 0;
  padding: 14px 10px 10px;
  border-right: 1px solid var(--line);
  background: var(--sidebar);
}
.nav-group {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.nav-title {
  padding: 0 10px 4px;
  font-size: 10.5px;
  font-weight: 600;
  letter-spacing: 0.06em;
  text-transform: uppercase;
  color: var(--overlay0);
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 10px;
  border: none;
  border-radius: 7px;
  background: transparent;
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
}
.nav-item:hover {
  background: var(--surface-hover);
  color: var(--ink);
}
.nav-item.active {
  background: var(--surface-active);
  color: var(--accent);
  font-weight: 600;
}
.nav-foot {
  margin-top: auto;
  display: flex;
  flex-direction: column;
  gap: 1px;
  padding: 6px 10px 2px;
  font-size: 11px;
  color: var(--overlay0);
}
.nav-foot-name {
  font-weight: 600;
  color: var(--text-tertiary);
}
.content {
  flex: 1;
  min-width: 0;
  overflow-y: auto;
  padding: 22px 28px 28px;
}
/* 参照 opencode：内容栏限宽、扁平行 + 细分隔线，不用卡片嵌套 */
.pane {
  max-width: 640px;
}
.pane-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 4px;
}
.pane-title {
  margin: 0 0 10px;
  font-size: 15px;
  font-weight: 600;
  color: var(--ink);
}
.pane-head .pane-title {
  margin-bottom: 0;
}
.pane-actions {
  display: flex;
  gap: 8px;
  margin-top: 14px;
}
.pane-actions.inline {
  margin-top: 0;
}
.list {
  background: var(--surface);
  border-radius: 10px;
  padding: 2px 14px;
}
.srow {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 11px 0;
  border-bottom: 1px solid var(--line);
}
.srow:last-child {
  border-bottom: none;
}
.srow-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.srow-title {
  font-size: 13px;
  font-weight: 500;
  color: var(--ink);
}
.srow-desc {
  font-size: 11.5px;
  color: var(--overlay1);
}
.srow-ctl {
  flex-shrink: 0;
  display: flex;
  justify-content: flex-end;
}
.srow-ctl.wide {
  flex-shrink: 1;
}
/* 强调色点阵：两行各 7 个，整齐排列；悬停经 OTooltip 显示名称 */
.dots {
  display: grid;
  grid-template-columns: repeat(7, 18px);
  gap: 9px 10px;
  justify-content: start;
}
.dot {
  width: 16px;
  height: 16px;
  padding: 0;
  border: 1px solid color-mix(in srgb, var(--crust) 28%, transparent);
  border-radius: 99px;
  background: var(--dot);
  cursor: pointer;
  transition:
    box-shadow 0.12s ease,
    filter 0.12s ease;
}
.dot:hover {
  filter: brightness(1.12);
}
.dot.active {
  box-shadow:
    0 0 0 2px var(--paper),
    0 0 0 4px var(--dot);
}
.grid {
  display: grid;
  grid-template-columns: 60px 1fr;
  gap: 8px 12px;
  align-items: center;
}
.grid label {
  font-size: 12px;
  color: var(--overlay0);
  font-family: var(--font-mono);
}
/* 提供商：扁平区块 + 细分隔线 */
.prov {
  padding: 16px 0 14px;
  border-bottom: 1px solid var(--line);
}
.prov:last-of-type {
  border-bottom: none;
}
.prov-head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.prov-name {
  flex: 1;
  max-width: 320px;
}
.prov-name :deep(input) {
  font-family: var(--font-mono);
  font-weight: 600;
}
.fields {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 14px;
  margin-top: 10px;
}
.field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  min-width: 0;
}
.field > label {
  font-size: 11.5px;
  color: var(--overlay1);
}
.field.span2 {
  grid-column: span 2;
}
.key-row {
  display: flex;
  align-items: center;
  gap: 6px;
}
.key-row .o-input {
  flex: 1;
}
.inline-field {
  display: flex;
  align-items: center;
  gap: 6px;
}
.inline-field .o-input {
  flex: 1;
}
.models-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 16px;
}
.group-label {
  font-size: 11.5px;
  color: var(--overlay1);
}
.model {
  padding: 12px 0 4px;
  border-bottom: 1px solid var(--line);
}
.model:last-of-type {
  border-bottom: none;
}
.m-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 14px;
}
.m-del {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--overlay0);
  cursor: pointer;
  flex-shrink: 0;
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
}
.m-del:hover {
  background: var(--danger-soft);
  color: var(--danger);
}
.checks {
  display: flex;
  flex-wrap: wrap;
  gap: 6px 14px;
  min-height: 30px;
  align-items: center;
}
.muted {
  font-size: 12.5px;
  color: var(--overlay0);
  margin: 6px 0 0;
}
</style>
