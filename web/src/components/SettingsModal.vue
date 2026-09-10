<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { toast } from 'vue-sonner';
import {
  LuEye,
  LuEyeOff,
  LuLanguages,
  LuPalette,
  LuPlug,
  LuPlus,
  LuServer,
  LuSparkles,
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
import { ACCENTS, PALETTES, config, customThemes, loadConfig, saveConfig, saveTheme, theme, type Flavor } from '../stores/theme';
import { agents } from '../stores/chat';
import { LOCALES, settingStore, setLocale, type Locale } from '../stores/setting';
import { activeSession } from '../stores/sessions';
import type { AgentFile, AgentSummary, CustomTheme, McpServerConfig, ModelInfo, OmaConfig, ProviderConfig, SkillFile, Theme } from '../types';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ open: boolean }>();
const emit = defineEmits<{ close: [] }>();

const { t } = useTranslations('settings');
const { t: tc } = useTranslations('common');

type SectionId = 'theme' | 'language' | 'defaults' | 'providers' | 'presets' | 'skills' | 'mcp';
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
      { id: 'presets' as SectionId, label: t('navPresets'), icon: LuSquareUserRound },
      { id: 'skills' as SectionId, label: t('navSkills'), icon: LuSparkles },
      { id: 'mcp' as SectionId, label: t('navMcp'), icon: LuPlug },
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

// 浅色/深色各自的主题 id 选择（内置 + 自定义）
const themeSel = reactive({ light: 'latte', dark: 'mocha' });

const lightThemeOptions = computed(() => [
  { value: 'latte', label: 'Latte' },
  ...customThemes.value.filter((t) => t.mode === 'light').map((t) => ({ value: t.id, label: t.name })),
]);
const darkThemeOptions = computed(() => [
  { value: 'frappe', label: 'Frappé' },
  { value: 'macchiato', label: 'Macchiato' },
  { value: 'mocha', label: 'Mocha' },
  ...customThemes.value.filter((t) => t.mode === 'dark').map((t) => ({ value: t.id, label: t.name })),
]);

async function applyTheme() {
  savingTheme.value = true;
  try {
    const next: Theme = {
      mode: mode.value,
      dark_flavor: themeSel.dark,
      light_theme: themeSel.light,
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

// ---------- 自定义主题编辑 ----------
const THEME_TOKENS = [
  'base', 'mantle', 'crust', 'text', 'subtext1', 'subtext0',
  'surface0', 'surface1', 'surface2', 'overlay0', 'overlay1', 'overlay2',
] as const;

const themeEdit = reactive<{
  open: boolean;
  isNew: boolean;
  id: string;
  name: string;
  mode: 'light' | 'dark';
  base: Flavor;
  colors: Record<string, string>;
}>({
  open: false,
  isNew: false,
  id: '',
  name: '',
  mode: 'dark',
  base: 'mocha',
  colors: {},
});

const themeBaseOptions = computed(() =>
  themeEdit.mode === 'light'
    ? [{ value: 'latte' as Flavor, label: 'Latte' }]
    : [
        { value: 'frappe' as Flavor, label: 'Frappé' },
        { value: 'macchiato' as Flavor, label: 'Macchiato' },
        { value: 'mocha' as Flavor, label: 'Mocha' },
      ],
);

function effectiveColor(token: string): string {
  const c = themeEdit.colors[token];
  if (c && /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(c)) return c;
  return PALETTES[themeEdit.base]?.[token] ?? '#000000';
}

function newTheme() {
  themeEdit.open = true;
  themeEdit.isNew = true;
  themeEdit.id = '';
  themeEdit.name = '';
  themeEdit.mode = mode.value === 'light' ? 'light' : 'dark';
  themeEdit.base = themeEdit.mode === 'light' ? 'latte' : 'mocha';
  themeEdit.colors = {};
}

function editTheme(t: CustomTheme) {
  themeEdit.open = true;
  themeEdit.isNew = false;
  themeEdit.id = t.id;
  themeEdit.name = t.name;
  themeEdit.mode = t.mode;
  themeEdit.base = (PALETTES[t.base as Flavor] ? t.base : 'mocha') as Flavor;
  themeEdit.colors = { ...t.colors };
}

function slugifyThemeName(name: string): string {
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  return slug || `custom-${Date.now()}`;
}

async function saveThemeEdit() {
  const name = themeEdit.name.trim();
  if (!name) {
    toast.error(t('themeNameRequired'));
    return;
  }
  const id = themeEdit.isNew ? slugifyThemeName(name) : themeEdit.id;
  const colors: Record<string, string> = {};
  for (const tok of THEME_TOKENS) {
    const v = (themeEdit.colors[tok] ?? '').trim();
    if (v && v !== PALETTES[themeEdit.base]?.[tok]) colors[tok] = v;
  }
  const entry: CustomTheme = { id, name, mode: themeEdit.mode, base: themeEdit.base, colors };
  const list = [...(config.value?.custom_themes ?? [])];
  const idx = list.findIndex((t) => t.id === id);
  if (idx >= 0) list[idx] = entry;
  else list.push(entry);
  try {
    await saveConfig({ ...(config.value as OmaConfig), custom_themes: list });
    // 编辑/新建后让选择跟随新主题
    if (entry.mode === 'light') themeSel.light = id;
    else themeSel.dark = id;
    themeEdit.open = false;
    toast.success(t('themeSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

async function removeTheme(id: string) {
  const list = (config.value?.custom_themes ?? []).filter((t) => t.id !== id);
  try {
    await saveConfig({ ...(config.value as OmaConfig), custom_themes: list });
    if (themeSel.light === id) themeSel.light = 'latte';
    if (themeSel.dark === id) themeSel.dark = 'mocha';
    toast.success(t('themeDeleted'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
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
    themeSel.light = theme.value.light_theme || 'latte';
    themeSel.dark = theme.value.dark_flavor || 'mocha';
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
    // 必须映射到 OmaConfig 的真实字段名：直接展开 defaults 会写入
    // model/agent/approval 这三个无效键，服务端忽略后配置纹丝不动，
    // 但请求本身成功 —— 表现为「提示保存成功，实际没生效」。
    await saveConfig({
      ...config.value,
      default_model: defaults.model,
      default_agent: defaults.agent,
      default_approval_mode: defaults.approval,
    });
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

function scopeLabel(scope: string): string {
  if (scope === 'bundled') return t('scopeBundled');
  return scope === 'project' ? t('scopeProject') : t('scopeGlobal');
}

const mcpKindOptions = [
  { value: 'local' as const, label: t('mcpKindLocal') },
  { value: 'remote' as const, label: t('mcpKindRemote') },
];

// ---------- Agent 预设（决定角色与可用工具） ----------
type EditScope = 'global' | 'project';
const scopeSel = ref<EditScope>('global');
const skillWorkspace = computed(() => activeSession.value?.workspace ?? '');

const scopeOptions = computed(() => [
  { value: 'global' as EditScope, label: t('scopeGlobal') },
  { value: 'project' as EditScope, label: t('scopeProject') },
]);

const presets = ref<AgentFile[]>([]);
const presetsLoading = ref(false);

async function loadPresets() {
  presetsLoading.value = true;
  try {
    const ws = scopeSel.value === 'project' ? skillWorkspace.value : '';
    presets.value = await api.presets(ws);
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  } finally {
    presetsLoading.value = false;
  }
}

const presetEdit = reactive({
  open: false,
  isNew: false,
  readonly: false,
  id: '',
  name: '',
  description: '',
  toolsText: '',
  content: '',
  scope: 'global' as EditScope,
});

function editPreset(a: AgentFile) {
  presetEdit.open = true;
  presetEdit.isNew = false;
  presetEdit.readonly = a.scope === 'bundled';
  presetEdit.id = a.id;
  presetEdit.name = a.name;
  presetEdit.description = a.description;
  presetEdit.toolsText = a.tools.join(', ');
  presetEdit.content = a.content;
  presetEdit.scope = a.scope === 'bundled' ? scopeSel.value : (a.scope as EditScope);
}

function newPreset() {
  presetEdit.open = true;
  presetEdit.isNew = true;
  presetEdit.readonly = false;
  presetEdit.id = '';
  presetEdit.name = '';
  presetEdit.description = '';
  presetEdit.toolsText = '';
  presetEdit.content = '';
  presetEdit.scope = scopeSel.value;
}

async function savePreset() {
  const id = presetEdit.id.trim();
  if (presetEdit.scope === 'project' && !skillWorkspace.value) {
    toast.error(t('skillNoWorkspace'));
    return;
  }
  try {
    await api.putPreset(
      id,
      {
        name: presetEdit.name.trim() || id,
        description: presetEdit.description.trim(),
        tools: presetEdit.toolsText.split(',').map((x) => x.trim()).filter(Boolean),
        content: presetEdit.content,
        scope: presetEdit.scope,
      },
      presetEdit.scope === 'project' ? skillWorkspace.value : '',
    );
    presetEdit.open = false;
    toast.success(t('presetSaved'));
    await loadPresets();
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

async function removePreset() {
  try {
    await api.deletePreset(
      presetEdit.id,
      presetEdit.scope,
      presetEdit.scope === 'project' ? skillWorkspace.value : '',
    );
    presetEdit.open = false;
    toast.success(t('presetDeleted'));
    await loadPresets();
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

// ---------- 技能（按需取用的领域知识，与预设相互独立） ----------
const skills = ref<SkillFile[]>([]);
const skillsLoading = ref(false);

async function loadSkills() {
  skillsLoading.value = true;
  try {
    const ws = scopeSel.value === 'project' ? skillWorkspace.value : '';
    skills.value = await api.skills(ws);
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  } finally {
    skillsLoading.value = false;
  }
}

const skillEdit = reactive({
  open: false,
  isNew: false,
  readonly: false,
  id: '',
  name: '',
  description: '',
  content: '',
  scope: 'global' as EditScope,
});

function editSkill(s: SkillFile) {
  skillEdit.open = true;
  skillEdit.isNew = false;
  skillEdit.readonly = false;
  skillEdit.id = s.id;
  skillEdit.name = s.name;
  skillEdit.description = s.description;
  skillEdit.content = s.content;
  skillEdit.scope = s.scope as EditScope;
}

function newSkill() {
  skillEdit.open = true;
  skillEdit.isNew = true;
  skillEdit.readonly = false;
  skillEdit.id = '';
  skillEdit.name = '';
  skillEdit.description = '';
  skillEdit.content = '';
  skillEdit.scope = scopeSel.value;
}

async function saveSkill() {
  const id = skillEdit.id.trim();
  if (skillEdit.scope === 'project' && !skillWorkspace.value) {
    toast.error(t('skillNoWorkspace'));
    return;
  }
  try {
    await api.putSkill(
      id,
      {
        name: skillEdit.name.trim() || id,
        description: skillEdit.description.trim(),
        content: skillEdit.content,
        scope: skillEdit.scope,
      },
      skillEdit.scope === 'project' ? skillWorkspace.value : '',
    );
    skillEdit.open = false;
    toast.success(t('skillSaved'));
    await loadSkills();
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

async function removeSkill() {
  try {
    await api.deleteSkill(
      skillEdit.id,
      skillEdit.scope,
      skillEdit.scope === 'project' ? skillWorkspace.value : '',
    );
    skillEdit.open = false;
    toast.success(t('skillDeleted'));
    await loadSkills();
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

watch(
  () => [props.open, section.value, scopeSel.value] as const,
  ([o, s]) => {
    if (!o) return;
    if (s === 'presets') void loadPresets();
    if (s === 'skills') void loadSkills();
  },
);

// ---------- MCP 服务器（全局配置，保存后 daemon 自动重连） ----------
interface McpDraft {
  origName: string | null;
  name: string;
  kind: 'local' | 'remote';
  command: string;
  argsText: string;
  envText: string;
  url: string;
  headersText: string;
}

const mcpDrafts = ref<McpDraft[]>([]);
const savingMcp = ref(false);

function toMcpDraft(name: string, cfg: McpServerConfig): McpDraft {
  const envText = Object.entries(cfg.type === 'local' ? (cfg.env ?? {}) : {})
    .map(([k, v]) => `${k}=${v}`)
    .join('\n');
  const headersText = Object.entries(cfg.type === 'remote' ? (cfg.headers ?? {}) : {})
    .map(([k, v]) => `${k}=${v}`)
    .join('\n');
  return {
    origName: name,
    name,
    kind: cfg.type,
    command: cfg.type === 'local' ? cfg.command : '',
    argsText: cfg.type === 'local' ? (cfg.args ?? []).join(', ') : '',
    envText,
    url: cfg.type === 'remote' ? cfg.url : '',
    headersText,
  };
}

function rebuildMcpDrafts() {
  mcpDrafts.value = config.value
    ? Object.entries(config.value.mcp_servers).map(([n, c]) => toMcpDraft(n, c))
    : [];
}

function parseKvText(text: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of text.split('\n')) {
    const i = line.indexOf('=');
    if (i <= 0) continue;
    const k = line.slice(0, i).trim();
    const v = line.slice(i + 1).trim();
    if (k) out[k] = v;
  }
  return out;
}

function addMcpServer() {
  mcpDrafts.value.push({
    origName: null,
    name: '',
    kind: 'local',
    command: 'npx',
    argsText: '-y, @modelcontextprotocol/server-xxx',
    envText: '',
    url: 'https://',
    headersText: '',
  });
}

/** 正在展开编辑的已有服务器名；null 表示无编辑（新增卡片由 origName === null 区分） */
const editingMcp = ref<string | null>(null);

function cancelEditMcp() {
  editingMcp.value = null;
  rebuildMcpDrafts();
}

function mcpSummary(d: McpDraft): string {
  const raw = d.kind === 'local' ? [d.command, d.argsText].filter(Boolean).join(' ') : d.url;
  return raw.replace(/\s+/g, ' ').trim() || '—';
}

async function saveMcp() {
  if (!config.value) return;
  const names = mcpDrafts.value.map((d) => d.name.trim());
  if (names.some((n) => !n)) {
    toast.error(t('mcpNameRequired'));
    return;
  }
  if (new Set(names).size !== names.length) {
    toast.error(t('mcpNameDuplicate'));
    return;
  }
  savingMcp.value = true;
  try {
    const mcp_servers: Record<string, McpServerConfig> = {};
    for (const d of mcpDrafts.value) {
      mcp_servers[d.name.trim()] =
        d.kind === 'local'
          ? { type: 'local' as const, command: d.command.trim(), args: d.argsText.split(',').map((a) => a.trim()).filter(Boolean), env: parseKvText(d.envText) }
          : { type: 'remote', url: d.url.trim(), headers: parseKvText(d.headersText) };
    }
    await saveConfig({ ...config.value, mcp_servers });
    rebuildMcpDrafts();
    toast.success(t('mcpSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  } finally {
    savingMcp.value = false;
  }
}

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
    uid: 0,
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
    if (o && s === 'mcp') rebuildMcpDrafts();
  },
);

const apiTypeOptions = ['anthropic', 'completion', 'response', 'google'].map((v) => ({ value: v, label: v }));
const effortOptions = computed(() => [
  { value: '', label: t('effortOff') },
  { value: 'low', label: 'low' },
  { value: 'medium', label: 'medium' },
  { value: 'high', label: 'high' },
]);

/**
 * 重建草稿列表。
 *
 * 默认清空全部局部状态（打开面板时以服务端为准）。`preserveView` 供保存后原地
 * 刷新使用：按提供商名称继承 uid 与密钥显隐，并恢复选中项，否则保存后密钥输入框
 * 会闪回密码态、标签页会跳回第一个。
 */
function rebuildDrafts(preserveView = false) {
  const previous = providerDrafts.value;
  const activeName = preserveView
    ? previous.find((d) => d.uid === activeProviderUid.value)?.name.trim()
    : undefined;
  // 名称是保存后唯一可跨重建对应的身份（此时它已成为服务端键名）
  const carried = new Map<string, { uid: number; apiKey: string | null }>();
  if (preserveView) {
    for (const d of previous) {
      carried.set(d.name.trim(), {
        uid: d.uid,
        apiKey: keyRevealed.value[d.uid] ? d.api_key : null,
      });
    }
  }

  keyRevealed.value = {};
  revealedReal = null;
  providerDrafts.value = config.value
    ? Object.entries(config.value.providers).map(([id, p]) => {
        const draft = toDraft(id, p);
        const inherited = carried.get(id);
        if (!inherited) {
          draft.uid = ++uidSeq;
          return draft;
        }
        draft.uid = inherited.uid;
        if (inherited.apiKey !== null) {
          // 服务端回读的是掩码值；沿用保存前的真实密钥，界面才不会闪回密码态
          draft.api_key = inherited.apiKey;
          keyRevealed.value[draft.uid] = true;
        }
        return draft;
      })
    : [];

  if (activeName !== undefined) {
    const next = providerDrafts.value.find((d) => d.name === activeName);
    if (next) activeProviderUid.value = next.uid;
  }
}

/** 删除当前选中的提供商草稿（保存后才会真正移除） */
function removeActiveProvider() {
  const index = providerDrafts.value.findIndex((d) => d.uid === activeProviderUid.value);
  if (index >= 0) removeDraft(index);
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


/** 当前展示的提供商 tab；草稿列表变化时兜底选中第一个 */
const activeProviderUid = ref<number | null>(null);
const activeDraft = computed(() => providerDrafts.value.find((d) => d.uid === activeProviderUid.value) ?? null);
watch(providerDrafts, (list) => {
  if (!list.some((d) => d.uid === activeProviderUid.value)) {
    activeProviderUid.value = list[0]?.uid ?? null;
  }
});

function addProvider() {
  const draft: ProviderDraft = {
    uid: ++uidSeq,
    origId: null,
    name: '',
    api_type: 'anthropic',
    base_url: 'https://api.anthropic.com',
    api_key: '',
    headers: {},
    body: {},
    models: [],
  };
  providerDrafts.value.push(draft);
  activeProviderUid.value = draft.uid;
}

function removeDraft(index: number) {
  const [removed] = providerDrafts.value.splice(index, 1);
  if (removed) delete keyRevealed.value[removed.uid];
  // splice 原地修改数组，ref 的浅层 watch 不会触发；删除选中项时须显式改选相邻项，
  // 否则 activeProviderUid 悬空、配置区变为空白
  if (removed && removed.uid === activeProviderUid.value) {
    const next = providerDrafts.value[index] ?? providerDrafts.value[index - 1];
    activeProviderUid.value = next?.uid ?? null;
  }
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
    rebuildDrafts(true);
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
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navTheme') }}</h2>
          </header>
          <div class="pane-scroll">
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
                  <span class="srow-title">{{ t('themeRowLight') }}</span>
                  <span class="srow-desc">{{ t('themeDesc') }}</span>
                </div>
                <div class="srow-ctl">
                  <ORadio v-model="themeSel.light" :options="lightThemeOptions" />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('themeRowDark') }}</span>
                </div>
                <div class="srow-ctl">
                  <ORadio v-model="themeSel.dark" :options="darkThemeOptions" />
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
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('manageThemes') }}</span>
                  <span class="srow-desc">{{ t('manageThemesDesc') }}</span>
                </div>
                <div class="srow-ctl">
                  <OButton size="sm" variant="soft" @click="newTheme">{{ t('newTheme') }}</OButton>
                </div>
              </div>
              <div v-for="ct in customThemes" :key="ct.id" class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ ct.name }} <span class="scope-tag">{{ ct.mode === 'light' ? t('modeLight') : t('modeDark') }}</span></span>
                  <span class="srow-desc">{{ t('themeBaseLabel') }}: {{ ct.base }}</span>
                </div>
                <div class="srow-ctl">
                  <OButton size="sm" variant="ghost" @click="editTheme(ct)">{{ t('edit') }}</OButton>
                  <OButton size="sm" variant="ghost" @click="removeTheme(ct.id)">{{ tc('delete') }}</OButton>
                </div>
              </div>
            </div>
          </div>
          <footer class="pane-foot">
            <OButton variant="primary" size="sm" :loading="savingTheme" @click="applyTheme">
              {{ t('saveTheme') }}
            </OButton>
          </footer>
        </section>

        <!-- 语言 -->
        <section v-else-if="section === 'language'" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navLanguage') }}</h2>
          </header>
          <div class="pane-scroll">
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
          </div>
        </section>

        <!-- 默认参数 -->
        <section v-else-if="section === 'defaults' && config" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navDefaults') }}</h2>
          </header>
          <div class="pane-scroll">
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
          </div>
          <footer class="pane-foot">
            <OButton variant="primary" size="sm" :loading="savingDefaults" @click="saveDefaults">
              {{ t('saveDefaults') }}
            </OButton>
          </footer>
        </section>

        <!-- 模型提供商 -->
        <section v-else-if="section === 'providers' && config" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navProviders') }}</h2>
          </header>
          <div class="pane-scroll">
            <div class="tabs-row">
              <div class="tabs">
                <button
                  v-for="d in providerDrafts"
                  :key="d.uid"
                  type="button"
                  class="tab"
                  :class="{ active: d.uid === activeProviderUid }"
                  @click="activeProviderUid = d.uid"
                >
                  {{ d.name || t('providerName') }}
                </button>
              </div>
              <div class="tabs-actions">
                <OButton size="sm" variant="soft" @click="addProvider">
                  <template #icon><LuPlus :size="13" /></template>
                  {{ t('add') }}
                </OButton>
                <OButton
                  size="sm"
                  variant="ghost"
                  :disabled="!activeDraft"
                  @click="removeActiveProvider"
                >
                  <template #icon><LuTrash2 :size="13" /></template>
                  {{ tc('delete') }}
                </OButton>
              </div>
            </div>

            <div v-for="(d, pi) in activeDraft ? [activeDraft] : []" :key="d.uid" class="prov">
              <!-- 提供商配置：标题与配置项同处一个区块 -->
              <section class="cfg-group">
                <header class="cfg-head">
                  <span class="cfg-title">{{ t('providerConfig') }}</span>
                </header>
                <div class="cfg-rows">
                  <label class="cfg-label">{{ t('providerName') }}</label>
                  <div class="cfg-ctl">
                    <OInput v-model="d.name" class="mono" :placeholder="t('providerName')" />
                  </div>

                  <label class="cfg-label">{{ t('fRequestFormat') }}</label>
                  <div class="cfg-ctl">
                    <OSelect v-model="d.api_type" :options="apiTypeOptions" />
                  </div>

                  <label class="cfg-label">{{ t('fBaseUrl') }}</label>
                  <div class="cfg-ctl">
                    <OInput v-model="d.base_url" />
                  </div>

                  <label class="cfg-label">{{ t('fApiKey') }}</label>
                  <div class="cfg-ctl">
                    <OInput v-model="d.api_key" :type="keyRevealed[d.uid] ? 'text' : 'password'" />
                    <OTooltip :label="keyRevealed[d.uid] ? t('hideKey') : t('showKey')" align="end">
                      <button
                        type="button"
                        class="m-del"
                        :aria-label="keyRevealed[d.uid] ? t('hideKey') : t('showKey')"
                        @click="toggleKeyVisibility(d)"
                      >
                        <LuEyeOff v-if="keyRevealed[d.uid]" :size="14" />
                        <LuEye v-else :size="14" />
                      </button>
                    </OTooltip>
                  </div>
                </div>
              </section>

              <!-- 模型配置：标题与模型列表同处一个区块 -->
              <section class="cfg-group">
                <header class="cfg-head">
                  <span class="cfg-title">{{ t('modelConfig') }}</span>
                  <OButton size="sm" variant="ghost" @click="addModel(d)">
                    <template #icon><LuPlus :size="13" /></template>
                    {{ t('add') }}
                  </OButton>
                </header>
                <p v-if="d.models.length === 0" class="muted">{{ t('modelsNone') }}</p>

                <div v-for="(m, mi) in d.models" :key="mi" class="model">
                  <div class="model-head">
                    <span class="model-title">{{ m.name || m.id || t('modelIndex', { index: mi + 1 }) }}</span>
                    <OTooltip :label="t('removeModel')" align="end">
                      <button type="button" class="m-del" :aria-label="t('removeModel')" @click="d.models.splice(mi, 1)">
                        <LuTrash2 :size="14" />
                      </button>
                    </OTooltip>
                  </div>
                  <div class="cfg-rows">
                    <label class="cfg-label">{{ t('modelId') }}</label>
                    <div class="cfg-ctl">
                      <OInput v-model="m.id" class="mono" placeholder="model-id" />
                    </div>

                    <label class="cfg-label">{{ t('modelName') }}</label>
                    <div class="cfg-ctl">
                      <OInput v-model="m.name" />
                    </div>

                    <label class="cfg-label">{{ t('contextLen') }}</label>
                    <div class="cfg-ctl">
                      <OInput v-model="m.context_len" />
                    </div>

                    <label class="cfg-label">{{ t('maxOutput') }}</label>
                    <div class="cfg-ctl">
                      <OInput v-model="m.max_output" :placeholder="t('unset')" />
                    </div>

                    <label class="cfg-label">{{ t('reasoningEffort') }}</label>
                    <div class="cfg-ctl">
                      <OSelect v-model="m.reasoning_effort" :options="effortOptions" />
                    </div>

                    <label class="cfg-label">{{ t('capabilities') }}</label>
                    <div class="cfg-ctl">
                      <div class="checks">
                        <OCheckbox v-model="m.supports_thinking" :label="t('supportsThinking')" />
                        <OCheckbox v-model="m.supports_vision" :label="t('supportsVision')" />
                      </div>
                    </div>

                    <label class="cfg-label">{{ t('inputTypes') }}</label>
                    <div class="cfg-ctl">
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
              </section>
            </div>

            <p v-if="providerDrafts.length === 0" class="muted">{{ t('providersEmpty') }}</p>
          </div>
          <footer class="pane-foot">
            <OButton variant="primary" size="sm" :loading="savingProviders" @click="saveProviders">
              {{ t('saveAll') }}
            </OButton>
          </footer>
        </section>

        <!-- Agent 预设 -->
        <section v-else-if="section === 'presets'" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navPresets') }}</h2>
            <div class="pane-head-actions">
              <ORadio v-model="scopeSel" :options="scopeOptions" />
              <OButton size="sm" variant="soft" @click="newPreset">{{ t('add') }}</OButton>
            </div>
          </header>
          <div class="pane-scroll">
            <p class="muted">{{ t('presetsHint') }}</p>
            <p v-if="scopeSel === 'project' && !skillWorkspace" class="muted">{{ t('skillNoWorkspace') }}</p>
            <div class="list">
              <div
                v-for="a in presets"
                :key="a.scope + '/' + a.id"
                class="srow skill-row"
                role="button"
                @click="editPreset(a)"
              >
                <div class="srow-main">
                  <span class="srow-title">
                    {{ a.name }}
                    <span class="scope-tag" :class="'scope-' + a.scope">{{ scopeLabel(a.scope) }}</span>
                  </span>
                  <span class="srow-desc">{{ a.description || a.id }}</span>
                </div>
                <div class="srow-ctl">
                  <span class="srow-desc mono">{{ a.tools.join(', ') }}</span>
                </div>
              </div>
              <div v-if="presets.length === 0 && !presetsLoading" class="srow">
                <span class="srow-desc">{{ t('presetsEmpty') }}</span>
              </div>
            </div>
          </div>
        </section>

        <!-- 技能 -->
        <section v-else-if="section === 'skills'" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navSkills') }}</h2>
            <div class="pane-head-actions">
              <ORadio v-model="scopeSel" :options="scopeOptions" />
              <OButton size="sm" variant="soft" @click="newSkill">{{ t('add') }}</OButton>
            </div>
          </header>
          <div class="pane-scroll">
            <p class="muted">{{ t('skillsHint') }}</p>
            <p v-if="scopeSel === 'project' && !skillWorkspace" class="muted">{{ t('skillNoWorkspace') }}</p>
            <div class="list">
              <div
                v-for="s in skills"
                :key="s.scope + '/' + s.id"
                class="srow skill-row"
                role="button"
                @click="editSkill(s)"
              >
                <div class="srow-main">
                  <span class="srow-title">
                    {{ s.name }}
                    <span class="scope-tag" :class="'scope-' + s.scope">{{ scopeLabel(s.scope) }}</span>
                  </span>
                  <span class="srow-desc">{{ s.description || s.id }}</span>
                </div>
              </div>
              <div v-if="skills.length === 0 && !skillsLoading" class="srow">
                <span class="srow-desc">{{ t('skillsEmpty') }}</span>
              </div>
            </div>
          </div>
        </section>

        <!-- MCP 服务器 -->
        <section v-else-if="section === 'mcp' && config" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navMcp') }}</h2>
            <div class="pane-head-actions">
              <OButton size="sm" variant="soft" @click="addMcpServer">
                <template #icon><LuPlus :size="13" /></template>
                {{ t('add') }}
              </OButton>
            </div>
          </header>
          <div class="pane-scroll">
            <p class="muted">{{ t('mcpHint') }}</p>
            <div class="list">
              <div v-for="(d, i) in mcpDrafts" :key="d.origName ?? `mcp-new-${i}`">
                <!-- 编辑中 / 新增：展开卡片表单 -->
                <div v-if="d.origName === null || d.origName === editingMcp" class="prov card">
                  <div class="prov-head">
                    <OInput v-model="d.name" class="prov-name" :placeholder="t('mcpNamePlaceholder')" />
                    <ORadio v-model="d.kind" :options="mcpKindOptions" />
                    <OTooltip :label="tc('delete')" align="end">
                      <button type="button" class="m-del" :aria-label="tc('delete')" @click="mcpDrafts.splice(i, 1)">
                        <LuTrash2 :size="14" />
                      </button>
                    </OTooltip>
                  </div>
                  <div class="fields">
                    <template v-if="d.kind === 'local'">
                      <div class="field">
                        <label>{{ t('mcpCommand') }}</label>
                        <OInput v-model="d.command" />
                      </div>
                      <div class="field">
                        <label>{{ t('mcpArgs') }}</label>
                        <OInput v-model="d.argsText" />
                      </div>
                      <div class="field span2">
                        <label>{{ t('mcpEnv') }}</label>
                        <OInput v-model="d.envText" placeholder="KEY=value" />
                      </div>
                    </template>
                    <template v-else>
                      <div class="field span2">
                        <label>{{ t('mcpUrl') }}</label>
                        <OInput v-model="d.url" />
                      </div>
                      <div class="field span2">
                        <label>{{ t('mcpHeaders') }}</label>
                        <OInput v-model="d.headersText" placeholder="Authorization=Bearer xxx" />
                      </div>
                    </template>
                  </div>
                  <div v-if="d.origName !== null" class="card-foot">
                    <OButton size="sm" variant="ghost" @click="cancelEditMcp">{{ tc('cancel') }}</OButton>
                  </div>
                </div>
                <!-- 未编辑：摘要行 -->
                <div v-else class="srow">
                  <div class="srow-main">
                    <span class="srow-title">
                      {{ d.name }}
                      <span class="scope-tag">{{ d.kind === 'local' ? t('mcpKindLocal') : t('mcpKindRemote') }}</span>
                    </span>
                    <span class="srow-desc mono">{{ mcpSummary(d) }}</span>
                  </div>
                  <div class="srow-ctl">
                    <OButton size="sm" variant="ghost" @click="editingMcp = d.origName">{{ t('edit') }}</OButton>
                    <OButton size="sm" variant="ghost" @click="mcpDrafts.splice(i, 1)">{{ tc('delete') }}</OButton>
                  </div>
                </div>
              </div>
              <div v-if="mcpDrafts.length === 0" class="srow">
                <span class="srow-desc">{{ t('mcpEmpty') }}</span>
              </div>
            </div>
          </div>
          <footer class="pane-foot">
            <OButton variant="primary" size="sm" :loading="savingMcp" @click="saveMcp">{{ t('saveAll') }}</OButton>
          </footer>
        </section>

        <section v-else-if="section === 'defaults' || section === 'providers'" class="pane">
          <div class="pane-scroll">
            <p class="muted">{{ t('configUnavailable') }}</p>
          </div>
        </section>
      </div>
    </div>
  </OModal>

  <OModal
    :open="presetEdit.open"
    :title="presetEdit.isNew ? t('newPreset') : t('editPreset')"
    width="640px"
    @close="presetEdit.open = false"
  >
    <div class="skill-form">
      <div class="sk-grid">
        <div class="field">
          <label>{{ t('presetId') }}</label>
          <OInput v-model="presetEdit.id" :disabled="!presetEdit.isNew" placeholder="my-preset" />
        </div>
        <div class="field">
          <label>{{ t('scopeLabel') }}</label>
          <OSelect
            v-model="presetEdit.scope"
            :options="[{ value: 'global', label: t('scopeGlobal') }, { value: 'project', label: t('scopeProject') }]"
            :disabled="!presetEdit.isNew"
          />
        </div>
      </div>
      <div class="field">
        <label>{{ t('modelName') }}</label>
        <OInput v-model="presetEdit.name" />
      </div>
      <div class="field">
        <label>{{ t('skillDesc') }}</label>
        <OInput v-model="presetEdit.description" />
      </div>
      <div class="field">
        <label>{{ t('skillTools') }}</label>
        <OInput v-model="presetEdit.toolsText" placeholder="read, write, shell" />
      </div>
      <div class="field">
        <label>{{ t('skillContent') }}</label>
        <div class="skill-content-box">
          <textarea v-model="presetEdit.content" rows="12" spellcheck="false" :disabled="presetEdit.readonly" />
        </div>
        <p v-if="presetEdit.readonly" class="muted">{{ t('presetReadonly') }}</p>
      </div>
    </div>
    <template #footer>
      <OButton
        v-if="!presetEdit.isNew && !presetEdit.readonly"
        variant="danger"
        size="sm"
        @click="removePreset"
      >
        {{ tc('delete') }}
      </OButton>
      <OButton variant="ghost" size="sm" @click="presetEdit.open = false">{{ tc('cancel') }}</OButton>
      <OButton variant="primary" size="sm" :disabled="presetEdit.readonly || !presetEdit.id.trim() || (presetEdit.scope === 'project' && !skillWorkspace)" @click="savePreset">
        {{ tc('save') }}
      </OButton>
    </template>
  </OModal>

  <OModal
    :open="skillEdit.open"
    :title="skillEdit.isNew ? t('newSkill') : t('editSkill')"
    width="640px"
    @close="skillEdit.open = false"
  >
    <div class="skill-form">
      <div class="sk-grid">
        <div class="field">
          <label>{{ t('skillId') }}</label>
          <OInput v-model="skillEdit.id" :disabled="!skillEdit.isNew" placeholder="my-skill" />
        </div>
        <div class="field">
          <label>{{ t('scopeLabel') }}</label>
          <OSelect
            v-model="skillEdit.scope"
            :options="[{ value: 'global', label: t('scopeGlobal') }, { value: 'project', label: t('scopeProject') }]"
            :disabled="!skillEdit.isNew"
          />
        </div>
      </div>
      <div class="field">
        <label>{{ t('modelName') }}</label>
        <OInput v-model="skillEdit.name" />
      </div>
      <div class="field">
        <label>{{ t('skillDesc') }}</label>
        <OInput v-model="skillEdit.description" />
      </div>
      <div class="field">
        <label>{{ t('skillContent') }}</label>
        <div class="skill-content-box">
          <textarea v-model="skillEdit.content" rows="12" spellcheck="false" :disabled="skillEdit.readonly" />
        </div>
        <p v-if="skillEdit.readonly" class="muted">{{ t('skillReadonly') }}</p>
      </div>
    </div>
    <template #footer>
      <OButton
        v-if="!skillEdit.isNew && !skillEdit.readonly"
        variant="danger"
        size="sm"
        @click="removeSkill"
      >
        {{ tc('delete') }}
      </OButton>
      <OButton variant="ghost" size="sm" @click="skillEdit.open = false">{{ tc('cancel') }}</OButton>
      <OButton variant="primary" size="sm" :disabled="skillEdit.readonly || !skillEdit.id.trim() || (skillEdit.scope === 'project' && !skillWorkspace)" @click="saveSkill">
        {{ tc('save') }}
      </OButton>
    </template>
  </OModal>

  <OModal
    :open="themeEdit.open"
    :title="themeEdit.isNew ? t('newTheme') : t('editTheme')"
    width="560px"
    @close="themeEdit.open = false"
  >
    <div class="skill-form">
      <div class="field">
        <label>{{ t('themeName') }}</label>
        <OInput v-model="themeEdit.name" placeholder="Nord Dark" />
      </div>
      <div class="sk-grid">
        <div class="field">
          <label>{{ t('themeModeLabel') }}</label>
          <ORadio
            v-model="themeEdit.mode"
            :options="[{ value: 'light', label: t('modeLight') }, { value: 'dark', label: t('modeDark') }]"
          />
        </div>
        <div class="field">
          <label>{{ t('themeBaseLabel') }}</label>
          <ORadio v-model="themeEdit.base" :options="themeBaseOptions" />
        </div>
      </div>
      <div class="field">
        <label>{{ t('themeColors') }}</label>
        <div class="color-grid">
          <div v-for="tok in THEME_TOKENS" :key="tok" class="color-cell">
            <span class="color-swatch" :style="{ background: effectiveColor(tok) }" />
            <input v-model="themeEdit.colors[tok]" class="color-input" :placeholder="effectiveColor(tok)" spellcheck="false" />
          </div>
        </div>
      </div>
    </div>
    <template #footer>
      <OButton variant="ghost" size="sm" @click="themeEdit.colors = {}">{{ t('themeReset') }}</OButton>
      <OButton variant="primary" size="sm" @click="saveThemeEdit">{{ tc('save') }}</OButton>
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
  padding: 12px 10px 4px;
  border-top: 1px solid var(--line);
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
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
/* 内容栏：标题与保存操作固定在上下两端，仅中间列表滚动 */
.pane {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  width: 100%;
  max-width: 720px;
  margin: 0 auto;
  padding: 18px 28px 0;
}
.pane-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-shrink: 0;
  padding-bottom: 14px;
}
.pane-title {
  margin: 0;
  font-size: 15px;
  font-weight: 600;
  color: var(--ink);
}
.tabs-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}
/* 与「外观 / 技能」所用的 ORadio 分段控件保持同一视觉语言：
   同一容器底色与描边，选中项为强调色实底 + base 文字 */
.tabs {
  display: flex;
  align-items: center;
  gap: 2px;
  flex: 0 1 auto;
  max-width: 100%;
  min-width: 0;
  overflow-x: auto;
  padding: 3px;
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 9px;
}
/* 新增/删除操作固定在最右侧，不随标签数量移动 */
.tabs-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-left: auto;
  flex-shrink: 0;
}
.tab {
  flex-shrink: 0;
  padding: 5px 12px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-tertiary);
  font-family: inherit;
  font-size: 12px;
  font-weight: 500;
  cursor: pointer;
  white-space: nowrap;
  transition:
    background-color 0.15s ease,
    color 0.15s ease;
}
.tab:hover {
  color: var(--ink);
}
.tab.active {
  background: var(--accent);
  color: var(--base);
}
.pane-head-actions {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}
.pane-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-bottom: 4px;
}
.pane-foot {
  flex-shrink: 0;
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  padding: 12px 0 16px;
  border-top: 1px solid var(--line);
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
.color-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 14px;
}
.color-cell {
  display: flex;
  align-items: center;
  gap: 8px;
}
.color-swatch {
  width: 20px;
  height: 20px;
  border-radius: 6px;
  border: 1px solid var(--line);
  flex-shrink: 0;
}
.color-input {
  flex: 1;
  min-width: 0;
  height: 28px;
  padding: 0 8px;
  border: 1px solid var(--control-border);
  border-radius: 6px;
  background: var(--surface);
  color: var(--ink);
  font-family: var(--font-mono);
  font-size: 12px;
  outline: none;
}
.color-input:focus {
  border-color: var(--accent);
}
.scope-tag {
  display: inline-block;
  margin-left: 6px;
  padding: 0 7px;
  border-radius: 99px;
  font-size: 10.5px;
  font-weight: 500;
  vertical-align: 1px;
  background: var(--surface-strong);
  color: var(--overlay1);
}
.scope-tag.scope-project {
  background: var(--surface-active);
  color: var(--accent);
}
.skill-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}
.sk-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 12px;
}
.skill-content-box {
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--surface);
}
.skill-content-box:focus-within {
  border-color: var(--accent);
}
.skill-content-box textarea {
  display: block;
  width: 100%;
  min-height: 220px;
  padding: 10px 12px;
  border: none;
  outline: none;
  background: transparent;
  color: var(--ink);
  font-family: var(--font-mono);
  font-size: 12.5px;
  line-height: 1.65;
  resize: vertical;
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
  border: none;
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
.card-foot {
  display: flex;
  justify-content: flex-end;
  margin-top: 10px;
}
.srow-desc.mono {
  font-family: var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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
/* MCP 编辑卡片的两列表单（左侧名称、右侧控件） */
.fields {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 14px;
  margin-top: 10px;
}
/* 提供商/MCP 编辑卡片：仅作为区块容器，视觉边界交给 .cfg-group */
.prov {
  margin-bottom: 10px;
}
/* MCP 编辑卡片底色（提供商卡片已改用 .cfg-group 分组） */
.card {
  background: var(--surface);
  border-radius: 10px;
  padding: 12px 14px;
}
/* 配置区块：标题与配置项同处一个带底色的区域，避免标题浮在区域之上 */
.cfg-group {
  background: var(--surface);
  border-radius: 10px;
  padding: 12px 14px;
  margin-bottom: 10px;
}
.cfg-group:last-child {
  margin-bottom: 0;
}
.cfg-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  min-height: 24px;
}
.cfg-title {
  font-size: 11.5px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--overlay0);
}
.cfg-rows {
  display: grid;
  grid-template-columns: 120px minmax(0, 1fr);
  align-items: center;
  gap: 8px 12px;
  padding-top: 8px;
}
.cfg-label {
  font-size: 12.5px;
  line-height: 1.35;
  color: var(--text-secondary);
  /* 不截断配置项名称：较长语言换行展示，避免出现 "Reasoning Effo…" */
  overflow-wrap: break-word;
}
.cfg-ctl {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.cfg-ctl > .o-input,
.cfg-ctl > .o-select {
  flex: 1;
  min-width: 0;
}
.cfg-ctl .checks {
  min-height: 0;
}
/* 标识符类输入（提供商名、模型 id）用等宽字体 */
.mono :deep(input) {
  font-family: var(--font-mono);
}
.model {
  padding-bottom: 6px;
  border-bottom: 1px solid var(--line);
}
.model:last-of-type {
  border-bottom: none;
}
.model-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  padding: 8px 0 0;
}
.model-title {
  font-size: 12px;
  font-weight: 500;
  color: var(--text-tertiary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
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
  margin: 0 0 10px;
}
</style>
