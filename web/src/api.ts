import type { ChatMessage, FileNode, OmaConfigView, SessionRecord } from './types';

const TOKEN_KEY = 'oma_token';

export function getToken(): string {
  const urlParams = new URLSearchParams(window.location.search);
  const paramToken = urlParams.get('token');
  if (paramToken) {
    localStorage.setItem(TOKEN_KEY, paramToken);
    return paramToken;
  }
  return localStorage.getItem(TOKEN_KEY) || '';
}

export function setToken(token: string): void {
  localStorage.setItem(TOKEN_KEY, token);
}

async function request<T>(path: string, options: RequestInit = {}): Promise<T> {
  const token = getToken();
  const headers = new Headers(options.headers || {});
  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }
  if (!headers.has('Content-Type') && !(options.body instanceof FormData)) {
    headers.set('Content-Type', 'application/json');
  }

  const res = await fetch(path, { ...options, headers });
  if (!res.ok) {
    const errText = await res.text();
    throw new Error(`HTTP ${res.status}: ${errText || res.statusText}`);
  }
  return res.json();
}

export async function fetchServerStatus(): Promise<{ version: string; active_sessions: number; uptime_secs: number }> {
  return request('/api/server/status');
}

export async function fetchSessions(workspace?: string): Promise<SessionRecord[]> {
  const q = workspace ? `?workspace=${encodeURIComponent(workspace)}` : '';
  return request(`/api/sessions${q}`);
}

export async function createSession(payload: {
  workspace: string;
  title?: string;
  model?: string;
  agent?: string;
  approval_mode?: string;
}): Promise<{ session_id: string; session: SessionRecord }> {
  return request('/api/sessions', {
    method: 'POST',
    body: JSON.stringify(payload),
  });
}

export async function deleteSession(sessionId: string): Promise<{ success: boolean }> {
  return request(`/api/sessions/${sessionId}`, {
    method: 'DELETE',
  });
}

export async function fetchMessages(sessionId: string, leafId?: string): Promise<ChatMessage[]> {
  const q = leafId ? `?leaf_id=${encodeURIComponent(leafId)}` : '';
  return request(`/api/sessions/${sessionId}/messages${q}`);
}

export async function fetchWorkspaceTree(workspace: string): Promise<FileNode> {
  return request(`/api/workspace/tree?workspace=${encodeURIComponent(workspace)}`);
}

export async function fetchWorkspaceFile(workspace: string, path: string): Promise<{ content: string }> {
  return request(`/api/workspace/file?workspace=${encodeURIComponent(workspace)}&path=${encodeURIComponent(path)}`);
}

export async function fetchConfig(): Promise<OmaConfigView> {
  return request('/api/config');
}

export async function updateConfig(config: OmaConfigView): Promise<{ success: boolean }> {
  return request('/api/config', {
    method: 'PUT',
    body: JSON.stringify(config),
  });
}
