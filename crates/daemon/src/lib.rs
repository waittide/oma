use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::Result;
use axum::{
    Router,
    extract::{
        DefaultBodyLimit, Multipart, Path as AxumPath, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use oma_config::{AgentLoader, OmaConfig, SkillLoader};
use oma_contract::{AgentEvent, ApprovalMode, ChatMessage, ClientMessage, McpServerSummary, Ready, ServerMessage};
use oma_mcp::McpManager;
use oma_runtime::{RoomError, RoomSubagentRunner, SessionRoom};
use oma_storage::{SessionRecord, StorageError, StorageManager, validate_attachment_name};
use oma_tool::{RunnerSlot, ShellTool, TaskTool, ToolRegistry, resolve_path};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

/// 附件上传体积上限（单请求）
const MAX_UPLOAD_BYTES: usize = 16 * 1024 * 1024;
/// 工作区文件树最大深度与总条目上限（避免大仓库阻塞/撑爆响应）
const TREE_MAX_DEPTH: usize = 4;
const TREE_MAX_ENTRIES: usize = 2000;

/// 解析或自动生成安全随机 Token
pub fn resolve_or_create_token(custom_token: Option<&str>, base_dir: &Path) -> Result<String> {
    if let Some(t) = custom_token {
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    if let Ok(env_token) = std::env::var("OMA_AUTH_TOKEN") {
        if !env_token.is_empty() {
            return Ok(env_token);
        }
    }
    let token_path = base_dir.join("auth.token");
    if token_path.exists() {
        if let Ok(saved) = std::fs::read_to_string(&token_path) {
            let t = saved.trim();
            if !t.is_empty() {
                return Ok(t.to_string());
            }
        }
    }
    let new_token = format!("oma_sec_{}", uuid::Uuid::new_v4().simple());
    if let Some(parent) = token_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&token_path, &new_token);
    Ok(new_token)
}

/// 服务端全局共享状态
#[derive(Clone)]
pub struct DaemonState {
    pub token:       String,
    pub storage:     StorageManager,
    pub config:      Arc<RwLock<OmaConfig>>,
    pub config_path: PathBuf,
    pub mcp:         Arc<McpManager>,
    pub rooms:       Arc<RwLock<HashMap<String, Arc<SessionRoom>>>>,
    pub start_time:  Instant,
}

impl DaemonState {
    pub fn new(
        token: String,
        storage: StorageManager,
        config: OmaConfig,
        config_path: PathBuf,
        mcp: Arc<McpManager>,
    ) -> Self {
        Self {
            token,
            storage,
            config: Arc::new(RwLock::new(config)),
            config_path,
            mcp,
            rooms: Arc::new(RwLock::new(HashMap::new())),
            start_time: Instant::now(),
        }
    }

    /// 获取或惰性加载指定 SessionRoom
    pub async fn get_or_create_room(&self, session_id: &str, workspace: &str) -> Result<Arc<SessionRoom>> {
        if let Some(r) = self.rooms.read().get(session_id) {
            return Ok(r.clone());
        }

        let mut record = self.storage.get_session(session_id).await?;
        if record.is_none() {
            let (default_model, default_agent, default_approval) = {
                let cfg = self.config.read();
                (
                    cfg.default_model.clone(),
                    cfg.default_agent.clone(),
                    cfg.default_approval_mode,
                )
            };
            let new_rec = self
                .storage
                .create_session(
                    session_id,
                    workspace,
                    "New Session",
                    &default_model,
                    &default_agent,
                    default_approval,
                )
                .await?;
            record = Some(new_rec);
        }

        let record = record.expect("session record present after create");

        // 先装配完整工具注册表（含 task 与 MCP），再交给房间：
        // 房间持有的是注册表快照，注册必须发生在构造之前。
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(oma_tool::ReadTool));
        reg.register(Arc::new(oma_tool::WriteTool));
        reg.register(Arc::new(oma_tool::EditTool));
        reg.register(Arc::new(ShellTool::default()));
        let runner_slot = Arc::new(RunnerSlot::new());
        reg.register(Arc::new(TaskTool::new(runner_slot.clone())));
        for mcp_tool in self.mcp.create_all_tools().await {
            reg.register(mcp_tool);
        }

        let room = SessionRoom::new(
            session_id,
            workspace,
            self.storage.clone(),
            self.config.clone(),
            reg,
            &record.active_model,
            &record.active_agent,
            record.approval_mode,
        );

        // 回填 subagent runner：TaskTool 需要房间，房间持有注册表，一次性槽位解环
        let subagent_runner = Arc::new(RoomSubagentRunner::new(room.clone()));
        if let Err(e) = runner_slot.set(subagent_runner) {
            room.broadcast(AgentEvent::Error {
                message: format!("Failed to bind subagent runner: {}", e),
            });
        }

        self.rooms
            .write()
            .insert(session_id.to_string(), room.clone());
        Ok(room)
    }

    /// 移除房间句柄（删除会话时调用）
    pub fn drop_room(&self, session_id: &str) -> Option<Arc<SessionRoom>> {
        self.rooms.write().remove(session_id)
    }
}

/// 鉴权来源：HTTP 头优先，其次查询参数（仅 WebSocket 握手需要，浏览器无法自定义 WS 头）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthSource {
    Header,
    Query,
}

/// 校验 Bearer Token。`allow_query` 仅对 WebSocket 握手开放：
/// REST 走查询参数会把长期凭证写进 URL、访问日志与 Referer。
fn check_auth(
    headers: &HeaderMap,
    query_token: Option<&str>,
    expected_token: &str,
    allow_query: bool,
) -> Option<AuthSource> {
    if let Some(auth_val) = headers.get("authorization") {
        if let Ok(s) = auth_val.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                if token.trim() == expected_token {
                    return Some(AuthSource::Header);
                }
                return None;
            }
        }
    }

    if allow_query {
        if let Some(q) = query_token {
            if q == expected_token {
                return Some(AuthSource::Query);
            }
        }
    }

    None
}

/// 鉴权失败统一响应
fn unauthorized() -> (StatusCode, String) {
    (StatusCode::UNAUTHORIZED, "Invalid or missing bearer token".into())
}

/// 存储错误 → HTTP 响应
fn storage_error(e: StorageError) -> (StatusCode, String) {
    match e {
        StorageError::InvalidId(m) => (StatusCode::BAD_REQUEST, m),
        StorageError::NotFound(m) => (StatusCode::NOT_FOUND, m),
        StorageError::Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// 房间错误 → HTTP 响应
fn room_error(e: RoomError) -> (StatusCode, String) {
    match e {
        RoomError::Busy(m) => (StatusCode::CONFLICT, m),
        RoomError::Invalid(m) => (StatusCode::BAD_REQUEST, m),
        RoomError::NotFound(m) => (StatusCode::NOT_FOUND, m),
        RoomError::Internal(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// =========================================================================
// REST API Handlers
// =========================================================================

#[derive(Serialize)]
struct ServerStatus {
    version:         &'static str,
    active_sessions: usize,
    uptime_secs:     u64,
}

async fn handle_server_status(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> Result<Json<ServerStatus>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(ServerStatus {
        version:         "0.1.0",
        active_sessions: state.rooms.read().len(),
        uptime_secs:     state.start_time.elapsed().as_secs(),
    }))
}

#[derive(Deserialize)]
struct ListSessionsQuery {
    workspace: Option<String>,
}

async fn handle_list_sessions(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<Vec<SessionRecord>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    state
        .storage
        .list_sessions(query.workspace.as_deref())
        .await
        .map(Json)
        .map_err(storage_error)
}

#[derive(Deserialize)]
struct CreateSessionReq {
    workspace:     String,
    title:         Option<String>,
    model:         Option<String>,
    agent:         Option<String>,
    approval_mode: Option<ApprovalMode>,
}

#[derive(Serialize, Deserialize)]
struct CreateSessionResp {
    session_id: String,
    session:    SessionRecord,
}

async fn handle_create_session(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(payload): Json<CreateSessionReq>,
) -> Result<Json<CreateSessionResp>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
    let title = payload.title.unwrap_or_else(|| "New Session".into());
    let (default_model, default_agent, default_approval) = {
        let cfg = state.config.read();
        (
            cfg.default_model.clone(),
            cfg.default_agent.clone(),
            cfg.default_approval_mode,
        )
    };
    let model = payload.model.unwrap_or(default_model);
    let agent = payload.agent.unwrap_or(default_agent);
    let approval_mode = payload.approval_mode.unwrap_or(default_approval);

    let rec = state
        .storage
        .create_session(&session_id, &payload.workspace, &title, &model, &agent, approval_mode)
        .await
        .map_err(storage_error)?;

    Ok(Json(CreateSessionResp {
        session_id,
        session: rec,
    }))
}

async fn handle_delete_session(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    // 运行中的会话不允许删除，否则轮次会往已删除的库继续写入
    if let Some(room) = state.rooms.read().get(&session_id) {
        if room.is_busy() {
            return Err((
                StatusCode::CONFLICT,
                "Cannot delete a session while a turn is running".into(),
            ));
        }
    }
    state.drop_room(&session_id);
    state
        .storage
        .delete_session(&session_id)
        .await
        .map_err(storage_error)?;
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
struct GetMessagesQuery {
    leaf_id: Option<String>,
}

async fn handle_get_messages(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<GetMessagesQuery>,
) -> Result<Json<Vec<ChatMessage>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    state
        .storage
        .get_linear_messages(&session_id, query.leaf_id.as_deref())
        .await
        .map(Json)
        .map_err(storage_error)
}

async fn handle_get_message_tree(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<Vec<ChatMessage>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    state
        .storage
        .get_all_messages(&session_id)
        .await
        .map(Json)
        .map_err(storage_error)
}

async fn handle_delete_message(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath((session_id, message_id)): AxumPath<(String, String)>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    // 有活跃房间时走房间（校验运行状态并广播事件），否则直接操作存储
    let room = state.rooms.read().get(&session_id).cloned();
    let result = match room {
        Some(room) => room.delete_message(&message_id).await.map_err(room_error),
        None => state
            .storage
            .delete_message_subtree(&session_id, &message_id)
            .await
            .map_err(storage_error),
    }?;

    let (deleted, leaf) = result;
    Ok(Json(serde_json::json!({
        "success": true,
        "deleted": deleted,
        "current_leaf_id": leaf,
    })))
}

#[derive(Deserialize)]
struct RenameSessionReq {
    title: String,
}

async fn handle_rename_session(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Json(payload): Json<RenameSessionReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    state
        .storage
        .rename_session(&session_id, &payload.title)
        .await
        .map_err(storage_error)?;

    if let Some(room) = state.rooms.read().get(&session_id) {
        room.broadcast(AgentEvent::SessionRenamed {
            session_id: session_id.clone(),
            title:      payload.title,
        });
    }
    Ok(Json(serde_json::json!({ "success": true })))
}

#[derive(Deserialize)]
struct WorkspaceTreeQuery {
    workspace: String,
}

#[derive(Serialize)]
struct FileNode {
    name:     String,
    path:     String,
    is_dir:   bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<FileNode>,
}

/// 目录遍历的重活（阻塞 IO）放专用线程池，避免卡住 tokio worker。
async fn handle_workspace_tree(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<FileNode>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    let ws_path = PathBuf::from(&query.workspace);
    if !ws_path.is_dir() {
        return Err((StatusCode::NOT_FOUND, "Workspace not found".into()));
    }

    let tree = tokio::task::spawn_blocking(move || {
        let mut budget = TREE_MAX_ENTRIES;
        build_tree(&ws_path, &ws_path, TREE_MAX_DEPTH, &mut budget)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(tree))
}

/// 递归构建文件树；跳过大目录与点文件，受深度与总条目预算约束。
fn build_tree(path: &Path, rel_root: &Path, max_depth: usize, budget: &mut usize) -> FileNode {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let rel_path = path
        .strip_prefix(rel_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let is_dir = path.is_dir();

    let mut children = Vec::new();
    if is_dir && max_depth > 0 && *budget > 0 {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                if *budget == 0 {
                    break;
                }
                let p = entry.path();
                let f_name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if f_name.starts_with('.') || f_name == "target" || f_name == "node_modules" {
                    continue;
                }
                *budget -= 1;
                children.push(build_tree(&p, rel_root, max_depth - 1, budget));
            }
        }
    }
    children.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    FileNode {
        name,
        path: rel_path,
        is_dir,
        children,
    }
}

#[derive(Deserialize)]
struct WorkspaceFileQuery {
    workspace: String,
    path:      String,
}

async fn handle_workspace_file(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceFileQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    let target = resolve_path(Path::new(&query.workspace), &query.path);
    tokio::fs::read_to_string(&target)
        .await
        .map(|c| Json(serde_json::json!({ "content": c })))
        .map_err(|e| (StatusCode::NOT_FOUND, format!("Failed to read file: {}", e)))
}

// =========================================================================
// 附件上传 / 下载 REST API (/api/sessions/{id}/attachments)
// =========================================================================

/// 上传附件：保存到会话附件目录并返回 `session_attachment://` 引用，
/// 该引用可直接放进 `AgentCommand::UserInput.attachments`。
async fn handle_upload_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    // 会话必须存在，避免在任意路径下创建附件目录
    if state
        .storage
        .get_session(&session_id)
        .await
        .map_err(storage_error)?
        .is_none()
    {
        return Err((StatusCode::NOT_FOUND, format!("Session {} not found", session_id)));
    }

    let mut saved: Vec<String> = Vec::new();
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid multipart body: {}", e)))?
    {
        let Some(file_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let name = format!("{}-{}", uuid::Uuid::new_v4().simple(), sanitize_file_name(&file_name));
        let path = state
            .storage
            .attachment_path(&session_id, &name)
            .map_err(storage_error)?;

        let bytes = field
            .bytes()
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("Failed to read upload: {}", e)))?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err((
                StatusCode::PAYLOAD_TOO_LARGE,
                format!("Attachment exceeds {} bytes", MAX_UPLOAD_BYTES),
            ));
        }

        tokio::fs::write(&path, &bytes).await.map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to store attachment: {}", e),
            )
        })?;
        saved.push(format!("session_attachment://{}", name));
    }

    if saved.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "No file field in upload".into()));
    }
    Ok(Json(serde_json::json!({ "success": true, "attachments": saved })))
}

/// 下载/预览附件
async fn handle_get_attachment(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath((session_id, name)): AxumPath<(String, String)>,
) -> Result<Response, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    validate_attachment_name(&name).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let path = state
        .storage
        .attachment_path(&session_id, &name)
        .map_err(storage_error)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, format!("Attachment not found: {}", e)))?;

    Ok(([(axum::http::header::CONTENT_TYPE, mime_for(&name))], bytes).into_response())
}

/// 文件名清洗：先取路径最后一段（丢弃任何目录成分），再收敛为安全字符集，
/// 并消除 `.`/`..` 这类目录语义，确保拼进路径后无法逃逸出附件目录。
fn sanitize_file_name(raw: &str) -> String {
    let base = raw
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(raw)
        .trim_start_matches(['.', ' ']);
    // 连续点收敛为单个点，避免残留 `..` 片段
    let mut cleaned = String::with_capacity(base.len());
    let mut last_dot = false;
    for c in base.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
            c
        } else {
            '_'
        };
        if c == '.' {
            if last_dot {
                continue;
            }
            last_dot = true;
        } else {
            last_dot = false;
        }
        cleaned.push(c);
    }

    let cleaned = cleaned.trim_matches('.');
    let name = if cleaned.is_empty() { "file" } else { cleaned };
    name.chars().take(48).collect()
}

fn mime_for(name: &str) -> &'static str {
    match name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

// =========================================================================
// Agent 预设 REST API (/api/presets)
// =========================================================================

#[derive(Deserialize)]
struct ScopeQuery {
    workspace: Option<String>,
    /// global | project；删除时必须显式指定
    scope:     Option<String>,
}

#[derive(Deserialize)]
struct PresetWriteReq {
    name:        String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools:       Vec<String>,
    content:     String,
    /// global | project；缺省为 project（project 必须提供 workspace）
    #[serde(default)]
    scope:       Option<String>,
}

/// workspace 可选：global 作用域无需提供
fn scoped_workspace(query: &ScopeQuery) -> Option<PathBuf> {
    query
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn require_project_workspace(query: &ScopeQuery) -> Result<PathBuf, (StatusCode, String)> {
    match scoped_workspace(query) {
        Some(ws) => Ok(ws),
        None => Err((
            StatusCode::BAD_REQUEST,
            "workspace is required for project scope".into(),
        )),
    }
}

/// 解析写入作用域对应的 workspace 参数
fn resolve_scope_workspace(query: &ScopeQuery, scope: &str) -> Result<Option<PathBuf>, (StatusCode, String)> {
    match scope {
        "project" => Ok(Some(require_project_workspace(query)?)),
        _ => Ok(scoped_workspace(query)),
    }
}

/// 技能写入请求（无工具白名单：技能是按需读取的知识，不限定工具）
#[derive(Deserialize)]
struct SkillWriteReq {
    name:        String,
    #[serde(default)]
    description: String,
    content:     String,
    #[serde(default)]
    scope:       Option<String>,
}

async fn handle_list_presets(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Vec<oma_config::AgentFile>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    Ok(Json(AgentLoader::list_agent_files(scoped_workspace(&query).as_deref())))
}

async fn handle_get_preset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(preset_id): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<oma_config::AgentFile>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    AgentLoader::read_agent_file(scoped_workspace(&query).as_deref(), &preset_id)
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

async fn handle_put_preset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(preset_id): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
    Json(payload): Json<PresetWriteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    let scope = payload.scope.as_deref().unwrap_or("project");
    let ws = resolve_scope_workspace(&query, scope)?;
    match AgentLoader::write_agent_file(
        ws.as_deref(),
        &preset_id,
        scope,
        payload.name.trim(),
        payload.description.trim(),
        &payload.tools,
        &payload.content,
    ) {
        Ok(path) => Ok(Json(
            serde_json::json!({ "success": true, "path": path.display().to_string() }),
        )),
        Err(e) if e.to_string().contains("read-only") => Err((StatusCode::FORBIDDEN, e.to_string())),
        Err(e) if e.to_string().contains("Invalid") => Err((StatusCode::BAD_REQUEST, e.to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn handle_delete_preset(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(preset_id): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    // 删除必须显式指定作用域：内置预设不可删
    let scope = query.scope.as_deref().unwrap_or("global");
    let ws = resolve_scope_workspace(&query, scope)?;

    match AgentLoader::delete_agent_file(ws.as_deref(), &preset_id, scope) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true }))),
        Err(e) if e.to_string().contains("not found") => Err((StatusCode::NOT_FOUND, e.to_string())),
        Err(e) if e.to_string().contains("Invalid") => Err((StatusCode::BAD_REQUEST, e.to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// =========================================================================
// 技能 REST API (/api/skills)
// =========================================================================

async fn handle_list_skills(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<Vec<oma_config::SkillFile>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    Ok(Json(SkillLoader::list_skills(scoped_workspace(&query).as_deref())))
}

async fn handle_get_skill(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(skill_id): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<oma_config::SkillFile>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    SkillLoader::read_skill(scoped_workspace(&query).as_deref(), &skill_id)
        .map(Json)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))
}

async fn handle_put_skill(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(skill_id): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
    Json(payload): Json<SkillWriteReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    let scope = payload.scope.as_deref().unwrap_or("project");
    let ws = resolve_scope_workspace(&query, scope)?;
    match SkillLoader::write_skill(
        ws.as_deref(),
        &skill_id,
        scope,
        payload.name.trim(),
        payload.description.trim(),
        &payload.content,
    ) {
        Ok(path) => Ok(Json(
            serde_json::json!({ "success": true, "path": path.display().to_string() }),
        )),
        Err(e) if e.to_string().contains("Invalid") => Err((StatusCode::BAD_REQUEST, e.to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn handle_delete_skill(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(skill_id): AxumPath<String>,
    Query(query): Query<ScopeQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    let scope = query.scope.as_deref().unwrap_or("global");
    let ws = resolve_scope_workspace(&query, scope)?;

    match SkillLoader::delete_skill(ws.as_deref(), &skill_id, scope) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true }))),
        Err(e) if e.to_string().contains("not found") => Err((StatusCode::NOT_FOUND, e.to_string())),
        Err(e) if e.to_string().contains("Invalid") => Err((StatusCode::BAD_REQUEST, e.to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

// =========================================================================
// 系统配置 REST API (/api/config)
// =========================================================================
/// 脱敏占位符：GET 下发时替换真实密钥；PUT 回传同值时保留服务端旧密钥
const API_KEY_MASK: &str = "***";

fn mask_config(config: &OmaConfig) -> serde_json::Value {
    let mut v = serde_json::to_value(config).unwrap_or(serde_json::Value::Null);
    if let Some(providers) = v.get_mut("providers").and_then(|p| p.as_object_mut()) {
        for (_, pv) in providers.iter_mut() {
            let masked = pv
                .get("api_key")
                .and_then(|k| k.as_str())
                .map(|s| !s.is_empty() && !s.starts_with("env:"))
                .unwrap_or(false);
            if masked {
                pv["api_key"] = serde_json::Value::String(API_KEY_MASK.to_string());
            }
        }
    }
    v
}

#[derive(Deserialize)]
struct GetConfigQuery {
    /// 1 = 返回真实密钥（供设置页「显示密钥」使用）；默认脱敏
    reveal: Option<String>,
}

async fn handle_get_config(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<GetConfigQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    let cfg = state.config.read();
    if query.reveal.as_deref() == Some("1") {
        return Ok(Json(serde_json::to_value(&*cfg).unwrap_or(serde_json::Value::Null)));
    }
    Ok(Json(mask_config(&cfg)))
}

async fn handle_put_config(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    let mut cfg: OmaConfig = serde_json::from_value(payload)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid config payload: {e}")))?;

    // 掩码回传的 api_key 沿用服务端既有值，避免前端未编辑密钥时被清空
    {
        let current = state.config.read();
        for (name, provider) in cfg.providers.iter_mut() {
            if provider.api_key == API_KEY_MASK {
                provider.api_key = current
                    .providers
                    .get(name)
                    .map(|old| old.api_key.clone())
                    .unwrap_or_default();
            }
        }
    }

    // 主题 label 在写入侧校验（含自定义主题），防止非法值污染前端
    cfg.theme
        .validate(&cfg.custom_themes)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid theme config: {e}")))?;

    // 原子落盘：先写同目录临时文件再 rename，避免半截配置
    cfg.save_to_file_atomic(&state.config_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to persist config: {e}"),
        )
    })?;
    *state.config.write() = cfg.clone();
    state.mcp.sync_servers(&cfg.mcp_servers);

    Ok(Json(serde_json::json!({ "success": true })))
}

// =========================================================================
// WebSocket Upgrade & Gateway (/ws)
// =========================================================================

#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

async fn handle_ws_upgrade(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    // 浏览器 WebSocket 无法自定义请求头，握手阶段允许查询参数携带 token
    if check_auth(&headers, query.token.as_deref(), &state.token, true).is_none() {
        return unauthorized().into_response();
    }

    ws.on_upgrade(move |socket| handle_ws_client(socket, state))
}

/// 向 socket 发送一条服务端消息
async fn send_server_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: &ServerMessage,
) -> Result<(), axum::Error> {
    let json = match serde_json::to_string(msg) {
        Ok(j) => j,
        Err(_) => return Ok(()),
    };
    sender.send(Message::text(json)).await
}

async fn handle_ws_client(mut socket: WebSocket, state: DaemonState) {
    // 1. 等待客户端首包 Connect
    let first_msg = match socket.recv().await {
        Some(Ok(Message::Text(txt))) => txt,
        _ => return,
    };

    let client_msg: ClientMessage = match serde_json::from_str(&first_msg) {
        Ok(m) => m,
        Err(_) => {
            let (mut sink, _) = socket.split();
            let _ = send_server_message(
                &mut sink,
                &ServerMessage::Error {
                    message: "Expected ClientMessage::Connect as first packet".into(),
                },
            )
            .await;
            return;
        }
    };

    let params = match client_msg {
        ClientMessage::Connect { params } => params,
        _ => return,
    };

    if params.session_id.trim().is_empty() {
        let (mut sink, _) = socket.split();
        let _ = send_server_message(
            &mut sink,
            &ServerMessage::Error {
                message: "session_id is required".into(),
            },
        )
        .await;
        return;
    }

    // 2. 挂载或创建 Room
    let room = match state
        .get_or_create_room(&params.session_id, &params.workspace)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let (mut sink, _) = socket.split();
            let _ = send_server_message(
                &mut sink,
                &ServerMessage::Error {
                    message: format!("Failed to access session room: {}", e),
                },
            )
            .await;
            return;
        }
    };

    // 3. 发送 Ready 握手确认（providers 携带配置的真实模型清单）
    let (providers, active_model, active_agent, approval_mode) = {
        let cfg = state.config.read();
        (
            cfg.model_catalog(),
            room.active_model.read().clone(),
            room.active_agent.read().clone(),
            *room.approval_mode.read(),
        )
    };
    let ready = Ready {
        version: "0.1.0".into(),
        session_id: room.session_id.clone(),
        workspace: room.workspace.to_string_lossy().to_string(),
        active_model,
        active_agent,
        approval_mode,
        current_leaf_id: room
            .storage
            .get_session(&room.session_id)
            .await
            .ok()
            .flatten()
            .and_then(|rec| rec.current_leaf_id),
        providers,
        agents: AgentLoader::list_agents(&room.workspace),
        mcp_servers: state
            .mcp
            .server_tool_counts()
            .await
            .into_iter()
            .map(|(name, tool_count)| McpServerSummary { name, tool_count })
            .collect(),
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();
    if send_server_message(&mut ws_sender, &ServerMessage::Ready { ready })
        .await
        .is_err()
    {
        return;
    }

    // 4. 若后台正处于活跃 Turn，发送追赶快照 ActiveTurnCatchUp
    if let Some(catch_up) = room.get_catch_up() {
        let _ = send_server_message(
            &mut ws_sender,
            &ServerMessage::Event {
                event: AgentEvent::ActiveTurnCatchUp(catch_up),
            },
        )
        .await;
    }

    // 5. 双向管道拆分
    let mut broadcast_rx = room.subscribe();

    // 任务 A: Room 广播转发给 WebSocket。
    // 广播缓冲溢出（慢客户端）不能直接断连：通知客户端整体回读后继续转发。
    let mut send_task = tokio::spawn(async move {
        loop {
            let event = match broadcast_rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "websocket client lagged; requesting resync");
                    if send_server_message(
                        &mut ws_sender,
                        &ServerMessage::Event {
                            event: AgentEvent::SyncRequired {},
                        },
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            };
            if send_server_message(&mut ws_sender, &ServerMessage::Event { event })
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // 任务 B: WebSocket 接收客户端指令下发给 Room
    let room_clone = room.clone();
    let client_id = params.client_id;
    let client_name = params.client_name;
    let client_type = params.client_type;

    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            if let Message::Text(txt) = msg {
                let Ok(c_msg) = serde_json::from_str::<ClientMessage>(&txt) else {
                    continue;
                };
                match c_msg {
                    ClientMessage::Command { command } => {
                        room_clone
                            .submit_command(&client_id, &client_name, client_type, command)
                            .await;
                    }
                    ClientMessage::Approval { response } => {
                        let resolved = room_clone
                            .arbiter
                            .resolve(&response.request_id, response.decision)
                            .await;
                        // 先到先得：仅首个响应广播，避免多客户端重复提示
                        if resolved {
                            room_clone.broadcast(AgentEvent::PermissionResolved {
                                request_id:  response.request_id,
                                decision:    response.decision,
                                resolved_by: client_name.clone(),
                            });
                        }
                    }
                    ClientMessage::Cancel {} => {
                        room_clone.cancel().await;
                    }
                    ClientMessage::Connect { .. } => {}
                }
            }
        }
    });

    // 任意一方断开即退出
    tokio::select! {
        _ = (&mut send_task) => recv_task.abort(),
        _ = (&mut recv_task) => send_task.abort(),
    }
}

use tokio::sync::broadcast;

/// 组装 Axum 路由
pub fn create_router(state: DaemonState) -> Router {
    let mut router = Router::new()
        .route("/api/server/status", get(handle_server_status))
        .route("/api/sessions", get(handle_list_sessions).post(handle_create_session))
        .route(
            "/api/sessions/{id}",
            delete(handle_delete_session).patch(handle_rename_session),
        )
        .route("/api/sessions/{id}/messages", get(handle_get_messages))
        .route("/api/sessions/{id}/messages/tree", get(handle_get_message_tree))
        .route(
            "/api/sessions/{id}/messages/{message_id}",
            delete(handle_delete_message),
        )
        .route(
            "/api/sessions/{id}/attachments",
            post(handle_upload_attachment).layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES)),
        )
        .route("/api/sessions/{id}/attachments/{name}", get(handle_get_attachment))
        .route("/api/workspace/tree", get(handle_workspace_tree))
        .route("/api/workspace/file", get(handle_workspace_file))
        .route("/api/presets", get(handle_list_presets))
        .route(
            "/api/presets/{preset_id}",
            get(handle_get_preset)
                .put(handle_put_preset)
                .delete(handle_delete_preset),
        )
        .route("/api/skills", get(handle_list_skills))
        .route(
            "/api/skills/{skill_id}",
            get(handle_get_skill)
                .put(handle_put_skill)
                .delete(handle_delete_skill),
        )
        .route("/api/config", get(handle_get_config).put(handle_put_config))
        .route("/ws", get(handle_ws_upgrade));

    let dist_path = Path::new("web/dist");
    if dist_path.exists() {
        let serve_dir = tower_http::services::ServeDir::new(dist_path)
            .fallback(tower_http::services::ServeFile::new(dist_path.join("index.html")));
        router = router.fallback_service(serve_dir);
    }

    // 同源前端由本服务直出，跨源仅放行本地开发端口，避免任意站点携带 token 调用
    router
        .layer(CorsLayer::new())
        .layer(axum::middleware::from_fn(dev_cors))
        .with_state(state)
}

/// 开发期 CORS：仅放行本机来源（localhost / 127.0.0.1 任意端口）
async fn dev_cors(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let origin = req
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut resp = next.run(req).await;

    if let Some(origin) = origin.filter(|o| is_local_origin(o)) {
        let headers = resp.headers_mut();
        headers.insert(
            axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
            origin
                .parse()
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("null")),
        );
        headers.insert(
            axum::http::header::ACCESS_CONTROL_ALLOW_METHODS,
            axum::http::HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
        );
        headers.insert(
            axum::http::header::ACCESS_CONTROL_ALLOW_HEADERS,
            axum::http::HeaderValue::from_static("authorization, content-type"),
        );
    }
    resp
}

fn is_local_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn spawn_app(state: DaemonState) -> Result<String> {
        let app = create_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(format!("http://{}", addr))
    }

    async fn test_state(tmp: &Path) -> Result<DaemonState> {
        let storage = StorageManager::new(tmp).await?;
        Ok(DaemonState::new(
            "test_secret_token".to_string(),
            storage,
            OmaConfig::default(),
            tmp.join("config.toml"),
            Arc::new(McpManager::new()),
        ))
    }

    #[tokio::test]
    async fn test_daemon_rest_routes() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let token = "test_secret_token".to_string();
        let base = spawn_app(test_state(tmp.path()).await?).await?;
        let client = reqwest::Client::new();

        // 1. 未鉴权请求测试 (401)
        let resp_unauth = client
            .get(format!("{}/api/server/status", base))
            .send()
            .await?;
        assert_eq!(resp_unauth.status(), StatusCode::UNAUTHORIZED);

        // 1b. 查询参数 token 不再接受（仅 WebSocket 握手允许）
        let resp_query_token = client
            .get(format!("{}/api/server/status?token={}", base, token))
            .send()
            .await?;
        assert_eq!(resp_query_token.status(), StatusCode::UNAUTHORIZED);

        // 2. 带 Bearer 鉴权请求测试 (200)
        let resp_auth = client
            .get(format!("{}/api/server/status", base))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;
        assert_eq!(resp_auth.status(), StatusCode::OK);

        // 3. 创建会话测试
        let resp_create = client
            .post(format!("{}/api/sessions", base))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "workspace": tmp.path().to_string_lossy().to_string(),
                "title": "Integration Session"
            }))
            .send()
            .await?;
        assert_eq!(resp_create.status(), StatusCode::OK);
        let create_data: CreateSessionResp = resp_create.json().await?;
        assert_eq!(create_data.session.title, "Integration Session");

        // 4. 会话列表测试
        let resp_list = client
            .get(format!("{}/api/sessions", base))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;
        assert_eq!(resp_list.status(), StatusCode::OK);
        let list_data: Vec<SessionRecord> = resp_list.json().await?;
        assert_eq!(list_data.len(), 1);

        Ok(())
    }

    /// session_id 参与文件系统路径拼接：穿越型 id 必须被拒且不得触碰目标目录。
    #[tokio::test]
    async fn test_session_id_traversal_is_rejected() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim)?;
        std::fs::write(victim.join("keep.txt"), b"keep")?;

        let token = "test_secret_token".to_string();
        let base = spawn_app(test_state(tmp.path()).await?).await?;
        let client = reqwest::Client::new();
        let auth = format!("Bearer {}", token);

        let resp = client
            .delete(format!("{}/api/sessions/..%2Fvictim", base))
            .header("Authorization", &auth)
            .send()
            .await?;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(victim.join("keep.txt").exists(), "victim must be untouched");

        let resp = client
            .get(format!("{}/api/sessions/..%2Fescaped/messages", base))
            .header("Authorization", &auth)
            .send()
            .await?;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(
            !tmp.path().join("escaped").exists(),
            "no dir may be created outside sessions/"
        );

        Ok(())
    }

    /// 房间必须暴露完整的工具集（含 task 与 MCP）：注册表在交给房间前就绪。
    #[tokio::test]
    async fn test_room_exposes_task_tool() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = test_state(tmp.path()).await?;
        let room = state.get_or_create_room("sess_tools", "/tmp").await?;

        let names: Vec<&str> = room.tools.list().iter().map(|t| t.name()).collect();
        assert!(names.contains(&"task"), "task tool missing: {:?}", names);
        assert!(room.tools.get("task").is_some());

        let defs = room.tools.to_definitions(&[]);
        assert!(
            defs.iter().any(|d| d["name"] == "task"),
            "task must be advertised to the model"
        );
        Ok(())
    }

    /// Agent 模板白名单必须同时约束「下发给模型的工具」与「实际可执行的工具」。
    #[tokio::test]
    async fn test_agent_tool_whitelist_filters_definitions() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = test_state(tmp.path()).await?;
        let room = state.get_or_create_room("sess_wl", "/tmp").await?;

        let template = AgentLoader::load_agent("explore", &room.workspace)?;
        assert!(!template.tools.is_empty(), "explore template declares tools");
        let defs = room.tools.to_definitions(&template.tools);
        let names: Vec<&str> = defs.iter().filter_map(|d| d["name"].as_str()).collect();
        assert!(names.contains(&"read"));
        assert!(!names.contains(&"write"), "write must not be advertised: {:?}", names);
        assert!(!names.contains(&"edit"));
        Ok(())
    }

    /// 上传的附件必须落在会话附件目录内，且文件名不可越权。
    #[tokio::test]
    async fn test_attachment_upload_and_fetch() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let token = "test_secret_token".to_string();
        let state = test_state(tmp.path()).await?;
        state
            .storage
            .create_session("sess_att", "/w", "T", "m", "task", ApprovalMode::Normal)
            .await?;
        let base = spawn_app(state).await?;
        let client = reqwest::Client::new();
        let auth = format!("Bearer {}", token);

        // 会话不存在 → 404（不得凭空创建目录）
        let missing = client
            .post(format!("{}/api/sessions/nope/attachments", base))
            .header("Authorization", &auth)
            .multipart(reqwest::multipart::Form::new().part(
                "file",
                reqwest::multipart::Part::bytes(vec![1, 2, 3]).file_name("a.png"),
            ))
            .send()
            .await?;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let resp = client
            .post(format!("{}/api/sessions/sess_att/attachments", base))
            .header("Authorization", &auth)
            .multipart(reqwest::multipart::Form::new().part(
                "file",
                reqwest::multipart::Part::bytes(vec![0x89, 0x50, 0x4E, 0x47]).file_name("../../escape.png"),
            ))
            .send()
            .await?;
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await?;
        let reference = body["attachments"][0]
            .as_str()
            .expect("attachment reference")
            .to_string();
        assert!(reference.starts_with("session_attachment://"));

        // 文件名中的路径穿越片段必须被清洗掉
        let name = reference.trim_start_matches("session_attachment://");
        assert!(
            !name.contains('/') && !name.contains(".."),
            "name not sanitized: {}",
            name
        );

        let fetched = client
            .get(format!("{}/api/sessions/sess_att/attachments/{}", base, name))
            .header("Authorization", &auth)
            .send()
            .await?;
        assert_eq!(fetched.status(), StatusCode::OK);
        assert_eq!(fetched.bytes().await?.len(), 4);

        // 下载侧同样拒绝穿越型名称
        let evil = client
            .get(format!("{}/api/sessions/sess_att/attachments/..%2F..%2Foma.db", base))
            .header("Authorization", &auth)
            .send()
            .await?;
        assert_eq!(evil.status(), StatusCode::BAD_REQUEST);

        Ok(())
    }

    #[tokio::test]
    async fn test_config_api_mask_and_preserve_key() -> Result<()> {
        use oma_config::ProviderConfig;

        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        let mut config = OmaConfig::default();
        config.providers.insert(
            "p1".into(),
            ProviderConfig {
                api_type: "completion".into(),
                base_url: "https://api.example.com/v1".into(),
                api_key:  "sk-secret-123".into(),
                headers:  Default::default(),
                body:     serde_json::json!({}),
                models:   vec![],
            },
        );
        let config_file = tmp.path().join("config.toml");
        let mcp = Arc::new(McpManager::new());
        let token = "test_secret_token".to_string();

        let state = DaemonState::new(token.clone(), storage, config, config_file.clone(), mcp);
        let base = spawn_app(state).await?;
        let client = reqwest::Client::new();
        let auth = format!("Bearer {}", token);

        // 1. GET 下发时密钥被脱敏
        let got: serde_json::Value = client
            .get(format!("{}/api/config", base))
            .header("Authorization", &auth)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(got["providers"]["p1"]["api_key"], "***");

        // 2. PUT 回传掩码：沿用旧密钥并落库
        let mut payload = got.clone();
        payload["default_model"] = serde_json::json!("p1/gpt-x");
        let put_resp = client
            .put(format!("{}/api/config", base))
            .header("Authorization", &auth)
            .json(&payload)
            .send()
            .await?;
        assert_eq!(put_resp.status(), StatusCode::OK);

        let reloaded = OmaConfig::load_from_file(&config_file)?;
        assert_eq!(reloaded.default_model, "p1/gpt-x");
        assert_eq!(reloaded.providers["p1"].api_key, "sk-secret-123");

        // 3. 未知字段必须被拒绝：否则前端字段名写错会「保存成功但配置没变」
        let mut bogus = got.clone();
        bogus["model"] = serde_json::json!("p1/typo");
        let bad = client
            .put(format!("{}/api/config", base))
            .header("Authorization", &auth)
            .json(&bogus)
            .send()
            .await?;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        // 且不得污染已落盘的配置
        let untouched = OmaConfig::load_from_file(&config_file)?;
        assert_eq!(untouched.default_model, "p1/gpt-x");

        Ok(())
    }

    /// 预设与技能是两套独立端点与存储：同名互不影响。
    #[tokio::test]
    async fn test_presets_and_skills_are_separate() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws)?;
        let token = "test_secret_token".to_string();
        let base = spawn_app(test_state(tmp.path()).await?).await?;
        let client = reqwest::Client::new();
        let auth = format!("Bearer {}", token);
        let ws_param = format!("workspace={}", ws.to_string_lossy());

        // 预设端点：内置 5 个模板始终可见
        let presets: Vec<serde_json::Value> = client
            .get(format!("{}/api/presets?{}", base, ws_param))
            .header("Authorization", &auth)
            .send()
            .await?
            .json()
            .await?;
        let ids: Vec<&str> = presets.iter().filter_map(|p| p["id"].as_str()).collect();
        for expected in ["task", "plan", "explore", "review", "build"] {
            assert!(
                ids.contains(&expected),
                "bundled preset {} missing: {:?}",
                expected,
                ids
            );
        }
        assert!(
            presets
                .iter()
                .any(|p| p["scope"] == "bundled" && p["tools"].is_array())
        );

        // 技能端点：项目内初始为空（不把预设当作技能）
        let skills: Vec<serde_json::Value> = client
            .get(format!("{}/api/skills?{}", base, ws_param))
            .header("Authorization", &auth)
            .send()
            .await?
            .json()
            .await?;
        assert!(
            skills.iter().all(|s| s["scope"] != "project"),
            "no project skill should exist yet: {:?}",
            skills
        );

        // 写入同名技能与预设
        let put_skill = client
            .put(format!("{}/api/skills/task?{}", base, ws_param))
            .header("Authorization", &auth)
            .json(&serde_json::json!({
                "name": "同名技能", "description": "与预设同名", "content": "正文", "scope": "project"
            }))
            .send()
            .await?;
        assert_eq!(put_skill.status(), StatusCode::OK);

        let put_preset = client
            .put(format!("{}/api/presets/my-preset?{}", base, ws_param))
            .header("Authorization", &auth)
            .json(&serde_json::json!({
                "name": "My Preset", "description": "自定义", "tools": ["read"], "content": "body", "scope": "project"
            }))
            .send()
            .await?;
        assert_eq!(put_preset.status(), StatusCode::OK);

        // 技能写入不影响预设
        let preset_task: serde_json::Value = client
            .get(format!("{}/api/presets/task?{}", base, ws_param))
            .header("Authorization", &auth)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(preset_task["scope"], "bundled");
        assert_ne!(preset_task["name"], "同名技能");

        // 技能带磁盘路径（供 System Prompt 目录注入）
        let skill_task: serde_json::Value = client
            .get(format!("{}/api/skills/task?{}", base, ws_param))
            .header("Authorization", &auth)
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(skill_task["scope"], "project");
        assert_eq!(skill_task["name"], "同名技能");
        assert!(
            skill_task["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("task.md")),
            "skill must expose its file path: {}",
            skill_task["path"]
        );

        // 内置预设只读
        let bundled_write = client
            .put(format!("{}/api/presets/task?{}", base, ws_param))
            .header("Authorization", &auth)
            .json(&serde_json::json!({
                "name": "X", "description": "", "tools": [], "content": "b", "scope": "project"
            }))
            .send()
            .await?;
        assert_eq!(bundled_write.status(), StatusCode::FORBIDDEN);

        // 两个端点各自删除互不影响
        let del = client
            .delete(format!(
                "{}/api/skills/task?scope=project&workspace={}",
                base,
                ws.to_string_lossy()
            ))
            .header("Authorization", &auth)
            .send()
            .await?;
        assert_eq!(del.status(), StatusCode::OK);
        let remaining: Vec<serde_json::Value> = client
            .get(format!("{}/api/skills?{}", base, ws_param))
            .header("Authorization", &auth)
            .send()
            .await?
            .json()
            .await?;
        assert!(
            remaining
                .iter()
                .all(|s| s["id"] != "task" || s["scope"] != "project")
        );
        // 预设仍在
        assert!(
            client
                .get(format!("{}/api/presets/my-preset?{}", base, ws_param))
                .header("Authorization", &auth)
                .send()
                .await?
                .status()
                .is_success()
        );
        Ok(())
    }

    #[test]
    fn test_origin_allowlist() {
        assert!(is_local_origin("http://localhost:5173"));
        assert!(is_local_origin("http://127.0.0.1:5173"));
        assert!(is_local_origin("https://localhost"));
        assert!(!is_local_origin("https://evil.example.com"));
        assert!(!is_local_origin("http://localhost.evil.com"));
        assert!(!is_local_origin("null"));
    }

    #[test]
    fn test_sanitize_file_name() {
        // 目录成分被丢弃，穿越片段无法存活
        assert_eq!(sanitize_file_name("../../escape.png"), "escape.png");
        assert_eq!(sanitize_file_name("a b/c.png"), "c.png");
        assert_eq!(sanitize_file_name(".hidden"), "hidden");
        assert_eq!(sanitize_file_name(".."), "file");
        assert_eq!(sanitize_file_name(""), "file");
        assert_eq!(sanitize_file_name("a b.png"), "a_b.png");
        assert!(!sanitize_file_name("x/../../y.png").contains(".."));
        assert!(sanitize_file_name(&"x".repeat(200)).len() <= 48);
    }
}
