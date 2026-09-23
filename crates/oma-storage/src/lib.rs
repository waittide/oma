//! 会话持久化：双层 SQLite（全局索引库 + 每会话库）。
//!
//! 布局与语义：
//! - 全局索引库 `<base_dir>/oma.db` 的 `sessions_index` 表保存会话元数据
//!   （workspace / 标题 / 模型 / agent / 推理等级 / 当前叶子 /
//!   时间戳）。列表与详情只查这张表，不必打开任何会话库。
//! - 每次会话一个目录 `<base_dir>/sessions/<session_id>/`，其中 `session.db`
//!   是对话库：`messages` 保存消息树（`parent_id` 成树，故可在不新建文件的
//!   前提下原地分叉），`session_meta` 保存运行时状态（当前叶子、上下文占用、
//!   标题副本）。`attachments/` 存放附件，附件本体始终留在磁盘，不入库。
//! - 当前叶子在会话库与全局索引两处各记一份：前者是树的真相，后者让列表
//!   响应无需打开会话库。
//!
//! 并发模型：每会话两套连接池——写池 `max_connections = 1` 严格串行，
//! 读池只读并行，读写互不阻塞；库以 WAL 打开，`busy_timeout` 容忍跨进程
//! 短暂争用。因此同一数据目录上的多个 `StorageManager` 实例能互相看到
//! 最新提交。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use oma_contract::{Block, ChatMessage, Role, TokenUsage, WorkspaceRecord};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

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

impl From<sqlx::Error> for StorageError {
    fn from(e: sqlx::Error) -> Self {
        StorageError::Internal(e.into())
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

/// 全局索引会话记录（列表与详情共用的元数据）
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

/// 单会话连接池：写池串行（max_connections = 1），读池并行只读，读写互不阻塞。
#[derive(Clone)]
struct SessionPools {
    write: SqlitePool,
    read:  SqlitePool,
}

/// 双层 SQLite 存储引擎管理器
#[derive(Clone)]
pub struct StorageManager {
    base_dir:    PathBuf,
    index_pool:  SqlitePool,
    /// 已打开的会话连接池缓存；避免每次读写都重建连接与执行 DDL
    pools:       Arc<RwLock<HashMap<String, SessionPools>>>,
    /// 池创建串行化锁（跨 await 持有，故用 tokio Mutex）
    create_lock: Arc<tokio::sync::Mutex<()>>,
}

fn session_connect_options(db_path: &Path, read_only: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(!read_only)
        .read_only(read_only)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_millis(5000))
        .foreign_keys(true)
}

/// 校验全局索引表结构符合当前版本。
///
/// 本版本不做自动迁移：`CREATE TABLE IF NOT EXISTS` 不会给已存在的旧表补列，
/// 静默容忍会让缺失字段在运行时才报错（如 `no such column`）。
/// 这里在启动阶段直接拒绝，给出可操作的修复提示。
async fn ensure_index_schema(pool: &SqlitePool) -> Result<()> {
    const REQUIRED: [&str; 9] = [
        "session_id",
        "workspace",
        "title",
        "active_model",
        "active_agent",
        "reasoning_level",
        "current_leaf_id",
        "created_at",
        "updated_at",
    ];
    let rows = sqlx::query("PRAGMA table_info(sessions_index)")
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to inspect sessions_index: {}", e)))?;
    let present: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    let missing: Vec<&str> = REQUIRED
        .into_iter()
        .filter(|c| !present.iter().any(|p| p == c))
        .collect();
    if !missing.is_empty() {
        return Err(StorageError::Internal(anyhow::anyhow!(
            "sessions_index 缺少必要列 {:?}（当前列: {:?}）。\
             本版本不再自动迁移旧库，请先备份并重建该表，或手动执行 ALTER TABLE 补列。",
            missing,
            present
        )));
    }
    Ok(())
}

/// 校验全局索引库的 `workspaces` 表结构符合当前版本（与 `sessions_index` 同一口径）。
async fn ensure_workspaces_schema(pool: &SqlitePool) -> Result<()> {
    const REQUIRED: [&str; 2] = ["path", "created_at"];
    let rows = sqlx::query("PRAGMA table_info(workspaces)")
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to inspect workspaces: {}", e)))?;
    let present: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    let missing: Vec<&str> = REQUIRED
        .into_iter()
        .filter(|c| !present.iter().any(|p| p == c))
        .collect();
    if !missing.is_empty() {
        return Err(StorageError::Internal(anyhow::anyhow!(
            "workspaces 缺少必要列 {:?}（当前列: {:?}）。\
             本版本不再自动迁移旧库，请先备份并重建该表，或手动执行 ALTER TABLE 补列。",
            missing,
            present
        )));
    }
    Ok(())
}

/// 校验会话库的 `messages` 表结构符合当前版本。
///
/// 不做迁移：旧版本的表（如缺 `model` / `usage_json`，或只带
/// `input_tokens` / `output_tokens`）一律拒绝，而不是 `ALTER TABLE` 补列后继续用。
/// 失败信息给出缺哪些列、当前有哪些列，便于备份重建或手工改表。
async fn ensure_messages_schema(pool: &SqlitePool) -> Result<()> {
    const REQUIRED: [&str; 7] = [
        "id",
        "parent_id",
        "role",
        "blocks_json",
        "model",
        "usage_json",
        "created_at",
    ];
    let rows = sqlx::query("PRAGMA table_info(messages)")
        .fetch_all(pool)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to inspect messages: {}", e)))?;
    let present: Vec<String> = rows.iter().map(|r| r.get::<String, _>("name")).collect();
    let missing: Vec<&str> = REQUIRED
        .into_iter()
        .filter(|c| !present.iter().any(|p| p == c))
        .collect();
    if !missing.is_empty() {
        return Err(StorageError::Internal(anyhow::anyhow!(
            "messages 缺少必要列 {:?}（当前列: {:?}）。\
             本版本不做数据迁移：请用旧版导出内容后重建该会话库。",
            missing,
            present
        )));
    }
    Ok(())
}

/// 行 → 消息。
///
/// 任何一列读不出来都直接报错（带上消息 id），不做兜底：把坏行降级成「空内容」
/// 或「当成用户消息」只会让损坏在界面上看起来像正常历史，反而更难查。
fn message_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ChatMessage> {
    let id: String = row.get("id");
    let role_str: String = row.get("role");
    let role = Role::parse(&role_str)
        .ok_or_else(|| StorageError::Internal(anyhow::anyhow!("消息 {id} 的 role 无法识别: {role_str:?}")))?;
    let blocks_json: String = row.get("blocks_json");
    let content = serde_json::from_str::<Vec<Block>>(&blocks_json)
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("消息 {id} 的 content 无法解析: {e}")))?;
    let usage_json: Option<String> = row.get("usage_json");
    // 只有非 NULL 才要求可解析：用户消息本就记 NULL
    let usage = match usage_json {
        Some(raw) => Some(
            serde_json::from_str::<TokenUsage>(&raw)
                .map_err(|e| StorageError::Internal(anyhow::anyhow!("消息 {id} 的 usage 无法解析: {e}")))?,
        ),
        None => None,
    };
    Ok(ChatMessage {
        id,
        parent_id: row.get("parent_id"),
        role,
        content,
        created_at: row.get("created_at"),
        model: row.get("model"),
        usage,
    })
}

/// 上下文占用在 `session_meta` 中的落盘形态：`{"tokens":..,"contextLen":..,"covered":..}`。
fn context_usage_json(tokens: usize, context_len: usize, covered: usize) -> String {
    serde_json::json!({
        "tokens":     tokens,
        "contextLen": context_len,
        "covered":    covered,
    })
    .to_string()
}

/// 解析上下文占用，只认当前 JSON 形态。
///
/// 不再兼容 SQLite 时代的冒号串（`152012:1048576:249`）：格式不对就报错，
/// 而不是退化成「未知占用」——后者会让进度条悄悄空掉，看不出是数据坏了。
fn parse_context_usage(raw: &str) -> Result<(usize, usize, usize)> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        StorageError::Internal(anyhow::anyhow!(
            "session_meta.context_usage 不是合法 JSON: {e}（值: {raw:?}）"
        ))
    })?;
    let field = |name: &str| -> Result<usize> {
        value
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .map(|n| n as usize)
            .ok_or_else(|| {
                StorageError::Internal(anyhow::anyhow!(
                    "session_meta.context_usage 缺少字段 {name}（值: {raw:?}）"
                ))
            })
    };
    Ok((field("tokens")?, field("contextLen")?, field("covered")?))
}

impl StorageManager {
    /// 初始化存储引擎，并在 base_dir 下建立全局索引库
    pub async fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&base_dir).await.map_err(|e| {
            StorageError::Internal(anyhow::anyhow!(
                "failed to create base dir {}: {}",
                base_dir.display(),
                e
            ))
        })?;

        let index_db_path = base_dir.join("oma.db");
        let connect_opts = SqliteConnectOptions::new()
            .filename(&index_db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_millis(5000));

        let index_pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(connect_opts)
            .await
            .map_err(|e| {
                StorageError::Internal(anyhow::anyhow!(
                    "failed to connect to index db {}: {}",
                    index_db_path.display(),
                    e
                ))
            })?;

        // 初始化全局 sessions_index 表
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions_index (
                session_id      TEXT PRIMARY KEY,
                workspace       TEXT NOT NULL,
                title           TEXT NOT NULL,
                active_model    TEXT NOT NULL,
                active_agent    TEXT NOT NULL DEFAULT 'build',
                reasoning_level TEXT NOT NULL DEFAULT '',
                current_leaf_id TEXT,
                created_at      INTEGER NOT NULL,
                updated_at      INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_workspace ON sessions_index(workspace);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions_index(updated_at DESC);
            "#,
        )
        .execute(&index_pool)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to initialize sessions_index: {}", e)))?;
        ensure_index_schema(&index_pool).await?;

        // 工作区登记表：会话之外的「空工作区」也要能保留在侧栏
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS workspaces (
                path       TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL
            );
            "#,
        )
        .execute(&index_pool)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to initialize workspaces: {}", e)))?;
        ensure_workspaces_schema(&index_pool).await?;

        Ok(Self {
            base_dir,
            index_pool,
            pools: Arc::new(RwLock::new(HashMap::new())),
            create_lock: Arc::new(tokio::sync::Mutex::new(())),
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

    /// 打开（或复用缓存的）会话连接池
    async fn session_pools(&self, session_id: &str) -> Result<SessionPools> {
        if let Some(p) = self.pools.read().get(session_id) {
            return Ok(p.clone());
        }

        let _guard = self.create_lock.lock().await;
        if let Some(p) = self.pools.read().get(session_id) {
            return Ok(p.clone());
        }

        let s_dir = self.session_dir(session_id)?;
        let attachments = s_dir.join("attachments");
        tokio::fs::create_dir_all(&attachments).await.map_err(|e| {
            StorageError::Internal(anyhow::anyhow!(
                "failed to create session dir {}: {}",
                s_dir.display(),
                e
            ))
        })?;

        let db_path = s_dir.join("session.db");
        let write = SqlitePoolOptions::new()
            .max_connections(1) // 严格单写连接
            .connect_with(session_connect_options(&db_path, false))
            .await
            .map_err(|e| {
                StorageError::Internal(anyhow::anyhow!(
                    "failed to connect to session db {}: {}",
                    db_path.display(),
                    e
                ))
            })?;

        // 初始化会话专属表
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS session_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS messages (
                id          TEXT PRIMARY KEY,
                parent_id   TEXT,
                role        TEXT NOT NULL,
                blocks_json TEXT NOT NULL,
                model       TEXT,
                usage_json  TEXT,
                created_at  INTEGER NOT NULL,
                FOREIGN KEY(parent_id) REFERENCES messages(id)
            );
            CREATE INDEX IF NOT EXISTS idx_parent_id ON messages(parent_id);
            "#,
        )
        .execute(&write)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to initialize session tables: {}", e)))?;

        // 表结构必须是当前版本，否则拒绝打开（不做补列迁移）
        ensure_messages_schema(&write).await?;

        // 写池建库完成后再开只读池（文件必已存在）；读快照不阻塞写路径
        let read = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(session_connect_options(&db_path, true))
            .await
            .map_err(|e| {
                StorageError::Internal(anyhow::anyhow!(
                    "failed to open read-only session db {}: {}",
                    db_path.display(),
                    e
                ))
            })?;

        let pools = SessionPools { write, read };
        self.pools
            .write()
            .insert(session_id.to_string(), pools.clone());
        Ok(pools)
    }

    /// 关闭并移除缓存的会话连接池（删库前调用，避免残留句柄与 WAL 文件）
    async fn evict_pools(&self, session_id: &str) {
        let pools = self.pools.write().remove(session_id);
        if let Some(pools) = pools {
            pools.write.close().await;
            pools.read.close().await;
        }
    }

    /// 创建会话；已存在时返回既有记录（幂等，不覆盖对话与标题）。
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
        let now = chrono::Utc::now().timestamp_millis();

        // 会话所属工作区自动登记：任何有会话的工作区都必然出现在登记表里，
        // 客户端据此即可拿到完整的工作区清单，无需自行补记。
        self.register_workspace(workspace).await?;

        // ON CONFLICT DO NOTHING：并发/重复创建都不会覆盖既有会话
        let inserted = sqlx::query(
            r#"
            INSERT INTO sessions_index
            (session_id, workspace, title, active_model, active_agent, reasoning_level, current_leaf_id, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)
            ON CONFLICT(session_id) DO NOTHING
            "#,
        )
        .bind(session_id)
        .bind(workspace)
        .bind(title)
        .bind(active_model)
        .bind(active_agent)
        .bind(reasoning_level)
        .bind(now)
        .bind(now)
        .execute(&self.index_pool)
        .await?
        .rows_affected();

        // 无论新建还是复用，都确保会话目录与会话库存在
        let pools = self.session_pools(session_id).await?;

        if inserted == 0 {
            let row = sqlx::query(
                r#"
                SELECT session_id, workspace, title, active_model, active_agent, reasoning_level, current_leaf_id, created_at, updated_at
                FROM sessions_index
                WHERE session_id = ?
                "#,
            )
            .bind(session_id)
            .fetch_one(&self.index_pool)
            .await?;
            return Ok(Self::record_from_row(&row));
        }

        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('workspace', ?)")
            .bind(workspace)
            .execute(&pools.write)
            .await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('title', ?)")
            .bind(title)
            .execute(&pools.write)
            .await?;

        Ok(SessionRecord {
            session_id:      session_id.to_string(),
            workspace:       workspace.to_string(),
            title:           title.to_string(),
            active_model:    active_model.to_string(),
            active_agent:    active_agent.to_string(),
            reasoning_level: reasoning_level.to_string(),
            current_leaf_id: None,
            created_at:      now,
            updated_at:      now,
            is_running:      false,
        })
    }

    /// 获取会话记录
    pub async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        validate_session_id(session_id)?;
        let row = sqlx::query(
            r#"
            SELECT session_id, workspace, title, active_model, active_agent, reasoning_level, current_leaf_id, created_at, updated_at
            FROM sessions_index
            WHERE session_id = ?
            "#,
        )
        .bind(session_id)
        .fetch_optional(&self.index_pool)
        .await?;

        Ok(row.map(|row| Self::record_from_row(&row)))
    }

    fn record_from_row(row: &sqlx::sqlite::SqliteRow) -> SessionRecord {
        SessionRecord {
            session_id:      row.get("session_id"),
            workspace:       row.get("workspace"),
            title:           row.get("title"),
            active_model:    row.get("active_model"),
            active_agent:    row.get("active_agent"),
            reasoning_level: row.get("reasoning_level"),
            current_leaf_id: row.get("current_leaf_id"),
            created_at:      row.get("created_at"),
            updated_at:      row.get("updated_at"),
            is_running:      false,
        }
    }

    /// 列出指定 workspace（或全部）会话
    pub async fn list_sessions(&self, workspace: Option<&str>) -> Result<Vec<SessionRecord>> {
        let rows = if let Some(ws) = workspace {
            sqlx::query(
                r#"
                SELECT session_id, workspace, title, active_model, active_agent, reasoning_level, current_leaf_id, created_at, updated_at
                FROM sessions_index
                WHERE workspace = ?
                ORDER BY updated_at DESC
                "#,
            )
            .bind(ws)
            .fetch_all(&self.index_pool)
            .await?
        } else {
            sqlx::query(
                r#"
                SELECT session_id, workspace, title, active_model, active_agent, reasoning_level, current_leaf_id, created_at, updated_at
                FROM sessions_index
                ORDER BY updated_at DESC
                "#,
            )
            .fetch_all(&self.index_pool)
            .await?
        };

        Ok(rows.iter().map(Self::record_from_row).collect())
    }

    /// 登记工作区（幂等）。返回是否**新建**，调用方据此决定是否广播变更。
    pub async fn register_workspace(&self, path: &str) -> Result<bool> {
        let path = path.trim();
        if path.is_empty() {
            return Err(StorageError::InvalidId("workspace path must not be empty".into()));
        }
        let now = chrono::Utc::now().timestamp_millis();
        let inserted = sqlx::query("INSERT OR IGNORE INTO workspaces (path, created_at) VALUES (?, ?)")
            .bind(path)
            .bind(now)
            .execute(&self.index_pool)
            .await?
            .rows_affected();
        Ok(inserted > 0)
    }

    /// 全部已登记工作区，按登记时间升序（顺序稳定，便于下拉与默认选择）。
    pub async fn list_workspaces(&self) -> Result<Vec<WorkspaceRecord>> {
        let rows = sqlx::query("SELECT path, created_at FROM workspaces ORDER BY created_at ASC, path ASC")
            .fetch_all(&self.index_pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| WorkspaceRecord {
                path:       r.get("path"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    /// 移除工作区登记。只删名单，不触碰该工作区下的会话与磁盘文件。
    pub async fn delete_workspace(&self, path: &str) -> Result<()> {
        let affected = sqlx::query("DELETE FROM workspaces WHERE path = ?")
            .bind(path)
            .execute(&self.index_pool)
            .await?
            .rows_affected();
        if affected == 0 {
            return Err(StorageError::NotFound(format!("workspace {}", path)));
        }
        Ok(())
    }

    /// 删除会话及其物理目录
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let affected = sqlx::query("DELETE FROM sessions_index WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.index_pool)
            .await?
            .rows_affected();

        // 先释放连接池，再删除目录（否则 WAL/SHM 句柄会让目录残留）
        self.evict_pools(session_id).await;

        let s_dir = self.session_dir(session_id)?;
        if s_dir.exists() {
            tokio::fs::remove_dir_all(&s_dir).await.map_err(|e| {
                StorageError::Internal(anyhow::anyhow!(
                    "failed to remove session dir {}: {}",
                    s_dir.display(),
                    e
                ))
            })?;
        }

        if affected == 0 {
            return Err(StorageError::NotFound(format!("session {}", session_id)));
        }
        Ok(())
    }

    /// 重命名会话（同步更新索引与会话库 meta）
    pub async fn rename_session(&self, session_id: &str, title: &str) -> Result<()> {
        validate_session_id(session_id)?;
        let now = chrono::Utc::now().timestamp_millis();
        let affected = sqlx::query("UPDATE sessions_index SET title = ?, updated_at = ? WHERE session_id = ?")
            .bind(title)
            .bind(now)
            .bind(session_id)
            .execute(&self.index_pool)
            .await?
            .rows_affected();
        if affected == 0 {
            return Err(StorageError::NotFound(format!("session {}", session_id)));
        }

        let pools = self.session_pools(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('title', ?)")
            .bind(title)
            .execute(&pools.write)
            .await?;

        Ok(())
    }

    /// 仅当会话标题仍为空时写入（用于模型自动命名）。
    ///
    /// 返回是否实际写入：`UPDATE ... WHERE title = ''` 的原子性保证「用户已手动
    /// 填写/重命名」不会被并发到达的自动命名覆盖。
    pub async fn set_title_if_empty(&self, session_id: &str, title: &str) -> Result<bool> {
        validate_session_id(session_id)?;
        if title.trim().is_empty() {
            return Ok(false);
        }
        let now = chrono::Utc::now().timestamp_millis();
        let affected =
            sqlx::query("UPDATE sessions_index SET title = ?, updated_at = ? WHERE session_id = ? AND title = ''")
                .bind(title)
                .bind(now)
                .bind(session_id)
                .execute(&self.index_pool)
                .await?
                .rows_affected();
        if affected == 0 {
            return Ok(false);
        }

        let pools = self.session_pools(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('title', ?)")
            .bind(title)
            .execute(&pools.write)
            .await?;

        Ok(true)
    }

    /// 更新会话配置 (model, agent, reasoning_level)
    pub async fn update_session_settings(
        &self,
        session_id: &str,
        active_model: Option<&str>,
        active_agent: Option<&str>,
        reasoning_level: Option<&str>,
    ) -> Result<()> {
        validate_session_id(session_id)?;
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(m) = active_model {
            sqlx::query("UPDATE sessions_index SET active_model = ?, updated_at = ? WHERE session_id = ?")
                .bind(m)
                .bind(now)
                .bind(session_id)
                .execute(&self.index_pool)
                .await?;
        }
        if let Some(a) = active_agent {
            sqlx::query("UPDATE sessions_index SET active_agent = ?, updated_at = ? WHERE session_id = ?")
                .bind(a)
                .bind(now)
                .bind(session_id)
                .execute(&self.index_pool)
                .await?;
        }
        if let Some(level) = reasoning_level {
            sqlx::query("UPDATE sessions_index SET reasoning_level = ?, updated_at = ? WHERE session_id = ?")
                .bind(level)
                .bind(now)
                .bind(session_id)
                .execute(&self.index_pool)
                .await?;
        }
        Ok(())
    }

    /// 追加消息至树状表并更新当前叶子节点
    pub async fn append_message(&self, session_id: &str, message: &ChatMessage) -> Result<()> {
        let pools = self.session_pools(session_id).await?;
        let blocks_json = serde_json::to_string(&message.content)?;
        let usage_json = message
            .usage
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        sqlx::query(
            r#"
            INSERT INTO messages
            (id, parent_id, role, blocks_json, model, usage_json, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&message.id)
        .bind(&message.parent_id)
        .bind(message.role.as_str())
        .bind(&blocks_json)
        .bind(message.model.as_deref())
        .bind(usage_json.as_deref())
        .bind(message.created_at)
        .execute(&pools.write)
        .await?;

        self.set_current_leaf(session_id, &message.id).await?;

        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query("UPDATE sessions_index SET current_leaf_id = ?, updated_at = ? WHERE session_id = ?")
            .bind(&message.id)
            .bind(now)
            .bind(session_id)
            .execute(&self.index_pool)
            .await?;

        Ok(())
    }

    /// 会话库内记录当前叶子（session_meta）
    async fn set_current_leaf(&self, session_id: &str, leaf_id: &str) -> Result<()> {
        let pools = self.session_pools(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('current_leaf_id', ?)")
            .bind(leaf_id)
            .execute(&pools.write)
            .await?;
        Ok(())
    }

    /// 切换分支：更新激活的叶子节点 ID
    pub async fn switch_branch(&self, session_id: &str, leaf_id: &str) -> Result<()> {
        let pools = self.session_pools(session_id).await?;
        let exists = sqlx::query("SELECT 1 FROM messages WHERE id = ?")
            .bind(leaf_id)
            .fetch_optional(&pools.read)
            .await?
            .is_some();
        if !exists {
            return Err(StorageError::NotFound(format!(
                "message {} in session {}",
                leaf_id, session_id
            )));
        }

        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('current_leaf_id', ?)")
            .bind(leaf_id)
            .execute(&pools.write)
            .await?;

        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query("UPDATE sessions_index SET current_leaf_id = ?, updated_at = ? WHERE session_id = ?")
            .bind(leaf_id)
            .bind(now)
            .bind(session_id)
            .execute(&self.index_pool)
            .await?;

        Ok(())
    }

    /// 当前激活叶子 ID
    pub async fn current_leaf(&self, session_id: &str) -> Result<Option<String>> {
        let pools = self.session_pools(session_id).await?;
        let row = sqlx::query("SELECT value FROM session_meta WHERE key = 'current_leaf_id'")
            .fetch_optional(&pools.read)
            .await?;
        Ok(row.map(|r| r.get::<String, _>("value")))
    }

    /// 记录最近一次请求的上下文占用。
    ///
    /// `covered` 为该请求发出时的历史条数：重启后历史可能又追加了消息，
    /// 只有知道上次覆盖到哪里，才能只对新增部分做估算（不能拿整段历史重估，
    /// 启发式折算对代码/中文会明显高估）。
    pub async fn set_context_usage(
        &self,
        session_id: &str,
        tokens: usize,
        context_len: usize,
        covered: usize,
    ) -> Result<()> {
        let pools = self.session_pools(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('context_usage', ?)")
            .bind(context_usage_json(tokens, context_len, covered))
            .execute(&pools.write)
            .await?;
        Ok(())
    }

    /// 读取上次记录的上下文占用（tokens, context_len, covered）。
    /// 无记录时返回 None；有记录但格式不合法则报错（不做兼容降级）。
    pub async fn context_usage(&self, session_id: &str) -> Result<Option<(usize, usize, usize)>> {
        let pools = self.session_pools(session_id).await?;
        let row = sqlx::query("SELECT value FROM session_meta WHERE key = 'context_usage'")
            .fetch_optional(&pools.read)
            .await?;
        let Some(raw) = row.map(|r| r.get::<String, _>("value")) else {
            return Ok(None);
        };
        parse_context_usage(&raw).map(Some)
    }

    /// 获取激活分支的线性消息链（从 root 到指定/当前 leaf_id）
    pub async fn get_linear_messages(&self, session_id: &str, leaf_id: Option<&str>) -> Result<Vec<ChatMessage>> {
        let pools = self.session_pools(session_id).await?;
        let target_leaf = match leaf_id {
            Some(lid) => Some(lid.to_string()),
            None => self.current_leaf(session_id).await?,
        };

        let Some(leaf_id) = target_leaf else {
            return Ok(Vec::new());
        };

        // 一次性取出该会话的所有消息并在内存中沿 parent_id 倒序回溯（高效且避免递归 CTE 深度瓶颈）
        let rows = sqlx::query("SELECT id, parent_id, role, blocks_json, model, usage_json, created_at FROM messages")
            .fetch_all(&pools.read)
            .await?;

        let mut msg_map: HashMap<String, ChatMessage> = HashMap::with_capacity(rows.len());
        for r in rows {
            let msg = message_from_row(&r)?;
            msg_map.insert(msg.id.clone(), msg);
        }

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

        // 翻转为从根节点到叶子节点的顺序
        linear.reverse();
        Ok(linear)
    }

    /// 获取全部消息（用于构建完整树状视图）
    pub async fn get_all_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        let pools = self.session_pools(session_id).await?;
        let rows = sqlx::query(
            "SELECT id, parent_id, role, blocks_json, model, usage_json, created_at FROM messages ORDER BY created_at ASC",
        )
        .fetch_all(&pools.read)
        .await?;

        rows.iter().map(message_from_row).collect()
    }

    /// 删除指定消息及其整棵子树。
    /// 若当前叶子位于被删子树内，则回退到被删消息的父节点（可能为空）。
    /// 返回 (被删除的消息 ID 列表, 删除后的当前叶子)。
    pub async fn delete_message_subtree(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<(Vec<String>, Option<String>)> {
        let pools = self.session_pools(session_id).await?;
        let rows = sqlx::query("SELECT id, parent_id FROM messages")
            .fetch_all(&pools.write)
            .await?;
        let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
        let mut deleted_parent: Option<String> = None;
        let mut exists = false;
        for r in rows {
            let id: String = r.get("id");
            let parent_id: Option<String> = r.get("parent_id");
            if id == message_id {
                deleted_parent = parent_id.clone();
                exists = true;
            }
            children.entry(parent_id).or_default().push(id);
        }
        if !exists {
            return Err(StorageError::NotFound(format!(
                "message {} in session {}",
                message_id, session_id
            )));
        }

        // BFS 收集子树
        let mut deleted = Vec::new();
        let mut stack = vec![message_id.to_string()];
        while let Some(cur) = stack.pop() {
            deleted.push(cur.clone());
            if let Some(kids) = children.remove(&Some(cur)) {
                stack.extend(kids);
            }
        }

        // 当前叶子位于子树内时回退到被删消息的父节点
        let current_leaf = self.current_leaf(session_id).await?;
        let new_leaf = match current_leaf {
            Some(l) if deleted.contains(&l) => deleted_parent.clone(),
            other => other,
        };
        // messages.parent_id 存在外键约束，必须先删子后删父（deleted 为父先子后序，取逆）
        for id in deleted.iter().rev() {
            sqlx::query("DELETE FROM messages WHERE id = ?")
                .bind(id)
                .execute(&pools.write)
                .await?;
        }

        let now = chrono::Utc::now().timestamp_millis();
        match &new_leaf {
            Some(l) => {
                self.set_current_leaf(session_id, l).await?;
                sqlx::query("UPDATE sessions_index SET current_leaf_id = ?, updated_at = ? WHERE session_id = ?")
                    .bind(l)
                    .bind(now)
                    .bind(session_id)
                    .execute(&self.index_pool)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM session_meta WHERE key = 'current_leaf_id'")
                    .execute(&pools.write)
                    .await?;
                sqlx::query("UPDATE sessions_index SET current_leaf_id = NULL, updated_at = ? WHERE session_id = ?")
                    .bind(now)
                    .bind(session_id)
                    .execute(&self.index_pool)
                    .await?;
            }
        }

        Ok((deleted, new_leaf))
    }
}

#[cfg(test)]
mod tests {
    use oma_contract::Block;

    use super::*;

    fn msg(id: &str, parent: Option<&str>, text: &str, at: i64) -> ChatMessage {
        ChatMessage {
            id:         id.into(),
            parent_id:  parent.map(|p| p.into()),
            role:       Role::User,
            content:    vec![Block::Text { text: text.into() }],
            created_at: at,
            model:      None,
            usage:      None,
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
            .append_message("s_1", &msg("m_1", None, "Hello", 1000))
            .await?;
        // 4. 追加消息 m2 (Assistant)
        storage
            .append_message("s_1", &msg("m_2", Some("m_1"), "Hi there", 2000))
            .await?;

        // 5. 校验当前线性链路 [m_1, m_2]
        let linear = storage.get_linear_messages("s_1", None).await?;
        assert_eq!(linear.len(), 2);
        assert_eq!(linear[0].id, "m_1");
        assert_eq!(linear[1].id, "m_2");

        // 6. 分叉：从 m_1 分叉产生 m_3
        storage
            .append_message("s_1", &msg("m_3", Some("m_1"), "Alternative branch", 3000))
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
            storage.append_message("s_del", &msg(id, p, id, 0)).await?;
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

        // 覆盖写：同 key 应被替换而非累积
        storage.set_context_usage("s_ctx", 200, 1000, 7).await?;
        assert_eq!(storage.context_usage("s_ctx").await?, Some((200, 1000, 7)));

        // 重新加载（模拟重启）后仍可读回
        let reloaded = StorageManager::new(tmp.path()).await?;
        assert_eq!(reloaded.context_usage("s_ctx").await?, Some((200, 1000, 7)));
        Ok(())
    }

    /// 非法存储值报错（带原始值），不退化成 None、也不 panic。
    #[tokio::test]
    async fn test_context_usage_rejects_corrupt_value() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_bad", "/w", "T", "m", "task", "medium")
            .await?;

        // 字段类型错误 / 缺少字段 / 根本不是 JSON
        let pools = storage.session_pools("s_bad").await?;
        for corrupt in [
            r#"{"tokens":"oops","contextLen":100,"covered":3}"#,
            r#"{"tokens":1,"contextLen":100}"#,
            "not-a-number",
        ] {
            sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('context_usage', ?)")
                .bind(corrupt)
                .execute(&pools.write)
                .await?;

            let err = storage.context_usage("s_bad").await.unwrap_err();
            assert!(format!("{err}").contains("context_usage"), "{err}");
        }
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
        b.append_message("s_x", &msg("m1", None, "from b", 1000))
            .await?;

        // a 未参与写入，但读取时应看到 b 的写入
        let seen = a.get_all_messages("s_x").await?;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].id, "m1");
        Ok(())
    }

    /// 旧格式会话库不做迁移：`messages` 缺 model / usage_json 列时直接拒绝打开，
    /// 而不是 `ALTER TABLE` 补列后继续读——要保住内容就先用旧版导出。
    #[tokio::test]
    async fn test_legacy_sqlite_session_db_is_rejected() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let s_dir = tmp.path().join("sessions").join("s_legacy");
        std::fs::create_dir_all(s_dir.join("attachments"))?;

        // 手工造出旧库：旧 messages 列（无 model / usage_json）
        let db_path = s_dir.join("session.db");
        let pool = sqlx::SqlitePool::connect_with(session_connect_options(&db_path, false)).await?;
        sqlx::query(
            r#"
            CREATE TABLE session_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE messages (
                id            TEXT PRIMARY KEY,
                parent_id     TEXT,
                role          TEXT NOT NULL,
                blocks_json   TEXT NOT NULL,
                input_tokens  INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                created_at    INTEGER NOT NULL,
                FOREIGN KEY(parent_id) REFERENCES messages(id)
            );
            CREATE INDEX idx_parent_id ON messages(parent_id);
            "#,
        )
        .execute(&pool)
        .await?;
        pool.close().await;

        let storage = StorageManager::new(tmp.path()).await?;

        // 一打开就报错，并指出缺哪些列
        let err = storage.get_all_messages("s_legacy").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("messages 缺少必要列"), "{msg}");
        assert!(msg.contains("model") && msg.contains("usage_json"), "{msg}");

        // 报错后表结构保持原样：不会被偷偷补列
        let pools = storage.session_pools("s_legacy").await;
        assert!(pools.is_err(), "旧库不应被打开");
        Ok(())
    }

    /// 旧的冒号串 `context_usage` 不再兼容：解析失败即报错，而不是退化成「未知占用」。
    #[tokio::test]
    async fn test_old_colon_context_usage_is_rejected() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_cu", "/w", "T", "m", "task", "medium")
            .await?;
        let pools = storage.session_pools("s_cu").await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('context_usage', '152012:1048576:249')")
            .execute(&pools.write)
            .await?;

        let err = storage.context_usage("s_cu").await.unwrap_err();
        assert!(format!("{err}").contains("不是合法 JSON"), "{err}");

        // 当前 JSON 形态照常可读
        storage
            .set_context_usage("s_cu", 152012, 1_048_576, 249)
            .await?;
        assert_eq!(storage.context_usage("s_cu").await?, Some((152012, 1_048_576, 249)));
        Ok(())
    }

    /// 坏行不再被兜底成「空内容 / 当成用户消息」：报错并指出是哪条消息。
    #[tokio::test]
    async fn test_corrupt_message_row_is_rejected() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_bad", "/w", "T", "m", "task", "medium")
            .await?;
        let pools = storage.session_pools("s_bad").await?;

        // 1) role 不认识
        sqlx::query(
            "INSERT INTO messages (id, parent_id, role, blocks_json, created_at) \
             VALUES ('bad_role', NULL, 'robot', '[]', 1)",
        )
        .execute(&pools.write)
        .await?;
        let err = storage.get_all_messages("s_bad").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("bad_role") && msg.contains("role"), "{msg}");

        // 2) content 不是合法的块数组
        sqlx::query("DELETE FROM messages")
            .execute(&pools.write)
            .await?;
        sqlx::query(
            "INSERT INTO messages (id, parent_id, role, blocks_json, created_at) \
             VALUES ('bad_blocks', NULL, 'assistant', '{oops', 1)",
        )
        .execute(&pools.write)
        .await?;
        let err = storage.get_all_messages("s_bad").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("bad_blocks") && msg.contains("content"), "{msg}");

        // 3) usage 不是合法 JSON（NULL 是正常的，只有非空值才要求可解析）
        sqlx::query("DELETE FROM messages")
            .execute(&pools.write)
            .await?;
        sqlx::query(
            "INSERT INTO messages (id, parent_id, role, blocks_json, usage_json, created_at) \
             VALUES ('bad_usage', NULL, 'assistant', '[]', 'nope', 1)",
        )
        .execute(&pools.write)
        .await?;
        let err = storage.get_all_messages("s_bad").await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("bad_usage") && msg.contains("usage"), "{msg}");
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

    /// 工作区登记是幂等的：重复登记返回未新建；新建会话会自动登记其工作区。
    #[tokio::test]
    async fn test_workspace_registration() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;

        assert!(storage.list_workspaces().await?.is_empty());
        assert!(storage.register_workspace("/ws/a").await?);
        assert!(!storage.register_workspace("/ws/a").await?, "重复登记不应算新建");

        // 新建会话自动登记其工作区（哪怕此前没有显式登记过）
        storage
            .create_session("s_ws", "/ws/b", "T", "m", "task", "medium")
            .await?;
        let paths: Vec<String> = storage
            .list_workspaces()
            .await?
            .into_iter()
            .map(|w| w.path)
            .collect();
        assert_eq!(paths, vec!["/ws/a".to_string(), "/ws/b".to_string()]);

        // 空路径拒绝：它会让 register_workspace 的幂等键形同虚设
        assert!(matches!(
            storage.register_workspace("   ").await,
            Err(StorageError::InvalidId(_))
        ));

        // 移除只删登记，不动会话
        storage.delete_workspace("/ws/a").await?;
        assert_eq!(storage.list_workspaces().await?.len(), 1);
        assert!(storage.get_session("s_ws").await?.is_some());
        assert!(matches!(
            storage.delete_workspace("/ws/a").await,
            Err(StorageError::NotFound(_))
        ));

        // 跨实例可见（重启/多进程共享同一数据目录）
        let reopened = StorageManager::new(tmp.path()).await?;
        assert_eq!(reopened.list_workspaces().await?.len(), 1);
        Ok(())
    }
}
