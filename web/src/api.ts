import { hasToken, resolveUrl, resolveWsUrl, token } from './stores/connection';
import type {
  AgentFile,
  ChatMessage,
  ClientConfig,
  FileNode,
  GitStatusResp,
  OmaConfig,
  Palette,
  ServerStatus,
  SessionRecord,
  SkillFile,
  SystemPromptResp,
  ToolInfo,
  UploadAttachmentResp,
} from './types';

/**
 * 同源请求：只用于 `oma web` 自身提供的客户端配置接口。
 *
 * 不能走 [`resolveUrl`]——它会把路径拼到「访问地址」（Daemon）上，
 * 而 /api/client/config 属于承载页面的 web 服务，与 Daemon 无关。
 */
async function localRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  if (init?.body) headers.set('Content-Type', 'application/json');
  const resp = await fetch(path, { ...init, headers });
  if (!resp.ok) {
    const detail = await resp.text().catch(() => '');
    throw new Error(detail || `HTTP ${resp.status}`);
  }
  if (resp.status === 204) return undefined as T;
  return (await resp.json()) as T;
}

/** 客户端本地配置（client.json）：仅 `oma web` 页面可用。 */
export const clientApi = {
  getConfig: () => localRequest<ClientConfig>('/api/client/config'),
  putConfig: (config: ClientConfig) =>
    localRequest<ClientConfig>('/api/client/config', {
      method: 'PUT',
      body: JSON.stringify(config),
    }),
};

/** 鉴权 Token：由连接配置提供（URL ?token= 优先，其次 localStorage）。 */
export function getToken(): string {
  return token.value;
}

export function wsUrl(sessionQuery: Record<string, string>): string {
  const params = new URLSearchParams({ token: getToken(), ...sessionQuery });
  return resolveWsUrl(`/ws?${params.toString()}`);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  if (hasToken()) headers.set('Authorization', `Bearer ${getToken()}`);
  if (init?.body) headers.set('Content-Type', 'application/json');

  const resp = await fetch(resolveUrl(path), { ...init, headers });
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

  /**
   * 真正下发的系统提示词（预设正文 + 环境块 + 项目上下文 + 技能目录）。
   *
   * 提示词由服务端拼装，客户端看不到对应的源文件；`model` 只影响环境块里的
   * `Active Model` 一行，`agent` 缺省用配置里的默认预设。
   */
  systemPrompt: (workspace: string, model?: string, agent?: string) => {
    const qs = new URLSearchParams({ workspace });
    if (model) qs.set('model', model);
    if (agent) qs.set('agent', agent);
    return request<SystemPromptResp>(`/api/system-prompt?${qs.toString()}`);
  },

  /** 工作区 git 变更（分支 + 文件清单） */
  gitStatus: (workspace: string) =>
    request<GitStatusResp>(`/api/git/status?workspace=${encodeURIComponent(workspace)}`),

  /** 单个文件相对 HEAD 的 diff（未跟踪文件按整篇新增展示） */
  gitDiff: (workspace: string, path: string) =>
    request<{ diff: string }>(
      `/api/git/diff?workspace=${encodeURIComponent(workspace)}&path=${encodeURIComponent(path)}`,
    ),

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

  /** 列目录：`path` 为空表示工作区根，传相对路径则只列该目录一层 */
  workspaceTree: (workspace: string, path?: string) =>
    request<FileNode>(
      `/api/workspace/tree?workspace=${encodeURIComponent(workspace)}${path ? `&path=${encodeURIComponent(path)}` : ''}`,
    ),

  workspaceFile: (workspace: string, path: string) =>
    request<{ content: string; truncated?: boolean }>(
      `/api/workspace/file?workspace=${encodeURIComponent(workspace)}&path=${encodeURIComponent(path)}`,
    ),

  /** 上传附件，返回可直接放进 user_input.attachments 的 session_attachment:// 引用。 */
  uploadAttachments: async (sessionId: string, files: File[]): Promise<string[]> => {
    const form = new FormData();
    for (const file of files) form.append('file', file, file.name);
    const resp = await fetch(
      resolveUrl(`/api/sessions/${encodeURIComponent(sessionId)}/attachments`),
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
      resolveUrl(
        `/api/sessions/${encodeURIComponent(sessionId)}/attachments/${encodeURIComponent(name)}`,
      ),
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
