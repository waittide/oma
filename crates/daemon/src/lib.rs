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
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post, put},
};
use futures_util::{SinkExt, StreamExt};
use oma_config::{AgentLoader, OmaConfig, PaletteLoader, SkillLoader};
use oma_contract::{AgentEvent, ChatMessage, ClientMessage, Palette, Ready, ServerMessage};
use oma_runtime::{RoomError, SessionRoom, estimate_tokens};
use oma_storage::{SessionRecord, StorageError, StorageManager, validate_attachment_name};
use oma_tool::{ToolRegistry, resolve_path};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tower_http::{
    compression::CompressionLayer,
    cors::{Any, CorsLayer},
};

/// 附件上传体积上限（单请求）
const MAX_UPLOAD_BYTES: usize = 16 * 1024 * 1024;
/// 工作区文件树最大深度与总条目上限（避免大仓库阻塞/撑爆响应）
const TREE_MAX_DEPTH: usize = 4;
const TREE_MAX_ENTRIES: usize = 2000;

/// 解析生效的访问 token。
///
/// 优先级：命令行 `--token` > 环境变量 `OMA_AUTH_TOKEN` > 配置文件 `[server].token`。
/// 不再单独落一个 `auth.token` 文件：token 属于配置的一部分，
/// 放在 `settings.json` 里用户才能自己查看与修改（设置界面也写回这里）。
/// 配置缺省时使用 [`oma_config::DEFAULT_AUTH_TOKEN`]，并在必要时回写配置以便可见。
pub fn resolve_token(cli_token: Option<&str>, config: &OmaConfig) -> String {
    resolve_token_with(cli_token, std::env::var("OMA_AUTH_TOKEN").ok().as_deref(), config)
}

/// [`resolve_token`] 的可测形式：环境变量以参数传入。
///
/// 这样测试无需修改进程级环境（本仓库禁用 unsafe，且并发测试会互相干扰）。
fn resolve_token_with(cli_token: Option<&str>, env_token: Option<&str>, config: &OmaConfig) -> String {
    if let Some(t) = cli_token.filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    if let Some(t) = env_token.filter(|t| !t.is_empty()) {
        return t.to_string();
    }
    if config.server.token.trim().is_empty() {
        return oma_config::DEFAULT_AUTH_TOKEN.to_string();
    }
    config.server.token.clone()
}

/// 服务端全局共享状态
#[derive(Clone)]
pub struct DaemonState {
    pub token:       String,
    pub storage:     StorageManager,
    pub config:      Arc<RwLock<OmaConfig>>,
    pub config_path: oma_config::ConfigPaths,
    pub rooms:       Arc<RwLock<HashMap<String, Arc<SessionRoom>>>>,
    pub start_time:  Instant,
}

impl DaemonState {
    pub fn new(
        token: String,
        storage: StorageManager,
        config: OmaConfig,
        config_path: oma_config::ConfigPaths,
    ) -> Self {
        Self {
            token,
            storage,
            config: Arc::new(RwLock::new(config)),
            config_path,
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
            let (default_model, default_agent, default_level) = {
                let cfg = self.config.read();
                (
                    cfg.default_model.clone(),
                    cfg.default_agent.clone(),
                    cfg.default_reasoning_level.clone(),
                )
            };
            // 由外部直接指定 session_id 时的兜底创建：标题留空，
            // 交由模型在首轮结束后命名（与 POST /api/sessions 行为一致）
            let new_rec = self
                .storage
                .create_session(
                    session_id,
                    workspace,
                    "",
                    &default_model,
                    &default_agent,
                    &default_level,
                )
                .await?;
            record = Some(new_rec);
        }

        let record = record.expect("session record present after create");

        // 先装配完整工具注册表，再交给房间：
        // 房间持有的是注册表快照，注册必须发生在构造之前。
        let mut reg = ToolRegistry::with_builtins();
        // 插件工具：按 workspace 发现并注册（JS 由内嵌 QuickJS 执行）
        let plugins = Arc::new(oma_plugin::PluginHost::load(std::path::Path::new(workspace)));
        for tool in plugins.tools() {
            reg.register(tool);
        }

        let room = SessionRoom::new(
            session_id,
            workspace,
            self.storage.clone(),
            self.config.clone(),
            reg,
            &record.active_model,
            &record.active_agent,
        );
        // 推理等级来自会话自身（创建时即写入，不会为空），无需再回退
        *room.reasoning_level.write() = record.reasoning_level.clone();
        // 插件宿主同时提供 `tool_call` 钩子（工具已在上面注册）
        room.set_plugins(plugins);

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
        version:         oma_contract::VERSION,
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

    let mut records = state
        .storage
        .list_sessions(query.workspace.as_deref())
        .await
        .map_err(storage_error)?;
    // 回填运行时状态：仅已加载且正在执行轮次的房间算作运行中
    for r in &mut records {
        r.is_running = state
            .rooms
            .read()
            .get(&r.session_id)
            .is_some_and(|room| room.is_busy());
    }
    Ok(Json(records))
}

#[derive(Deserialize)]
struct CreateSessionReq {
    workspace: String,
    title:     Option<String>,
    model:     Option<String>,
    agent:     Option<String>,
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
    // 标题留空表示「交给模型根据首轮对话自动命名」；
    // 用户填写时原样保留，模型不会再覆盖（见 set_title_if_empty）。
    let title = payload.title.unwrap_or_default().trim().to_string();
    let (default_model, default_agent, default_level) = {
        let cfg = state.config.read();
        (
            cfg.default_model.clone(),
            cfg.default_agent.clone(),
            cfg.default_reasoning_level.clone(),
        )
    };
    let model = payload.model.unwrap_or(default_model);
    let agent = payload.agent.unwrap_or(default_agent);

    let rec = state
        .storage
        .create_session(&session_id, &payload.workspace, &title, &model, &agent, &default_level)
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
// 工具清单 REST API (/api/tools)
// =========================================================================

/// 预设编辑器可勾选的工具：内置工具 + 插件工具
#[derive(Deserialize)]
struct ToolsQuery {
    /// 指定 workspace 时一并列出该项目/全局发现到的插件工具
    workspace: Option<String>,
}

async fn handle_list_tools(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<ToolsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    let builtin = ToolRegistry::with_builtins();
    let mut out: Vec<serde_json::Value> = builtin
        .list()
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name(),
                "description": t.description(),
                "kind": "builtin",
            })
        })
        .collect();

    if let Some(ws) = query.workspace.as_deref().filter(|w| !w.is_empty()) {
        let plugins = oma_plugin::PluginHost::load(std::path::Path::new(ws));
        for tool in plugins.tools() {
            out.push(serde_json::json!({
                "name": tool.name(),
                "description": tool.description(),
                "kind": "plugin",
            }));
        }
    }

    Ok(Json(out))
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
// 调色板 REST API (/api/palettes)
// =========================================================================
/// 可写的调色板响应：附带 builtin 标记，前端据此禁用删除／编辑
#[derive(Serialize)]
struct PaletteView {
    #[serde(flatten)]
    palette: Palette,
    /// 是否为内置调色板（内置不可删改，但可被用户新增的同 id 文件遮蔽）
    builtin: bool,
}

impl PaletteView {
    fn of(palette: Palette) -> Self {
        Self {
            builtin: PaletteLoader::is_builtin(&palette.id),
            palette,
        }
    }
}

async fn handle_list_palettes(
    State(state): State<DaemonState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PaletteView>>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    Ok(Json(
        PaletteLoader::list()
            .into_iter()
            .map(PaletteView::of)
            .collect(),
    ))
}

/// 写入调色板：id 取自路径（也是文件名），须先做 slug 校验防止路径穿越
async fn handle_put_palette(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(palette_id): AxumPath<String>,
    Json(mut payload): Json<Palette>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    payload.id = palette_id;
    match PaletteLoader::save(&payload) {
        Ok(()) => Ok(Json(serde_json::json!({ "success": true }))),
        Err(e) if e.to_string().contains("reserved") => Err((StatusCode::FORBIDDEN, e.to_string())),
        Err(e) if e.to_string().contains("invalid") => Err((StatusCode::BAD_REQUEST, e.to_string())),
        Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

async fn handle_delete_palette(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(palette_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }
    match PaletteLoader::delete(&palette_id) {
        Ok(()) => Ok(Json(serde_json::json!({ "success": true }))),
        Err(e) if e.to_string().contains("cannot be deleted") => Err((StatusCode::FORBIDDEN, e.to_string())),
        Err(e) if e.to_string().contains("not found") => Err((StatusCode::NOT_FOUND, e.to_string())),
        Err(e) if e.to_string().contains("invalid") => Err((StatusCode::BAD_REQUEST, e.to_string())),
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
            let key = pv.get("api_key").and_then(|k| k.as_str()).unwrap_or("");
            if !key.is_empty() && !key.starts_with("env:") {
                // 掩码位数与真实密钥等长：前端据此渲染等长占位符，
                // 否则任何长度的密钥都只显示固定的三位。
                let key_len = key.chars().count();
                pv["api_key"] = serde_json::Value::String(API_KEY_MASK.to_string());
                pv["api_key_len"] = serde_json::json!(key_len);
            }
        }
    }
    v
}

/// 剔除 GET 脱敏时附加的只读元数据：客户端回传整份配置时会带上它们，
/// 但它们不属于配置结构（ProviderConfig 拒绝未知字段）。
fn strip_api_key_len(payload: &mut serde_json::Value) {
    let Some(providers) = payload.get_mut("providers").and_then(|p| p.as_object_mut()) else {
        return;
    };
    for pv in providers.values_mut() {
        if let Some(obj) = pv.as_object_mut() {
            obj.remove("api_key_len");
        }
    }
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
    Json(mut payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if check_auth(&headers, None, &state.token, false).is_none() {
        return Err(unauthorized());
    }

    // `api_key_len` 是 GET 脱敏时附加的只读元数据（见 mask_config），
    // 客户端会把整份配置原样回传；它不是配置结构的一部分，写入前剔除，
    // 否则 ProviderConfig 的 deny_unknown_fields 会把合法回传判为非法载荷
    strip_api_key_len(&mut payload);

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

    // 主题引用在写入侧校验（调色板存在且明暗属性匹配），防止非法值污染客户端
    PaletteLoader::validate_theme(&cfg.theme, &PaletteLoader::list())
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid theme config: {e}")))?;

    // 默认推理等级必项：模型支持思考时一定有等级，空值会让会话拿不到初始值
    if cfg.default_reasoning_level.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "default_reasoning_level must not be empty".to_string(),
        ));
    }
    if !oma_contract::is_valid_reasoning_level(&cfg.default_reasoning_level) {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "Invalid default_reasoning_level {:?}: expect one of {:?}",
                cfg.default_reasoning_level,
                oma_contract::REASONING_LEVELS
            ),
        ));
    }

    // 原子落盘：先写同目录临时文件再 rename，避免半截配置（settings.json + models.json）
    cfg.save_to_paths(&state.config_path).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to persist config: {e}"),
        )
    })?;
    *state.config.write() = cfg.clone();

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

    // 3. 发送 Ready 握手确认（model_catalog 携带配置的真实模型清单）
    let (model_catalog, active_model, active_agent, reasoning_level, active_theme) = {
        let cfg = state.config.read();
        (
            cfg.model_catalog(),
            room.active_model.read().clone(),
            room.active_agent.read().clone(),
            room.reasoning_level.read().clone(),
            // 已解析主题随握手下发：终端客户端无需自读配置或内置色值
            PaletteLoader::resolve(&cfg.theme),
        )
    };
    // 上下文占用：用上次记录值 + 其后新增部分的估算，与 TokenAnchor 的做法一致。
    // 绝不能对整段历史重估 —— 启发式折算对代码/中文会明显高估，会把准确值盖掉。
    let context_usage = match room
        .storage
        .context_usage(&room.session_id)
        .await
        .ok()
        .flatten()
    {
        Some((tokens, context_len, covered)) => {
            let added = match room
                .storage
                .get_linear_messages(&room.session_id, None)
                .await
            {
                // 历史短于记录时说明前缀已变（分支切换/删除消息），估算不可靠：
                // 只用记录值，宁可短暂偏差等下次请求刷新
                Ok(msgs) if msgs.len() >= covered => estimate_tokens(&msgs[covered..]),
                _ => 0,
            };
            Some(oma_contract::ContextUsage {
                tokens: tokens + added,
                context_len,
            })
        }
        None => None,
    };

    let ready = Ready {
        version: oma_contract::VERSION.into(),
        session_id: room.session_id.clone(),
        workspace: room.workspace.to_string_lossy().to_string(),
        active_model,
        active_agent,
        reasoning_level,
        current_leaf_id: room
            .storage
            .get_session(&room.session_id)
            .await
            .ok()
            .flatten()
            .and_then(|rec| rec.current_leaf_id),
        model_catalog,
        agents: AgentLoader::list_agents(&room.workspace),
        context_usage,
        active_theme,
    };

    let (mut ws_sender, mut ws_receiver) = socket.split();
    if send_server_message(&mut ws_sender, &ServerMessage::Ready { ready: Box::new(ready) })
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
                event: Box::new(AgentEvent::ActiveTurnCatchUp(catch_up)),
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
                            event: Box::new(AgentEvent::SyncRequired {}),
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
            if send_server_message(&mut ws_sender, &ServerMessage::Event { event: Box::new(event) })
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
    let router = Router::new()
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
        .route("/api/tools", get(handle_list_tools))
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
        .route("/api/palettes", get(handle_list_palettes))
        .route(
            "/api/palettes/{palette_id}",
            put(handle_put_palette).delete(handle_delete_palette),
        )
        .route("/ws", get(handle_ws_upgrade));

    // Daemon 保持 Headless：不内置也不直出任何前端资产，
    // 界面统一由 `oma web` 提供（见 crates/bin）。
    router
        //
        // 跨源开放：前端与 Daemon 是分离的两个服务，浏览器会直接跨源发请求。
        // token 是唯一凭证且不走 cookie，因此任意站点既拿不到凭证也无法冒用。
        //
        // 不能图省事用 permissive()：它会把 `Access-Control-Allow-Headers` 写成 `*`，
        // 而 Authorization 属于规范里的 “CORS non-wildcard request-header name”，
        // 通配符不覆盖它，浏览器会直接拒绝预检。必须显式列出。
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
                // 预检结果缓存 10 分钟：上面两个头都是非简单头，每个请求都会先发
                // OPTIONS，不缓存会让跨源下的请求数翻倍
                .max_age(std::time::Duration::from_secs(600)),
        )
        // 消息历史可达数 MB，压缩后可降至 1/4 左右，远端/公网访问收益明显。
        // 取默认级别：Fastest 压缩率偏低，Best CPU 代价过高，默认级为折中
        .layer(CompressionLayer::new())
        .with_state(state)
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
            oma_config::ConfigPaths::in_dir(tmp),
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

    /// 大会话历史必须被压缩：这是远端访问时最大的单项优化。
    #[tokio::test]
    async fn test_large_json_response_is_compressed() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = spawn_app(test_state(tmp.path()).await?).await?;
        let token = "test_secret_token";
        let client = reqwest::Client::new();

        // 造一个带大块文本的会话，确保响应超过压缩阈值
        let resp = client
            .post(format!("{}/api/sessions", base))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "workspace": tmp.path().to_string_lossy().to_string(),
                "title": "compress"
            }))
            .send()
            .await?;
        let created: CreateSessionResp = resp.json().await?;

        // 直接写库，避免依赖 provider
        let big = "压缩测试文本".repeat(20_000);
        let storage = StorageManager::new(tmp.path()).await?;
        let msg = oma_contract::ChatMessage {
            id:         "m1".to_string(),
            parent_id:  None,
            role:       oma_contract::Role::User,
            content:    vec![oma_contract::Block::Text { text: big }],
            created_at: 0,
        };
        storage
            .append_message(&created.session_id, &msg, 0, 0)
            .await?;

        // 不带 Accept-Encoding 时不得压缩
        let plain = client
            .get(format!("{}/api/sessions/{}/messages", base, created.session_id))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;
        assert!(
            plain.headers().get("content-encoding").is_none(),
            "without Accept-Encoding the body must stay identity"
        );
        let plain_len = plain.bytes().await?.len();

        // 带 gzip 时必须声明 content-encoding 且体积显著变小
        let gz = client
            .get(format!("{}/api/sessions/{}/messages", base, created.session_id))
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept-Encoding", "gzip")
            .send()
            .await?;
        assert_eq!(
            gz.headers()
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("gzip"),
            "large JSON must be gzip encoded"
        );
        let gz_len = gz.bytes().await?.len();
        assert!(
            gz_len < plain_len / 2,
            "compressed body should be much smaller: {} -> {}",
            plain_len,
            gz_len
        );
        Ok(())
    }

    /// 小响应不应被压缩（避免为几十字节付出压缩开销）。
    #[tokio::test]
    async fn test_small_json_response_not_compressed() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = spawn_app(test_state(tmp.path()).await?).await?;
        let resp = reqwest::Client::new()
            .get(format!("{}/api/sessions", base))
            .header("Authorization", "Bearer test_secret_token")
            .header("Accept-Encoding", "gzip")
            .send()
            .await?;
        // 空列表很小，默认 predicate 会跳过压缩
        assert!(
            resp.headers().get("content-encoding").is_none(),
            "tiny responses must not be compressed"
        );
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
            .create_session("sess_att", "/w", "T", "m", "task", "medium")
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

    /// 脱敏只针对字面密钥：`env:` 引用原样下发，空密钥不脱敏；
    /// 且掩码必须携带真实长度，否则前端无法渲染等长占位符。
    #[test]
    fn test_mask_config_key_len() {
        use oma_config::ProviderConfig;

        let mut config = OmaConfig::default();
        for (name, key) in [("plain", "sk-secret-123"), ("env", "env:MY_KEY"), ("empty", "")] {
            config.providers.insert(
                name.into(),
                ProviderConfig {
                    api_type: "completion".into(),
                    base_url: "https://api.example.com/v1".into(),
                    api_key:  key.into(),
                    headers:  Default::default(),
                    body:     serde_json::json!({}),
                    models:   vec![],
                },
            );
        }

        let v = mask_config(&config);
        let p = &v["providers"];

        assert_eq!(p["plain"]["api_key"], "***");
        assert_eq!(p["plain"]["api_key_len"], 13, "掩码位数 = 真实密钥字符数");

        assert_eq!(p["env"]["api_key"], "env:MY_KEY", "env: 引用无需脱敏");
        assert!(p["env"].get("api_key_len").is_none());

        assert_eq!(p["empty"]["api_key"], "");
        assert!(p["empty"].get("api_key_len").is_none());

        // 多字节密钥按字符而非字节计数，掩码位数才不会虚高
        let mut c2 = OmaConfig::default();
        c2.providers.insert(
            "cjk".into(),
            ProviderConfig {
                api_type: "completion".into(),
                base_url: "u".into(),
                api_key:  "密钥密钥".into(),
                headers:  Default::default(),
                body:     serde_json::json!({}),
                models:   vec![],
            },
        );
        assert_eq!(mask_config(&c2)["providers"]["cjk"]["api_key_len"], 4);
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
        let config_paths = oma_config::ConfigPaths::in_dir(tmp.path());
        let token = "test_secret_token".to_string();

        let state = DaemonState::new(token.clone(), storage, config, config_paths.clone());
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
        // 掩码位数须与真实密钥等长，供前端渲染等长占位
        assert_eq!(got["providers"]["p1"]["api_key_len"], 13);

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

        let reloaded = OmaConfig::load_from_paths(&config_paths)?;
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
        let untouched = OmaConfig::load_from_paths(&config_paths)?;
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
        // 技能以 <name>/SKILL.md 落盘，并额外暴露技能目录（scripts/ 等资源的基准）
        assert!(
            skill_task["path"]
                .as_str()
                .is_some_and(|p| p.ends_with("task/SKILL.md")),
            "skill must expose its SKILL.md path: {}",
            skill_task["path"]
        );
        assert!(
            skill_task["dir"]
                .as_str()
                .is_some_and(|p| p.ends_with("agents/skills/task")),
            "skill must expose its directory: {}",
            skill_task["dir"]
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
    fn test_resolve_token_precedence() {
        // 命令行 > 环境变量 > 配置文件；配置为空时回落到默认值
        let mut config = OmaConfig::default();
        config.server.token = "from-config".into();

        assert_eq!(resolve_token_with(Some("from-cli"), None, &config), "from-cli");
        // 空串等同未传，不应把 token 悄悄清空
        assert_eq!(resolve_token_with(Some(""), None, &config), "from-config");

        // 环境变量优先于配置，但仍低于命令行
        assert_eq!(resolve_token_with(None, Some("from-env"), &config), "from-env");
        assert_eq!(
            resolve_token_with(Some("from-cli"), Some("from-env"), &config),
            "from-cli"
        );
        assert_eq!(resolve_token_with(None, Some(""), &config), "from-config");

        // 配置里没写 token（旧配置文件）时给默认值，而不是空串把接口裸奔
        config.server.token = String::new();
        assert_eq!(resolve_token_with(None, None, &config), oma_config::DEFAULT_AUTH_TOKEN);
        config.server.token = "   ".into();
        assert_eq!(resolve_token_with(None, None, &config), oma_config::DEFAULT_AUTH_TOKEN);
    }

    #[tokio::test]
    async fn test_cors_allows_any_origin() -> Result<()> {
        // 前端与 Daemon 是分离服务，浏览器从任意前端地址跨源请求都应被允许；
        // 凭证仅靠 Authorization 头，预检必须放行它，否则浏览器不会发出真实请求。
        let tmp = tempfile::tempdir()?;
        let state = test_state(tmp.path()).await?;
        let base = spawn_app(state).await?;
        let http = reqwest::Client::new();

        for origin in ["http://localhost:5173", "https://web.example.com"] {
            let resp = http
                .request(reqwest::Method::OPTIONS, format!("{}/api/server/status", base))
                .header("Origin", origin)
                .header("Access-Control-Request-Method", "GET")
                .header("Access-Control-Request-Headers", "authorization")
                .send()
                .await?;
            assert!(resp.status().is_success(), "preflight failed for {origin}");
            let headers = resp.headers();
            assert_eq!(
                headers
                    .get("access-control-allow-origin")
                    .and_then(|v| v.to_str().ok()),
                Some("*"),
                "preflight must allow any origin ({origin})"
            );
            let allow_headers = headers
                .get("access-control-allow-headers")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            assert!(
                allow_headers.contains("authorization"),
                "preflight must allow the Authorization header ({origin})"
            );
        }
        Ok(())
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

    /// 插件工具应出现在 `/api/tools?workspace=...` 清单中（kind = plugin）。
    #[tokio::test]
    async fn test_plugin_tools_listed() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let base = spawn_app(test_state(tmp.path()).await?).await?;
        let token = "test_secret_token";

        let ws = tmp.path().join("ws");
        let plugin_dir = ws.join(".oma").join("plugins").join("demo");
        std::fs::create_dir_all(&plugin_dir)?;
        std::fs::write(
            plugin_dir.join("plugin.js"),
            r#"oma.registerTool({ name: "hello", description: "Greet", parameters: {}, execute: () => "hi" });"#,
        )?;

        let tools: Vec<serde_json::Value> = reqwest::Client::new()
            .get(format!("{}/api/tools?workspace={}", base, ws.to_string_lossy()))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?
            .json()
            .await?;

        let hello = tools
            .iter()
            .find(|t| t["name"] == "hello")
            .expect("plugin tool must be listed");
        assert_eq!(hello["kind"], "plugin");
        Ok(())
    }
}
