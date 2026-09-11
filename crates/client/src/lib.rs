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
    AgentCommand, AgentEvent, ApprovalDecision, ClientMessage, ClientType, ConnectParams, Ready, ServerMessage,
};
use serde::Deserialize;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::tungstenite::Message;

/// SDK 版本（随握手包上报）
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 会话索引记录（与 daemon 的 `SessionRecord` 对应）
#[derive(Debug, Clone, Deserialize)]
pub struct SessionRecord {
    pub session_id:   String,
    pub workspace:    String,
    pub title:        String,
    pub active_model: String,
    pub active_agent: String,
}

/// REST 客户端：会话列出与创建（实时交互走 `OmaClient`）
pub struct SessionApi {
    base:  String,
    token: String,
    http:  reqwest::Client,
}

impl SessionApi {
    /// `addr` 形如 `127.0.0.1:17431`（与 `--addr` 一致）
    pub fn new(addr: &str, token: &str) -> Self {
        Self {
            base:  format!("http://{}", addr.trim_end_matches('/')),
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
        let url = format!("ws://{}/ws?token={}", opts.addr, opts.token);
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
                    Ok(ServerMessage::Ready { ready }) => break ready,
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

    /// 回答权限审批请求
    pub async fn respond_approval(&self, request_id: &str, decision: ApprovalDecision) -> Result<()> {
        self.send(ClientMessage::Approval {
            response: oma_contract::ApprovalResponse {
                request_id: request_id.to_string(),
                decision,
            },
        })
        .await
    }

    /// 回答 ask 工具的提问
    pub async fn respond_ask(&self, response: oma_contract::AskResponse) -> Result<()> {
        self.send(ClientMessage::Ask { response }).await
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
                                if event_tx.send(event).await.is_err() {
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
