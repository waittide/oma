import { ref } from 'vue';
import { clientApi } from '../api';
import { tr } from '../composables/i18n';
import { baseUrl, setConnection, token } from './connection';
import type { ClientConfig, ClientConnection } from '../types';

/**
 * 客户端本地配置（连接列表）。
 *
 * `oma web` 提供同源接口读写 <配置目录>/oma/client.json；页面不是 `oma web` 提供时
 * （如独立 vite 开发，接口 404）退到 localStorage 里的一份完整快照。两条路径下
 * 界面与按钮语义完全一致，差别只有「持久化到哪儿」。
 */

/** 无同源接口时的兜底持久化键：一份完整的 ClientConfig 快照 */
const LOCAL_KEY = 'oma.clientConfig';
/** 快照里的绑定地址：只有 `oma web` 启动时用得上，本地兜底仅为满足结构完整 */
const LOCAL_WEB = { host: '127.0.0.1', port: 5173 };

export const clientConfig = ref<ClientConfig | null>(null);
/**
 * 同源接口是否可用。
 *
 * 只决定两件事：往 client.json 还是 localStorage 持久化、以及是否提示
 * 「连接列表保存在本浏览器」；不再决定界面展示什么。
 */
export const clientConfigReady = ref(false);

/** 列表中的活动连接；找不到时回落首个。 */
export function activeConnection(cfg: ClientConfig): ClientConnection | undefined {
  return cfg.connections.find((c) => c.name === cfg.active) ?? cfg.connections[0];
}

/** 把配置中的活动连接应用到运行中的连接状态。 */
export function applyActiveConnection(cfg: ClientConfig) {
  const active = activeConnection(cfg);
  if (active) setConnection(active.url, active.token);
}

/** 读取兜底快照；键不存在或内容不可用时按「没有保存过」处理。 */
function readLocalConfig(): ClientConfig | null {
  try {
    const raw = localStorage.getItem(LOCAL_KEY);
    if (!raw) return null;
    const cfg = JSON.parse(raw) as ClientConfig;
    return Array.isArray(cfg?.connections) ? cfg : null;
  } catch {
    // 隐私模式下 localStorage 可能抛错，内容也可能被改坏：都按没有处理
    return null;
  }
}

/** 把这份配置落到本地后端并启用：写快照、更新缓存、应用活动连接。 */
function applyLocalConfig(cfg: ClientConfig) {
  try {
    localStorage.setItem(LOCAL_KEY, JSON.stringify(cfg));
  } catch {
    // 忽略写入失败：本次会话仍然生效，只是刷新后丢失
  }
  clientConfig.value = cfg;
  applyActiveConnection(cfg);
}

/**
 * 用现有的连接设置播出一条连接。
 *
 * 旧版本只把「当前这一条」存在 `oma.baseUrl` / `oma.token`；如果没有这份播种，
 * 升级后连接列表会是空的，用户会以为连接丢了。名字取本地化默认名（与表单里
 * 「名称」的占位一致）。
 */
function seedConfig(): ClientConfig {
  const name = tr('settings.connDefaultName');
  return {
    web: { ...LOCAL_WEB },
    connections: [{ name, url: baseUrl.value, token: token.value }],
    active: name,
  };
}

/**
 * 启动时读取配置并应用其中的活动连接。
 *
 * 同源接口可用时读 client.json；否则读 localStorage 快照，首次没有快照就用当前
 * 连接播种，之后两条路径共用同一套读写逻辑。
 */
export async function initClientConfig(): Promise<void> {
  try {
    const cfg = await clientApi.getConfig();
    clientConfig.value = cfg;
    clientConfigReady.value = true;
    applyActiveConnection(cfg);
    return;
  } catch {
    // 没有同源接口：退到 localStorage 兜底
  }
  const cfg = readLocalConfig() ?? seedConfig();
  applyLocalConfig(cfg);
}

/**
 * 保存整份配置。
 *
 * 接口可用时 PUT 回 client.json（服务端校验并回读规范化结果）；否则写
 * localStorage，并把它当作唯一事实来源直接应用到运行中的连接——没有服务端
 * 可切换，本地这份就是当前连接。
 */
export async function saveClientConfig(cfg: ClientConfig): Promise<void> {
  if (clientConfigReady.value) {
    clientConfig.value = await clientApi.putConfig(cfg);
    return;
  }
  applyLocalConfig(cfg);
}
