//! 会话持久化：pi 风格的 JSONL 树文件。
//!
//! 每次会话一个目录 `<base_dir>/sessions/<session_id>/`，其中 `session.jsonl`
//! 是对话树，`attachments/` 存放附件。文件首行是 `type=session` 的头部，
//! 其后每行一个带 `id`/`parentId` 的条目，靠 `parentId` 形成树，
//! 从而在不新建文件的前提下原地分叉。
//!
//! 条目类型与 pi 对齐：`message`（对话消息）、`session_info`（标题）、
//! `model_change`、`thinking_level_change`，以及承载 oma 运行时状态的
//! `custom` 条目（当前叶子、上下文占用、agent/审批模式）。`custom` 条目
//! 与 pi 一样不参与 LLM 上下文。
//!
//! 说明：消息内容块仍使用 oma 契约里的 `Block` 序列化形态（`tool_use` /
//! `tool_result`），因此文件可被本仓库完整回读，但不是 pi 的逐字节格式。
//!
//! 读操作始终以磁盘为准（不缓存快照），写操作在同会话写锁内先重读再落盘，
//! 因此同一数据目录上的多个 `StorageManager` 实例也能彼此看到最新内容。

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{SecondsFormat, TimeZone};
use oma_contract::{Block, ChatMessage, Role};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// 存储层错误：调用方据此映射 HTTP 状态码，禁止靠字符串匹配判定类别。
#[derive(Debug)]
pub enum StorageError {
    /// session_id / 附件名等标识非法（含路径穿越尝试）
    InvalidId(String),
    NotFound(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::InvalidId(m) => write!(f, "invalid identifier: {}", m),
            StorageError::NotFound(m) => write!(f, "not found: {}", m),
            StorageError::Internal(e) => write!(f, "{:#}", e),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StorageError::Internal(e) => e.source(),
            _ => None,
        }
    }
}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Internal(e.into())
    }
}

impl From<serde_json::Error> for StorageError {
    fn from(e: serde_json::Error) -> Self {
        StorageError::Internal(e.into())
    }
}

pub type Result<T, E = StorageError> = std::result::Result<T, E>;

/// 仅允许字母/数字/连字符/下划线，且长度受限：
/// session_id 会被拼进文件系统路径，任何分隔符或 `..` 都可能导致越权读写/删除。
pub fn validate_session_id(session_id: &str) -> Result<()> {
    let ok = !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(StorageError::InvalidId(format!(
            "session_id {:?} must be 1..=128 chars of [A-Za-z0-9_-]",
            session_id
        )))
    }
}

/// 附件文件名：禁止路径分隔符与 `..`，避免逃逸出会话附件目录。
pub fn validate_attachment_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(StorageError::InvalidId(format!(
            "attachment name {:?} must be 1..=128 chars of [A-Za-z0-9._-] and not start with '.'",
            name
        )))
    }
}

/// 会话记录（列表与详情共用的元数据）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id:      String,
    pub workspace:       String,
    pub title:           String,
    pub active_model:    String,
    pub active_agent:    String,
    /// 会话推理等级（REASONING_LEVELS 之一；空 = 未设置，回退模型默认）
    #[serde(default)]
    pub reasoning_level: String,
    pub current_leaf_id: Option<String>,
    pub created_at:      i64,
    pub updated_at:      i64,
    /// 运行时状态：该会话当前是否有正在执行的轮次（不持久化，仅列表响应回填）
    #[serde(default)]
    pub is_running:      bool,
}

// =========================================================================
// JSONL 条目模型
// =========================================================================

const SESSION_VERSION: u32 = 3;
/// custom 条目类型：当前叶子
const CUSTOM_LEAF: &str = "oma.leaf";
/// custom 条目类型：上下文占用
const CUSTOM_CONTEXT_USAGE: &str = "oma.context_usage";
/// custom 条目类型：agent 设置
const CUSTOM_SETTINGS: &str = "oma.settings";

/// 会话头（文件首行，不参与树）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionHeader {
    #[serde(rename = "type")]
    kind:            String,
    version:         u32,
    id:              String,
    timestamp:       String,
    cwd:             String,
    #[serde(default)]
    model:           String,
    #[serde(default)]
    agent:           String,
    #[serde(default, rename = "reasoningLevel")]
    reasoning_level: String,
}

impl SessionHeader {
    fn new(
        session_id: &str,
        workspace: &str,
        active_model: &str,
        active_agent: &str,
        reasoning_level: &str,
        now_ms: i64,
    ) -> Self {
        Self {
            kind:            "session".to_string(),
            version:         SESSION_VERSION,
            id:              session_id.to_string(),
            timestamp:       iso_from_ms(now_ms),
            cwd:             workspace.to_string(),
            model:           active_model.to_string(),
            agent:           active_agent.to_string(),
            reasoning_level: reasoning_level.to_string(),
        }
    }
}

/// 树条目；`type` 为内部标签。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Entry {
    Message(MessageEntry),
    SessionInfo(SessionInfoEntry),
    ModelChange(ModelChangeEntry),
    ThinkingLevelChange(ThinkingLevelChangeEntry),
    Custom(CustomEntry),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageEntry {
    id:        String,
    #[serde(default)]
    parent_id: Option<String>,
    timestamp: String,
    message:   StoredMessage,
}

/// 落盘消息体：`id`/`parentId` 由所在条目承载，故此处只保留语义字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMessage {
    role:      Role,
    content:   Vec<Block>,
    timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfoEntry {
    id:        String,
    #[serde(default)]
    parent_id: Option<String>,
    timestamp: String,
    name:      String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelChangeEntry {
    id:        String,
    #[serde(default)]
    parent_id: Option<String>,
    timestamp: String,
    #[serde(default)]
    provider:  String,
    model_id:  String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThinkingLevelChangeEntry {
    id:             String,
    #[serde(default)]
    parent_id:      Option<String>,
    timestamp:      String,
    thinking_level: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CustomEntry {
    id:          String,
    #[serde(default)]
    parent_id:   Option<String>,
    timestamp:   String,
    custom_type: String,
    #[serde(default)]
    data:        serde_json::Value,
}

impl Entry {
    fn timestamp_ms(&self) -> i64 {
        let ts = match self {
            Entry::Message(e) => &e.timestamp,
            Entry::SessionInfo(e) => &e.timestamp,
            Entry::ModelChange(e) => &e.timestamp,
            Entry::ThinkingLevelChange(e) => &e.timestamp,
            Entry::Custom(e) => &e.timestamp,
        };
        ms_from_iso(ts)
    }
}

fn iso_from_ms(ms: i64) -> String {
    chrono::Utc
        .timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn ms_from_iso(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.timestamp_millis())
        .unwrap_or(0)
}

/// 把 oma 的 "provider/model" 选择器拆成 (provider, modelId)。
fn split_model(selector: &str) -> (String, String) {
    match selector.split_once('/') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (String::new(), selector.to_string()),
    }
}

/// 把 (provider, modelId) 重新拼回 oma 选择器。
fn join_model(provider: &str, model_id: &str) -> String {
    if provider.is_empty() {
        model_id.to_string()
    } else {
        format!("{}/{}", provider, model_id)
    }
}

/// 生成条目 id（不进入 LLM 上下文，仅需唯一）。
fn new_entry_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// =========================================================================
// 单会话存储
// =========================================================================

struct SessionStore {
    dir:     PathBuf,
    header:  SessionHeader,
    entries: Vec<Entry>,
}

impl SessionStore {
    /// 从磁盘加载；文件不存在返回 None。
    fn load(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join("session.jsonl");
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        let mut header: Option<SessionHeader> = None;
        let mut entries = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let value: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // 单行损坏只跳过，避免整个会话不可读
            };
            let is_header = value.get("type").and_then(|t| t.as_str()) == Some("session");
            if is_header {
                if header.is_none() {
                    header = serde_json::from_value(value).ok();
                }
                continue;
            }
            if let Ok(entry) = serde_json::from_value::<Entry>(value) {
                entries.push(entry);
            }
        }
        let Some(header) = header else {
            return Ok(None);
        };
        Ok(Some(Self {
            dir: dir.to_path_buf(),
            header,
            entries,
        }))
    }

    fn jsonl_path(&self) -> PathBuf {
        self.dir.join("session.jsonl")
    }

    /// 追加一条条目：先落盘再进内存，写失败不留内存脏状态。
    fn append(&mut self, entry: Entry) -> Result<()> {
        let line = serde_json::to_string(&entry)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.jsonl_path())?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        self.entries.push(entry);
        Ok(())
    }

    /// 全量重写文件（删除子树等需要移除既有行的场景）。
    fn persist(&self) -> Result<()> {
        let mut out = String::new();
        out.push_str(&serde_json::to_string(&self.header)?);
        out.push('\n');
        for entry in &self.entries {
            out.push_str(&serde_json::to_string(entry)?);
            out.push('\n');
        }
        let path = self.jsonl_path();
        let tmp = path.with_file_name(".session.jsonl.tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(out.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// 从条目序列推导当前叶子：消息条目使叶子前移，`oma.leaf` 标记显式覆盖。
    fn leaf(&self) -> Option<String> {
        let mut leaf: Option<String> = None;
        for entry in &self.entries {
            match entry {
                Entry::Message(m) => leaf = Some(m.id.clone()),
                Entry::Custom(c) if c.custom_type == CUSTOM_LEAF => {
                    leaf = c
                        .data
                        .get("leafId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
                _ => {}
            }
        }
        leaf
    }

    fn messages(&self) -> Vec<ChatMessage> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Message(m) => Some(ChatMessage {
                    id:         m.id.clone(),
                    parent_id:  m.parent_id.clone(),
                    role:       m.message.role,
                    content:    m.message.content.clone(),
                    created_at: m.message.timestamp,
                }),
                _ => None,
            })
            .collect()
    }

    fn title(&self) -> String {
        self.entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                Entry::SessionInfo(s) => Some(s.name.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    fn context_usage(&self) -> Option<(usize, usize, usize)> {
        let data = self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Custom(c) if c.custom_type == CUSTOM_CONTEXT_USAGE => Some(&c.data),
            _ => None,
        })?;
        let tokens = data.get("tokens")?.as_u64()? as usize;
        let context_len = data.get("contextLen")?.as_u64()? as usize;
        let covered = data.get("covered")?.as_u64()? as usize;
        Some((tokens, context_len, covered))
    }

    fn record(&self) -> SessionRecord {
        let mut title = String::new();
        let mut model = self.header.model.clone();
        let mut agent = self.header.agent.clone();
        let mut reasoning = self.header.reasoning_level.clone();
        let created_at = ms_from_iso(&self.header.timestamp);
        let mut updated_at = created_at;

        for entry in &self.entries {
            updated_at = updated_at.max(entry.timestamp_ms());
            match entry {
                Entry::SessionInfo(s) => title = s.name.clone(),
                Entry::ModelChange(m) => model = join_model(&m.provider, &m.model_id),
                Entry::ThinkingLevelChange(t) => reasoning = t.thinking_level.clone(),
                Entry::Custom(c) if c.custom_type == CUSTOM_SETTINGS => {
                    if let Some(a) = c.data.get("agent").and_then(|v| v.as_str()) {
                        agent = a.to_string();
                    }
                }
                _ => {}
            }
        }

        SessionRecord {
            session_id: self.header.id.clone(),
            workspace: self.header.cwd.clone(),
            title,
            active_model: model,
            active_agent: agent,
            reasoning_level: reasoning,
            current_leaf_id: self.leaf(),
            created_at,
            updated_at,
            is_running: false,
        }
    }
}

// =========================================================================
// 存储引擎
// =========================================================================

/// 会话存储引擎：JSONL 树文件 + 每会话写锁。
#[derive(Clone)]
pub struct StorageManager {
    base_dir:  PathBuf,
    /// 每会话一把写锁，串行化同进程内并发写
    locks:     Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    /// 写锁创建串行化（跨 await 持有，故用 tokio Mutex）
    open_lock: Arc<tokio::sync::Mutex<()>>,
}

impl StorageManager {
    /// 初始化存储引擎，并在 base_dir 下建立会话目录
    pub async fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(base_dir.join("sessions"))
            .await
            .map_err(|e| {
                StorageError::Internal(anyhow::anyhow!(
                    "failed to create base dir {}: {}",
                    base_dir.display(),
                    e
                ))
            })?;

        Ok(Self {
            base_dir,
            locks: Arc::new(RwLock::new(HashMap::new())),
            open_lock: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn session_dir(&self, session_id: &str) -> Result<PathBuf> {
        validate_session_id(session_id)?;
        Ok(self.base_dir.join("sessions").join(session_id))
    }

    pub fn attachments_dir(&self, session_id: &str) -> Result<PathBuf> {
        Ok(self.session_dir(session_id)?.join("attachments"))
    }

    /// 附件绝对路径（名称经校验，无法逃逸出附件目录）
    pub fn attachment_path(&self, session_id: &str, name: &str) -> Result<PathBuf> {
        validate_attachment_name(name)?;
        Ok(self.attachments_dir(session_id)?.join(name))
    }

    /// 读取磁盘上的会话快照（无则 None）。
    fn read_store(&self, session_id: &str) -> Result<Option<SessionStore>> {
        SessionStore::load(&self.session_dir(session_id)?)
    }

    /// 取得该会话的写锁（不存在则创建）。
    async fn write_lock(&self, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        if let Some(l) = self.locks.read().get(session_id) {
            return l.clone();
        }
        let _guard = self.open_lock.lock().await;
        if let Some(l) = self.locks.read().get(session_id) {
            return l.clone();
        }
        let l = Arc::new(tokio::sync::Mutex::new(()));
        self.locks.write().insert(session_id.to_string(), l.clone());
        l
    }

    /// 在写锁内重读最新会话、执行变更并落盘。
    ///
    /// 数据始终以磁盘为准：另一个 `StorageManager` 实例写入的条目也能被读到，
    /// 不会因内存快照过期而丢数据。
    async fn with_store_mut<T>(&self, session_id: &str, f: impl FnOnce(&mut SessionStore) -> Result<T>) -> Result<T> {
        validate_session_id(session_id)?;
        let lock = self.write_lock(session_id).await;
        let _guard = lock.lock().await;
        let mut store = self
            .read_store(session_id)?
            .ok_or_else(|| StorageError::NotFound(format!("session {}", session_id)))?;
        f(&mut store)
    }

    /// 创建会话；已存在时返回既有记录（幂等，不覆盖对话）。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_session(
        &self,
        session_id: &str,
        workspace: &str,
        title: &str,
        active_model: &str,
        active_agent: &str,
        reasoning_level: &str,
    ) -> Result<SessionRecord> {
        validate_session_id(session_id)?;
        let dir = self.session_dir(session_id)?;
        tokio::fs::create_dir_all(dir.join("attachments")).await?;

        if let Some(existing) = self.read_store(session_id)? {
            return Ok(existing.record());
        }

        let now = chrono::Utc::now().timestamp_millis();
        let header = SessionHeader::new(session_id, workspace, active_model, active_agent, reasoning_level, now);
        let mut entries = Vec::new();
        if !title.is_empty() {
            entries.push(Entry::SessionInfo(SessionInfoEntry {
                id:        new_entry_id(),
                parent_id: None,
                timestamp: iso_from_ms(now),
                name:      title.to_string(),
            }));
        }
        let store = SessionStore { dir, header, entries };
        store.persist()?;
        Ok(store.record())
    }

    /// 获取会话记录
    pub async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        Ok(self.read_store(session_id)?.map(|s| s.record()))
    }

    /// 列出指定 workspace（或全部）会话，按更新时间倒序。
    pub async fn list_sessions(&self, workspace: Option<&str>) -> Result<Vec<SessionRecord>> {
        let sessions_root = self.base_dir.join("sessions");
        let mut records = Vec::new();
        let mut dir = match tokio::fs::read_dir(&sessions_root).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = dir.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            if let Some(store) = SessionStore::load(&entry.path())? {
                let rec = store.record();
                if workspace.is_none() || workspace == Some(rec.workspace.as_str()) {
                    records.push(rec);
                }
            }
        }
        records.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        Ok(records)
    }

    /// 删除会话及其物理目录
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let dir = self.session_dir(session_id)?;
        if !dir.exists() {
            return Err(StorageError::NotFound(format!("session {}", session_id)));
        }
        self.locks.write().remove(session_id);
        tokio::fs::remove_dir_all(&dir).await.map_err(|e| {
            StorageError::Internal(anyhow::anyhow!("failed to remove session dir {}: {}", dir.display(), e))
        })?;
        Ok(())
    }

    /// 重命名会话
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        self.with_store_mut(session_id, |store| {
            let now = chrono::Utc::now().timestamp_millis();
            let parent = store.leaf();
            store.append(Entry::SessionInfo(SessionInfoEntry {
                id:        new_entry_id(),
                parent_id: parent,
                timestamp: iso_from_ms(now),
                name:      title.to_string(),
            }))
        })
        .await
    }

    /// 仅当会话标题仍为空时写入（用于模型自动命名）。
    pub async fn set_title_if_empty(&self, session_id: &str, title: &str) -> Result<bool> {
        if title.trim().is_empty() {
            return Ok(false);
        }
        self.with_store_mut(session_id, |store| {
            if !store.title().is_empty() {
                return Ok(false);
            }
            let now = chrono::Utc::now().timestamp_millis();
            let parent = store.leaf();
            store.append(Entry::SessionInfo(SessionInfoEntry {
                id:        new_entry_id(),
                parent_id: parent,
                timestamp: iso_from_ms(now),
                name:      title.to_string(),
            }))?;
            Ok(true)
        })
        .await
    }

    /// 更新会话配置 (model, agent, reasoning_level)
    pub async fn update_session_settings(
        &self,
        session_id: &str,
        active_model: Option<&str>,
        active_agent: Option<&str>,
        reasoning_level: Option<&str>,
    ) -> Result<()> {
        self.with_store_mut(session_id, |store| {
            let now = chrono::Utc::now().timestamp_millis();
            let parent = store.leaf();
            if let Some(model) = active_model {
                let (provider, model_id) = split_model(model);
                store.append(Entry::ModelChange(ModelChangeEntry {
                    id: new_entry_id(),
                    parent_id: parent.clone(),
                    timestamp: iso_from_ms(now),
                    provider,
                    model_id,
                }))?;
            }
            if let Some(level) = reasoning_level {
                store.append(Entry::ThinkingLevelChange(ThinkingLevelChangeEntry {
                    id:             new_entry_id(),
                    parent_id:      parent.clone(),
                    timestamp:      iso_from_ms(now),
                    thinking_level: level.to_string(),
                }))?;
            }
            if let Some(a) = active_agent {
                let mut data = serde_json::Map::new();
                data.insert("agent".into(), serde_json::Value::String(a.to_string()));
                store.append(Entry::Custom(CustomEntry {
                    id:          new_entry_id(),
                    parent_id:   parent,
                    timestamp:   iso_from_ms(now),
                    custom_type: CUSTOM_SETTINGS.to_string(),
                    data:        serde_json::Value::Object(data),
                }))?;
            }
            Ok(())
        })
        .await
    }

    /// 追加消息并使其成为当前叶子。
    pub async fn append_message(
        &self,
        session_id: &str,
        message: &ChatMessage,
        _input_tokens: usize,
        _output_tokens: usize,
    ) -> Result<()> {
        self.with_store_mut(session_id, |store| {
            store.append(Entry::Message(MessageEntry {
                id:        message.id.clone(),
                parent_id: message.parent_id.clone(),
                timestamp: iso_from_ms(message.created_at),
                message:   StoredMessage {
                    role:      message.role,
                    content:   message.content.clone(),
                    timestamp: message.created_at,
                },
            }))
        })
        .await
    }

    /// 切换分支：以显式 `oma.leaf` 标记持久化当前叶子。
    pub async fn switch_branch(&self, session_id: &str, leaf_id: &str) -> Result<()> {
        self.with_store_mut(session_id, |store| {
            if !store.messages().iter().any(|m| m.id == leaf_id) {
                return Err(StorageError::NotFound(format!(
                    "message {} in session {}",
                    leaf_id, session_id
                )));
            }
            let now = chrono::Utc::now().timestamp_millis();
            store.append(Entry::Custom(CustomEntry {
                id:          new_entry_id(),
                parent_id:   Some(leaf_id.to_string()),
                timestamp:   iso_from_ms(now),
                custom_type: CUSTOM_LEAF.to_string(),
                data:        serde_json::json!({ "leafId": leaf_id }),
            }))
        })
        .await
    }

    /// 当前激活叶子 ID
    pub async fn current_leaf(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self.read_store(session_id)?.and_then(|s| s.leaf()))
    }

    /// 记录最近一次请求的上下文占用。
    ///
    /// `covered` 为该请求发出时的历史条数：重启后历史可能又追加了消息，
    /// 只有知道上次覆盖到哪里，才能只对新增部分做估算。
    pub async fn set_context_usage(
        &self,
        session_id: &str,
        tokens: usize,
        context_len: usize,
        covered: usize,
    ) -> Result<()> {
        self.with_store_mut(session_id, |store| {
            let now = chrono::Utc::now().timestamp_millis();
            let parent = store.leaf();
            store.append(Entry::Custom(CustomEntry {
                id:          new_entry_id(),
                parent_id:   parent,
                timestamp:   iso_from_ms(now),
                custom_type: CUSTOM_CONTEXT_USAGE.to_string(),
                data:        serde_json::json!({
                    "tokens": tokens,
                    "contextLen": context_len,
                    "covered": covered,
                }),
            }))
        })
        .await
    }

    /// 读取上次记录的上下文占用（tokens, context_len, covered）。
    pub async fn context_usage(&self, session_id: &str) -> Result<Option<(usize, usize, usize)>> {
        Ok(self.read_store(session_id)?.and_then(|s| s.context_usage()))
    }

    /// 获取激活分支的线性消息链（从 root 到指定/当前 leaf）
    pub async fn get_linear_messages(&self, session_id: &str, leaf_id: Option<&str>) -> Result<Vec<ChatMessage>> {
        let Some(store) = self.read_store(session_id)? else {
            return Ok(Vec::new());
        };
        let target = match leaf_id {
            Some(l) => Some(l.to_string()),
            None => store.leaf(),
        };
        let Some(leaf_id) = target else {
            return Ok(Vec::new());
        };

        let mut msg_map: HashMap<String, ChatMessage> = store
            .messages()
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();

        let mut linear = Vec::new();
        let mut curr = Some(leaf_id);
        while let Some(cid) = curr {
            if let Some(msg) = msg_map.remove(&cid) {
                curr = msg.parent_id.clone();
                linear.push(msg);
            } else {
                break;
            }
        }
        linear.reverse();
        Ok(linear)
    }

    /// 获取全部消息（用于构建完整树状视图）
    pub async fn get_all_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        Ok(self
            .read_store(session_id)?
            .map(|s| s.messages())
            .unwrap_or_default())
    }

    /// 删除指定消息及其整棵子树。
    /// 若当前叶子位于被删子树内，则回退到被删消息的父节点（可能为空）。
    /// 返回 (被删除的消息 ID 列表, 删除后的当前叶子)。
    pub async fn delete_message_subtree(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<(Vec<String>, Option<String>)> {
        self.with_store_mut(session_id, |store| {
            // 收集消息父子关系与所处叶子
            let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
            let mut deleted_parent: Option<String> = None;
            let mut exists = false;
            for msg in store.messages() {
                if msg.id == message_id {
                    deleted_parent = msg.parent_id.clone();
                    exists = true;
                }
                children
                    .entry(msg.parent_id.clone())
                    .or_default()
                    .push(msg.id);
            }
            if !exists {
                return Err(StorageError::NotFound(format!(
                    "message {} in session {}",
                    message_id, session_id
                )));
            }

            // BFS 收集子树（父先子后）
            let mut deleted = Vec::new();
            let mut stack = vec![message_id.to_string()];
            while let Some(cur) = stack.pop() {
                deleted.push(cur.clone());
                if let Some(kids) = children.remove(&Some(cur)) {
                    stack.extend(kids);
                }
            }

            let current_leaf = store.leaf();
            let new_leaf = match current_leaf {
                Some(l) if deleted.contains(&l) => deleted_parent.clone(),
                other => other,
            };

            // 过滤掉被删条目（及指向它们的叶子标记），再显式落一次新叶子
            let removed: std::collections::HashSet<&String> = deleted.iter().collect();
            store.entries.retain(|entry| match entry {
                Entry::Message(m) => !removed.contains(&m.id),
                Entry::Custom(c) if c.custom_type == CUSTOM_LEAF => c
                    .data
                    .get("leafId")
                    .and_then(|v| v.as_str())
                    .is_none_or(|id| !deleted.iter().any(|d| d == id)),
                _ => true,
            });
            store.append(Entry::Custom(CustomEntry {
                id:          new_entry_id(),
                parent_id:   new_leaf.clone(),
                timestamp:   iso_from_ms(chrono::Utc::now().timestamp_millis()),
                custom_type: CUSTOM_LEAF.to_string(),
                data:        serde_json::json!({ "leafId": new_leaf }),
            }))?;
            store.persist()?;

            Ok((deleted, new_leaf))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, parent: Option<&str>, text: &str, at: i64) -> ChatMessage {
        ChatMessage {
            id:         id.into(),
            parent_id:  parent.map(|p| p.into()),
            role:       Role::User,
            content:    vec![Block::Text { text: text.into() }],
            created_at: at,
        }
    }

    #[tokio::test]
    async fn test_storage_crud_and_branching() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;

        // 1. 创建会话
        let s1 = storage
            .create_session("s_1", "/workspace/test", "Test Session", "claude-3-7", "task", "medium")
            .await?;
        assert_eq!(s1.session_id, "s_1");

        // 2. 列表查询
        let list = storage.list_sessions(Some("/workspace/test")).await?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Test Session");

        // 3. 追加消息 m1 (User)
        storage
            .append_message("s_1", &msg("m_1", None, "Hello", 1000), 10, 0)
            .await?;
        // 4. 追加消息 m2 (Assistant)
        storage
            .append_message("s_1", &msg("m_2", Some("m_1"), "Hi there", 2000), 0, 15)
            .await?;

        // 5. 校验当前线性链路 [m_1, m_2]
        let linear = storage.get_linear_messages("s_1", None).await?;
        assert_eq!(linear.len(), 2);
        assert_eq!(linear[0].id, "m_1");
        assert_eq!(linear[1].id, "m_2");

        // 6. 分叉：从 m_1 分叉产生 m_3
        storage
            .append_message("s_1", &msg("m_3", Some("m_1"), "Alternative branch", 3000), 0, 20)
            .await?;
        let linear_fork = storage.get_linear_messages("s_1", None).await?;
        assert_eq!(linear_fork.len(), 2);
        assert_eq!(linear_fork[1].id, "m_3");

        // 切换回 m_2 分支
        storage.switch_branch("s_1", "m_2").await?;
        let linear_switched = storage.get_linear_messages("s_1", None).await?;
        assert_eq!(linear_switched.len(), 2);
        assert_eq!(linear_switched[1].id, "m_2");

        // 7. 删除会话
        storage.delete_session("s_1").await?;
        assert!(storage.get_session("s_1").await?.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_delete_message_subtree() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_del", "/w", "Del", "m", "task", "medium")
            .await?;

        // 树：m1 → m2 → m3（活跃链），m1 → m4（兄弟分支）
        for (id, p) in [("m1", None), ("m2", Some("m1")), ("m3", Some("m2")), ("m4", Some("m1"))] {
            storage
                .append_message("s_del", &msg(id, p, id, 0), 0, 0)
                .await?;
        }

        // 删除 m2 子树：{m2, m3}；当前叶子 m4 不在子树内 → 保持不变
        let (deleted, leaf) = storage.delete_message_subtree("s_del", "m2").await?;
        assert_eq!(deleted, vec!["m2", "m3"]);
        assert_eq!(leaf.as_deref(), Some("m4"));
        let all = storage.get_all_messages("s_del").await?;
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|m| m.id != "m2" && m.id != "m3"));

        // 删除 m4：当前叶子在子树内 → 回退到父节点 m1
        let (deleted, leaf) = storage.delete_message_subtree("s_del", "m4").await?;
        assert_eq!(deleted, vec!["m4"]);
        assert_eq!(leaf.as_deref(), Some("m1"));
        assert_eq!(storage.get_linear_messages("s_del", None).await?.len(), 1);

        // 删除不存在的消息报错
        assert!(matches!(
            storage.delete_message_subtree("s_del", "nope").await,
            Err(StorageError::NotFound(_))
        ));

        Ok(())
    }

    /// session_id 会被拼进文件系统路径：必须拒绝任何分隔符与 `..`。
    #[tokio::test]
    async fn test_session_id_path_traversal_rejected() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        let victim = tmp.path().join("victim");
        std::fs::create_dir_all(&victim)?;
        std::fs::write(victim.join("keep.txt"), b"keep")?;

        for evil in ["../victim", "..%2Fvictim", "a/b", ".", "..", "", "a b", "sess\\x"] {
            assert!(
                matches!(storage.delete_session(evil).await, Err(StorageError::InvalidId(_))),
                "delete_session must reject {:?}",
                evil
            );
            assert!(
                matches!(
                    storage.get_linear_messages(evil, None).await,
                    Err(StorageError::InvalidId(_))
                ),
                "get_linear_messages must reject {:?}",
                evil
            );
            assert!(
                matches!(storage.session_dir(evil), Err(StorageError::InvalidId(_))),
                "session_dir must reject {:?}",
                evil
            );
        }
        assert!(victim.join("keep.txt").exists(), "victim dir must be untouched");

        storage
            .create_session("sess_OK-1", "/w", "T", "m", "task", "medium")
            .await?;
        storage.delete_session("sess_OK-1").await?;
        Ok(())
    }

    /// 自动命名必须遵守「用户填写优先」：已非空则不得覆盖。
    #[tokio::test]
    async fn test_set_title_if_empty_respects_existing_title() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;

        storage
            .create_session("s_user", "/w", "我的标题", "m", "task", "medium")
            .await?;
        assert!(!storage.set_title_if_empty("s_user", "模型起的名字").await?);
        assert_eq!(storage.get_session("s_user").await?.unwrap().title, "我的标题");

        storage
            .create_session("s_auto", "/w", "", "m", "task", "medium")
            .await?;
        assert!(storage.set_title_if_empty("s_auto", "自动命名").await?);
        assert_eq!(storage.get_session("s_auto").await?.unwrap().title, "自动命名");

        assert!(!storage.set_title_if_empty("s_auto", "另一个名字").await?);
        assert_eq!(storage.get_session("s_auto").await?.unwrap().title, "自动命名");

        storage
            .create_session("s_blank", "/w", "", "m", "task", "medium")
            .await?;
        assert!(!storage.set_title_if_empty("s_blank", "   ").await?);
        assert_eq!(storage.get_session("s_blank").await?.unwrap().title, "");
        Ok(())
    }

    /// 上下文占用需可跨进程恢复：写入后读回一致，无记录时为 None。
    #[tokio::test]
    async fn test_context_usage_roundtrip() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_ctx", "/w", "T", "m", "task", "medium")
            .await?;

        assert!(storage.context_usage("s_ctx").await?.is_none());
        storage
            .set_context_usage("s_ctx", 123_456, 1_048_576, 42)
            .await?;
        assert_eq!(storage.context_usage("s_ctx").await?, Some((123_456, 1_048_576, 42)));

        // 覆盖写：取最后一条，而非累积
        storage.set_context_usage("s_ctx", 200, 1000, 7).await?;
        assert_eq!(storage.context_usage("s_ctx").await?, Some((200, 1000, 7)));

        // 重新加载（模拟重启）后仍可读回
        let reloaded = StorageManager::new(tmp.path()).await?;
        assert_eq!(reloaded.context_usage("s_ctx").await?, Some((200, 1000, 7)));
        Ok(())
    }

    /// 非法存储值不得让读取 panic，应退化为 None。
    #[tokio::test]
    async fn test_context_usage_tolerates_corrupt_value() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_bad", "/w", "T", "m", "task", "medium")
            .await?;

        // 直接向 JSONL 追加一条字段缺失的 context_usage 条目
        let dir = storage.session_dir("s_bad")?;
        let line = r#"{"type":"custom","id":"x","parentId":null,"timestamp":"2026-01-01T00:00:00.000Z","customType":"oma.context_usage","data":{"tokens":"oops"}}"#;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("session.jsonl"))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;

        let reloaded = StorageManager::new(tmp.path()).await?;
        assert!(reloaded.context_usage("s_bad").await?.is_none());
        Ok(())
    }

    /// 同一数据目录上的多个实例必须互相可见（多个 StorageManager 共享磁盘）。
    #[tokio::test]
    async fn test_cross_instance_visibility() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let a = StorageManager::new(tmp.path()).await?;
        let b = StorageManager::new(tmp.path()).await?;

        a.create_session("s_x", "/w", "T", "m", "task", "medium")
            .await?;
        b.append_message("s_x", &msg("m1", None, "from b", 1000), 0, 0)
            .await?;

        // a 未参与写入，但读取时应看到 b 的写入
        let seen = a.get_all_messages("s_x").await?;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, "m1");
        Ok(())
    }

    /// 附件名同样参与路径拼接，必须拒绝穿越。
    #[tokio::test]
    async fn test_attachment_name_validation() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_att", "/w", "T", "m", "task", "medium")
            .await?;

        assert!(storage.attachment_path("s_att", "shot-1.png").is_ok());
        for evil in ["../../etc/passwd", "..", ".hidden", "a/b", "a\\b", ""] {
            assert!(
                matches!(storage.attachment_path("s_att", evil), Err(StorageError::InvalidId(_))),
                "attachment name {:?} must be rejected",
                evil
            );
        }
        Ok(())
    }
}
