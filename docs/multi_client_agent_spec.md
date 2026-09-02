# Oma 多类型多客户端协同 Agent 技术规格书 (Technical Specification)

> 版本：v2.0  
> 状态：Implementation Confirmed & Ready  
> 适用形态：CLI / TUI、Vue 3 Web 前端、Tauri 桌面端（前端资产由客户端独立提供，Daemon 保持纯净 Headless）

---

## 1. 架构总览与工程拓扑 (Architecture & Crates Topology)

Oma 采用 **“单 Daemon 核心 + 统一 WebSocket/HTTP 网关 + 多端协同客户端”** 的分布式 C/S 架构，并在代码库层面采用模块化细粒度解耦设计：

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                           Client Matrix                                 │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────────┐  │
│  │   CLI / TUI     │    │  Web (Browser)  │    │  Tauri (Desktop)    │  │
│  │ (Rust/Ratatui)  │    │ (Vue 3 / TS)    │    │ (Rust + Vue 3 UI)   │  │
│  │ crates/tui      │    │ web/            │    │ (Future desktop)    │  │
│  └────────┬────────┘    └────────┬────────┘    └──────────┬──────────┘  │
│           │                      │                        │             │
│           │ Authorization: Bearer <token>                 │             │
│           │ (WebSocket /ws  &  HTTP REST /api/*)          │             │
└───────────┼──────────────────────┼────────────────────────┼─────────────┘
            ▼                      ▼                        ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                     Oma Core Daemon (Single Port: 17431)                │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │           Axum Gateway & Auth Middleware (crates/daemon)          │  │
│  │  - Headless API Gateway (REST APIs & WebSocket Endpoint)             │  │
│  │  - REST APIs (/api/sessions, /api/workspace/*, /api/server/*)        │  │
│  │  - WebSocket Upgrade & Room Routing (/ws)                            │  │
│  │  - CORS & Dev Proxy Middleware                                       │  │
│  └─────────────────────────────────┬─────────────────────────────────┘  │
│                                    │                                    │
│  ┌─────────────────────────────────▼─────────────────────────────────┐  │
│  │        Session Registry & Multi-Client Rooms (crates/runtime)     │  │
│  │  - Session Room Dispatcher & Connection Lifecycle Manager         │  │
│  │  - Per-Session Command FIFO Queue & Cancel Cascader               │  │
│  │  - Event Broadcaster (tokio::broadcast + In-memory Catch-Up)      │  │
│  │  - Approval Arbiter (First-Response-Wins + 120s Auto-Deny)        │  │
│  └─────────────────────────────────┬─────────────────────────────────┘  │
│                                    │                                    │
│  ┌─────────────────────────────────▼─────────────────────────────────┐  │
│  │               Agent Runtime Engine & Subsystems                   │  │
│  │  - oma-provider: LLM Streaming Normalization & Deep Merge         │  │
│  │  - oma-storage:  Dual SQLite Engine (WAL + Single-Writer Pool)    │  │
│  │  - oma-tool:     5 Core Tools (read, write, edit, shell, task)    │  │
│  │  - oma-mcp:      MCP Client (stdio & remote SSE, namespaced)      │  │
│  │  - oma-config:   TOML Config & Bundled Agent Templates            │  │
│  │  - oma-contract: Shared Wire Protocol, Events, Data Models        │  │
│  └───────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

### 1.1 代码库 Crates 职责划分

| Crate / 目录 | 职责与依赖 |
|---|---|
| `crates/contract` | 纯类型与协议契约（`Role`, `Block`, `ChatMessage`, `ClientMessage`, `ServerMessage`, `AgentEvent`, `ActiveTurnCatchUp` 等），零重依赖。 |
| `crates/storage` | SQLite 持久化抽象，实现全局中心库 `oma.db` 与会话专属库 `session.db`，管理 WAL 模式与串行写锁。 |
| `crates/provider` | 手写轻量 SSE 状态机，统一归一化 Anthropic、OpenAI / DeepSeek、Google Gemini 流式协议与 Deep Merge 请求体。 |
| `crates/tool` | 内置 5 大工具（`read`, `write`, `edit` 原子替换补丁, `shell` 进程组管理, `task` 子任务委托），集成 Guardrails。 |
| `crates/mcp` | MCP 客户端，支持本地 stdio 子进程与远程 SSE 传输，按 `mcp__{server}__{tool}` 统一命名空间注册。 |
| `crates/config` | 配置文件 `config.toml` 解析，内置 5 大 Agent 模板（`include_str!`）与本地/项目级覆盖、环境块动态注入。 |
| `crates/runtime` | 核心 Agent Loop、Room 调度、命令 FIFO 队列、级联取消、熔断器、70% 阈值两阶段上下文压缩与内存审批白名单。 |
| `crates/daemon` | 基于 Axum 的 HTTP REST 与 WebSocket 网关、Bearer Token 鉴权中间件、静态路由与 CORS。 |
| `crates/client` | 纯 Rust 客户端 SDK，供 CLI/TUI 或测试代码快速建立连接、订阅事件并发送指令。 |
| `crates/tui` | 基于 Ratatui 的终端交互客户端。 |
| `crates/bin` | 统一命令行可执行文件 `oma`，集成 `oma daemon`、`oma web`、`oma tui` 等子命令。 |
| `web/` | 纯手写原生 Vue 3 + TypeScript + 手写 CSS（零外部 UI/CSS 库）的 Web 协同客户端。 |

---

## 2. 传输协议与鉴权规范 (Transport & Security)

### 2.1 端口与网络拓扑
- **默认监听地址**：`127.0.0.1:17431`；支持 `--addr 0.0.0.0:17431` 供局域网与远程服务器接入。
- **单端口统一路由**：
  - `/ws`：双向 WebSocket（承载 `ClientMessage` / `ServerMessage` JSON 协议，基于 `session_id` 自动加入对应 Room）；
  - `/api/*`：HTTP REST 接口（会话 CRUD、消息回放、状态探测、文件树、Diff 读取、附件上传）；
  - 前端静态资源由各客户端（Web/Tauri/TUI）独立打包与托管，Daemon 专职作为纯 Headless 后台服务。

### 2.2 严格 Bearer Token 鉴权
1. **传输规范**：
   - 严格仅支持 HTTP 请求头 `Authorization: Bearer <token>`；
   - WebSocket 握手阶段在 HTTP Upgrade Header 中附带该头部，鉴权不通过直接返回 HTTP `401 Unauthorized`；
   - 所有 `/api/*` 接口统一通过 Axum `AuthMiddleware` 拦截。
2. **Token 生成与存储**：
   - 启动时优先读取环境变量 `OMA_AUTH_TOKEN`、命令行 `--token` 或配置文件 `auth.token`；
   - 若未配置，首次启动在 `~/.local/share/oma/auth.token` 自动生成安全随机 Token 并持久化；
   - 本地客户端（TUI / 本机 Tauri）启动时自动读取该文件获取 Token；
   - 远程客户端（Web / 远程 Tauri）首次输入成功后，在客户端本地持久化（`localStorage`）。

---

## 3. 数据模型与 SQLite 持久化规范 (Data Contracts & DDL)

### 3.1 消息块模型 (Message Block Model & Multimodal)
Oma 采用结构化 Content Block 表达混合与多模态消息：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text { 
        text: String 
    },
    Thinking { 
        thinking: String 
    },
    Image { 
        mime_type: String, 
        data: String // Base64 或 session_attachment:// 资源 URI
    },
    ToolUse {
        id:    String,
        name:  String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content:     String,
        is_error:    bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id:         String,             // UUIDv4
    pub parent_id:  Option<String>,     // 树状消息链父节点 ID
    pub role:       Role,
    pub content:    Vec<Block>,
    pub created_at: i64,                // Unix 毫秒时间戳
}
```

> **存储与协议映射策略**：  
> - 数据库中 `ToolResult` 统一包裹在 `role: Role::User` 消息中存储；  
> - 发送给各厂商 API 时：Anthropic 协议保持 `role: "user"`；OpenAI / DeepSeek 协议在适配层自动将 `Block::ToolResult` 解构成独立的 `role: "tool"` 消息。

### 3.2 双层 SQLite 持久化 Schema (DDL) 与并发写安全

#### 1. 全局中心索引库：`~/.local/share/oma/oma.db`
维护工作区与全部会话元数据，支持极速列表查询与管理：

```sql
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
```

#### 2. 会话专属数据库：`~/.local/share/oma/sessions/<session_id>/session.db`
每个 Session 独立存储完整树形消息图与会话级元数据，附件保存至同目录下的 `attachments/`：

```sql
-- 会话专属元数据
CREATE TABLE IF NOT EXISTS session_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 树状消息表
CREATE TABLE IF NOT EXISTS messages (
    id            TEXT PRIMARY KEY,
    parent_id     TEXT,
    role          TEXT NOT NULL,          -- 'system' | 'user' | 'assistant'
    blocks_json   TEXT NOT NULL,         -- JSON 序列化的 Vec<Block>
    input_tokens  INTEGER DEFAULT 0,
    output_tokens INTEGER DEFAULT 0,
    created_at    INTEGER NOT NULL,
    FOREIGN KEY(parent_id) REFERENCES messages(id)
);

CREATE INDEX IF NOT EXISTS idx_parent_id ON messages(parent_id);
```

#### 3. 并发安全与连接池规范 (WAL & Serialized Writes)
- **WAL 模式启用**：所有 SQLite 数据库在打开时统一执行 `PRAGMA journal_mode = WAL;` 与 `PRAGMA busy_timeout = 5000;`。
- **单写连接池隔离 (`max_connections = 1`)**：写路径（`SessionManager` 与消息追加）使用 `max_connections = 1` 的连接池，保证同一时刻单会话内写入绝对串行，杜绝 SQLite `database is locked` 竞争；
- **只读连接用于快照**：客户端接入时的历史消息回放和会话列表查询开辟独立的只读连接（`read_only = true`），读写互不阻塞。

---

## 4. 通信契约与 WebSocket 协议规范 (Wire Contract)

### 4.1 连接参数 (`ConnectParams`)
客户端建立 WebSocket 连接后发送首包进行握手并加入指定 Session Room：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectParams {
    /// 客户端唯一实例 ID（UUIDv4）
    pub client_id:     String,
    /// 目标工作区绝对路径
    pub workspace:     String,
    /// 绑定的会话 ID
    pub session_id:    String,
    /// 客户端类型
    pub client_type:   ClientType,
    /// 客户端展示名称（如 "MacBook-TUI", "Chrome-Web"）
    pub client_name:   String,
    /// 客户端版本号
    pub version:       String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientType {
    Tui,
    Web,
    Tauri,
    Cli,
}
```

### 4.2 客户端请求消息 (`ClientMessage`)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    Connect {
        #[serde(flatten)]
        params: ConnectParams,
    },
    Command {
        command: AgentCommand,
    },
    Approval {
        response: ApprovalResponse,
    },
    Cancel {},
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentCommand {
    UserInput {
        content: String,
        #[serde(default)]
        attachments: Vec<String>, // 包含 session_attachment:// 附件标识
    },
    SetModel {
        model: String, // 格式: "provider/model"
    },
    SetAgent {
        agent: String, // 切换预设 Agent (如 "task", "plan", "review")
    },
    SetApprovalMode {
        mode: ApprovalMode,
    },
    ForkAndRun {
        parent_message_id: String,
        new_content: Option<String>,
    },
    SwitchBranch {
        leaf_message_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub request_id: String,
    pub decision:   ApprovalDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowSession,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    Normal,     // Shell/危险操作弹窗确认，读写放行
    Strict,     // 所有写文件与命令执行均需确认
    Auto,       // 全自动免确认
}
```

### 4.3 服务端推送消息 (`ServerMessage` & `AgentEvent`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready { ready: Ready },
    Event { event: AgentEvent },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ready {
    pub version:          String,
    pub session_id:       String,
    pub workspace:        String,
    pub active_model:     String,
    pub active_agent:     String,
    pub approval_mode:    ApprovalMode,
    pub current_leaf_id:  Option<String>,
    pub providers:        std::collections::BTreeMap<String, Vec<ModelInfo>>,
    pub agents:           Vec<AgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id:                String,
    pub name:              String,
    pub context_len:       usize,
    pub supports_vision:   bool,
    pub supports_thinking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id:          String,
    pub name:        String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTurnCatchUp {
    pub turn_id:              String,
    pub accumulated_thinking: String,
    pub accumulated_text:     String,
    pub active_tool_call:     Option<ToolCallStartedData>,
    pub pending_approval:     Option<PermissionRequestedData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallStartedData {
    pub call_id:     String,
    pub name:        String,
    pub input:       serde_json::Value,
    pub subagent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequestedData {
    pub request_id: String,
    pub name:       String,
    pub summary:    String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    // 1. 会话与轮次生命周期
    TurnStarted { 
        turn_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    TurnFinished { 
        turn_id: String, 
        stop_reason: StopReason,
        usage: TokenUsage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    
    // 2. 多端用户输入广播与队列清空
    UserMessage {
        client_id:   String,
        client_name: String,
        client_type: ClientType,
        content:     String,
        queued:      bool,
    },
    QueueCleared {},

    // 3. 模型推理流式增量
    ThinkingDelta { 
        delta: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },
    TextDelta { 
        delta: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },

    // 4. 工具生命周期
    ToolCallStarted(ToolCallStartedData),
    ToolCallFinished {
        call_id:  String,
        name:     String,
        output:   String,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
    },

    // 5. 权限审批广播与先到先得
    PermissionRequested(PermissionRequestedData),
    PermissionResolved {
        request_id:  String,
        decision:    ApprovalDecision,
        resolved_by: String,
    },

    // 6. 分支、模型与 Agent 动态变更
    ActiveBranchChanged { current_leaf_id: String },
    ModelChanged        { active_model: String },
    AgentChanged        { active_agent: String },
    ApprovalModeChanged { mode: ApprovalMode },

    // 7. 重连快照与错误提示
    ActiveTurnCatchUp(ActiveTurnCatchUp),
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens:  usize,
    pub output_tokens: usize,
}
```

---

## 5. 配置体系、Provider 架构与 Agent/Skill 模板

### 5.1 配置文件规范 (`~/.config/oma/config.toml`)

```toml
default_model = "my_anthropic/claude-3-7-sonnet"
default_agent = "task"
default_approval_mode = "normal"

[server]
listen_addr = "127.0.0.1:17431"

[providers.my_anthropic]
api_type = "anthropic"       # "anthropic" | "completion" | "response" | "google"
base_url = "https://api.anthropic.com"
api_key = "env:ANTHROPIC_API_KEY"

[providers.my_anthropic.headers]
"anthropic-beta" = "prompt-caching-2024-07-31"

[[providers.my_anthropic.models]]
id = "claude-3-7-sonnet"
name = "Claude 3.7 Sonnet"
context_len = 200000
supports_vision = true
supports_thinking = true

[providers.my_deepseek]
api_type = "completion"
base_url = "https://api.deepseek.com/v1"
api_key = "env:DEEPSEEK_API_KEY"
models = [
    { id = "deepseek-chat", name = "DeepSeek V3", context_len = 64000, supports_vision = false, supports_thinking = false },
    { id = "deepseek-reasoner", name = "DeepSeek R1", context_len = 64000, supports_vision = false, supports_thinking = true }
]

# MCP 服务配置
[mcp_servers.local_sqlite]
type = "local"
command = "uvx"
args = ["mcp-server-sqlite", "--db-path", "oma.db"]
env = { DEBUG = "1" }

[mcp_servers.remote_docs]
type = "remote"
url = "https://mcp.internal.example.com/sse"
headers = { Authorization = "Bearer secret_token" }
```

### 5.2 请求头与请求体三级递归合并规范
优先级：`Provider 级配置` $\prec$ `Model 级配置` $\prec$ `Reasoning Effort 覆盖`。
- 同名 HTTP Header 强制后序覆盖前序；
- JSON Body 字段执行递归对象合并，支持灵活注入厂商私有字段。

### 5.3 内置 Agent 与 Skill 模板规范
1. **Agent 模板加载与回退**：
   - 编译期内嵌 5 个基础 Agent 模板：`task.md`、`plan.md`、`explore.md`、`review.md`、`build.md`；
   - 优先检查项目级 `<workspace>/.oma/agents/` 与全局 `~/.config/oma/agents/`，存在同名文件则覆盖；
   - 系统动态向 System Prompt 追加实时环境信息：
     ```text
     <runtime_context>
     - Workspace: /absolute/path/to/project
     - Operating System: linux (x86_64)
     - Today: 2026-09-03
     - Active Model: my_anthropic/claude-3-7-sonnet
     </runtime_context>
     ```
2. **Skill 动态发现**：
   - 自动扫描 `~/.config/oma/skills/` 与 `<workspace>/.oma/skills/` 下的 `SKILL.md`，组装可用技能目录注入 System Prompt。

---

## 6. 多端协同与 Agent 运行引擎

### 6.1 Session Room 隔离与 FIFO 队列
- 服务端内存中按 `session_id` 维护独立的 `SessionRoom` 实例；
- 每个 Room 拥有独立的 `tokio::broadcast` 通道与命令 FIFO 队列；
- 当前正在执行 Turn 时，新输入的 `UserInput` 进入队列排队，广播 `UserMessage { queued: true, ... }`；当前 Turn 完成后自动弹出下一条命令执行。

### 6.2 主动取消 (Cancel) 级联中断与队列清空
- **级联中断**：任意客户端发送 `ClientMessage::Cancel {}` 时，触发该 Session 的 `CancellationToken`，终止 LLM 请求并通过 `libc::killpg` 终止正在运行的 Shell 进程组；
- **清空队列**：立即清空所有排队的命令，广播 `AgentEvent::QueueCleared`；
- **残存消息落库**：将当前已生成的片段写入数据库，记录 `stop_reason: Cancelled`，广播 `TurnFinished`。

### 6.3 Agent 循环熔断器 (Circuit Breaker)
- 跟踪最近工具调用签名（`tool_name:input_json`）；
- 若单工具**连续 3 次传入完全相同的参数**且结果未发生改变，自动触发系统熔断；
- 拦截执行并注入引导性系统错误，提示模型切换思路。

### 6.4 先到先得审批仲裁与会话白名单
- **触发**：高危工具调用向所有连接客户端广播 `PermissionRequested`；
- **抢占**：首个到达的 `ApprovalResponse` 立即生效并广播 `PermissionResolved`；
- **超时与拒绝**：120 秒超时未响应或用户拒绝时，向模型注入标准错误 `ToolResult { is_error: true, content: "Execution rejected: Permission denied by user (or approval timed out after 120s)." }`；
- **`AllowSession` 作用域**：放行当前会话内该工具名称后续的所有调用，白名单保存在 `SessionRoom` 内存状态中，会话销毁或服务重启自动失效。

### 6.5 上下文预估与两阶段压缩 (Two-Stage Compaction)
- **Token 预估机制**：以大模型最新返回的权威 `input_tokens` 为基准，后续新增消息与输出采用启发式估算（代码/中文约 1.5~2 字符/Token，英文约 4 字符/Token）；
- **第一阶段（工具修剪）**：上下文估算达到模型最大窗口的 70% 时，将 3 轮之前的冗长 `ToolResult` 截断并保留简明摘要；
- **第二阶段（滑动窗口）**：修剪后仍超限时，固定保留 System 提示词与最新 $K$ 轮对话。

---

## 7. 工具集与执行环境 (Tools & MCP)

所有工具返回统一契约：`ToolOutput { output: String, is_error: bool }`。

### 7.1 执行安全防护
1. **输出字符截断 (`RESULT_MAX_CHARS = 24_000`)**：
   超长输出自动截断并在末尾追加提示，防止单次打爆上下文。
2. **Shell 进程组管理 (`libc::killpg`)**：
   创建独立进程组，发生超时（默认 60s）或用户取消时统一 `killpg` 杀掉整个进程树。
3. **宿主直跑与权限保护**：
   不做 OS 容器沙箱与路径限制，相对路径按工作区解析，绝对路径直通；依靠 Normal / Strict 模式的人机交互确认提供安全底线。

### 7.2 5 大核心内置工具
1. **`read`**：
   - 参数：`{ "path": "...", "offset": 1, "limit": 100 }`
   - 按行分片安全读取文件。
2. **`write`**：
   - 参数：`{ "path": "...", "content": "..." }`
   - 覆盖写入或新建文件。
3. **`edit`**（多 Hunk 原子替换与 Unified Diff）：
   - 参数：
     ```json
     {
       "path": "src/lib.rs",
       "edits": [
         { "old_text": "...", "new_text": "..." }
       ]
     }
     ```
   - 校验：原始基准唯一定位匹配、重叠区间拦截检测、全量原子事务；成功后返回统一 Diff。
4. **`shell`**：
   - 参数：`{ "command": "cargo test" }`
   - 工作目录绑定当前 workspace，实时捕获标准输出与标准错误。
5. **`task`**（Subagent 子任务委托）：
   - 参数：`{ "agent": "explore", "prompt": "..." }`
   - 在后台启动指定角色的子运行时，事件携带 `subagent_id` 实时广播；
   - 运行完成后的最终摘要文本作为该工具的 `output` 汇聚回主链路。

### 7.3 MCP 扩展机制
- Daemon 全局单例管理本地 stdio 进程与远程 SSE 客户端；
- 导出工具名统一加前缀 `mcp__{server}__{tool}`，避免命名冲突；
- 参数格式与返回统一桥接至 Agent 核心工具网格。

---

## 8. REST API 接口定义

所有接口均强制校验 `Authorization: Bearer <token>`：

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/server/status` | 服务端探活、版本号、活跃 Session 与连接数 |
| `GET` | `/api/sessions?workspace=...` | 获取指定 Workspace 下的所有会话列表元数据 |
| `POST` | `/api/sessions` | 在指定 Workspace 下创建新会话，返回 `session_id` |
| `DELETE` | `/api/sessions/:id` | 删除会话及其 SQLite 数据与全部附件目录 |
| `GET` | `/api/sessions/:id/messages?leaf_id=...` | 获取指定会话当前激活消息链的全部 `ChatMessage` |
| `GET` | `/api/workspace/tree` | 获取工作区目录文件树 |
| `GET` | `/api/workspace/file?path=...` | 安全读取工作区文件内容（供代码查看与编辑器） |
| `POST` | `/api/session/:id/upload` | 客户端上传多模态图片/附件（Multipart），返回 `session_attachment://` 引用 |

---

## 9. 命令行入口与 Web 前端工程

### 9.1 统一 `oma` 命令行入口 (`crates/bin`)
- `oma daemon`：独立启动后台 Daemon 服务（默认监听 `127.0.0.1:17431`）；
- `oma web`：启动 Daemon 并拉起 Web 前端页面服务，自动打开浏览器；
- `oma tui`：启动/连接 Daemon 并进入 Ratatui 终端交互界面。

### 9.2 原生 Web 前端工程 (`web/`)
- 技术选型：**纯原生 Vue 3 + TypeScript + 手写 CSS**（不引入 Tailwind、UnoCSS、Element Plus、NaiveUI 等任何第三方 UI 或样式库）；
- 具备功能：
  1. 会话列表管理与工作区选择；
  2. 树状对话流展示、Markdown 渲染、Thinking 思维链折叠；
  3. Tool 执行过程与参数/Diff 展示、Subagent 嵌套折叠；
  4. 权限审批模态框（AllowOnce, AllowSession, Deny）；
  5. 分支切换与回溯（`SwitchBranch`, `ForkAndRun`）；
  6. 设置面板（Token 配置、Model 切换、Agent 切换、Approval Mode 切换）。
