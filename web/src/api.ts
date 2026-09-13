import type {
  AgentFile,
  ChatMessage,
  OmaConfig,
  Palette,
  ServerStatus,
  SessionRecord,
  SkillFile,
  ToolInfo,
  UploadAttachmentResp,
} from './types';

/** 鉴权 Token：优先 URL ?token=，其次 localStorage。 */
const TOKEN_KEY = 'oma.token';

export function getToken(): string {
  const url = new URL(location.href);
  const fromUrl = url.searchParams.get('token');
  if (fromUrl) {
    localStorage.setItem(TOKEN_KEY, fromUrl);
    // 凭证不应长期停留在地址栏（会被历史记录、Referer 与日志留存）
    url.searchParams.delete('token');
    const query = url.searchParams.toString();
    history.replaceState(null, '', url.pathname + (query ? `?${query}` : '') + url.hash);
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

  /** 删除消息及其子树；返回删除的 ID 列表与新的当前叶子 */
  deleteMessage: (id: string, messageId: string) =>
    request<{ success: boolean; deleted: string[]; current_leaf_id: string | null }>(
      `/api/sessions/${id}/messages/${messageId}`,
      { method: 'DELETE' },
    ),
  /** 预设编辑器可勾选的工具（内置 + 已发现的 MCP） */
  tools: () => request<ToolInfo[]>('/api/tools'),

  presets: (workspace?: string) =>
    request<AgentFile[]>(
      `/api/presets${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ''}`,
    ),

  getPreset: (id: string, workspace?: string) =>
    request<AgentFile>(
      `/api/presets/${id}${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ''}`,
    ),

  putPreset: (
    id: string,
    body: { name: string; description: string; tools: string[]; content: string; scope: string },
    workspace?: string,
  ) =>
    request<{ success: boolean; path: string }>(
      `/api/presets/${id}${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ''}`,
      { method: 'PUT', body: JSON.stringify(body) },
    ),

  deletePreset: (id: string, scope: string, workspace?: string) => {
    const qs = new URLSearchParams({ scope });
    if (workspace) qs.set('workspace', workspace);
    return request<{ success: boolean }>(`/api/presets/${id}?${qs.toString()}`, {
      method: 'DELETE',
    });
  },

  skills: (workspace?: string) =>
    request<SkillFile[]>(
      `/api/skills${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ''}`,
    ),

  getSkill: (id: string, workspace?: string) =>
    request<SkillFile>(
      `/api/skills/${id}${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ''}`,
    ),

  putSkill: (
    id: string,
    body: { name: string; description: string; content: string; scope: string },
    workspace?: string,
  ) =>
    request<{ success: boolean; path: string }>(
      `/api/skills/${id}${workspace ? `?workspace=${encodeURIComponent(workspace)}` : ''}`,
      { method: 'PUT', body: JSON.stringify(body) },
    ),

  deleteSkill: (id: string, scope: string, workspace?: string) => {
    const qs = new URLSearchParams({ scope });
    if (workspace) qs.set('workspace', workspace);
    return request<{ success: boolean }>(
      `/api/skills/${id}?${qs.toString()}`,
      { method: 'DELETE' },
    );
  },
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

  /** 上传附件，返回可直接放进 user_input.attachments 的 session_attachment:// 引用。 */
  uploadAttachments: async (sessionId: string, files: File[]): Promise<string[]> => {
    const form = new FormData();
    for (const file of files) form.append('file', file, file.name);
    const resp = await fetch(
      `/api/sessions/${encodeURIComponent(sessionId)}/attachments`,
      {
        method: 'POST',
        headers: { Authorization: `Bearer ${getToken()}` },
        body: form,
      },
    );
    if (!resp.ok) {
      const detail = await resp.text().catch(() => '');
      throw new Error(detail || `HTTP ${resp.status}`);
    }
    return ((await resp.json()) as UploadAttachmentResp).attachments;
  },

  /** 取附件字节（需鉴权，故不能直接用 <img src>）。 */
  fetchAttachment: async (sessionId: string, name: string): Promise<Blob> => {
    const resp = await fetch(
      `/api/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(name)}`,
      { headers: { Authorization: `Bearer ${getToken()}` } },
    );
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    return resp.blob();
  },

  /** reveal=1 时服务端返回真实 api_key（默认脱敏为 "***" 并附 api_key_len） */
  getConfig: (opts?: { reveal?: boolean }) =>
    request<OmaConfig>('/api/config' + (opts?.reveal ? '?reveal=1' : '')),

  putConfig: (config: OmaConfig) =>
    request<{ success: boolean }>('/api/config', {
      method: 'PUT',
      body: JSON.stringify(config),
    }),

  /** 全部可用调色板（内置 + 用户 themes 目录），含 builtin 标记。 */
  palettes: () => request<Palette[]>('/api/palettes'),

  /** 写入用户调色板；id 为文件名，内置 id 会被服务端拒绝 (403)。 */
  putPalette: (palette: Palette) =>
    request<{ success: boolean }>(`/api/palettes/${encodeURIComponent(palette.id)}`, {
      method: 'PUT',
      body: JSON.stringify(palette),
    }),

  deletePalette: (id: string) =>
    request<{ success: boolean }>(`/api/palettes/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    }),
};
