import { ref } from 'vue';

/**
 * 连接配置：前端是独立静态服务，可以指向任意 Daemon，因此需要显式记录目标地址与凭证。
 *
 * 与主题/配置不同，这些信息属于「这台浏览器自己的设置」，保存在 localStorage，
 * 不随 `GET /api/config` 走：多台机器各自连不同 Daemon 是正常用法。
 */
const BASE_KEY = 'oma.baseUrl';
const TOKEN_KEY = 'oma.token';

/** 默认指向 Daemon 的默认监听地址：`oma web` 只提供静态界面，
 * 同源没有 API，因此留空会在首次打开时直接报错。 */
const DEFAULT_BASE_URL = 'http://127.0.0.1:17431';

/** 从 URL 的 ?token= 取凭证后立即从地址栏抹掉（历史记录/Referer 都会留存）。 */
function takeTokenFromUrl(): string | null {
  if (typeof location === 'undefined') return null;
  const url = new URL(location.href);
  const fromUrl = url.searchParams.get('token');
  if (!fromUrl) return null;
  url.searchParams.delete('token');
  const query = url.searchParams.toString();
  history.replaceState(null, '', url.pathname + (query ? `?${query}` : '') + url.hash);
  return fromUrl;
}

function readStorage(key: string): string {
  try {
    return localStorage.getItem(key) ?? '';
  } catch {
    // 隐私模式下 localStorage 可能直接抛错：按未配置处理
    return '';
  }
}

function writeStorage(key: string, value: string) {
  try {
    if (value) localStorage.setItem(key, value);
    else localStorage.removeItem(key);
  } catch {
    // 忽略写入失败：本次会话仍然生效，只是刷新后丢失
  }
}

/** 访问地址：缺省指向 Daemon 默认地址；清空表示与页面同源（反向代理场景）。 */
export const baseUrl = ref(readStorage(BASE_KEY) || DEFAULT_BASE_URL);
export const token = ref(readStorage(TOKEN_KEY));

// 首次加载：URL 里的 token 优先，视为用户显式指定
const urlToken = takeTokenFromUrl();
if (urlToken) {
  token.value = urlToken;
  writeStorage(TOKEN_KEY, urlToken);
}

/** 规范化用户输入的地址：允许省略协议与结尾斜杠。 */
export function normalizeBaseUrl(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) return '';
  const withProto = /^https?:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;
  return withProto.replace(/\/+$/, '');
}

/**
 * 把相对路径解析为绝对 URL。
 *
 * `fetch`/`WebSocket` 都需要绝对地址才能跨源；地址为空时回落到当前页面同源。
 */
export function resolveUrl(path: string): string {
  const base = normalizeBaseUrl(baseUrl.value);
  return base ? `${base}${path}` : path;
}

/** WebSocket 地址：由 baseUrl 推导 http(s) → ws(s)。 */
export function resolveWsUrl(path: string): string {
  const base = normalizeBaseUrl(baseUrl.value);
  if (!base) {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    return `${proto}://${location.host}${path}`;
  }
  return base.replace(/^http/i, 'ws') + path;
}

export function setConnection(nextBaseUrl: string, nextToken: string) {
  baseUrl.value = normalizeBaseUrl(nextBaseUrl);
  token.value = nextToken.trim();
  writeStorage(BASE_KEY, baseUrl.value);
  writeStorage(TOKEN_KEY, token.value);
}

/** 当前连接是否已具备可用凭证。 */
export function hasToken(): boolean {
  return token.value.length > 0;
}
