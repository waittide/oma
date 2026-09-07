<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import {
  LuCheck,
  LuLanguages,
  LuPalette,
  LuServer,
  LuSquareUserRound,
} from 'vue-icons-plus/lu';
import OButton from './ui/OButton.vue';
import OCheckbox from './ui/OCheckbox.vue';
import OInput from './ui/OInput.vue';
import OModal from './ui/OModal.vue';
import ORadio from './ui/ORadio.vue';
import OSelect from './ui/OSelect.vue';
import { ACCENTS, config, loadConfig, saveConfig, saveTheme, theme } from '../stores/theme';
import { LOCALES, settingStore, setLocale, type Locale } from '../stores/setting';
import { useTranslations } from '../composables/i18n';
import type { OmaConfig, Theme } from '../types';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('settings');
const { t: tc } = useTranslations('common');

type SectionId = 'theme' | 'language' | 'defaults' | 'providers';
const section = ref<SectionId>('theme');

const sections = computed(() => [
  { id: 'theme' as SectionId, label: t('navTheme'), icon: LuPalette },
  { id: 'language' as SectionId, label: t('navLanguage'), icon: LuLanguages },
  { id: 'defaults' as SectionId, label: t('navDefaults'), icon: LuSquareUserRound },
  { id: 'providers' as SectionId, label: t('navProviders'), icon: LuServer },
]);

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

// ---------- 默认参数 / Provider ----------
const defaults = reactive({ model: '', agent: '', approval: 'normal' as OmaConfig['default_approval_mode'] });
const showKey = ref<Record<string, boolean>>({});
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
    toast.error(t('providerNameRule'));
    return;
  }
  if (config.value.providers[id]) {
    toast.error(t('providerExists'));
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
  toast.success(t('providerAdded'));
}

async function saveProviders() {
  if (!config.value) return;
  try {
    await saveConfig(config.value);
    toast.success(t('providersSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

function removeProvider(id: string) {
  if (!config.value) return;
  delete config.value.providers[id];
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
  <OModal :open="open" :title="t('title')" width="880px" flush @close="emit('close')">
    <div class="split">
      <nav class="nav">
        <button
          v-for="s in sections"
          :key="s.id"
          type="button"
          class="nav-item"
          :class="{ active: section === s.id }"
          @click="section = s.id"
        >
          <component :is="s.icon" :size="15" />
          <span>{{ s.label }}</span>
        </button>
      </nav>

      <div class="content">
        <!-- 主题 -->
        <section v-if="section === 'theme'" class="card">
          <div class="row">
            <span class="k">{{ t('mode') }}</span>
            <div class="v">
              <ORadio v-model="mode" :options="modeOptions" />
            </div>
          </div>

          <div class="row">
            <span class="k">{{ t('themeLabel') }}</span>
            <div class="v">
              <span v-if="mode === 'light'" class="flavor-fixed">Latte</span>
              <ORadio v-else v-model="flavor" :options="flavorOptions" />
            </div>
          </div>

          <div class="row">
            <span class="k">{{ t('accent') }}</span>
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

          <div class="row end">
            <OButton variant="primary" :loading="savingTheme" @click="applyTheme">{{ t('saveTheme') }}</OButton>
          </div>
        </section>

        <!-- 语言 -->
        <section v-else-if="section === 'language'" class="card">
          <div class="row">
            <span class="k">{{ t('languageLabel') }}</span>
            <div class="v">
              <OSelect
                :model-value="settingStore.locale"
                :options="localeOptions"
                width="180px"
                @update:model-value="pickLocale"
              />
            </div>
          </div>
        </section>

        <!-- 默认参数 -->
        <section v-else-if="section === 'defaults' && config" class="card">
          <div class="row">
            <span class="k">{{ t('defaultModel') }}</span>
            <div class="v"><OInput v-model="defaults.model" :placeholder="t('defaultModelPlaceholder')" /></div>
          </div>
          <div class="row">
            <span class="k">{{ t('defaultAgent') }}</span>
            <div class="v"><OInput v-model="defaults.agent" placeholder="task" /></div>
          </div>
          <div class="row">
            <span class="k">{{ t('defaultApproval') }}</span>
            <div class="v">
              <OSelect v-model="defaults.approval" :options="approvalOptions" width="240px" />
            </div>
          </div>
          <div class="row end">
            <OButton variant="primary" :loading="savingDefaults" @click="saveDefaults">{{ t('saveDefaults') }}</OButton>
          </div>
        </section>

        <!-- Providers -->
        <section v-else-if="section === 'providers' && config" class="card">
          <div class="card-head">
            <h3>{{ t('providersCount', { count: Object.keys(config.providers).length }) }}</h3>
            <div class="ch-actions">
              <OButton size="sm" variant="soft" @click="addProvider">{{ t('add') }}</OButton>
              <OButton size="sm" variant="primary" @click="saveProviders">{{ t('saveAll') }}</OButton>
            </div>
          </div>

          <div v-for="(p, id) in config.providers" :key="id" class="provider">
            <div class="pv-head">
              <span class="pv-id">{{ id }}</span>
              <OCheckbox
                :model-value="showKey[id] === true"
                :label="t('showKey')"
                @update:model-value="showKey[id] = $event"
              />
              <OButton size="sm" variant="danger" @click="removeProvider(String(id))">{{ tc('delete') }}</OButton>
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
                <span v-if="(p.models ?? []).length === 0" class="muted">{{ t('modelsNone') }}</span>
              </div>
            </div>
          </div>

          <p v-if="Object.keys(config.providers).length === 0" class="muted empty">
            {{ t('providersEmpty') }}
          </p>
        </section>

        <section v-else-if="section === 'defaults' || section === 'providers'" class="card">
          <p class="muted empty">{{ t('configUnavailable') }}</p>
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
  height: min(520px, calc(100vh - 200px));
}
.nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
  width: 172px;
  flex-shrink: 0;
  padding: 10px;
  border-right: 1px solid var(--line);
  background: var(--sidebar);
}
.nav-item {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px;
  border: none;
  border-radius: 8px;
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
.content {
  flex: 1;
  min-width: 0;
  overflow-y: auto;
  padding: 16px 18px 18px;
}
.card {
  border: 1px solid var(--line);
  border-radius: 14px;
  background: var(--surface);
  padding: 16px 18px 18px;
}
.card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}
.card-head h3 {
  margin: 0;
  font-size: 14px;
}
.ch-actions {
  display: flex;
  gap: 8px;
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
.flavor-fixed {
  display: inline-flex;
  align-items: center;
  padding: 5px 12px;
  border: 1px solid var(--line);
  border-radius: 7px;
  background: var(--surface-strong);
  font-size: 12.5px;
  color: var(--text-secondary);
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
.grid {
  display: grid;
  grid-template-columns: 90px 1fr;
  gap: 8px 12px;
  align-items: center;
}
.grid.one {
  grid-template-columns: 60px 1fr;
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
