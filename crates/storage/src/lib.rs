use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use oma_contract::{ApprovalMode, Block, ChatMessage, Role};
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

/// 全局索引会话记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id:      String,
    pub workspace:       String,
    pub title:           String,
    pub active_model:    String,
    pub active_agent:    String,
    pub approval_mode:   ApprovalMode,
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

impl StorageManager {
    /// 初始化存储引擎，并在 base_dir 下建立全局 oma.db
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
                active_agent    TEXT NOT NULL DEFAULT 'task',
                approval_mode   TEXT NOT NULL DEFAULT 'normal',
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
                id            TEXT PRIMARY KEY,
                parent_id     TEXT,
                role          TEXT NOT NULL,
                blocks_json   TEXT NOT NULL,
                input_tokens  INTEGER DEFAULT 0,
                output_tokens INTEGER DEFAULT 0,
                created_at    INTEGER NOT NULL,
                FOREIGN KEY(parent_id) REFERENCES messages(id)
            );
            CREATE INDEX IF NOT EXISTS idx_parent_id ON messages(parent_id);
            "#,
        )
        .execute(&write)
        .await
        .map_err(|e| StorageError::Internal(anyhow::anyhow!("failed to initialize session tables: {}", e)))?;

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

    /// 创建会话
    pub async fn create_session(
        &self,
        session_id: &str,
        workspace: &str,
        title: &str,
        active_model: &str,
        active_agent: &str,
        approval_mode: ApprovalMode,
    ) -> Result<SessionRecord> {
        validate_session_id(session_id)?;
        let now = chrono::Utc::now().timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO sessions_index 
            (session_id, workspace, title, active_model, active_agent, approval_mode, current_leaf_id, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)
            "#,
        )
        .bind(session_id)
        .bind(workspace)
        .bind(title)
        .bind(active_model)
        .bind(active_agent)
        .bind(approval_mode.as_str())
        .bind(now)
        .bind(now)
        .execute(&self.index_pool)
        .await?;

        // 触发会话库初始化并写入 meta
        let pools = self.session_pools(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('workspace', ?)")
            .bind(workspace)
            .execute(&pools.write)
            .await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('title', ?)")
            .bind(title)
            .execute(&pools.write)
            .await?;

        Ok(SessionRecord {
            session_id: session_id.to_string(),
            workspace: workspace.to_string(),
            title: title.to_string(),
            active_model: active_model.to_string(),
            active_agent: active_agent.to_string(),
            approval_mode,
            current_leaf_id: None,
            created_at: now,
            updated_at: now,
            is_running: false,
        })
    }

    /// 获取会话记录
    pub async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        validate_session_id(session_id)?;
        let row = sqlx::query(
            r#"
            SELECT session_id, workspace, title, active_model, active_agent, approval_mode, current_leaf_id, created_at, updated_at
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
        let mode_str: String = row.get("approval_mode");
        let approval_mode = ApprovalMode::parse(&mode_str).unwrap_or_default();
        SessionRecord {
            session_id: row.get("session_id"),
            workspace: row.get("workspace"),
            title: row.get("title"),
            active_model: row.get("active_model"),
            active_agent: row.get("active_agent"),
            approval_mode,
            current_leaf_id: row.get("current_leaf_id"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            is_running: false,
        }
    }

    /// 列出指定 workspace（或全部）会话
    pub async fn list_sessions(&self, workspace: Option<&str>) -> Result<Vec<SessionRecord>> {
        let rows = if let Some(ws) = workspace {
            sqlx::query(
                r#"
                SELECT session_id, workspace, title, active_model, active_agent, approval_mode, current_leaf_id, created_at, updated_at
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
                SELECT session_id, workspace, title, active_model, active_agent, approval_mode, current_leaf_id, created_at, updated_at
                FROM sessions_index
                ORDER BY updated_at DESC
                "#,
            )
            .fetch_all(&self.index_pool)
            .await?
        };

        Ok(rows.iter().map(Self::record_from_row).collect())
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

    /// 更新会话配置 (model, agent, approval_mode)
    pub async fn update_session_settings(
        &self,
        session_id: &str,
        active_model: Option<&str>,
        active_agent: Option<&str>,
        approval_mode: Option<ApprovalMode>,
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
        if let Some(mode) = approval_mode {
            sqlx::query("UPDATE sessions_index SET approval_mode = ?, updated_at = ? WHERE session_id = ?")
                .bind(mode.as_str())
                .bind(now)
                .bind(session_id)
                .execute(&self.index_pool)
                .await?;
        }
        Ok(())
    }

    /// 追加消息至树状表并更新当前叶子节点
    pub async fn append_message(
        &self,
        session_id: &str,
        message: &ChatMessage,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()> {
        let pools = self.session_pools(session_id).await?;
        let blocks_json = serde_json::to_string(&message.content)?;

        sqlx::query(
            r#"
            INSERT INTO messages 
            (id, parent_id, role, blocks_json, input_tokens, output_tokens, created_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&message.id)
        .bind(&message.parent_id)
        .bind(message.role.as_str())
        .bind(&blocks_json)
        .bind(input_tokens as i64)
        .bind(output_tokens as i64)
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
        let rows = sqlx::query("SELECT id, parent_id, role, blocks_json, created_at FROM messages")
            .fetch_all(&pools.read)
            .await?;

        let mut msg_map: HashMap<String, ChatMessage> = HashMap::with_capacity(rows.len());
        for r in rows {
            let id: String = r.get("id");
            let parent_id: Option<String> = r.get("parent_id");
            let role_str: String = r.get("role");
            let blocks_json: String = r.get("blocks_json");
            let blocks: Vec<Block> = serde_json::from_str(&blocks_json).unwrap_or_default();
            msg_map.insert(
                id.clone(),
                ChatMessage {
                    id,
                    parent_id,
                    role: Role::parse(&role_str).unwrap_or(Role::User),
                    content: blocks,
                    created_at: r.get("created_at"),
                },
            );
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
        let rows =
            sqlx::query("SELECT id, parent_id, role, blocks_json, created_at FROM messages ORDER BY created_at ASC")
                .fetch_all(&pools.read)
                .await?;

        let mut msgs = Vec::with_capacity(rows.len());
        for r in rows {
            let role_str: String = r.get("role");
            let blocks_json: String = r.get("blocks_json");
            msgs.push(ChatMessage {
                id:         r.get("id"),
                parent_id:  r.get("parent_id"),
                role:       Role::parse(&role_str).unwrap_or(Role::User),
                content:    serde_json::from_str(&blocks_json).unwrap_or_default(),
                created_at: r.get("created_at"),
            });
        }
        Ok(msgs)
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
    use super::*;

    #[tokio::test]
    async fn test_storage_crud_and_branching() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;

        // 1. 创建会话
        let s1 = storage
            .create_session(
                "s_1",
                "/workspace/test",
                "Test Session",
                "claude-3-7",
                "task",
                ApprovalMode::Normal,
            )
            .await?;
        assert_eq!(s1.session_id, "s_1");

        // 2. 列表查询
        let list = storage.list_sessions(Some("/workspace/test")).await?;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].title, "Test Session");

        // 3. 追加消息 m1 (User)
        let m1 = ChatMessage {
            id:         "m_1".into(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![Block::Text { text: "Hello".into() }],
            created_at: 1000,
        };
        storage.append_message("s_1", &m1, 10, 0).await?;

        // 4. 追加消息 m2 (Assistant)
        let m2 = ChatMessage {
            id:         "m_2".into(),
            parent_id:  Some("m_1".into()),
            role:       Role::Assistant,
            content:    vec![Block::Text {
                text: "Hi there".into(),
            }],
            created_at: 2000,
        };
        storage.append_message("s_1", &m2, 0, 15).await?;

        // 5. 校验当前线性链路 [m_1, m_2]
        let linear = storage.get_linear_messages("s_1", None).await?;
        assert_eq!(linear.len(), 2);
        assert_eq!(linear[0].id, "m_1");
        assert_eq!(linear[1].id, "m_2");

        // 6. 分叉：从 m_1 分叉产生 m_3
        let m3 = ChatMessage {
            id:         "m_3".into(),
            parent_id:  Some("m_1".into()),
            role:       Role::Assistant,
            content:    vec![Block::Text {
                text: "Alternative branch".into(),
            }],
            created_at: 3000,
        };
        storage.append_message("s_1", &m3, 0, 20).await?;

        // 当前 leaf 自动变更为 m_3
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
        let deleted = storage.get_session("s_1").await?;
        assert!(deleted.is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_delete_message_subtree() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_del", "/w", "Del", "m", "task", ApprovalMode::Normal)
            .await?;

        let msg = |id: &str, parent: Option<&str>| ChatMessage {
            id:         id.into(),
            parent_id:  parent.map(|p| p.into()),
            role:       Role::User,
            content:    vec![Block::Text { text: id.into() }],
            created_at: 0,
        };

        // 树：m1 → m2 → m3（活跃链），m1 → m4（兄弟分支）
        for (id, p) in [("m1", None), ("m2", Some("m1")), ("m3", Some("m2")), ("m4", Some("m1"))] {
            storage.append_message("s_del", &msg(id, p), 0, 0).await?;
        }
        // 当前叶子为 m4（最后追加）

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

    /// session_id 会被拼进文件系统路径：必须拒绝任何分隔符与 `..`，否则可越权读写/递归删除。
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

        // 合法 id 仍然可用
        storage
            .create_session("sess_OK-1", "/w", "T", "m", "task", ApprovalMode::Normal)
            .await?;
        storage.delete_session("sess_OK-1").await?;
        Ok(())
    }

    /// 连接池必须缓存复用，且读池是只读的（写路径不被快照读阻塞）。
    #[tokio::test]
    async fn test_session_pools_are_cached_and_read_only() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_cache", "/w", "T", "m", "task", ApprovalMode::Normal)
            .await?;

        let pools = storage.session_pools("s_cache").await?;
        // 二次调用不得新建池（缓存命中路径）
        let _again = storage.session_pools("s_cache").await?;
        assert_eq!(
            storage.pools.read().len(),
            1,
            "session pools must be cached, not rebuilt"
        );

        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('k', 'v')")
            .execute(&pools.write)
            .await?;
        let write_on_read_pool = sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('k2', 'v')")
            .execute(&pools.read)
            .await;
        assert!(write_on_read_pool.is_err(), "read pool must reject writes (read_only)");

        storage.delete_session("s_cache").await?;
        assert!(
            storage.pools.read().get("s_cache").is_none(),
            "pools must be evicted on delete"
        );
        Ok(())
    }

    /// 附件名同样参与路径拼接，必须拒绝穿越。
    #[tokio::test]
    async fn test_attachment_name_validation() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let storage = StorageManager::new(tmp.path()).await?;
        storage
            .create_session("s_att", "/w", "T", "m", "task", ApprovalMode::Normal)
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
