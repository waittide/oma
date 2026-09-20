<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { toast, UiButton, UiIconButton, UiInput, UiModal, UiMultiSelect, UiSegmented, UiSelect, UiTextarea, UiTooltip } from '@waittide/ui';
import {
  LuCheck,
  LuChevronRight,
  LuEye,
  LuEyeOff,
  LuLanguages,
  LuLoader,
  LuPalette,
  LuPlugZap,
  LuPlus,
  LuServer,
  LuSparkles,
  LuSquareUserRound,
  LuTrash2,
  LuX,
} from 'vue-icons-plus/lu';
import { api } from '../api';
import { ACCENTS, NEUTRAL_TOKENS, PALETTE_TOKENS, config, darkPalettes, lightPalettes, loadConfig, palettes, refreshPalettes, saveConfig, saveTheme, theme, type Palette } from '../stores/theme';
import { baseUrl, normalizeBaseUrl, setConnection, token } from '../stores/connection';
import { modelSelectorLabel, toModelSelectGroups } from '../lib/modelSelect';
import { clientConfig, clientConfigReady, saveClientConfig } from '../stores/clientConfig';
import { LOCALES, settingStore, setLocale, type Locale } from '../stores/setting';
import { activeSession, refresh as refreshSessions } from '../stores/sessions';
import { MODEL_CAPABILITIES } from '../types';
import type {
  ToolInfo,
  ModelCapability,
  ModelInfo,
  ClientConfig,
  ClientConnection,
  OmaConfig,
  PaletteMode,
  ProviderConfig,
  SkillFile,
  Theme,
} from '../types';
import { useTranslations } from '../composables/i18n';

const props = defineProps<{ open: boolean; online?: boolean }>();
const emit = defineEmits<{ close: []; reconnect: [] }>();

const { t } = useTranslations('settings');
const { t: tc } = useTranslations('common');

type SectionId =
  | 'connection'
  | 'theme'
  | 'language'
  | 'defaults'
  | 'providers'
  | 'skills';
const section = ref<SectionId>('connection');

/** 参照 opencode 设置弹窗：导航按分组小标题聚类，底部展示应用版本。 */
const navGroups = computed(() => [
  {
    title: t('navSectionPersonal'),
    items: [
      { id: 'connection' as SectionId, label: t('navConnection'), icon: LuPlugZap },
      { id: 'theme' as SectionId, label: t('navTheme'), icon: LuPalette },
      { id: 'language' as SectionId, label: t('navLanguage'), icon: LuLanguages },
    ],
  },
  {
    title: t('navSectionService'),
    items: [
      { id: 'defaults' as SectionId, label: t('navDefaults'), icon: LuSquareUserRound },
      { id: 'providers' as SectionId, label: t('navProviders'), icon: LuServer },
      { id: 'skills' as SectionId, label: t('navSkills'), icon: LuSparkles },
    ],
  },
]);

const version = ref('');

// ---------- 连接 ----------
// 连接列表保存在 client.json，由 oma web 的同源接口读写；接口不可用（vite dev）时
// 退回旧的 localStorage 行为，只记住当前这一条。
const conn = reactive({ name: '', baseUrl: baseUrl.value, token: token.value });
const connTesting = ref(false);
// 首次进入时自动探测一次，让用户不用手动点就知道当前配得对不对
const connState = ref<'unknown' | 'ok' | 'fail'>('unknown');
const connDetail = ref('');
/** 列表中选中的连接名；null = 新建 */
const selectedConn = ref<string | null>(null);

const connections = computed(() => clientConfig.value?.connections ?? []);

/** 是否为当前生效的连接（active 为空时列表首个即生效）。 */
function isActive(name: string): boolean {
  const cfg = clientConfig.value;
  return !!cfg && (cfg.active === name || (!cfg.active && cfg.connections[0]?.name === name));
}

/** 选中列表中的连接并载入表单。 */
function selectConnection(c: ClientConnection) {
  selectedConn.value = c.name;
  conn.name = c.name;
  conn.baseUrl = c.url;
  conn.token = c.token;
  connState.value = 'unknown';
  connDetail.value = '';
}

/** 清空表单，新建一条连接。 */
function newConnection() {
  selectedConn.value = null;
  conn.name = '';
  conn.baseUrl = '';
  conn.token = '';
  connState.value = 'unknown';
  connDetail.value = '';
}

// 配置就绪后定位到活动连接（列表加载完成时自动选中）
watch(
  clientConfig,
  (cfg) => {
    if (!cfg || selectedConn.value) return;
    const active = cfg.connections.find((c) => c.name === cfg.active) ?? cfg.connections[0];
    if (active) selectConnection(active);
  },
  { immediate: true },
);

/** 探测目标 Daemon：需要用表单里的值（而不是已保存的值）即时验证。 */
async function testConnection(): Promise<boolean> {
  const probeToken = conn.token.trim();
  if (!probeToken) {
    // 空 token 只会发出一个 `Authorization: Bearer ` 的请求，401 也看不出所以然，
    // 这里直接说明原因，省得用户去翻控制台
    connState.value = 'fail';
    connDetail.value = t('connTokenRequired');
    return false;
  }

  const base = normalizeBaseUrl(conn.baseUrl);

  connTesting.value = true;
  try {
    const resp = await fetch(`${base}/api/server/status`, {
      headers: { Authorization: `Bearer ${probeToken}` },
    });
    if (resp.ok) {
      const info = (await resp.json()) as { version?: string; active_sessions?: number };
      connState.value = 'ok';
      connDetail.value = t('connReachable', {
        version: info.version ?? '?',
        sessions: info.active_sessions ?? 0,
      });
      return true;
    }
    connState.value = 'fail';
    connDetail.value =
      resp.status === 401
        ? t('connUnauthorized')
        : t('connHttpError', { status: resp.status });
    return false;
  } catch {
    // fetch 失败只有浏览器自带的英文原因（`Failed to fetch` 之类），既不是中文也
    // 帮不上排查（原因本身对 JS 不透明），这里换成目标地址 + 可能的排查方向
    connState.value = 'fail';
    connDetail.value = t('connUnreachable', { base: base || location.origin });
    return false;
  } finally {
    connTesting.value = false;
  }
}

/** 组装下一份 client.json：upsert 当前连接并置为活动，重命名时移除旧名。 */
function buildNextConfig(next: ClientConnection): ClientConfig | null {
  const cfg = clientConfig.value;
  if (!cfg) return null;
  const list = cfg.connections.filter((c) => c.name !== next.name && c.name !== selectedConn.value);
  list.push(next);
  return { ...cfg, connections: list, active: next.name };
}

async function saveConnection() {
  const name = conn.name.trim();
  // 仅当能落盘 client.json 时才要求名称；无接口时退回旧的 localStorage 行为
  if (clientConfigReady.value && !name) {
    toast.error(t('connNameRequired'));
    return;
  }
  const url = normalizeBaseUrl(conn.baseUrl);
  // 保存的连接必须带地址（服务器端也会校验）；空地址只对当前会话的「同源」有意义
  if (clientConfigReady.value && !url) {
    toast.error(t('connUrlRequired'));
    return;
  }
  setConnection(url, conn.token);
  conn.baseUrl = baseUrl.value;
  conn.token = token.value;

  // 先落盘 client.json（接口可用时），再验证可达性：即使目标暂时连不上，
  // 保存的连接与 token 也不应丢失。
  if (clientConfigReady.value) {
    const next = buildNextConfig({ name, url, token: conn.token });
    if (next) {
      try {
        await saveClientConfig(next);
        selectedConn.value = name;
      } catch (e) {
        toast.error(t('connSaveFailed', { message: (e as Error).message }));
        return;
      }
    }
  }

  if (await testConnection()) {
    toast.success(t('connSaved'));
    // 地址/凭证换了之后，旧数据（会话、消息、主题、配置）都属于上一个 Daemon，
    // 必须整体重载；由 App 统一做：会话列表 + 配置主题 + 重新建立 WS
    await reloadAll();
  } else {
    // 失败也要给出可见反馈：否则点完按钮像是「什么都没发生」，只能去翻控制台
    toast.error(connDetail.value || t('connFailed'));
  }
}

/** 删除一条已保存连接并落盘；若删的是活动连接则顺延到列表首个。 */
async function removeConnection(name: string) {
  const cfg = clientConfig.value;
  if (!cfg) return;
  const list = cfg.connections.filter((c) => c.name !== name);
  const active = cfg.active === name ? (list[0]?.name ?? '') : cfg.active;
  try {
    await saveClientConfig({ ...cfg, connections: list, active });
  } catch (e) {
    toast.error(t('connSaveFailed', { message: (e as Error).message }));
    return;
  }
  if (selectedConn.value === name) {
    const next = list[0];
    if (next) selectConnection(next);
    else newConnection();
  }
  toast.success(t('connRemoved'));
}

/** 重新拉取属于「某个 Daemon」的全部状态（会话、主题/配置、调色板）。 */
async function reloadAll() {
  await Promise.allSettled([refreshSessions(), loadConfig()]);
  emit('reconnect');
}

// ---------- 主题 ----------
const mode = ref<Theme['mode']>(theme.value.mode);
const accent = ref<string>(theme.value.accent);
const savingTheme = ref(false);

const modeOptions = computed<{ value: Theme['mode']; label: string }[]>(() => [
  { value: 'light', label: t('modeLight') },
  { value: 'dark', label: t('modeDark') },
  { value: 'system', label: t('modeSystem') },
]);

// 浅色/深色各自引用的调色板 id。
// 两个下拉框只在同一明暗组内选择，从根上避免「浅色引用了深色调色板」。
const themeSel = reactive({ light: 'latte', dark: 'mocha' });

const lightPaletteOptions = computed(() =>
  lightPalettes.value.map((p) => ({ value: p.id, label: p.name })),
);
const darkPaletteOptions = computed(() =>
  darkPalettes.value.map((p) => ({ value: p.id, label: p.name })),
);

async function applyTheme() {
  savingTheme.value = true;
  try {
    const next: Theme = {
      mode: mode.value,
      dark_palette: themeSel.dark,
      light_palette: themeSel.light,
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

// ---------- 调色板编辑 ----------
// 调色板是一整组色值（26 个令牌），而非「基底 + 覆盖表」：
// 因此编辑器始终持有一份完整拷贝，保存时整份落盘。
const paletteEdit = reactive<{
  open: boolean;
  isNew: boolean;
  id: string;
  name: string;
  mode: PaletteMode;
  base: string;
  values: Record<string, string>;
}>({
  open: false,
  isNew: false,
  id: '',
  name: '',
  mode: 'dark',
  base: 'mocha',
  values: {},
});

/** 当前编辑模式下可作基底的调色板。 */
const paletteBaseOptions = computed(() =>
  (paletteEdit.mode === 'light' ? lightPalettes.value : darkPalettes.value).map((p) => ({
    value: p.id,
    label: p.name,
  })),
);

function paletteById(id: string): Palette | undefined {
  return palettes.value.find((p) => p.id === id);
}

function isHexColor(v: string): boolean {
  return /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(v.trim());
}

/** 切换基底时整份拷贝一份，避免旧基底颜色残留造成「看起来没换」。 */
function applyBase(id: string) {
  const base = paletteById(id);
  if (!base) return;
  paletteEdit.base = id;
  paletteEdit.values = Object.fromEntries(PALETTE_TOKENS.map((tk) => [tk, base[tk]]));
}

/** 切换明暗：基底候选随之更换，否则会出现深色基底配浅色主题。 */
function newPaletteMode(next: PaletteMode) {
  paletteEdit.mode = next;
  const group = next === 'light' ? lightPalettes.value : darkPalettes.value;
  applyBase(group[0]?.id ?? (next === 'light' ? 'latte' : 'mocha'));
}

function newPalette() {
  paletteEdit.open = true;
  paletteEdit.isNew = true;
  paletteEdit.id = '';
  paletteEdit.name = '';
  paletteEdit.mode = mode.value === 'light' ? 'light' : 'dark';
  const group = paletteEdit.mode === 'light' ? lightPalettes.value : darkPalettes.value;
  applyBase(group[0]?.id ?? (paletteEdit.mode === 'light' ? 'latte' : 'mocha'));
}

function editPalette(p: Palette) {
  paletteEdit.open = true;
  paletteEdit.isNew = false;
  paletteEdit.id = p.id;
  paletteEdit.name = p.name;
  paletteEdit.mode = p.mode;
  paletteEdit.base = p.id;
  paletteEdit.values = Object.fromEntries(PALETTE_TOKENS.map((tk) => [tk, p[tk]]));
}

/** 展示名 → 稳定 id（文件名）；非 ASCII 名称无法 slug 时回退时间戳。 */
function slugifyPaletteName(name: string): string {
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  return slug || `palette-${Date.now()}`;
}

async function savePaletteEdit() {
  const name = paletteEdit.name.trim();
  if (!name) {
    toast.error(t('paletteNameRequired'));
    return;
  }
  const invalid = PALETTE_TOKENS.filter((tk) => !isHexColor(paletteEdit.values[tk] ?? ''));
  if (invalid.length > 0) {
    toast.error(t('paletteInvalidColor', { token: invalid[0] }));
    return;
  }

  const id = paletteEdit.isNew ? slugifyPaletteName(name) : paletteEdit.id;
  if (paletteEdit.isNew && palettes.value.some((p) => p.id === id)) {
    toast.error(t('paletteIdExists', { id }));
    return;
  }

  const payload = {
    id,
    name,
    mode: paletteEdit.mode,
    ...Object.fromEntries(PALETTE_TOKENS.map((tk) => [tk, paletteEdit.values[tk].trim()])),
  } as Palette;

  try {
    await api.putPalette(payload);
    await refreshPalettes();
    // 新建/编辑后让主题选择跟随它，否则保存完看不到效果
    if (payload.mode === 'light') themeSel.light = id;
    else themeSel.dark = id;
    paletteEdit.open = false;
    toast.success(t('themeSaved'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

async function removePalette(id: string) {
  try {
    await api.deletePalette(id);
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
    return;
  }
  await refreshPalettes();

  // 被删的调色板若正被主题引用，必须立即把新选择写回服务端：
  // 否则 settings.json 里会留有悬空 id，下次启动/刷新时主题直接失效。
  const referenced = theme.value.light_palette === id || theme.value.dark_palette === id;
  if (themeSel.light === id) themeSel.light = lightPalettes.value[0]?.id ?? 'latte';
  if (themeSel.dark === id) themeSel.dark = darkPalettes.value[0]?.id ?? 'mocha';

  try {
    if (referenced) await applyTheme();
    toast.success(t('themeDeleted'));
  } catch (e) {
    toast.error(t('saveFailed', { message: (e as Error).message }));
  }
}

// ---------- 默认参数 ----------
const defaults = reactive({
  model: '',
  reasoning: '',
});
const savingDefaults = ref(false);

watch(
  () => props.open,
  async (v) => {
    if (!v) return;
    if (!config.value) await loadConfig();
    if (config.value) {
      defaults.model = config.value.default_model;
      defaults.reasoning = config.value.default_reasoning_level || DEFAULT_REASONING_LEVEL;
    }
    mode.value = theme.value.mode;
    themeSel.light = theme.value.light_palette || 'latte';
    themeSel.dark = theme.value.dark_palette || 'mocha';
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
    // model/reasoning 这两个无效键，服务端忽略后配置纹丝不动，
    // 但请求本身成功 —— 表现为「提示保存成功，实际没生效」。
    await saveConfig({
      ...config.value,
      default_model: defaults.model,
      default_reasoning_level: defaults.reasoning,
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

/** 模型选择器的分组选项与当前展示名。 */
const modelGroups = computed(() => toModelSelectGroups(providerGroups.value));
const modelLabel = computed(() => modelSelectorLabel(providerGroups.value, defaults.model));

function scopeLabel(scope: string): string {
  switch (scope) {
    case 'agent':
      return t('scopeAgent');
    case 'project':
      return t('scopeProject');
    default:
      return t('scopeGlobal');
  }
}

/** 技能作用域：跨工具全局 + oma 自身 + 项目（越靠后越具体） */
type SkillScopeName = 'global' | 'agent' | 'project';

const skillWorkspace = computed(() => activeSession.value?.workspace ?? '');

const skillScopeOptions = computed(() => [
  { value: 'global' as SkillScopeName, label: t('scopeGlobal') },
  { value: 'agent' as SkillScopeName, label: t('scopeAgent') },
  { value: 'project' as SkillScopeName, label: t('scopeProject') },
]);

/** 当前查看的来源层：兼作新建位置，上方标签页即为此切换 */
const skillLayer = ref<SkillScopeName>('global');

const visibleSkills = computed(() => skills.value.filter((sk) => sk.scope === skillLayer.value));


// ---------- 技能（按需取用的领域知识，与预设相互独立） ----------
const skills = ref<SkillFile[]>([]);
const skillsLoading = ref(false);

async function loadSkills() {
  skillsLoading.value = true;
  try {
    // 始终带上工作区：三层技能一并取回（同名时后端已按 project > agent > global 合并）
    skills.value = await api.skills(skillWorkspace.value || undefined);
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
  body: '',
  scope: 'global' as SkillScopeName,
});

function editSkill(s: SkillFile) {
  skillEdit.open = true;
  skillEdit.isNew = false;
  skillEdit.readonly = false;
  skillEdit.id = s.id;
  skillEdit.name = s.name;
  skillEdit.description = s.description;
  skillEdit.body = s.body;
  skillEdit.scope = s.scope as SkillScopeName;
}

function newSkill() {
  skillEdit.open = true;
  skillEdit.isNew = true;
  skillEdit.readonly = false;
  skillEdit.id = '';
  skillEdit.name = '';
  skillEdit.description = '';
  skillEdit.body = '';
  skillEdit.scope = skillLayer.value;
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
        content: skillEdit.body,
        scope: skillEdit.scope,
      },
      skillEdit.scope === 'project' ? skillWorkspace.value : '',
    );
    skillEdit.open = false;
    skillLayer.value = skillEdit.scope;
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
  () => [props.open, section.value] as const,
  ([o, s]) => {
    if (!o) return;
    // 进入连接页先探一次：用户开箱即见当前是否连得上，不用先点「测试」
    if (s === 'connection') void testConnection();
    if (s === 'skills') void loadSkills();
  },
);

/** JSON 对象 → 缩进文本；空对象与非法值一律渲染为空串，界面从干净状态开始编辑。 */
function formatJsonText(value: unknown): string {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return '';
  if (Object.keys(value).length === 0) return '';
  return JSON.stringify(value, null, 2);
}

/**
 * 解析请求覆写文本（请求头与请求体同格式）。
 *
 * 返回 null 表示格式非法（调用方据此阻断保存）；空文本视为未填写，返回 `{}`。
 * 只接受 JSON 对象：覆写字段是合并目标，数组/标量无法与内置请求合并。
 */
function parseJsonText(text: string): Record<string, unknown> | null {
  const trimmed = text.trim();
  if (!trimmed) return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return null;
  return parsed as Record<string, unknown>;
}

/**
 * 解析请求头文本。
 *
 * 在后端请求头是 `Map<String, String>`，取值非字符串会让整份配置校验失败，
 * 故这里提前收紧：非法 JSON 或取值非字符串一律返回 null 阻断保存。
 */
function parseHeaderText(text: string): Record<string, string> | null {
  const obj = parseJsonText(text);
  if (obj === null) return null;
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(obj)) {
    if (typeof v !== 'string') return null;
    out[k] = v;
  }
  return out;
}

/** 是否含有实际字段：空对象不写入配置，保持配置文件干净。 */
function hasJsonKeys(value: Record<string, unknown>): boolean {
  return Object.keys(value).length > 0;
}


// ---------- 模型提供商（草稿编辑，保存时统一校验与写回） ----------
interface ModelDraft {
  id: string;
  name: string;
  context_len: string;
  max_output: string;
  /** 推理等级 → 厂商自定义字符串；空串表示按等级名下发 */
  reasoning_map: Record<string, string>;
  capabilities: ModelCapability[];
  /** 模型级请求头（JSON 对象文本） */
  headersText: string;
  /** 模型级请求体覆写（JSON 文本） */
  bodyText: string;
  /** 推理等级映射折叠区是否展开（默认收起） */
  reasoningOpen: boolean;
  /** 请求头/请求体折叠区是否展开（默认收起） */
  requestOpen: boolean;
}

interface ProviderDraft {
  /** 稳定标识：与可编辑的 name 解耦，供密钥显隐等局部状态作键 */
  uid: number;
  /** null = 本次新增，尚无服务端原键 */
  origId: string | null;
  name: string;
  api_type: string;
  base_url: string;
  /** 明文密钥；`keyMasked` 为 true 时此处为空，界面显示等长掩码占位 */
  api_key: string;
  /** true = 服务端下发的脱敏值且用户未编辑，提交时回传掩码以保留原密钥 */
  keyMasked: boolean;
  /** 真实密钥字符数，供脱敏占位渲染等长掩码 */
  keyLen: number;
  /** 提供商级请求头（JSON 对象文本） */
  headersText: string;
  /** 提供商级请求体覆写（JSON 文本） */
  bodyText: string;
  /** 请求头/请求体折叠区是否展开（默认收起） */
  requestOpen: boolean;
  models: ModelDraft[];
}

/** 与服务端约定的脱敏占位符：PUT 回传该值时保留服务端既有密钥 */
const API_KEY_MASK = '***';

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
    api_key: p.api_key === API_KEY_MASK ? '' : p.api_key,
    keyMasked: p.api_key === API_KEY_MASK,
    keyLen: p.api_key === API_KEY_MASK ? (p.api_key_len ?? API_KEY_MASK.length) : 0,
    headersText: formatJsonText(p.headers),
    bodyText: formatJsonText(p.body),
    requestOpen: false,
    models: (p.models ?? []).map((m) => ({
      id: m.id,
      name: m.name,
      context_len: String(m.context_len),
      max_output: m.max_output === undefined ? '' : String(m.max_output),
      reasoning_map: { ...(m.reasoning_map ?? {}) },
      capabilities: [...(m.capabilities ?? [])],
      headersText: formatJsonText(m.headers),
      bodyText: formatJsonText(m.body),
      reasoningOpen: false,
      requestOpen: false,
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

/** 规范推理等级：与后端 REASONING_LEVELS 保持一致，顺序即界面展示顺序 */
const REASONING_LEVELS = ['minimal', 'low', 'medium', 'high', 'xhigh', 'max', 'ultra'] as const;
/** 等级为必选项，历史配置缺值时统一回退到这里（与后端默认一致） */
const DEFAULT_REASONING_LEVEL = 'medium';

const effortOptions = computed(() =>
  REASONING_LEVELS.map((v) => ({ value: v, label: v })),
);

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
          draft.keyMasked = false;
          draft.keyLen = 0;
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
  if (on) {
    if (d.keyMasked) {
      if (!revealedReal) {
        const real = await api.getConfig({ reveal: true });
        revealedReal = Object.fromEntries(
          Object.entries(real.providers).map(([id, p]) => [id, p.api_key]),
        );
      }
      const orig = d.origId ? revealedReal[d.origId] : undefined;
      if (orig !== undefined) {
        d.api_key = orig;
        d.keyMasked = false;
      }
    }
  } else if (revealedReal) {
    // 未编辑过才收回为掩码；用户改过则保留其输入
    const orig = d.origId ? revealedReal[d.origId] : undefined;
    if (orig !== undefined && d.api_key === orig) {
      d.api_key = '';
      d.keyMasked = true;
      d.keyLen = orig.length;
    }
  }
  keyRevealed.value[d.uid] = on;
}

/** 用户一旦编辑密钥即离开脱敏占位态，提交时按输入的字面值发送 */
function setApiKey(d: ProviderDraft, value: string) {
  d.api_key = value;
  d.keyMasked = false;
}

/** 能力多选下拉：取值与展示顺序来自共享的 MODEL_CAPABILITIES 定义 */
const capabilityOptions = computed(() =>
  MODEL_CAPABILITIES.map((c) => ({ value: c.value, label: t(c.label) })),
);

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
    keyMasked: false,
    keyLen: 0,
    headersText: '',
    bodyText: '',
    requestOpen: false,
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
    reasoning_map: {},
    capabilities: ['text_input', 'text_output'],
    headersText: '',
    bodyText: '',
    reasoningOpen: false,
    requestOpen: false,
  });
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
    if (providerDrafts.value.some((d) => d.keyMasked)) {
      const real = await api.getConfig({ reveal: true });
      for (const d of providerDrafts.value) {
        if (!d.keyMasked) continue;
        const orig = d.origId ? real.providers[d.origId]?.api_key : undefined;
        if (orig !== undefined) {
          d.api_key = orig;
          d.keyMasked = false;
        }
      }
    }

    const providers: Record<string, ProviderConfig> = {};
    for (const d of providerDrafts.value) {
      // 校验覆写文本：格式错误应阻断保存，而不是静默写进配置
      const headers = parseHeaderText(d.headersText);
      const body = parseJsonText(d.bodyText);
      if (headers === null) {
        toast.error(t('requestHeadersInvalid', { name: d.name || t('providerName') }));
        return;
      }
      if (body === null) {
        toast.error(t('requestBodyInvalid', { name: d.name || t('providerName') }));
        return;
      }
      const models = [];
      for (const m of d.models) {
        if (!m.id.trim()) continue;
        const ctx = parseInt(m.context_len, 10);
        const mo = parseInt(m.max_output, 10);
        const modelBody = parseJsonText(m.bodyText);
        const modelHeaders = parseHeaderText(m.headersText);
        if (modelHeaders === null) {
          toast.error(t('requestHeadersInvalid', { name: m.name || m.id }));
          return;
        }
        if (modelBody === null) {
          toast.error(t('requestBodyInvalid', { name: m.name || m.id }));
          return;
        }
        models.push({
          id: m.id.trim(),
          name: m.name.trim(),
          context_len: Number.isFinite(ctx) && ctx > 0 ? ctx : 128000,
          capabilities: [...m.capabilities],
          ...(Number.isFinite(mo) && mo > 0 ? { max_output: mo } : {}),
          // 只回写非空映射项，避免把整表空值写进配置
          ...(m.capabilities.includes('thinking') && Object.keys(m.reasoning_map).length
            ? {
                reasoning_map: Object.fromEntries(
                  Object.entries(m.reasoning_map).filter(([, v]) => v.trim() !== ''),
                ),
              }
            : {}),
          ...(hasJsonKeys(modelHeaders) ? { headers: modelHeaders } : {}),
          ...(hasJsonKeys(modelBody) ? { body: modelBody } : {}),
        });
      }
      providers[d.name.trim()] = {
        api_type: d.api_type,
        base_url: d.base_url.trim(),
        api_key: d.keyMasked ? API_KEY_MASK : d.api_key,
        headers,
        body,
        models,
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

const localeOptions = computed(() =>
  LOCALES.map((l) => ({ value: l.value, label: l.label })),
);

function pickLocale(v: Locale) {
  setLocale(v);
}
</script>

<template>
  <UiModal :open="open" width="min(calc(100vw - 32px), 980px)" flush floating-close @close="emit('close')">
    <div class="split">
      <nav class="nav">
        <div v-for="g in navGroups" :key="g.title" class="nav-group">
          <span class="nav-title">{{ g.title }}</span>
          <UiButton
            v-for="s in g.items"
            :key="s.id"
            variant="ghost"
            tone="neutral"
            block
            class="nav-item"
            :class="{ active: section === s.id }"
            @click="section = s.id"
          >
            <component :is="s.icon" :size="14" />
            <span>{{ s.label }}</span>
          </UiButton>
        </div>
        <div class="nav-foot">
          <span class="nav-foot-name">Oma</span>
          <span v-if="version" class="nav-foot-ver">v{{ version }}</span>
        </div>
      </nav>

      <div class="content">
        <!-- 连接 -->
        <section v-if="section === 'connection'" class="pane">
          <header class="pane-head">
            <h2 class="pane-title">{{ t('navConnection') }}</h2>
            <UiButton variant="soft" tone="neutral" v-if="clientConfigReady" size="sm" @click="newConnection">
              <template #prefix><LuPlus :size="13" /></template>
              {{ t('connAdd') }}
            </UiButton>
          </header>
          <div class="pane-scroll">
            <!-- 已保存的连接：点击切换，活动项带勾选标记 -->
            <div v-if="clientConfigReady" class="list conn-list">
              <p v-if="connections.length === 0" class="conn-empty">{{ t('connEmpty') }}</p>
              <div
                v-for="c in connections"
                :key="c.name"
                class="conn-item"
                :class="{ on: selectedConn === c.name }"
                role="button"
                tabindex="0"
                @click="selectConnection(c)"
                @keydown.enter.prevent="selectConnection(c)"
              >
                <span class="conn-mark"><LuCheck v-if="isActive(c.name)" :size="11" /></span>
                <span class="conn-main">
                  <span class="conn-name">{{ c.name }}</span>
                  <span class="conn-url">{{ c.url }}</span>
                </span>
                <UiIconButton
                  class="conn-del"
                  size="sm"
                  :label="tc('delete')"
                  @click.stop="removeConnection(c.name)"
                >
                  <LuTrash2 :size="13" />
                </UiIconButton>
              </div>
            </div>

            <div class="list">
              <div v-if="clientConfigReady" class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('connName') }}</span>
                  <span class="srow-desc">{{ t('connNameDesc') }}</span>
                </div>
                <div class="srow-ctl wide">
                  <UiInput
                    v-model="conn.name"
                    class="conn-input"
                    :placeholder="t('connNamePlaceholder')"
                  />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('connBaseUrl') }}</span>
                  <span class="srow-desc">{{ t('connBaseUrlDesc') }}</span>
                </div>
                <div class="srow-ctl wide">
                  <UiInput
                    v-model="conn.baseUrl"
                    class="conn-input"
                    :placeholder="t('connBaseUrlPlaceholder')"
                  />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('connToken') }}</span>
                  <span class="srow-desc">{{ t('connTokenDesc') }}</span>
                </div>
                <div class="srow-ctl wide">
                  <UiInput
                    v-model="conn.token"
                    class="conn-input"
                    type="password"
                    :placeholder="t('connTokenPlaceholder')"
                  />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('connStatus') }}</span>
                  <span class="srow-desc">{{ connDetail || t('connStatusUnknown') }}</span>
                </div>
                <div class="srow-ctl">
                  <span class="conn-badge" :class="connState">
                    <component
                      :is="connState === 'ok' ? LuCheck : connState === 'fail' ? LuX : LuLoader"
                      :size="12"
                    />
                    {{
                      connState === 'ok'
                        ? t('connOk')
                        : connState === 'fail'
                          ? t('connFailed')
                          : t('connUnknown')
                    }}
                  </span>
                </div>
              </div>
              <p v-if="!clientConfigReady" class="conn-note">{{ t('connLocalOnly') }}</p>
            </div>
          </div>
          <footer class="pane-foot">
            <UiButton variant="ghost" tone="neutral" size="sm" :loading="connTesting" @click="testConnection">
              {{ t('connTest') }}
            </UiButton>
            <UiButton variant="solid" tone="accent" size="sm" :loading="connTesting" @click="saveConnection">
              {{ t('connSave') }}
            </UiButton>
          </footer>
        </section>

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
                  <UiSegmented
                    :model-value="mode"
                    :options="modeOptions"
                    @update:model-value="(v) => (mode = v as typeof mode)"
                  />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('themeRowLight') }}</span>
                  <span class="srow-desc">{{ t('themeDesc') }}</span>
                </div>
                <div class="srow-ctl">
                  <UiSelect
                    :model-value="themeSel.light"
                    :options="lightPaletteOptions"
                    style="width: 200px"
                    @update:model-value="(v) => (themeSel.light = String(v ?? ''))"
                  />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('themeRowDark') }}</span>
                </div>
                <div class="srow-ctl">
                  <UiSelect
                    :model-value="themeSel.dark"
                    :options="darkPaletteOptions"
                    style="width: 200px"
                    @update:model-value="(v) => (themeSel.dark = String(v ?? ''))"
                  />
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('accent') }}</span>
                </div>
                <div class="srow-ctl">
                  <div class="dots">
                    <UiTooltip
                      v-for="(a, i) in ACCENTS"
                      :key="a"
                      :content="t(`accentNames.${a}`)"
                      :align="i % 7 === 0 ? 'start' : i % 7 === 6 ? 'end' : 'center'"
                    >
                      <UiIconButton
                        class="dot"
                        round="circle"
                        size="sm"
                        :label="t(`accentNames.${a}`)"
                        :class="{ active: a === accent }"
                        :style="{ '--dot-color': `var(--${a})` }"
                        @click="accent = a"
                      />
                    </UiTooltip>
                  </div>
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('manageThemes') }}</span>
                  <span class="srow-desc">{{ t('manageThemesDesc') }}</span>
                </div>
                <div class="srow-ctl">
                  <UiButton variant="soft" tone="neutral" size="sm" @click="newPalette">{{ t('newTheme') }}</UiButton>
                </div>
              </div>
              <div v-for="p in palettes" :key="p.id" class="srow">
                <div class="srow-main">
                  <span class="srow-title">
                    {{ p.name }}
                    <span class="scope-tag">{{ p.mode === 'light' ? t('modeLight') : t('modeDark') }}</span>
                    <span v-if="p.builtin" class="scope-tag">{{ t('paletteBuiltin') }}</span>
                  </span>
                  <span class="srow-desc">{{ t('paletteTokens', { count: PALETTE_TOKENS.length }) }}</span>
                </div>
                <div class="srow-ctl">
                  <span class="swatches" aria-hidden="true">
                    <span v-for="tok in ['base', 'text', 'blue', 'mauve']" :key="tok" :style="{ background: p[tok] }" />
                  </span>
                  <UiButton variant="ghost" tone="neutral" size="sm" @click="editPalette(p)">{{ t('edit') }}</UiButton>
                  <UiTooltip :content="p.builtin ? t('paletteBuiltinLocked') : tc('delete')">
                    <span>
                      <UiButton
                        variant="ghost"
                        tone="neutral"
                        size="sm"
                        :disabled="p.builtin"
                        @click="removePalette(p.id)"
                      >
                        {{ tc('delete') }}
                      </UiButton>
                    </span>
                  </UiTooltip>
                </div>
              </div>
            </div>
          </div>
          <footer class="pane-foot">
            <UiButton variant="solid" tone="accent" size="sm" :loading="savingTheme" @click="applyTheme">
              {{ t('saveTheme') }}
            </UiButton>
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
                  <UiSelect
                    :model-value="settingStore.locale"
                    :options="localeOptions"
                    style="width: 160px"
                    @update:model-value="(v) => pickLocale(v as typeof settingStore.locale)"
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
                  <UiSelect
                    :model-value="defaults.model"
                    :groups="modelGroups"
                    style="width: 300px"
                    @update:model-value="(v) => (defaults.model = String(v ?? ''))"
                  >
                    <template v-if="modelLabel" #value>{{ modelLabel }}</template>
                  </UiSelect>
                </div>
              </div>
              <div class="srow">
                <div class="srow-main">
                  <span class="srow-title">{{ t('defaultReasoning') }}</span>
                  <span class="srow-desc">{{ t('defaultReasoningDesc') }}</span>
                </div>
                <div class="srow-ctl">
                  <UiSelect
                    :model-value="defaults.reasoning"
                    :options="effortOptions"
                    style="width: 200px"
                    @update:model-value="(v) => (defaults.reasoning = String(v ?? ''))"
                  />
                </div>
              </div>
            </div>
          </div>
          <footer class="pane-foot">
            <UiButton variant="solid" tone="accent" size="sm" :loading="savingDefaults" @click="saveDefaults">
              {{ t('saveDefaults') }}
            </UiButton>
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
                <UiButton
                  v-for="d in providerDrafts"
                  :key="d.uid"
                  variant="ghost"
                  tone="neutral"
                  class="tab"
                  :class="{ active: d.uid === activeProviderUid }"
                  @click="activeProviderUid = d.uid"
                  >
                  {{ d.name || t('providerName') }}
                  </UiButton>
              </div>
              <div class="tabs-actions">
                <UiButton variant="soft" tone="neutral" size="sm" @click="addProvider">
                  <template #prefix><LuPlus :size="13" /></template>
                  {{ t('add') }}
                </UiButton>
                <UiButton variant="ghost" tone="neutral"
                  size="sm"
                 
                  :disabled="!activeDraft"
                  @click="removeActiveProvider"
                >
                  <template #prefix><LuTrash2 :size="13" /></template>
                  {{ tc('delete') }}
                </UiButton>
              </div>
            </div>

            <div v-for="(d, pi) in activeDraft ? [activeDraft] : []" :key="d.uid" class="prov">
              <!-- 提供商配置：单个容器，标题与配置项同在其中 -->
              <section class="card cfg-block">
                <header class="blk-head">
                  <span class="blk-title">{{ t('providerConfig') }}</span>
                </header>
                <div class="cfg-rows">
                  <label class="cfg-label">{{ t('providerName') }}</label>
                  <div class="cfg-ctl">
                    <UiInput v-model="d.name" class="mono" :placeholder="t('providerName')" />
                  </div>

                  <label class="cfg-label">{{ t('fRequestFormat') }}</label>
                  <div class="cfg-ctl">
                    <UiSelect
                      :model-value="d.api_type"
                      :options="apiTypeOptions"
                      @update:model-value="(v) => (d.api_type = String(v ?? ''))"
                    />
                  </div>

                  <label class="cfg-label">{{ t('fBaseUrl') }}</label>
                  <div class="cfg-ctl">
                    <UiInput v-model="d.base_url" />
                  </div>

                  <label class="cfg-label">{{ t('fApiKey') }}</label>
                  <div class="cfg-ctl">
                    <UiInput
                      :model-value="d.api_key"
                      :type="keyRevealed[d.uid] ? 'text' : 'password'"
                      :placeholder="d.keyMasked ? '*'.repeat(d.keyLen) : ''"
                      @update:model-value="(v) => setApiKey(d, v)"
                    />
                    <UiTooltip :content="keyRevealed[d.uid] ? t('hideKey') : t('showKey')" align="end">
                      <UiIconButton
                        class="m-del"
                        size="sm"
                        :label="keyRevealed[d.uid] ? t('hideKey') : t('showKey')"
                        @click="toggleKeyVisibility(d)"
                      >
                        <LuEyeOff v-if="keyRevealed[d.uid]" :size="14" />
                        <LuEye v-else :size="14" />
                      </UiIconButton>
                    </UiTooltip>
                  </div>

                  <!-- 请求头/请求体覆写：默认收起，展开后内容与标签顶格对齐 -->
                  <label class="cfg-label">{{ t('requestOverride') }}</label>
                  <div class="cfg-ctl cfg-stack">
                    <UiButton
                      variant="ghost"
                      tone="neutral"
                      block
                      class="cfg-fold"
                      v-bind="{ 'aria-expanded': d.requestOpen }"
                      @click="d.requestOpen = !d.requestOpen"
                      >
                      <span>{{ t('requestOverrideToggle') }}</span>
                      <LuChevronRight :size="13" class="caret" :class="{ open: d.requestOpen }" />
                      </UiButton>
                    <template v-if="d.requestOpen">
                      <p class="cfg-hint">{{ t('requestOverrideHint') }}</p>
                      <label class="cfg-sub-label">{{ t('requestHeaders') }}</label>
                      <UiTextarea
                        v-model="d.headersText"
                        :rows="5"
                        placeholder='{ "X-Header": "value" }'
                        />
                      <label class="cfg-sub-label">{{ t('requestBody') }}</label>
                      <UiTextarea
                        v-model="d.bodyText"
                        :rows="5"
                        placeholder='{ "temperature": 0.2 }'
                        />
                    </template>
                  </div>
                </div>
              </section>

              <!-- 模型配置：标题独占一行，每个模型各自一个区域 -->
              <header class="sec-head">
                <span class="blk-title">{{ t('modelConfig') }}</span>
                <UiButton variant="ghost" tone="neutral" size="sm" @click="addModel(d)">
                  <template #prefix><LuPlus :size="13" /></template>
                  {{ t('add') }}
                </UiButton>
              </header>
              <p v-if="d.models.length === 0" class="muted">{{ t('modelsNone') }}</p>

              <section v-for="(m, mi) in d.models" :key="mi" class="card cfg-block model-block">
                <header class="blk-head">
                  <span class="blk-sub">{{ m.name || m.id || t('modelIndex', { index: mi + 1 }) }}</span>
                  <UiTooltip :content="t('removeModel')" align="end">
                    <UiIconButton class="m-del" size="sm" :label="t('removeModel')" @click="d.models.splice(mi, 1)">
                      <LuTrash2 :size="14" />
                    </UiIconButton>
                  </UiTooltip>
                </header>
                <div class="cfg-rows">
                  <label class="cfg-label">{{ t('modelId') }}</label>
                  <div class="cfg-ctl">
                    <UiInput v-model="m.id" class="mono" placeholder="model-id" />
                  </div>

                  <label class="cfg-label">{{ t('modelName') }}</label>
                  <div class="cfg-ctl">
                    <UiInput v-model="m.name" />
                  </div>

                  <label class="cfg-label">{{ t('contextLen') }}</label>
                  <div class="cfg-ctl">
                    <UiInput v-model="m.context_len" />
                  </div>

                  <label class="cfg-label">{{ t('maxOutput') }}</label>
                  <div class="cfg-ctl">
                    <UiInput v-model="m.max_output" :placeholder="t('unset')" />
                  </div>

                  <label class="cfg-label">{{ t('capabilities') }}</label>
                  <div class="cfg-ctl">
                    <UiMultiSelect
                      :model-value="m.capabilities"
                      :options="capabilityOptions"
                      @update:model-value="(v) => (m.capabilities = v.map(String) as typeof m.capabilities)"
                    />
                  </div>

                  <!-- 推理映射仅在模型支持思考时才有意义；默认收起 -->
                  <template v-if="m.capabilities.includes('thinking')">
                    <label class="cfg-label">{{ t('reasoningMap') }}</label>
                    <div class="cfg-ctl cfg-stack">
                      <UiButton
                        variant="ghost"
                        tone="neutral"
                        block
                        class="cfg-fold"
                        v-bind="{ 'aria-expanded': m.reasoningOpen }"
                        @click="m.reasoningOpen = !m.reasoningOpen"
                        >
                        <span>{{ t('reasoningMapToggle') }}</span>
                        <LuChevronRight :size="13" class="caret" :class="{ open: m.reasoningOpen }" />
                        </UiButton>
                      <template v-if="m.reasoningOpen">
                        <p class="cfg-hint">{{ t('reasoningMapHint') }}</p>
                        <div class="effort-map">
                          <div v-for="lv in REASONING_LEVELS" :key="lv" class="effort-row">
                            <span class="effort-key mono">{{ lv }}</span>
                            <UiInput
                              :model-value="m.reasoning_map[lv] ?? ''"
                              :placeholder="lv"
                              @update:model-value="(v) => (m.reasoning_map[lv] = v)"
                            />
                          </div>
                        </div>
                      </template>
                    </div>
                  </template>

                  <!-- 模型级请求头/请求体覆写：默认收起 -->
                  <label class="cfg-label">{{ t('requestOverride') }}</label>
                  <div class="cfg-ctl cfg-stack">
                    <UiButton
                      variant="ghost"
                      tone="neutral"
                      block
                      class="cfg-fold"
                      v-bind="{ 'aria-expanded': m.requestOpen }"
                      @click="m.requestOpen = !m.requestOpen"
                      >
                      <span>{{ t('requestOverrideToggle') }}</span>
                      <LuChevronRight :size="13" class="caret" :class="{ open: m.requestOpen }" />
                      </UiButton>
                    <template v-if="m.requestOpen">
                      <p class="cfg-hint">{{ t('requestOverrideHint') }}</p>
                      <label class="cfg-sub-label">{{ t('requestHeaders') }}</label>
                      <UiTextarea
                        v-model="m.headersText"
                        :rows="5"
                        placeholder='{ "X-Header": "value" }'
                        />
                      <label class="cfg-sub-label">{{ t('requestBody') }}</label>
                      <UiTextarea
                        v-model="m.bodyText"
                        :rows="5"
                        placeholder='{ "temperature": 0.2 }'
                        />
                    </template>
                  </div>
                </div>
              </section>
            </div>

            <p v-if="providerDrafts.length === 0" class="muted">{{ t('providersEmpty') }}</p>
          </div>
          <footer class="pane-foot">
            <UiButton variant="solid" tone="accent" size="sm" :loading="savingProviders" @click="saveProviders">
              {{ t('saveAll') }}
            </UiButton>
          </footer>
        </section>


        <!-- 技能（按来源层分标签页） -->
        <section v-else-if="section === 'skills'" class="pane">
          <header class="pane-head">
            <div class="tabs">
              <UiButton
                v-for="l in skillScopeOptions"
                :key="l.value"
                variant="ghost"
                tone="neutral"
                class="tab"
                :class="{ active: skillLayer === l.value }"
                @click="skillLayer = l.value"
                >
                {{ l.label }}
                </UiButton>
            </div>
            <div class="pane-head-actions">
              <UiButton variant="soft" tone="neutral" size="sm" @click="newSkill">{{ t('add') }}</UiButton>
            </div>
          </header>
          <div class="pane-scroll">
            <p class="muted">{{ t('skillsHint') }}</p>
            <p v-if="skillLayer === 'project' && !skillWorkspace" class="muted">{{ t('skillNoWorkspace') }}</p>
            <div class="list">
              <div
                v-for="sk in visibleSkills"
                :key="sk.scope + '/' + sk.id"
                class="srow skill-row"
                role="button"
                @click="editSkill(sk)"
              >
                <div class="srow-main">
                  <span class="srow-title">
                    {{ sk.name }}
                    <span class="scope-tag" :class="'scope-' + sk.scope">{{ scopeLabel(sk.scope) }}</span>
                  </span>
                  <span class="srow-desc">{{ sk.description || sk.id }}</span>
                </div>
              </div>
              <div v-if="visibleSkills.length === 0 && !skillsLoading" class="srow">
                <span class="srow-desc">{{ t('skillsEmpty') }}</span>
              </div>
            </div>
          </div>
        </section>


        <section v-else-if="section === 'defaults' || section === 'providers'" class="pane">
          <div class="pane-scroll">
            <p class="muted">{{ t('configUnavailable') }}</p>
          </div>
        </section>
      </div>
    </div>
  </UiModal>


  <UiModal
    :open="skillEdit.open"
    :title="skillEdit.isNew ? t('newSkill') : t('editSkill')"
    width="640px"
    @close="skillEdit.open = false"
  >
    <div class="skill-form">
      <div class="sk-grid">
        <div class="field">
          <label>{{ t('skillId') }}</label>
          <UiInput v-model="skillEdit.id" :disabled="!skillEdit.isNew" placeholder="my-skill" />
        </div>
        <div class="field">
          <label>{{ t('scopeLabel') }}</label>
          <UiSelect
          :model-value="skillEdit.scope"
          :options="skillScopeOptions"
          :disabled="!skillEdit.isNew"
          @update:model-value="(v) => (skillEdit.scope = v as typeof skillEdit.scope)"
        />
        </div>
      </div>
      <div class="field">
        <label>{{ t('modelName') }}</label>
        <UiInput v-model="skillEdit.name" />
      </div>
      <div class="field">
        <label>{{ t('skillDesc') }}</label>
        <UiInput v-model="skillEdit.description" />
      </div>
      <div class="field">
        <label>{{ t('skillContent') }}</label>
        <div class="skill-content-box">
          <UiTextarea v-model="skillEdit.body" :rows="12" :disabled="skillEdit.readonly" class="skill-content-input" />
        </div>
        <p v-if="skillEdit.readonly" class="muted">{{ t('skillReadonly') }}</p>
      </div>
    </div>
    <template #footer>
      <UiButton variant="solid" tone="danger"
        v-if="!skillEdit.isNew && !skillEdit.readonly"
       
        size="sm"
        @click="removeSkill"
      >
        {{ tc('delete') }}
      </UiButton>
      <UiButton variant="ghost" tone="neutral" size="sm" @click="skillEdit.open = false">{{ tc('cancel') }}</UiButton>
      <UiButton variant="solid" tone="accent" size="sm" :disabled="skillEdit.readonly || !skillEdit.id.trim() || (skillEdit.scope === 'project' && !skillWorkspace)" @click="saveSkill">
        {{ tc('save') }}
      </UiButton>
    </template>
  </UiModal>

  <UiModal
    :open="paletteEdit.open"
    :title="paletteEdit.isNew ? t('newTheme') : t('editTheme')"
    width="620px"
    @close="paletteEdit.open = false"
  >
    <div class="skill-form">
      <div class="sk-grid">
        <div class="field">
          <label>{{ t('themeName') }}</label>
          <UiInput v-model="paletteEdit.name" placeholder="Nord Dark" />
        </div>
        <div class="field">
          <label>{{ t('themeModeLabel') }}</label>
          <UiSegmented
            :model-value="paletteEdit.mode"
            :options="[{ value: 'light', label: t('modeLight') }, { value: 'dark', label: t('modeDark') }]"
            :disabled="!paletteEdit.isNew"
            @update:model-value="(v) => newPaletteMode(v as typeof paletteEdit.mode)"
          />
        </div>
      </div>
      <div class="field">
        <label>{{ t('themeBaseLabel') }}</label>
        <UiSelect
          :model-value="paletteEdit.base"
          :options="paletteBaseOptions"
          style="width: 100%"
          @update:model-value="(v) => applyBase(String(v ?? ''))"
        />
        <span class="field-hint">{{ t('paletteBaseHint') }}</span>
      </div>
      <div class="field">
        <label>{{ t('paletteNeutralColors') }}</label>
        <div class="color-grid">
          <div v-for="tok in NEUTRAL_TOKENS" :key="tok" class="color-cell">
            <span class="color-swatch" :style="{ background: paletteEdit.values[tok] }" />
            <span class="color-name">{{ tok }}</span>
            <UiInput v-model="paletteEdit.values[tok]" class="color-input" placeholder="#89b4fa" />
          </div>
        </div>
      </div>
      <div class="field">
        <label>{{ t('paletteAccentColors') }}</label>
        <div class="color-grid">
          <div v-for="tok in ACCENTS" :key="tok" class="color-cell">
            <span class="color-swatch" :style="{ background: paletteEdit.values[tok] }" />
            <span class="color-name">{{ tok }}</span>
            <UiInput v-model="paletteEdit.values[tok]" class="color-input" placeholder="#89b4fa" />
          </div>
        </div>
      </div>
    </div>
    <template #footer>
      <UiButton variant="ghost" tone="neutral" size="sm" @click="applyBase(paletteEdit.base)">{{ t('themeReset') }}</UiButton>
      <UiButton variant="ghost" tone="neutral" size="sm" @click="paletteEdit.open = false">{{ tc('cancel') }}</UiButton>
      <UiButton variant="solid" tone="accent" size="sm" @click="savePaletteEdit">{{ tc('save') }}</UiButton>
    </template>
  </UiModal>

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
  height: auto;
  justify-content: flex-start;
  cursor: pointer;
  transition:
    background-color 0.12s ease,
    color 0.12s ease;
}
.nav-item :deep(.ui-button__label) {
  display: inline-flex;
  align-items: center;
  gap: 8px;
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
/* 与「外观 / 技能」所用的 UiSegmented 分段控件保持同一视觉语言：
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
  height: auto;
  transition:
    background-color 0.15s ease,
    color 0.15s ease;
}
.tab :deep(.ui-button__label) {
  display: inline-flex;
  align-items: center;
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
  align-items: center;
  justify-content: flex-end;
}
.srow-ctl.wide {
  flex-shrink: 1;
}
/* 连接页输入框：地址与 token 都需要足够宽度展示，窄屏时允许收缩 */
.conn-input {
  width: 300px;
  min-width: 0;
  flex-shrink: 1;
}
.conn-badge {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  padding: 3px 9px;
  border-radius: 99px;
  font-size: 11.5px;
  font-weight: 500;
  background: var(--surface-strong);
  color: var(--text-tertiary);
}
.conn-badge.ok {
  background: var(--success-soft);
  color: var(--success);
}
.conn-badge.fail {
  background: var(--danger-soft);
  color: var(--danger);
}
/* 已保存连接列表：与下方表单单列排列，活动项带勾选标记 */
.conn-list {
  margin-bottom: 12px;
}
.conn-item {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 8px 11px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--paper);
  cursor: pointer;
  transition:
    border-color 0.12s ease,
    background-color 0.12s ease;
}
.conn-item:hover {
  border-color: var(--overlay0);
}
.conn-item.on {
  border-color: var(--accent);
  background: color-mix(in srgb, var(--accent) 8%, transparent);
}
.conn-mark {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  flex-shrink: 0;
  width: 16px;
  height: 16px;
  border: 1.5px solid var(--control-border);
  border-radius: 99px;
  color: var(--base);
}
.conn-item.on .conn-mark {
  border-color: var(--accent);
  background: var(--accent);
}
.conn-main {
  display: flex;
  flex-direction: column;
  gap: 1px;
  flex: 1;
  min-width: 0;
}
.conn-name {
  font-size: 13px;
  font-weight: 500;
  color: var(--ink);
}
.conn-url {
  font-size: 11.5px;
  color: var(--text-tertiary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.conn-del {
  flex-shrink: 0;
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
}
.conn-del:hover {
  background: var(--danger-soft);
  color: var(--danger);
}
.conn-empty,
.conn-note {
  margin: 0;
  font-size: 12.5px;
  color: var(--text-tertiary);
}
.conn-empty {
  padding: 8px 2px;
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
.color-name {
  /* 令牌名定宽：长短不一的名字会让后面的色值输入框左右错位 */
  width: 62px;
  flex-shrink: 0;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--overlay1);
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
}
.field-hint {
  margin-top: 4px;
  font-size: 11.5px;
  color: var(--overlay0);
}
/* 调色板列表里的迷你色卡：一眼看出配色走向，不占太多横向空间 */
.swatches {
  display: inline-flex;
  gap: 3px;
  margin-right: 4px;
}
.swatches > span {
  width: 12px;
  height: 12px;
  border-radius: 4px;
  border: 1px solid var(--line);
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
.skill-content-box :deep(textarea) {
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
/* 强调色点阵：两行各 7 个，整齐排列；悬停经 UiTooltip 显示名称 */
.dots {
  display: grid;
  grid-template-columns: repeat(7, 18px);
  gap: 9px 10px;
  justify-content: start;
}
.dot {
  width: 16px !important;
  height: 16px !important;
  min-width: 0 !important;
  padding: 0;
  border: none;
  border-radius: 99px !important;
  /* 用独立变量名：--dot 与类名同名容易被误当作类选择器，
     也会和调色板令牌的命名空间混在一起 */
  background: var(--dot-color) !important;
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
    0 0 0 4px var(--dot-color);
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
/* 预设/技能编辑卡片的两列表单（左侧名称、右侧控件） */
.fields {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px 14px;
  margin-top: 10px;
}
/* 提供商卡片：仅作容器，具体区块由 .cfg-block 承担底色 */
.prov {
  margin-bottom: 10px;
}
/* 带底色的区块（提供商配置 / 每个模型）共用 */
.card {
  background: var(--surface);
  border-radius: 10px;
  padding: 12px 14px;
}
.cfg-block {
  margin-bottom: 10px;
}
.cfg-block:last-child {
  margin-bottom: 0;
}
/* 区块标题：与配置项同处一个容器内 */
.blk-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  min-height: 24px;
}
.blk-title {
  font-size: 11.5px;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  color: var(--overlay0);
}
.blk-sub {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
/* 分区标题（模型配置）在容器之外，统领其下的各个模型区块 */
.sec-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin: 16px 0 8px;
}
.cfg-rows {
  display: grid;
  grid-template-columns: 120px minmax(0, 1fr);
  /* 标签对齐控件首行而不是垂直居中：映射表、请求头等多行控件下，
     居中的标签会飘在中间，与内容脱节 */
  align-items: start;
  gap: 8px 12px;
  padding-top: 8px;
}
.cfg-label {
  padding-top: 8px;
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
.cfg-ctl > .o-select,
.cfg-ctl > .o-multi {
  flex: 1;
  min-width: 0;
}
/* 需纵向排布的配置项（如推理映射表） */
.cfg-ctl.cfg-stack {
  display: flex;
  flex-direction: column;
  align-items: stretch;
  gap: 6px;
}
/* 折叠区开关：整行可点，箭头靠右表示展开状态 */
.cfg-fold {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
  padding: 6px 10px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--surface);
  color: var(--text-secondary);
  font-family: inherit;
  font-size: 12.5px;
  text-align: left;
  height: auto;
  cursor: pointer;
  transition:
    border-color 0.15s ease,
    color 0.15s ease;
}
.cfg-fold :deep(.ui-button__label) {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
}
.cfg-fold:hover {
  border-color: var(--overlay0);
  color: var(--ink);
}
.cfg-fold .caret {
  flex-shrink: 0;
  color: var(--text-tertiary);
  transition: transform 0.15s ease;
}
.cfg-fold .caret.open {
  transform: rotate(90deg);
}
/* 折叠区内的子标签：同样顶格，与上方提示、下方输入框左边缘齐平 */
.cfg-sub-label {
  font-size: 11.5px;
  color: var(--text-tertiary);
}
/* 请求头/请求体文本域：默认即顶格换行，不引入额外内缩 */
.cfg-ctl.cfg-stack :deep(textarea) {
  width: 100%;
  padding: 8px 10px;
  border: 1px solid var(--control-border);
  border-radius: 8px;
  background: var(--surface);
  color: var(--ink);
  font-family: var(--font-mono);
  font-size: 12px;
  line-height: 1.55;
  resize: vertical;
  transition: border-color 0.15s ease;
}
.cfg-ctl.cfg-stack :deep(textarea:focus) {
  outline: none;
  border-color: var(--accent);
}
.cfg-hint {
  margin: 0;
  font-size: 11.5px;
  line-height: 1.5;
  color: var(--overlay0);
}
.effort-map {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.effort-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.effort-key {
  flex-shrink: 0;
  width: 62px;
  font-size: 11.5px;
  color: var(--text-tertiary);
}
/* 标识符类输入（提供商名、模型 id）用等宽字体 */
.mono :deep(input) {
  font-family: var(--font-mono);
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
.muted {
  font-size: 12.5px;
  color: var(--overlay0);
  margin: 0 0 10px;
}
</style>
