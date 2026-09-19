import { ref } from 'vue';
import { clientApi } from '../api';
import { setConnection } from './connection';
import type { ClientConfig, ClientConnection } from '../types';

/**
 * 客户端本地配置（<配置目录>/oma/client.json）。
 *
 * 由承载页面的 `oma web` 提供同源接口读写；连接列表是用户在客户端保存、
 * 可切换的目标 Daemon，绑定地址与端口则只在 `oma web` 启动时使用。
 */

export const clientConfig = ref<ClientConfig | null>(null);
/** 同源接口是否可用：vite dev（无 web 服务）下不可用，退回 localStorage。 */
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

/**
 * 启动时读取 client.json 并应用其中的活动连接。
 *
 * 接口不可用（独立 vite 开发）时保持 localStorage 中的连接设置不动，
 * 保证开发模式与旧行为一致。
 */
export async function initClientConfig(): Promise<void> {
  try {
    const cfg = await clientApi.getConfig();
    clientConfig.value = cfg;
    clientConfigReady.value = true;
    applyActiveConnection(cfg);
  } catch {
    // 没有同源接口：保留 localStorage 里的连接设置
  }
}

/** 保存整份客户端配置；成功后同步缓存。 */
export async function saveClientConfig(cfg: ClientConfig): Promise<void> {
  clientConfig.value = await clientApi.putConfig(cfg);
}
