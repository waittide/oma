import type {
  ChatMessage,
  OmaConfig,
  ServerStatus,
  SessionRecord,
} from './types';

/** 鉴权 Token：优先 URL ?token=，其次 localStorage。 */
const TOKEN_KEY = 'oma.token';

export function getToken(): string {
  const fromUrl = new URLSearchParams(location.search).get('token');
  if (fromUrl) {
    localStorage.setItem(TOKEN_KEY, fromUrl);
    return fromUrl;
  }
  return localStorage.getItem(TOKEN_KEY) ?? '';
}

export function wsUrl(sessionQuery: Record<string, string>): string {
  const proto = location.protocol === 'https:' ? 'wss' : 'ws';
  const params = new URLSearchParams({ token: getToken(), ...sessionQuery });
  return `${proto}://${location.host}/ws?${params.toString()}`;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  headers.set('Authorization', `Bearer ${getToken()}`);
  if (init?.body) headers.set('Content-Type', 'application/json');

  const resp = await fetch(path, { ...init, headers });
  if (!resp.ok) {
    const detail = await resp.text().catch(() => '');
    throw new Error(detail || `HTTP ${resp.status}`);
  }
  if (resp.status === 204) return undefined as T;
  return (await resp.json()) as T;
}

export const api = {
  status: () => request<ServerStatus>('/api/server/status'),

  listSessions: (workspace?: string) =>
    request<SessionRecord[]>(
      workspace
        ? `/api/sessions?workspace=${encodeURIComponent(workspace)}`
        : '/api/sessions',
    ),

  createSession: (payload: {
    workspace: string;
    title?: string;
    model?: string;
    agent?: string;
  }) =>
    request<{ session_id: string; session: SessionRecord }>('/api/sessions', {
      method: 'POST',
      body: JSON.stringify(payload),
    }),

  deleteSession: (id: string) =>
    request<{ success: boolean }>(`/api/sessions/${id}`, { method: 'DELETE' }),

  renameSession: (id: string, title: string) =>
    request<{ success: boolean }>(`/api/sessions/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ title }),
    }),

  messageTree: (id: string) => request<ChatMessage[]>(`/api/sessions/${id}/messages/tree`),

  messages: (id: string, leafId?: string | null) => {
    const q = leafId ? `?leaf_id=${encodeURIComponent(leafId)}` : '';
    return request<ChatMessage[]>(`/api/sessions/${id}/messages${q}`);
  },

  workspaceTree: (workspace: string) =>
    request<import('./types').FileNode>(
      `/api/workspace/tree?workspace=${encodeURIComponent(workspace)}`,
    ),

  workspaceFile: (workspace: string, path: string) =>
    request<{ content: string }>(
      `/api/workspace/file?workspace=${encodeURIComponent(workspace)}&path=${encodeURIComponent(path)}`,
    ),

  getConfig: () => request<OmaConfig>('/api/config'),

  putConfig: (config: OmaConfig) =>
    request<{ success: boolean }>('/api/config', {
      method: 'PUT',
      body: JSON.stringify(config),
    }),
};
