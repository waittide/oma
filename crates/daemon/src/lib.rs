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
        Path as AxumPath, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get},
};
use futures_util::{SinkExt, StreamExt};
use oma_config::{AgentLoader, OmaConfig};
use oma_contract::{AgentEvent, ApprovalMode, ChatMessage, ClientMessage, Ready, ServerMessage};
use oma_mcp::McpManager;
use oma_runtime::{RoomSubagentRunner, SessionRoom};
use oma_storage::{SessionRecord, StorageManager};
use oma_tool::{ToolRegistry, resolve_path};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

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

        let record = record.unwrap();

        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(oma_tool::ReadTool));
        reg.register(Arc::new(oma_tool::WriteTool));
        reg.register(Arc::new(oma_tool::EditTool));
        reg.register(Arc::new(oma_tool::ShellTool::default()));

        let room = SessionRoom::new(
            session_id,
            workspace,
            self.storage.clone(),
            self.config.clone(),
            reg.clone(),
            self.mcp.clone(),
            &record.active_model,
            &record.active_agent,
            record.approval_mode,
        );

        // 挂载 subagent runner
        let subagent_runner = Arc::new(RoomSubagentRunner::new(room.clone()));
        reg.register(Arc::new(oma_tool::TaskTool::new(Some(subagent_runner))));

        // 挂载 MCP 工具
        for mcp_tool in self.mcp.create_all_tools().await {
            reg.register(mcp_tool);
        }

        self.rooms
            .write()
            .insert(session_id.to_string(), room.clone());
        Ok(room)
    }
}

/// 鉴权校验助手函数
fn check_auth(headers: &HeaderMap, query_token: Option<&str>, expected_token: &str) -> bool {
    if let Some(q) = query_token {
        if q == expected_token {
            return true;
        }
    }

    if let Some(auth_val) = headers.get("authorization") {
        if let Ok(s) = auth_val.to_str() {
            if let Some(token) = s.strip_prefix("Bearer ") {
                return token.trim() == expected_token;
            }
        }
    }

    false
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

#[derive(Deserialize)]
struct AuthQuery {
    token: Option<String>,
}

async fn handle_server_status(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<ServerStatus>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
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
    token:     Option<String>,
}

async fn handle_list_sessions(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<Vec<SessionRecord>>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match state
        .storage
        .list_sessions(query.workspace.as_deref())
        .await
    {
        Ok(list) => Ok(Json(list)),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
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
    Query(query): Query<AuthQuery>,
    Json(payload): Json<CreateSessionReq>,
) -> Result<Json<CreateSessionResp>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let session_id = format!("sess_{}", uuid::Uuid::new_v4().simple());
    let title = payload.title.unwrap_or_else(|| "New Session".into());
    let model = payload
        .model
        .unwrap_or_else(|| state.config.read().default_model.clone());
    let agent = payload
        .agent
        .unwrap_or_else(|| state.config.read().default_agent.clone());
    let approval_mode = payload
        .approval_mode
        .unwrap_or(state.config.read().default_approval_mode);

    match state
        .storage
        .create_session(&session_id, &payload.workspace, &title, &model, &agent, approval_mode)
        .await
    {
        Ok(rec) => Ok(Json(CreateSessionResp {
            session_id,
            session: rec,
        })),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn handle_delete_session(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    state.rooms.write().remove(&session_id);
    match state.storage.delete_session(&session_id).await {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true }))),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct GetMessagesQuery {
    leaf_id: Option<String>,
    token:   Option<String>,
}

async fn handle_get_messages(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Query(query): Query<GetMessagesQuery>,
) -> Result<Json<Vec<ChatMessage>>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match state
        .storage
        .get_linear_messages(&session_id, query.leaf_id.as_deref())
        .await
    {
        Ok(msgs) => Ok(Json(msgs)),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct RenameSessionReq {
    title: String,
}

async fn handle_rename_session(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    AxumPath(session_id): AxumPath<String>,
    Json(payload): Json<RenameSessionReq>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match state
        .storage
        .rename_session(&session_id, &payload.title)
        .await
    {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true }))),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

#[derive(Deserialize)]
struct WorkspaceTreeQuery {
    workspace: String,
    token:     Option<String>,
}

#[derive(Serialize)]
struct FileNode {
    name:     String,
    path:     String,
    is_dir:   bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    children: Vec<FileNode>,
}

async fn handle_workspace_tree(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceTreeQuery>,
) -> Result<Json<FileNode>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let ws_path = Path::new(&query.workspace);
    if !ws_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    fn build_tree(path: &Path, rel_root: &Path, max_depth: usize) -> FileNode {
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
        if is_dir && max_depth > 0 {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    let f_name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if f_name.starts_with('.') || f_name == "target" || f_name == "node_modules" {
                        continue;
                    }
                    children.push(build_tree(&p, rel_root, max_depth - 1));
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

    let tree = build_tree(ws_path, ws_path, 4);
    Ok(Json(tree))
}

#[derive(Deserialize)]
struct WorkspaceFileQuery {
    workspace: String,
    path:      String,
    token:     Option<String>,
}

async fn handle_workspace_file(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<WorkspaceFileQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let target = resolve_path(Path::new(&query.workspace), &query.path);
    if !target.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    match std::fs::read_to_string(&target) {
        Ok(c) => Ok(Json(serde_json::json!({ "content": c }))),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
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

async fn handle_get_config(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(Json(mask_config(&state.config.read())))
}

async fn handle_put_config(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(query): Query<AuthQuery>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return Err((StatusCode::UNAUTHORIZED, "Invalid token".into()));
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

    // 主题 label 在写入侧校验，防止非法值污染前端
    cfg.theme
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("Invalid theme config: {e}")))?;

    cfg.save_to_file(&state.config_path).map_err(|e| {
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
    if !check_auth(&headers, query.token.as_deref(), &state.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    ws.on_upgrade(move |socket| handle_ws_client(socket, state))
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
            let err = ServerMessage::Error {
                message: "Expected ClientMessage::Connect as first packet".into(),
            };
            let _ = socket
                .send(Message::text(serde_json::to_string(&err).unwrap()))
                .await;
            return;
        }
    };

    let params = match client_msg {
        ClientMessage::Connect { params } => params,
        _ => return,
    };

    // 2. 挂载或创建 Room
    let room = match state
        .get_or_create_room(&params.session_id, &params.workspace)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let err = ServerMessage::Error {
                message: format!("Failed to access session room: {}", e),
            };
            let _ = socket
                .send(Message::text(serde_json::to_string(&err).unwrap()))
                .await;
            return;
        }
    };

    // 3. 发送 Ready 握手确认（providers 携带配置的真实模型清单）
    let providers_map = state.config.read().model_catalog();

    let agents = AgentLoader::list_agents(&room.workspace);

    let ready = Ready {
        version: "0.1.0".into(),
        session_id: room.session_id.clone(),
        workspace: room.workspace.to_string_lossy().to_string(),
        active_model: room.active_model.read().clone(),
        active_agent: room.active_agent.read().clone(),
        approval_mode: *room.approval_mode.read(),
        current_leaf_id: None,
        providers: providers_map,
        agents,
    };

    let _ = socket
        .send(Message::text(
            serde_json::to_string(&ServerMessage::Ready { ready }).unwrap(),
        ))
        .await;

    // 4. 若后台正处于活跃 Turn，发送追赶快照 ActiveTurnCatchUp
    if let Some(catch_up) = room.get_catch_up() {
        let _ = socket
            .send(Message::text(
                serde_json::to_string(&ServerMessage::Event {
                    event: AgentEvent::ActiveTurnCatchUp(catch_up),
                })
                .unwrap(),
            ))
            .await;
    }

    // 5. 双向管道拆分
    let (mut ws_sender, mut ws_receiver) = socket.split();
    let mut broadcast_rx = room.subscribe();

    // 任务 A: Room 广播转发给 WebSocket
    let mut send_task = tokio::spawn(async move {
        while let Ok(event) = broadcast_rx.recv().await {
            let s_msg = ServerMessage::Event { event };
            if let Ok(json) = serde_json::to_string(&s_msg) {
                if ws_sender.send(Message::text(json)).await.is_err() {
                    break;
                }
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
                let parsed: Result<ClientMessage, _> = serde_json::from_str(&txt);
                if let Ok(c_msg) = parsed {
                    match c_msg {
                        ClientMessage::Command { command } => {
                            room_clone
                                .submit_command(&client_id, &client_name, client_type, command)
                                .await;
                        }
                        ClientMessage::Approval { response } => {
                            room_clone
                                .arbiter
                                .resolve(&response.request_id, response.decision)
                                .await;
                            room_clone.broadcast(AgentEvent::PermissionResolved {
                                request_id:  response.request_id,
                                decision:    response.decision,
                                resolved_by: client_name.clone(),
                            });
                        }
                        ClientMessage::Cancel {} => {
                            room_clone.cancel().await;
                        }
                        _ => {}
                    }
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
        .route("/api/workspace/tree", get(handle_workspace_tree))
        .route("/api/workspace/file", get(handle_workspace_file))
        .route("/api/config", get(handle_get_config).put(handle_put_config))
        .route("/ws", get(handle_ws_upgrade));

    let dist_path = Path::new("web/dist");
    if dist_path.exists() {
        let serve_dir = tower_http::services::ServeDir::new(dist_path)
            .fallback(tower_http::services::ServeFile::new(dist_path.join("index.html")));
        router = router.fallback_service(serve_dir);
    }

    router.layer(CorsLayer::permissive()).with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_daemon_rest_routes() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        let config = OmaConfig::default();
        let mcp = Arc::new(McpManager::new());
        let token = "test_secret_token".to_string();

        let state = DaemonState::new(token.clone(), storage, config, tmp.path().join("config.toml"), mcp);
        let app = create_router(state);

        // 使用 tokio 监听随机端口测试服务
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();

        // 1. 未鉴权请求测试 (401)
        let resp_unauth = client
            .get(format!("http://{}/api/server/status", addr))
            .send()
            .await?;
        assert_eq!(resp_unauth.status(), StatusCode::UNAUTHORIZED);

        // 2. 带 Bearer 鉴权请求测试 (200)
        let resp_auth = client
            .get(format!("http://{}/api/server/status", addr))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;
        assert_eq!(resp_auth.status(), StatusCode::OK);

        // 3. 创建会话测试
        let resp_create = client
            .post(format!("http://{}/api/sessions", addr))
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
            .get(format!("http://{}/api/sessions", addr))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await?;
        assert_eq!(resp_list.status(), StatusCode::OK);
        let list_data: Vec<SessionRecord> = resp_list.json().await?;
        assert_eq!(list_data.len(), 1);

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
        let app = create_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let client = reqwest::Client::new();
        let auth = format!("Bearer {}", token);

        // 1. GET 下发时密钥被脱敏
        let got: serde_json::Value = client
            .get(format!("http://{}/api/config", addr))
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
            .put(format!("http://{}/api/config", addr))
            .header("Authorization", &auth)
            .json(&payload)
            .send()
            .await?;
        assert_eq!(put_resp.status(), StatusCode::OK);

        let reloaded = OmaConfig::load_from_file(&config_file)?;
        assert_eq!(reloaded.default_model, "p1/gpt-x");
        assert_eq!(reloaded.providers["p1"].api_key, "sk-secret-123");

        Ok(())
    }
}
