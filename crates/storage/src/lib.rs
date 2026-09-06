use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use oma_contract::{ApprovalMode, Block, ChatMessage, Role};
use serde::{Deserialize, Serialize};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

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
}

/// 双层 SQLite 存储引擎管理器
#[derive(Clone)]
pub struct StorageManager {
    base_dir:   PathBuf,
    index_pool: SqlitePool,
}

impl StorageManager {
    /// 初始化存储引擎，并在 base_dir 下建立全局 oma.db
    pub async fn new(base_dir: impl AsRef<Path>) -> Result<Self> {
        let base_dir = base_dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&base_dir)
            .await
            .with_context(|| format!("Failed to create base dir {:?}", base_dir))?;

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
            .with_context(|| format!("Failed to connect to index db at {:?}", index_db_path))?;

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
        .context("Failed to initialize sessions_index table")?;

        Ok(Self { base_dir, index_pool })
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.base_dir.join("sessions").join(session_id)
    }

    pub fn attachments_dir(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("attachments")
    }

    /// 获取单个会话专属库的单写连接池
    pub async fn get_session_pool(&self, session_id: &str) -> Result<SqlitePool> {
        let s_dir = self.session_dir(session_id);
        tokio::fs::create_dir_all(&s_dir)
            .await
            .with_context(|| format!("Failed to create session dir {:?}", s_dir))?;
        tokio::fs::create_dir_all(self.attachments_dir(session_id))
            .await
            .with_context(|| "Failed to create attachments dir")?;

        let db_path = s_dir.join("session.db");
        let connect_opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_millis(5000));

        let pool = SqlitePoolOptions::new()
            .max_connections(1) // 严格单写连接
            .connect_with(connect_opts)
            .await
            .with_context(|| format!("Failed to connect to session db at {:?}", db_path))?;

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
        .execute(&pool)
        .await
        .context("Failed to initialize session tables")?;

        Ok(pool)
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
        let now = chrono::Utc::now().timestamp_millis();
        let mode_str = serde_json::to_string(&approval_mode)
            .unwrap_or_else(|_| "\"normal\"".into())
            .trim_matches('"')
            .to_string();

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
        .bind(&mode_str)
        .bind(now)
        .bind(now)
        .execute(&self.index_pool)
        .await
        .context("Failed to insert session into index")?;

        // 触发会话库初始化并写入 meta
        let pool = self.get_session_pool(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('workspace', ?)")
            .bind(workspace)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('title', ?)")
            .bind(title)
            .execute(&pool)
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
        })
    }

    /// 获取会话记录
    pub async fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        let row = sqlx::query(
            r#"
            SELECT session_id, workspace, title, active_model, active_agent, approval_mode, current_leaf_id, created_at, updated_at
            FROM sessions_index
            WHERE session_id = ?
            "#,
        )
        .bind(session_id)
        .fetch_optional(&self.index_pool)
        .await
        .context("Failed to query session from index")?;

        let Some(row) = row else { return Ok(None) };

        let mode_str: String = row.get("approval_mode");
        let approval_mode = serde_json::from_str(&format!("\"{}\"", mode_str)).unwrap_or_default();

        Ok(Some(SessionRecord {
            session_id: row.get("session_id"),
            workspace: row.get("workspace"),
            title: row.get("title"),
            active_model: row.get("active_model"),
            active_agent: row.get("active_agent"),
            approval_mode,
            current_leaf_id: row.get("current_leaf_id"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        }))
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

        let mut list = Vec::with_capacity(rows.len());
        for row in rows {
            let mode_str: String = row.get("approval_mode");
            let approval_mode = serde_json::from_str(&format!("\"{}\"", mode_str)).unwrap_or_default();
            list.push(SessionRecord {
                session_id: row.get("session_id"),
                workspace: row.get("workspace"),
                title: row.get("title"),
                active_model: row.get("active_model"),
                active_agent: row.get("active_agent"),
                approval_mode,
                current_leaf_id: row.get("current_leaf_id"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            });
        }

        Ok(list)
    }

    /// 删除会话及其物理目录
    pub async fn delete_session(&self, session_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions_index WHERE session_id = ?")
            .bind(session_id)
            .execute(&self.index_pool)
            .await
            .context("Failed to delete session from index")?;

        let s_dir = self.session_dir(session_id);
        if s_dir.exists() {
            let _ = tokio::fs::remove_dir_all(&s_dir).await;
        }

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
            let mode_str = serde_json::to_string(&mode)
                .unwrap_or_else(|_| "\"normal\"".into())
                .trim_matches('"')
                .to_string();
            sqlx::query("UPDATE sessions_index SET approval_mode = ?, updated_at = ? WHERE session_id = ?")
                .bind(mode_str)
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
        let pool = self.get_session_pool(session_id).await?;
        let blocks_json =
            serde_json::to_string(&message.content).context("Failed to serialize message content blocks")?;

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
        .execute(&pool)
        .await
        .context("Failed to insert message")?;

        // 更新 session_meta 与 sessions_index 的 current_leaf_id
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('current_leaf_id', ?)")
            .bind(&message.id)
            .execute(&pool)
            .await?;

        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query("UPDATE sessions_index SET current_leaf_id = ?, updated_at = ? WHERE session_id = ?")
            .bind(&message.id)
            .bind(now)
            .bind(session_id)
            .execute(&self.index_pool)
            .await?;

        Ok(())
    }

    /// 切换分支：更新激活的叶子节点 ID
    pub async fn switch_branch(&self, session_id: &str, leaf_id: &str) -> Result<()> {
        let pool = self.get_session_pool(session_id).await?;
        sqlx::query("INSERT OR REPLACE INTO session_meta (key, value) VALUES ('current_leaf_id', ?)")
            .bind(leaf_id)
            .execute(&pool)
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

    /// 获取激活分支的线性消息链（从 root 到指定/当前 leaf_id）
    pub async fn get_linear_messages(&self, session_id: &str, leaf_id: Option<&str>) -> Result<Vec<ChatMessage>> {
        let pool = self.get_session_pool(session_id).await?;
        let target_leaf = if let Some(lid) = leaf_id {
            Some(lid.to_string())
        } else {
            let row = sqlx::query("SELECT value FROM session_meta WHERE key = 'current_leaf_id'")
                .fetch_optional(&pool)
                .await?;
            row.map(|r| r.get::<String, _>("value"))
        };

        let Some(leaf_id) = target_leaf else {
            return Ok(Vec::new());
        };

        // 一次性取出该会话的所有消息并在内存中沿 parent_id 倒序回溯（高效且避免递归 CTE 深度瓶颈）
        let rows = sqlx::query("SELECT id, parent_id, role, blocks_json, created_at FROM messages")
            .fetch_all(&pool)
            .await?;

        use std::collections::HashMap;
        let mut msg_map: HashMap<String, (Option<String>, Role, Vec<Block>, i64)> = HashMap::new();
        for r in rows {
            let id: String = r.get("id");
            let parent_id: Option<String> = r.get("parent_id");
            let role_str: String = r.get("role");
            let role = Role::parse(&role_str).unwrap_or(Role::User);
            let blocks_json: String = r.get("blocks_json");
            let blocks: Vec<Block> = serde_json::from_str(&blocks_json).unwrap_or_default();
            let created_at: i64 = r.get("created_at");
            msg_map.insert(id, (parent_id, role, blocks, created_at));
        }

        let mut linear = Vec::new();
        let mut curr = Some(leaf_id);

        while let Some(cid) = curr {
            if let Some((pid, role, blocks, created_at)) = msg_map.remove(&cid) {
                linear.push(ChatMessage {
                    id: cid,
                    parent_id: pid.clone(),
                    role,
                    content: blocks,
                    created_at,
                });
                curr = pid;
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
        let pool = self.get_session_pool(session_id).await?;
        let rows =
            sqlx::query("SELECT id, parent_id, role, blocks_json, created_at FROM messages ORDER BY created_at ASC")
                .fetch_all(&pool)
                .await?;

        let mut msgs = Vec::with_capacity(rows.len());
        for r in rows {
            let id: String = r.get("id");
            let parent_id: Option<String> = r.get("parent_id");
            let role_str: String = r.get("role");
            let role = Role::parse(&role_str).unwrap_or(Role::User);
            let blocks_json: String = r.get("blocks_json");
            let blocks: Vec<Block> = serde_json::from_str(&blocks_json).unwrap_or_default();
            let created_at: i64 = r.get("created_at");
            msgs.push(ChatMessage {
                id,
                parent_id,
                role,
                content: blocks,
                created_at,
            });
        }
        Ok(msgs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_storage_crud_and_branching() -> Result<()> {
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
}
