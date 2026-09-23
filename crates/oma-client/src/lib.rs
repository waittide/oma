//! oma-client: 纯 Rust 客户端 SDK
//!
//! 为 CLI / TUI 等非浏览器客户端封装 Daemon 的双向协议：
//! WebSocket（实时事件与指令）与 REST（会话管理）。
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! use oma_client::{ConnectOptions, OmaClient, SessionApi};
//! use oma_contract::ClientType;
//!
//! let api = SessionApi::new("127.0.0.1:17431", "token");
//! let session = api.create_session("/tmp/ws", Some("demo")).await?;
//!
//! let mut client = OmaClient::connect(ConnectOptions {
//!     addr:        "127.0.0.1:17431".into(),
//!     token:       "token".into(),
//!     workspace:   "/tmp/ws".into(),
//!     session_id:  session.session_id.clone(),
//!     client_type: ClientType::Cli,
//!     client_name: "demo-cli".into(),
//! })
//! .await?;
//!
//! // 实时事件流
//! while let Some(event) = client.next_event().await {
//!     println!("{:?}", event);
//! }
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use oma_contract::{
    AgentCommand, AgentEvent, ChatMessage, ClientMessage, ClientType, ConnectParams, Ready, ServerMessage,
    WorkspaceRecord,
};
use serde::Deserialize;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

/// SDK 版本（随握手包上报）。
///
/// 与 daemon 握手包里用的是同一个 [`oma_contract::VERSION`]，
/// 保证同一个二进制对外只自称一个版本。
pub const CLIENT_VERSION: &str = oma_contract::VERSION;

/// 把地址规整成 REST 基础 URL。
///
/// 调用方给的地址有两种形态，来源不同、形态也不同：命令行 `--addr` 与
/// `settings.json` 是裸 `host:port`，而 `client.json` 的连接项是 Web 界面写入的
/// 完整 URL（`http://host:port`）。这里两种都接受。
///
/// 早期实现无条件前缀 `http://`，遇到后者会拼出 `http://http://host:port`——
/// reqwest 把 `http` 当成主机名，请求发到一个不存在的主机上，
/// 于是「TUI 连不上、界面什么都显示不出来」。
pub fn http_base(addr: &str) -> String {
    match split_scheme(addr) {
        Some((scheme, rest)) => format!("{scheme}://{rest}"),
        None => format!("http://{}", trim_addr(addr)),
    }
}

/// 把地址规整成 WebSocket 基础 URL：`http` → `ws`，`https` → `wss`。
///
/// 与 [`http_base`] 同一套入参形态；已经写成 `ws://` / `wss://` 的原样保留。
pub fn ws_base(addr: &str) -> String {
    match split_scheme(addr) {
        Some(("https" | "wss", rest)) => format!("wss://{rest}"),
        Some((_, rest)) => format!("ws://{rest}"),
        None => format!("ws://{}", trim_addr(addr)),
    }
}

/// 去掉首尾空白与结尾斜杠（`http://host:port/` 与 `host:port` 等价）
fn trim_addr(addr: &str) -> &str {
    addr.trim().trim_end_matches('/')
}

/// 拆出地址里的 scheme：`http://host:port` → `Some(("http", "host:port"))`，
/// 裸 `host:port` → `None`。
///
/// scheme 限定为字母数字，避免把 IPv6 字面量里的东西误当 scheme
/// （IPv6 用方括号书写，本来也不含 `://`）。
fn split_scheme(addr: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = trim_addr(addr).split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+')
    {
        return None;
    }
    Some((scheme, rest.trim_end_matches('/')))
}

/// 会话索引记录（与 daemon 的 `SessionRecord` 对应）
#[derive(Debug, Clone, Deserialize)]
pub struct SessionRecord {
    pub session_id:   String,
    pub workspace:    String,
    pub title:        String,
    pub active_model: String,
    pub active_agent: String,
    /// 创建 / 最后更新时间（毫秒）：列表排序用
    #[serde(default)]
    pub created_at:   i64,
    #[serde(default)]
    pub updated_at:   i64,
    /// 该会话当前是否有正在执行的轮次（服务端按运行时回填）
    #[serde(default)]
    pub is_running:   bool,
}

/// REST 客户端：会话列出与创建（实时交互走 `OmaClient`）
pub struct SessionApi {
    base:  String,
    token: String,
    http:  reqwest::Client,
}

impl SessionApi {
    /// `addr` 可以是裸 `127.0.0.1:17431`，也可以是 `http://127.0.0.1:17431`
    /// （`client.json` 里存的就是后者），见 [`http_base`]。
    pub fn new(addr: &str, token: &str) -> Self {
        Self {
            base:  http_base(addr),
            token: token.to_string(),
            http:  reqwest::Client::new(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    async fn ensure_ok(resp: reqwest::Response) -> Result<serde_json::Value> {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("HTTP {}: {}", status, body.trim());
        }
        Ok(serde_json::from_str(&body).unwrap_or(serde_json::Value::Null))
    }

    /// 列出会话；`workspace` 为 None 时返回全部
    pub async fn list_sessions(&self, workspace: Option<&str>) -> Result<Vec<SessionRecord>> {
        let mut url = format!("{}/api/sessions", self.base);
        if let Some(ws) = workspace {
            url.push_str(&format!("?workspace={}", urlencode(ws)));
        }
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        serde_json::from_value(value).context("Unexpected session list payload")
    }

    /// 拉取整个消息树（含**所有分支**，不只是当前分支）。
    ///
    /// 实时交互走 `OmaClient`，这里给的是「有哪些分支可切」这类需要全貌的场景：
    /// 握手下发的是当前分支的线性历史，兄弟分支不在其中。
    pub async fn message_tree(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let resp = self
            .http
            .get(format!("{}/api/sessions/{}/messages/tree", self.base, session_id))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        serde_json::from_value(value).context("Unexpected message tree payload")
    }

    /// 删除消息及其整棵子树；返回（被删消息 id, 删除后的当前叶子）。
    pub async fn delete_message(&self, session_id: &str, message_id: &str) -> Result<(Vec<String>, Option<String>)> {
        let resp = self
            .http
            .delete(format!(
                "{}/api/sessions/{}/messages/{}",
                self.base, session_id, message_id
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        let deleted = value["deleted"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let leaf = value["current_leaf_id"].as_str().map(str::to_string);
        Ok((deleted, leaf))
    }

    /// 已登记工作区（`GET /api/workspaces`）。
    pub async fn list_workspaces(&self) -> Result<Vec<WorkspaceRecord>> {
        let resp = self
            .http
            .get(format!("{}/api/workspaces", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        serde_json::from_value(value).context("Unexpected workspace list payload")
    }

    /// 登记一个工作区（幂等；路径须存在的目录）。
    pub async fn create_workspace(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .post(format!("{}/api/workspaces", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "path": path }))
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    /// 移除工作区登记（仅名单；仍有会话时服务端拒绝）。
    pub async fn delete_workspace(&self, path: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}/api/workspaces?path={}", self.base, urlencode(path)))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    /// 真正下发的系统提示词（预设正文 + 环境块 + 项目上下文 + 技能目录）。
    pub async fn system_prompt(&self, workspace: &str, model: &str, agent: &str) -> Result<String> {
        let mut url = format!("{}/api/system-prompt?workspace={}", self.base, urlencode(workspace));
        if !model.is_empty() {
            url.push_str(&format!("&model={}", urlencode(model)));
        }
        if !agent.is_empty() {
            url.push_str(&format!("&agent={}", urlencode(agent)));
        }
        let resp = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        Ok(value["prompt"].as_str().unwrap_or_default().to_string())
    }

    /// 创建工作区会话
    pub async fn create_session(&self, workspace: &str, title: Option<&str>) -> Result<SessionRecord> {
        let mut payload = serde_json::json!({ "workspace": workspace });
        if let Some(title) = title {
            payload["title"] = serde_json::Value::String(title.to_string());
        }
        let resp = self
            .http
            .post(format!("{}/api/sessions", self.base))
            .bearer_auth(&self.token)
            .json(&payload)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        serde_json::from_value(value["session"].clone()).context("Unexpected create-session payload")
    }

    /// 删除会话
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        let resp = self
            .http
            .delete(format!("{}/api/sessions/{}", self.base, session_id))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    /// 重命名会话
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        let resp = self
            .http
            .patch(format!("{}/api/sessions/{}", self.base, session_id))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "title": title }))
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        Self::ensure_ok(resp).await?;
        Ok(())
    }

    /// 上传附件，返回可直接放进 `UserInput.attachments` 的 `session_attachment://` 引用。
    ///
    /// `file_name` 是服务端给附件命名的原料（会再清洗并加随机前缀），
    /// 因此调用方传原始文件名即可，不必自己去重名。
    pub async fn upload_attachment(&self, session_id: &str, file_name: &str, bytes: Vec<u8>) -> Result<Vec<String>> {
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(bytes).file_name(file_name.to_string()),
        );
        let resp = self
            .http
            .post(format!("{}/api/sessions/{}/attachments", self.base, session_id))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .context("Failed to reach oma daemon")?;
        let value = Self::ensure_ok(resp).await?;
        serde_json::from_value(value["attachments"].clone()).context("Unexpected upload payload")
    }
}

/// 最小百分号编码（仅覆盖查询值中必须转义的字符）
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(byte as char),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// 与服务端交互的入参
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub addr:        String,
    pub token:       String,
    pub workspace:   String,
    pub session_id:  String,
    pub client_type: ClientType,
    pub client_name: String,
}

impl ConnectOptions {
    pub fn new(
        addr: impl Into<String>,
        token: impl Into<String>,
        workspace: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            addr:        addr.into(),
            token:       token.into(),
            workspace:   workspace.into(),
            session_id:  session_id.into(),
            client_type: ClientType::Cli,
            client_name: "oma-cli".into(),
        }
    }
}

/// 已建立的实时连接
pub struct OmaClient {
    ready:    Ready,
    cmd_tx:   mpsc::Sender<ClientMessage>,
    event_rx: mpsc::Receiver<AgentEvent>,
    pump:     JoinHandle<()>,
}

impl OmaClient {
    /// 建立连接并完成握手，返回服务端就绪载荷
    pub async fn connect(opts: ConnectOptions) -> Result<Self> {
        let url = format!("{}/ws?token={}", ws_base(&opts.addr), opts.token);
        let (socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .with_context(|| format!("Failed to connect to oma daemon at {}", opts.addr))?;
        let (mut sink, mut stream) = socket.split();

        let handshake = ClientMessage::Connect {
            params: ConnectParams {
                client_id:   uuid::Uuid::new_v4().to_string(),
                workspace:   opts.workspace,
                session_id:  opts.session_id,
                client_type: opts.client_type,
                client_name: opts.client_name,
                version:     CLIENT_VERSION.to_string(),
            },
        };
        sink.send(Message::text(serde_json::to_string(&handshake)?))
            .await
            .context("Failed to send handshake")?;

        // 首包必须是 Ready；Error 表示服务端拒绝（例如会话不可用）
        let ready = loop {
            match stream.next().await {
                Some(Ok(Message::Text(txt))) => match serde_json::from_str::<ServerMessage>(&txt) {
                    Ok(ServerMessage::Ready { ready }) => break *ready,
                    Ok(ServerMessage::Error { message }) => bail!("server rejected connection: {}", message),
                    // Ready 之前不应有事件，忽略以保持健壮
                    Ok(ServerMessage::Event { .. }) => continue,
                    Err(e) => bail!("malformed handshake response: {}", e),
                },
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e).context("WebSocket error during handshake"),
                None => bail!("connection closed before handshake completed"),
            }
        };

        let (cmd_tx, cmd_rx) = mpsc::channel(64);
        let (event_tx, event_rx) = mpsc::channel(512);
        let pump = tokio::spawn(pump(sink, stream, cmd_rx, event_tx));

        Ok(Self {
            ready,
            cmd_tx,
            event_rx,
            pump,
        })
    }

    pub fn ready(&self) -> &Ready {
        &self.ready
    }

    /// 等待下一个实时事件；连接关闭时返回 None
    pub async fn next_event(&mut self) -> Option<AgentEvent> {
        self.event_rx.recv().await
    }

    /// 发送 Agent 指令
    pub async fn send_command(&self, command: AgentCommand) -> Result<()> {
        self.send(ClientMessage::Command { command }).await
    }

    /// 中止当前轮次并清空排队指令
    pub async fn cancel(&self) -> Result<()> {
        self.send(ClientMessage::Cancel {}).await
    }

    async fn send(&self, msg: ClientMessage) -> Result<()> {
        self.cmd_tx
            .send(msg)
            .await
            .map_err(|_| anyhow::anyhow!("connection is closed"))
    }

    /// 关闭连接并等待发送任务退出
    pub async fn shutdown(self) {
        drop(self.cmd_tx);
        let _ = self.pump.await;
    }
}

/// 单任务驱动读写：避免为 socket 加锁，断开时自然结束
async fn pump<S, R>(
    mut sink: S,
    mut stream: R,
    mut cmd_rx: mpsc::Receiver<ClientMessage>,
    event_tx: mpsc::Sender<AgentEvent>,
) where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
    R: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        tokio::select! {
            outgoing = cmd_rx.recv() => {
                let Some(msg) = outgoing else {
                    let _ = sink.close().await;
                    return;
                };
                let Ok(json) = serde_json::to_string(&msg) else {
                    continue;
                };
                if sink.send(Message::text(json)).await.is_err() {
                    return;
                }
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(txt))) => {
                        let Ok(parsed) = serde_json::from_str::<ServerMessage>(&txt) else {
                            continue;
                        };
                        match parsed {
                            ServerMessage::Event { event } => {
                                if event_tx.send(*event).await.is_err() {
                                    return; // 消费端已退出
                                }
                            }
                            // Ready 只出现在握手阶段；错误在事件流里也可能出现
                            ServerMessage::Error { message } => {
                                if event_tx.send(AgentEvent::Error { message }).await.is_err() {
                                    return;
                                }
                            }
                            ServerMessage::Ready { .. } => {}
                        }
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(_)) | None => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 两种形态都要接受：命令行与 `settings.json` 是裸 `host:port`，
    /// `client.json` 的连接项是 Web 界面写入的完整 URL。
    #[test]
    fn test_http_base_accepts_bare_and_full_urls() {
        assert_eq!(http_base("127.0.0.1:17431"), "http://127.0.0.1:17431");
        assert_eq!(http_base("http://127.0.0.1:9633"), "http://127.0.0.1:9633");
        assert_eq!(http_base("https://oma.example.com"), "https://oma.example.com");
        // 结尾斜杠与首尾空白不该影响结果
        assert_eq!(http_base("  127.0.0.1:17431/  "), "http://127.0.0.1:17431");
        assert_eq!(http_base("http://127.0.0.1:9633/"), "http://127.0.0.1:9633");
        // IPv6 字面量：方括号里含冒号，但不含 `://`，不该被当 scheme
        assert_eq!(http_base("[::1]:17431"), "http://[::1]:17431");
    }

    /// WS 侧要按 scheme 换协议，且不能把 `wss` 降级成 `ws`。
    #[test]
    fn test_ws_base_maps_scheme() {
        assert_eq!(ws_base("127.0.0.1:17431"), "ws://127.0.0.1:17431");
        assert_eq!(ws_base("http://127.0.0.1:9633"), "ws://127.0.0.1:9633");
        assert_eq!(ws_base("https://oma.example.com"), "wss://oma.example.com");
        // 已经是 ws 形态时原样保留
        assert_eq!(ws_base("ws://127.0.0.1:17431"), "ws://127.0.0.1:17431");
        assert_eq!(ws_base("wss://oma.example.com"), "wss://oma.example.com");
        assert_eq!(ws_base("http://127.0.0.1:9633/"), "ws://127.0.0.1:9633");
    }

    /// 回归：早期实现无条件前缀 `http://`，完整 URL 会被拼成 `http://http://…`，
    /// reqwest 把 `http` 当主机名，TUI 因此连不上任何东西。
    #[test]
    fn test_full_url_is_not_double_prefixed() {
        let base = http_base("http://127.0.0.1:9633");
        assert!(!base.contains("http://http"), "scheme 被重复前缀: {base}");
        assert_eq!(base.matches("://").count(), 1, "出现了多个 scheme: {base}");
    }
}
