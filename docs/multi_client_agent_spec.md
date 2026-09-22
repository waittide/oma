# Oma 多类型多客户端协同 Agent 技术规格书 (Technical Specification)

> 版本：v2.10
> 状态：Implementation Verified（文档与代码同步）
> 适用形态：CLI / TUI、Vue 3 Web 前端、Tauri 桌面端（前端资产由客户端独立提供，Daemon 保持纯净 Headless）

> **v2.10 变更（工具集再收敛）**：
> - **移除 `ls`**：内置工具集由 5 个收敛为 4 个（`read` / `write` / `edit` / `shell`）；
>   目录列举改由 `shell` 执行系统命令承担

> **v2.9 变更（工具集收敛）**：
> - **移除 `find` / `grep`**：内置工具集由 7 个收敛为 5 个（`read` / `write` / `edit` /
>   `shell` / `ls`）；检索改由 `shell` 调用外部命令统一承担
> - **移除外部二进制管理**：`binaries` 模块（`fd` / `ripgrep` 的解析、下载与安装）一并删除，
>   `oma-tool` 不再依赖 `reqwest` / `oma-config`

> **v2.8 变更（工具命名与形态、默认预设与调色板、配置字段、前端外壳）**：
> - **工具命名与形态**：终端执行工具名为 `shell`，工具集即 `read` / `write` /
>   `edit` / `shell` / `ls` / `find` / `grep`；`edit` 改为 apply_patch 形态——输入是单个
>   `input` 字符串，内容为 `*** Begin Patch` … `*** End Patch` 包裹的补丁，支持
>   `*** Add File` / `*** Delete File` / `*** Update File`（含 `*** Move to:`）、
>   `@@` 上下文 hunk 与 `*** End of File`，一次可改多个文件
> - **默认预设**：删除内置 `Task`，默认预设改为 `Build`（拥有全部工具，模板不再声明
>   工具清单）；无配置文件时写入 `default_agent: build`
> - **调色板**：中性色调色板全部删除，内置只剩 Catppuccin 四套（浅色 `latte`，
>   深色 `frappe` / `macchiato` / `mocha`），默认深色 `mocha` + 浅色 `latte`
> - **配置字段**：`settings.json` 的 `server` 段拆为 `host` + `port`（`oma daemon --addr` 仍可覆盖）
> - **前端外壳**：删除底部状态栏与顶栏「导出为 markdown」「系统提示词」两个入口
>   （`GET /api/system-prompt` 端点保留）；右侧面板的关闭按钮移到面板最右端，
>   与顶栏展开 / 折叠同图标；空态主按钮为「新建会话」；设置里的默认 Agent 可选全局预设

> **v2.7 变更（恢复内置 MCP 客户端）**：
> 恢复清单的最后一项。`crates/oma-mcp` 回来了，支持两种传输：
> - **本地 stdio**：拉起子进程，按行 JSON-RPC 通信
> - **远程 HTTP**：JSON-RPC over POST（非 SSE）
> 工具按 `mcp__{server}__{tool}` 统一命名空间注册，与内置工具同权；
> 配置在 `settings.json` 的 `mcp_servers`（`type: local | remote`），
> 保存后自动重连，启动与配置变更时后台预热（建会话路径只取已预热缓存）。
> 名单经 `Ready.mcp_summaries` 下发，Web 顶栏有概览徽标、设置面板有 MCP 页。
>
> **仍保持删除**：熔断器、`ask` 提问工具、子代理 `task` 工具。

> **v2.5 变更（恢复 Agent 预设）**：
> 用户要求恢复被裁剪的预设能力（前后端），现回到「**模板即提示词**」：
> - 重新引入 5 套内置模板（`task` / `plan` / `explore` / `review` / `build`）
> - 项目级 `<workspace>/.oma/agents/` 与全局 `~/.config/oma/agents/` 可覆盖内置；
>   清单经 `GET /api/presets` 暴露，设置面板「预设」页可增删改
> - 预设声明的 `tools` 同时约束**下发给模型的工具清单**与**可执行工具集合**
> - `settings.json` 重新有 `default_agent`；会话头重新记录 `agent`
> - 预设正文之上仍依次拼接：运行环境块 → 项目上下文（`AGENTS.md`）→ 技能目录
>
> **仍然保持删除**：熔断器、`ask` 提问工具、内置 MCP 客户端、
> 子代理 `task` 工具。

> **v2.4 变更（工具实现方式与截断口径）**：
> - **工具实现方式**：`find` / `grep` 不再自研文件遍历，改为调用外部 `fd` / `ripgrep`
>   （缺失时自动下载到 `<数据目录>/bin`，`OMA_OFFLINE=1` 可禁用）；工具输出截断统一为
>   行数（2000）+ 字节（50KB）双上限，各工具条数限额与截断提示也一并统一
> - **工具集**：内置工具共 7 个（`edit` / `find` / `grep` / `ls` / `read` / `write` + 终端执行）
> - 新增 `AGENTS.md` / `CLAUDE.md` 上下文文件加载

---

## 1. 架构总览与工程拓扑 (Architecture & Crates Topology)

Oma 采用 **“单 Daemon 核心 + 统一 WebSocket/HTTP 网关 + 多端协同客户端”** 的分布式 C/S 架构，并在代码库层面采用模块化细粒度解耦设计：

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                           Client Matrix                                 │
│  ┌─────────────────┐    ┌─────────────────┐    ┌─────────────────────┐  │
│  │   CLI / TUI     │    │  Web (Browser)  │    │  Tauri (Desktop)    │  │
│  │ (Rust/Ratatui)  │    │ (Vue 3 / TS)    │    │ (Rust + Vue 3 UI)   │  │
│  │ crates/oma-tui  │    │ web/            │    │ (Future desktop)    │  │
│  └────────┬────────┘    └────────┬────────┘    └──────────┬──────────┘  │
│           │                      │                        │             │
│           │ Authorization: Bearer <token>                 │             │
│           │ (WebSocket /ws  &  HTTP REST /api/*)          │             │
└───────────┼──────────────────────┼────────────────────────┼─────────────┘
            ▼                      ▼                        ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                     Oma Core Daemon (Single Port: 17431)                │
│  ┌───────────────────────────────────────────────────────────────────┐  │
│  │           Axum Gateway & Auth Middleware (crates/oma-daemon)      │  │
│  │  - Headless API Gateway (REST APIs & WebSocket Endpoint)             │  │
│  │  - REST APIs (/api/sessions, /api/workspace/*, /api/server/*)        │  │
│  │  - WebSocket Upgrade & Room Routing (/ws)                            │  │
│  │  - CORS（仅放行本机来源）& Dev Proxy Middleware                      │  │
│  └─────────────────────────────────┬─────────────────────────────────┘  │
│                                    │                                    │
│  ┌─────────────────────────────────▼─────────────────────────────────┐  │
│  │        Session Registry & Multi-Client Rooms (crates/oma-runtime) │  │
│  │  - Session Room Dispatcher & Connection Lifecycle Manager         │  │
│  │  - Per-Session Command FIFO Queue & Cancel Cascader               │  │
│  │  - Event Broadcaster (tokio::broadcast + In-memory Catch-Up)      │  │
│  └─────────────────────────────────┬─────────────────────────────────┘  │
│                                    │                                    │
│  ┌─────────────────────────────────▼─────────────────────────────────┐  │
│  │               Agent Runtime Engine & Subsystems                   │  │
│  │  - oma-provider: LLM Streaming Normalization & Deep Merge         │  │
│  │  - oma-storage:  SQLite Session Store (index + per-session DB)   │  │
│  │  - oma-tool:     4 Tools (read/write/edit/shell)                  │  │
│  │  - oma-config:   JSON Config & Built-in System Prompt             │  │
│  │  - oma-contract: Shared Wire Protocol, Events, Data Models        │  │
│  └───────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
```

### 1.1 代码库 Crates 职责划分

| Crate / 目录 | 职责与依赖 |
|---|---|
| `crates/oma-contract` | 纯类型与协议契约（`Role`, `Block`, `ChatMessage`, `ClientMessage`, `ServerMessage`, `AgentEvent`, `ActiveTurnCatchUp` 等），零重依赖。 |
| `crates/oma-storage` | SQLite 双层持久化：全局索引库 `oma.db` 的 `sessions_index` 存会话元数据，每会话库 `sessions/<id>/session.db` 存 `messages` 消息树与 `session_meta` 运行时状态；附件仍落 `attachments/` 目录。WAL + 写池串行、读池并行，同数据目录多实例互相可见。 |
| `crates/oma-provider` | 手写轻量 SSE 状态机，统一归一化 Anthropic、OpenAI / DeepSeek、Responses 与 Google Gemini 的流式协议（含工具调用与多模态），HTTP 客户端进程级共享。 |
| `crates/oma-tool` | 内置 4 大工具（`read`, `write`, `edit` apply_patch 补丁, `shell` 进程组守卫），含统一的输出截断与工具集定义。 |
| `crates/oma-mcp` | MCP 客户端：本地 stdio 子进程与远程 HTTP（JSON-RPC over POST），工具按 `mcp__{server}__{tool}` 统一命名空间注册；只暴露已预热的工具缓存。 |
| `crates/oma-config` | 配置文件 `settings.json` / `models.json` 解析、内置 Agent 预设（4 套模板：`plan` / `explore` / `review` / `build`）与三层覆盖、上下文文件（`AGENTS.md`）加载、提示词拼装、调色板加载、数据目录定位。 |
| `crates/oma-runtime` | 核心 Agent Loop、Room 调度、命令 FIFO 队列、级联取消、70% 阈值两阶段上下文压缩。 |
| `crates/oma-daemon` | 基于 Axum 的 HTTP REST 与 WebSocket 网关、Bearer Token 鉴权中间件、静态路由与 CORS。 |
| `crates/oma-client` | 纯 Rust 客户端 SDK：`OmaClient` 封装 WebSocket 握手/事件流/指令，`SessionApi` 提供 REST 会话管理。 |
| `crates/oma-tui` | 基于 Ratatui 0.30 的终端交互客户端（流式渲染、CJK 折行），由 `oma tui` 驱动。 |
| `crates/oma` | 统一命令行可执行文件 `oma`，集成 `oma daemon`、`oma web`、`oma tui` 等子命令。 |
| `web/` | Vue 3 + TypeScript 的 Web 协同客户端；控件全部来自内部组件库 `@waittide/ui`（不用任何第三方 UI 框架）。 |

---

## 2. 传输协议与鉴权规范 (Transport & Security)

### 2.1 端口与网络拓扑
- **默认监听地址**：`127.0.0.1:17431`（`settings.json` 的 `server.host` / `server.port`）；
  支持 `oma daemon --addr 0.0.0.0:17431` 覆盖，供局域网与远程服务器接入。
- **单端口统一路由**：
  - `/ws`：双向 WebSocket（承载 `ClientMessage` / `ServerMessage` JSON 协议，基于 `session_id` 自动加入对应 Room）；
  - `/api/*`：HTTP REST 接口（会话 CRUD、消息回放、状态探测、文件树与文件内容、
    工具/技能清单、配置与调色板、附件上传与下载）；
  - Daemon 保持 Headless：不内置也不直出任何前端资产，只提供 API；
    界面统一由 `oma web` 提供（见 §9.1）。

### 2.2 严格 Bearer Token 鉴权
1. **传输规范**：
   - REST 接口仅接受 HTTP 请求头 `Authorization: Bearer <token>`，查询参数携带 token 一律拒绝
     （凭证会被访问日志、浏览器历史与 Referer 留存）；
   - WebSocket 握手因浏览器无法为 WS 请求设置自定义头，额外接受 `?token=<token>`；
   - 鉴权失败统一返回 HTTP `401 Unauthorized`，且不建立连接。
2. **Token 配置与存储**：
   - 唯一存放在配置文件 `settings.json` 的 `server.token`，用户可直接查看与修改；
   - 优先级：命令行 `--token` > 环境变量 `OMA_AUTH_TOKEN` > 配置文件；
   - 配置中缺省或为空时，启动时向配置文件补写默认值 `admin` 并落盘
     （不再生成 `auth.token` 随机密钥文件）；
   - 本地客户端（TUI / status）启动时读取同一配置文件获取 Token；
   - 远程客户端（Web / 远程 Tauri）在设置界面的「连接」页填写地址与 Token，
     保存在客户端本地（`localStorage`）；地址与 Token 都预填默认值
     （`http://127.0.0.1:17431` 与 `admin`），开箱即可连上默认 Daemon；
   - 连接页的空 Token 不会发出无凭证请求（否则只会看到 `Authorization: Bearer `，
     401 也查不出原因），保存失败时以通知给出「被拒绝 / 服务不可达」的具体原因。

> 默认只监听回环地址，因此默认 token 仅用于「装完即用」；
> 将 Daemon 暴露到局域网或公网前，必须先在设置界面或配置文件中改成强密钥。

---

## 3. 数据模型与 SQLite 会话持久化规范 (Data Contracts & Session Store)

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
        thinking: String,
        // 这一段思考的墙钟耗时（毫秒），运行时测量；不可测时为 null
        duration_ms: Option<u64>
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
> - 存储中 `ToolResult` 统一包裹在 `role: Role::User` 消息中；  
> - 发送给各厂商 API 时：Anthropic 协议保持 `role: "user"`；OpenAI / DeepSeek 协议在适配层自动将 `Block::ToolResult` 解构成独立的 `role: "tool"` 消息。

### 3.2 SQLite 会话持久化规范

会话数据落 SQLite：全局索引库 + 每会话库的双层结构；附件本体仍留在磁盘。

```text
~/.local/share/oma/
├── oma.db                     # 全局索引库：会话元数据，列表与详情只查它
└── sessions/<session_id>/
    ├── session.db             # 会话库：messages 消息树 + session_meta 运行时状态
    └── attachments/           # 会话附件
```

索引库 `sessions_index`：

```sql
CREATE TABLE sessions_index (
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
CREATE INDEX idx_sessions_workspace ON sessions_index(workspace);
CREATE INDEX idx_sessions_updated_at ON sessions_index(updated_at DESC);
```

会话库：

```sql
CREATE TABLE session_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE messages (
    id          TEXT PRIMARY KEY,
    parent_id   TEXT,                            -- 靠 parent_id 成树，可原地分叉
    role        TEXT NOT NULL,                   -- user / assistant
    blocks_json TEXT NOT NULL,                   -- oma 契约的 Block 数组
    model       TEXT,                            -- 产出该消息的模型（provider/modelId）
    usage_json  TEXT,                            -- 产生该消息那次请求的用量与墙钟耗时（TokenUsage）
    created_at  INTEGER NOT NULL,
    FOREIGN KEY(parent_id) REFERENCES messages(id)
);
CREATE INDEX idx_parent_id ON messages(parent_id);
```

`session_meta` 的键：
- `current_leaf_id`：当前叶子。会话库是树的真相，索引库同名列供列表直接使用；
- `context_usage`：上下文占用 `{"tokens":n,"contextLen":n,"covered":n}`（`covered` 为上次请求覆盖的历史条数），重启后据此恢复进度条；
- `workspace` / `title`：会话级副本。

#### 并发与一致性
- 每会话两套连接池：写池 `max_connections = 1` 严格串行，读池只读并行，读写互不阻塞；
- 库以 WAL 打开，`busy_timeout = 5s` 容忍跨进程短暂争用，`foreign_keys = ON`；
- 同数据目录上的多个 `StorageManager` 实例互相可见（一切以库为准，不缓存快照）；
- 删除消息子树按「父先子后」的逆序 `DELETE`（外键要求），并把当前叶子回退到被删消息的父节点；
- 索引库与会话库的 `messages` 表都在打开时逐列校验，缺列即报错并提示「用旧版导出后重建」，不做静默容忍；
- **不做任何数据迁移/格式兼容**：`messages` 缺 `model` / `usage_json`（SQLite 时代的老库）拒绝打开；`context_usage` 只认 `{"tokens","contextLen","covered"}`（旧的冒号串 `tokens:context_len:covered` 直接报错）；`blocks_json` / `role` / `usage_json` 读不出来也报错（带消息 id），不再兜底成「空内容」「当成用户消息」或「未知占用」——坏数据必须在第一时间暴露，而不是在界面上看起来像正常历史。

> 内容块沿用 oma 契约的 `Block` 形态（`tool_use`/`tool_result`），以 JSON 文本存于 `blocks_json`，可被本仓库完整回读。

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
    /// 设置当前会话的推理等级（必须在 REASONING_LEVELS 内，空串 = 未设置）
    SetReasoningLevel {
        level: String,
    },
    ForkAndRun {
        parent_message_id: String,
        new_content: Option<String>,
    },
    SwitchBranch {
        leaf_message_id: String,
    },
}

```

> 上行三类：连接、命令、取消。提问（`ask`）仍为移除状态。

### 4.3 服务端推送消息 (`ServerMessage` & `AgentEvent`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready { ready: Box<Ready> },   // 载荷内嵌两套完整调色板，体积远大于其他变体
    Event { event: Box<AgentEvent> },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ready {
    pub version:          String,
    pub session_id:       String,
    pub workspace:        String,
    pub active_model:     String,
    /// 当前会话的推理等级（空 = 未设置）
    pub reasoning_level:  String,
    pub current_leaf_id:  Option<String>,
    pub model_catalog:    BTreeMap<String, Vec<ModelInfo>>,
    pub context_usage:    Option<ContextUsage>,
    pub active_theme:     ResolvedTheme,   // 已解析主题（两套完整调色板）
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// 模型能力：既能描述输入模态，也覆盖产出形态
pub enum ModelCapability {
    Thinking,            // 思考
    TextInput,           // 文本输入
    TextOutput,          // 文本输出
    ImageInput,          // 图像输入
    ImageOutput,         // 图像输出
    VideoInput,          // 视频输入
    VideoOutput,         // 视频输出
    AudioInput,          // 音频输入
    AudioOutput,         // 音频输出
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id:            String,
    pub name:          String,
    pub context_len:   usize,
    /// 模型能力集合
    #[serde(default = "default_model_capabilities")]
    pub capabilities:  BTreeSet<ModelCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:    Option<usize>,
    /// 推理等级 → 厂商自定义字符串；未配置的等级按等级名下发
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTurnCatchUp {
    pub turn_id:              String,
    pub accumulated_thinking: String,
    pub accumulated_text:     String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tool_call:     Option<ToolCallStartedData>,
    /// 最近一次完成的请求的用量；全零表示本轮尚未有请求完成，界面据此不显示用量行
    #[serde(default)]
    pub usage:                TokenUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallStartedData {
    pub call_id:   String,
    /// 被调用的工具名
    pub tool_name: String,
    pub input:     serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum AgentEvent {
    // 1. 会话与轮次生命周期
    TurnStarted { 
        turn_id: String,
    },
    TurnFinished { 
        turn_id: String, 
        stop_reason: StopReason,
        usage: TokenUsage,
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

    /// 会话轮次占用状态变化：轮次开始 / 全部结束时广播，
    /// 所有已连接客户端据此更新侧栏运行中标记（不限当前会话）
    SessionRunning {
        session_id: String,
        running:    bool,
    },

    // 2b. 服务端权威队列深度；客户端不再自行累加，避免与丢弃的指令漂移
    QueueUpdated { pending: usize },

    // 2c. 广播缓冲溢出导致历史事件缺口：客户端须整体回读持久化状态
    SyncRequired {},

    // 3. 模型推理流式增量
    ThinkingDelta { 
        delta: String,
    },
    TextDelta { 
        delta: String,
    },

    // 4. 工具生命周期
    ToolCallStarted(ToolCallStartedData),
    ToolCallFinished {
        call_id:  String,
        /// 被调用的工具名
        tool_name: String,
        output:   String,
        is_error: bool,
    },

    // 5. 分支、模型与推理等级变更
    ActiveBranchChanged { current_leaf_id: String },
    ModelChanged        { active_model: String },
    ReasoningLevelChanged { level: String },

    // 5b. 上下文占用：每次模型请求拿到用量后广播，供界面展示进度。
    // tokens 为提示侧总量（含缓存），context_len 为模型窗口
    ContextUsage {
        tokens:      usize,
        context_len: usize,
    },

    // 5c. 本次请求的用量：每次模型请求结束（该请求的助手消息落库）后广播。
    // usage 与同时落库到该条助手消息的口径一致（只含这一次请求），
    // 界面据此在**流式期间**就显示输入/输出，不必等整轮结束后的回读；
    // 整轮总量仍由 TurnFinished.usage 给出
    UsageUpdated {
        usage: TokenUsage,
    },

    // 6. 重连快照与错误提示
    ActiveTurnCatchUp(ActiveTurnCatchUp),
    SessionRenamed { session_id: String, title: String },
    MessagesDeleted { deleted_ids: Vec<String>, current_leaf_id: Option<String> },
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
    /// 提示侧（输入）总量：统一含缓存读写，与各厂商口径对齐
    pub input_tokens:       usize,
    pub output_tokens:      usize,
    #[serde(default)]
    pub cache_read_tokens:  usize,
    #[serde(default)]
    pub cache_write_tokens: usize,
    /// 该用量所属那次模型请求的墙钟耗时；聚合值（TurnFinished / 追赶快照）为 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms:        Option<u64>,
}

/// 上下文占用快照
pub struct ContextUsage {
    /// 提示侧 token 总量（含缓存）
    pub tokens:      usize,
    /// 模型上下文窗口
    pub context_len: usize,
}
```

---

## 5. 配置体系、Provider 架构与提示词/技能

### 5.1 配置文件规范 (`~/.config/oma/settings.json` + `models.json`)

常规设置与提供商/模型清单分文件存放。

`settings.json`：

```json
{
  "default_model": "my_anthropic/claude-3-7-sonnet",
  "default_agent": "build",
  "default_reasoning_level": "medium",
  "mcp_servers": {
    "local_sqlite": { "type": "local", "command": "uvx", "args": ["mcp-server-sqlite", "--db-path", "oma.db"] },
    "remote_docs": { "type": "remote", "url": "https://mcp.example.com/rpc", "headers": { "Authorization": "Bearer …" } }
  },
  "theme": {
    "mode": "dark",
    "dark_palette": "mocha",
    "light_palette": "latte",
    "accent": "blue"
  },
  "server": { "host": "127.0.0.1", "port": 17431, "token": "admin" }
}
```

`models.json`：

```json
{
  "providers": {
    "my_anthropic": {
      "api_type": "anthropic",
      "base_url": "https://api.anthropic.com",
      "api_key": "env:ANTHROPIC_API_KEY",
      "headers": { "anthropic-beta": "prompt-caching-2024-07-31" },
      "models": [
        { "id": "claude-3-7-sonnet", "name": "Claude 3.7 Sonnet", "context_len": 200000, "capabilities": ["thinking", "text_input", "text_output", "image_input"] }
      ]
    },
    "my_deepseek": {
      "api_type": "completion",
      "base_url": "https://api.deepseek.com/v1",
      "api_key": "env:DEEPSEEK_API_KEY",
      "models": [
        { "id": "deepseek-chat", "name": "DeepSeek V3", "context_len": 64000 },
        { "id": "deepseek-reasoner", "name": "DeepSeek R1", "context_len": 64000, "capabilities": ["thinking", "text_input", "text_output"] }
      ]
    }
  }
}
```

> `api_type` 取值 `anthropic | completion | response | google`。
>
> `server` 段为 `host` + `port`（默认 `127.0.0.1` / `17431`），`token` 为空时启动补写 `admin`；
> 命令行 `oma daemon --addr` 仍可覆盖监听地址，优先级高于配置文件。
>
> `OmaConfig` 启用 `deny_unknown_fields`：字段为 `default_model` / `default_agent` /
> `default_reasoning_level` / `theme` / `server` / `providers` / `mcp_servers`。
> 拼错的键名会被 `PUT /api/config` 直接 400，而不是「保存成功但配置没变」。

### 5.2 请求头与请求体三级递归合并规范
优先级：`Provider 级配置` $\prec$ `Model 级配置` $\prec$ `Reasoning Effort 覆盖`。
- 同名 HTTP Header 强制后序覆盖前序；
- JSON Body 字段执行递归对象合并，支持灵活注入厂商私有字段。

### 5.3 Agent 预设与技能（Skill）

1. **Agent 预设 = 角色 + 工具白名单，且模板即提示词**：
   - 编译期内嵌 4 套模板：`plan` / `explore` / `review` / `build`
     （`crates/oma-config/src/templates/*.md`）；
   - `default_agent` 默认指向 `build`：它拥有全部 4 个内置工具，
     因此 `build.md` 不声明 `tools`（空 = 不限制，新装即完整可用）；
     `plan` / `explore` / `review` 各自声明受限的工具子集；
   - 同名文件优先取更具体的一层：项目 `<workspace>/.oma/agents/<id>.md`
     > 全局 `~/.config/oma/agents/<id>.md` > 内置模板；
   - 文件为 Markdown + YAML frontmatter，字段全部可选且忽略未知键：
     `name` / `description` / `tools`；frontmatter 损坏时整篇退化为正文；
   - 清单经 `GET /api/presets` 暴露（scope = bundled | global | project），
     设置面板「预设」页可增删改；列表同时下发 `body`（仅正文）与 `content`
     （完整原文），编辑器只展示/回写 `body`；
   - **`tools` 是硬约束**：既过滤下发给模型的工具清单，也拦截实际执行
     （为空表示不限制）。因此预设若声明工具，必须只写真实存在的名字——
     写过时的名字会静默地「少工具」，已加断言拦住；
   - `settings.json` 的 `default_agent` 决定新会话默认角色，
     设置面板「默认参数」页的选择器列出三层全部预设（bundled / global / project），
     会话内可切换（`SetAgent` 指令 → `AgentChanged` 事件），会话头记录 `agent`。

2. **System Prompt 的拼装顺序**：
   ```text
   <预设正文>

   <runtime_context>            ← 工作区 / shell 工作目录 / 系统 / 日期 / 当前模型

   <project_context>            ← AGENTS.md / CLAUDE.md（见 5.4）

   <available_skills>           ← 仅目录（id + 描述 + 路径）
   ```
   环境块示例：
   ```text
   <runtime_context>
   - Workspace: /absolute/path/to/project
   - Shell working directory: /absolute/path/to/project (shell commands already start here, do not prepend `cd`)
   - Operating System: linux (x86_64)
   - Today: 2026-09-20
   - Active Model: my_anthropic/claude-3-7-sonnet
   </runtime_context>
   ```

3. **技能（Skill）是按需取用的领域知识**：
   - 按三层根目录发现，每层布局为 `<root>/<skill-name>/SKILL.md`
     （技能自带资源放同级的 `scripts/` 等子目录）：
     | 层 | 路径 |
     |---|---|
     | global | `~/.agents/skills`（跨工具共享的用户级技能） |
     | agent | `~/.config/oma/skills`（oma 自身的技能） |
     | project | `<workspace>/.agents/skills`（随仓库分发的技能） |
   - 同名时更具体的一层覆盖更宽泛的一层：project > agent > global；
   - 技能**不**常驻 System Prompt，而是以目录形式注入（仅 id、描述与磁盘绝对路径）：
     ```text
     <available_skills>
     - cargo-conventions: Rust 构建约定 (read: /home/me/.agents/skills/cargo-conventions/SKILL.md)
     </available_skills>
     ```
     模型在任务相关时用 `read` 工具读取；无关轮次不占用上下文。

### 5.4 上下文文件（AGENTS.md / CLAUDE.md）

项目里写好的约定不该等模型想起去 `read` 才生效，因此常驻系统提示词：

| 层 | 路径 |
|---|---|
| global | `<配置目录>/oma/AGENTS.md`（由 `oma_config::global_context_dir()` 定位） |
| 祖先 | 自工作区向上**直到文件系统根**逐级查找 |

- 候选名按优先级：`AGENTS.override.md` > `AGENTS.md` > `AGENTS.MD` > `CLAUDE.md` > `CLAUDE.MD`；
  同一目录只取**一个**（两份互相矛盾的说明一起进上下文只会更糟）；
- 顺序为「全局 → 根 → … → 工作区」，越靠近工作区越靠后（越具体）；
- 注入形态：`<project_context>` 包一组 `<project_instructions path="…">`；
- 去 BOM；单个文件不可读只跳过，不让整段提示词构建失败。

> 已知差异：嵌套 git worktree 中被「遮蔽」的那份尚未跳过（待 worktrees 能力一并做）。

---

## 6. 多端协同与 Agent 运行引擎

### 6.1 Session Room 隔离与 FIFO 队列
- 服务端内存中按 `session_id` 维护独立的 `SessionRoom` 实例；
- 每个 Room 拥有独立的 `tokio::broadcast` 通道与命令 FIFO 队列；
- 轮次名额由 `is_running` 的 CAS 抢占，保证同会话任意时刻至多一个执行中的轮次；
- 抢不到名额的 `UserInput` 进入 FIFO 队列，广播 `UserMessage { queued: true, ... }`
  与 `QueueUpdated { pending }`；Turn 结束时串行交棒给下一条命令，队列排空后才释放名额。

### 6.2 主动取消 (Cancel) 级联中断与队列清空
- **级联中断**：任意客户端发送 `ClientMessage::Cancel {}` 时触发该 Session 的
  `CancellationToken`。取消信号以 `select!` 短路两处等待：LLM 流式接收与
  工具执行本身，因此长命令会立即返回；
- **进程回收**：`shell` 工具以 `ProcessGroupGuard` 守卫独立进程组，在超时或
  future 被取消（drop）时统一 `killpg(SIGKILL)`，不留孤儿进程；
- **清空队列**：立即清空排队命令，广播 `AgentEvent::QueueCleared` 与 `QueueUpdated { pending: 0 }`；
- **残存消息落库**：已生成的片段照常落库，未执行的 `tool_use` 补齐占位
  `tool_result`（保持历史自洽），随后广播 `TurnFinished { stop_reason: Cancelled }`；
- **单一轮次所有权**：轮次名额由 `is_running` 的 CAS 抢占，取消与排队交棒都
  经过同一状态机，确保同一会话任意时刻至多一个执行中的轮次。

### 6.3 上下文预估与两阶段压缩 (Two-Stage Compaction)
- **Token 预估机制（权威锚点 + 启发式增量）**：厂商每次响应上报的 `input_tokens`
  已包含 system prompt 与工具声明等固定开销，将其记录为**权威锚点**（覆盖发出该
  请求时的历史前缀）；此后只需对锚点之后新增的消息做字符折算增量
  （约 2 字符/Token，偏保守），即得
  `估算 = 权威输入 + 启发式(新增消息)`。相比纯字符折算——后者对英文与代码会高估
  约一倍、导致过早压缩——锚点让预算判定贴近真实占用。锚点仅在同一轮次内有效：
  前缀被裁剪后失效并自动回退纯启发式。阈值取模型 `context_len` 的 70%；
- **第一阶段（工具修剪）**：把最后一条消息之外的冗长 `ToolResult`（>300 字符）内容替换为
  占位文案。只改内容、不动消息，因此不会破坏 `tool_use` / `tool_result` 的配对；
- **第二阶段（轮次对齐裁剪）**：仍超限时，从最旧的**完整用户轮次**开始丢弃，
  直到落在预算内。绝不在轮次中途切断——否则会留下孤儿 `tool_result` 或让
  assistant 消息成为首条，Anthropic 与 OpenAI 均会以 400 拒绝该请求。
  该阶段的候选是历史的尾部切片，锚点前缀已不完整，故改用字符折算评估保留量
  （保守：宁可多裁一点也不会低估后超窗）；
- 压缩只作用于本次请求的副本，持久化历史保持完整。

---

## 7. 工具集与执行环境 (Tools & Runtime)

所有工具返回统一契约：

```rust
ToolOutput {
    output:   String,
    is_error: bool,
    /// 随输出回到模型的图片（mime + base64）；默认空
    images:   Vec<ToolImage>,
}
```

`images` 由上层交付给模型：Anthropic 内联进 `tool_result.content`，其余协议
拆为紧随回执的用户消息；模型不支持视觉时工具本身就不会产出图片。

### 7.1 输出截断与执行安全

1. **统一的输出截断（行数 + 字节双上限）**：
   `crates/oma-tool/src/truncate.rs` 提供两套互相独立的上限：
   - 默认上限：**2000 行 / 50KB**，先到者生效；除「末行本身超限」边界外不返回半行；
   - 方向：`read` 用 `truncate_head`（保留开头）；
     `shell` 用 `truncate_tail`（保留末尾，错误与结果在那里）；
   - 截断时在末尾追加**可操作**提示而非静默截断，例如
     `[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]`；
   - `edit` 的结果（成功文案 + unified diff）另受 24k 字符兜底限制。
2. **进程组管理 (`libc::killpg`)**：
   `shell` 创建独立进程组，发生超时（默认 60s）或用户取消时统一 `killpg` 杀掉整个进程树。
3. **宿主直跑与安全边界**：
   不做 OS 容器沙箱与路径限制，相对路径按工作区解析，绝对路径直通；
   安全边界由「本地回环监听 + Bearer Token」共同提供。

### 7.2 4 大核心内置工具

内置工具集：`shell` / `edit` / `read` / `write`。

1. **`read`**：
   - 参数：`{ "path": "...", "offset": 1, "limit": null }`（offset 为 1 起始行号，仅对文本生效）
   - 按行分片读取文本文件，输出带行号前缀（oma 自有的展示形式）；
     未给 `limit` 时由双上限决定，并在提示中给出续读用的 `offset`。
   - **图片文件**：按文件头识别 PNG / JPEG / GIF / WebP / BMP / TIFF（不引入解码器，
     仅解析头部取得宽高、通道数、alpha 与 MIME）：
     - 当前模型具备 `image_input` 能力时，图片经 `ToolOutput.images` 随工具回执
       回到模型（Anthropic 内联在 `tool_result.content`；其余协议作为紧随回执的
       用户消息），文本部分同时给出尺寸、通道、alpha、MIME 与体积；
     - 模型不具备图像输入能力、或图片超过 5 MiB 时，只返回元数据块，不下发图片字节。
2. **`write`**：
   - 参数：`{ "path": "...", "content": "..." }`
   - 覆盖写入或新建文件（自动建父目录）。
3. **`edit`**（apply_patch：一次可改多个文件）：
   - 参数：`{ "input": "<patch>" }`——补丁文本必须包在 `*** Begin Patch` /
     `*** End Patch` 之间（两行允许首尾空白），也接受 `<<'EOF'` heredoc 包裹；
   - 段落指令（一个补丁可含多段，覆盖多个文件）：
     - `*** Add File: <path>`：新建文件，其后每行都必须以 `+` 开头；
     - `*** Delete File: <path>`：删除文件；
     - `*** Update File: <path>`：修改文件，其后可跟一行 `*** Move to: <new path>` 表示改名；
   - `*** Update File` 之下是一个或多个 `@@` hunk：裸 `@@`、`@@ <上下文 / 函数名>`、
     统一 diff 的 `@@ -a,b +c,d @@`、行号提示 `@@ lines 5-7` / `@@ top of file`
     （可叠加多行 `@@`）；hunk 内以空格 / `-` / `+` 标记上下文 / 删除 / 新增行；
     末尾可写 `*** End of File` 表示补丁贴在文件结尾；
   - **两阶段落盘**：所有段落先在工作区内存里算好（任一段匹配失败即整体放弃，
     多文件补丁会附上 `No files were modified — sections apply atomically.`），
     再统一写盘；写盘阶段发生 IO 错误时返回已写入的文件清单，指明工作区处于部分更新状态；
   - 错误信息带问题文件前缀与 hunk 上下文：
     `[b.txt]: Failed to find expected lines in b.txt:` 后接期望内容与最接近的匹配；
   - 路径按工作区解析（相对路径相对 workspace）；成功后返回
     `Successfully applied N file operation(s).` 与一段 `Unified Diff:`。
4. **`shell`**：
   - 参数：`{ "command": "cargo test" }`
   - 工作目录绑定当前 workspace，捕获标准输出与标准错误（合并后按尾部截断）。
     环境块与工具描述都显式声明「命令已从工作区启动，无需 `cd`」，
     避免模型在每条命令前重复拼接工作区路径。
   - 解释器与环境取自启动时的登录 shell 快照：Daemon 启动时以 `$SHELL -lic`
     采集一次完整环境（含 rc 文件里的 `export` 与 `PATH`）并缓存，
     因此命令与用户交互终端一致；采集失败时退回直接继承 Daemon 进程环境。
     （写死的 `/bin/sh` 不会读 rc，环境也只继承 Daemon 而未必是用户终端。）

---

## 8. REST API 接口定义

所有接口均强制校验 `Authorization: Bearer <token>`：

| Method | Endpoint | Description |
|---|---|---|
| `GET` | `/api/server/status` | 服务端探活、版本号、活跃 Session 与连接数 |
| `GET` | `/api/sessions?workspace=...` | 获取指定 Workspace 下的所有会话列表元数据 |
| `POST` | `/api/sessions` | 在指定 Workspace 下创建新会话，返回 `session_id` |
| `DELETE` | `/api/sessions/{id}` | 删除会话库与该会话的全部附件（运行中的轮次拒绝删除） |
| `PATCH` | `/api/sessions/{id}` | 重命名会话（广播 `SessionRenamed`） |
| `GET` | `/api/sessions/{id}/messages?leaf_id=...` | 获取指定会话当前激活消息链的全部 `ChatMessage` |
| `GET` | `/api/sessions/{id}/messages/tree` | 获取该会话全部消息（含兄弟分支），用于构建历史树 |
| `DELETE` | `/api/sessions/{id}/messages/{message_id}` | 删除消息及其整棵子树，返回新的当前叶子 |
| `POST` | `/api/sessions/{id}/attachments` | 上传多模态附件（Multipart，单请求上限 16 MiB），返回 `session_attachment://` 引用 |
| `GET` | `/api/sessions/{id}/attachments/{name}` | 下载/预览附件 |
| `GET` | `/api/workspace/tree?workspace=...` | 获取工作区目录文件树（深度 4、最多 2000 项） |
| `GET` | `/api/workspace/file?workspace=...&path=...` | 读取工作区文件内容（供代码查看与编辑器） |
| `GET` | `/api/tools` | 列出可用工具（内置 4 个 + 已发现的 MCP 工具），供预设编辑器勾选 |
| `GET` | `/api/presets?workspace=...` | 列出 Agent 预设（bundled / global / project） |
| `GET`/`PUT`/`DELETE` | `/api/presets/{preset_id}` | 读取 / 写入 / 删除预设；内置预设只读 |
| `GET` | `/api/system-prompt?workspace=&model=&agent=` | 返回真正下发的系统提示词（预设正文 + 环境块 + 项目上下文 + 技能目录）；缺 `agent` 用 `default_agent` |
| `GET` | `/api/git/status?workspace=...` | 当前分支 + 变更文件清单 |
| `GET` | `/api/git/diff?workspace=&path=` | 单文件相对 HEAD 的 diff（未跟踪文件按整篇新增） |
| `GET` | `/api/skills?workspace=...` | 列出技能（global / agent / project） |
| `GET`/`PUT`/`DELETE` | `/api/skills/{skill_id}` | 读取 / 写入 / 删除技能 |
| `GET`/`PUT` | `/api/config` | 读取（默认脱敏 `api_key`，`?reveal=1` 返回明文）/ 写入服务端配置 |
| `GET` | `/api/palettes` | 列出全部调色板（内置 + 用户目录），带 `builtin` 标记 |
| `PUT`/`DELETE` | `/api/palettes/{palette_id}` | 写入 / 删除用户调色板；内置调色板只读（403），id 非法（含路径穿越）拒绝（400） |
| `GET` | `/ws` | WebSocket 升级（Bearer 头或 `?token=`，承载全部实时事件与指令） |

### 8.1 主题与调色板 (`oma-contract::Theme` / `Palette`)

调色板是**一整组 26 个色值**（12 个中性色 + 14 个强调色），而非「基底 + 覆盖表」：
`Theme` 只持有两个 id 引用与一个强调色令牌，浅色/深色各自独立选择。

```rust
pub struct Theme {
    pub mode:          ThemeMode,   // light | dark | system
    pub dark_palette:  String,      // 必须是 mode == dark  的调色板 id
    pub light_palette: String,      // 必须是 mode == light 的调色板 id
    pub accent:        String,      // ACCENTS 之一（对应 Palette 的强调色字段）
}

pub struct Palette {
    pub id:   String,   // 稳定 slug：引用键 + 文件名（改名不影响引用）
    pub name: String,   // 仅展示，可自由修改
    pub mode: PaletteMode, // light | dark
    pub crust: String, pub mantle: String, pub base: String,
    // …surface0-2 / overlay0-2 / subtext0-1 / text
    // …lavender / blue / sapphire / sky / teal / green / yellow / peach
    // …maroon / red / mauve / pink / flamingo / rosewater
}
```

- **存储**：内置 4 套编译期以 `include_str!` 嵌入二进制（与系统提示词同理，无启动写入）：
  浅色 `latte`，深色 `frappe` / `macchiato` / `mocha`；
  默认主题为深色 `mocha` + 浅色 `latte`。用户调色板存于 `<配置目录>/oma/themes/<id>.json`，
  内置 id 保留给内置文件（同名用户文件被忽略，写入亦被拒绝）。
  `settings.json` 中的 `theme` 仅保存引用，其 id 必须在调色板集合内。
- **下发**：`Ready.active_theme` 携带已解析的 `ResolvedTheme { mode, accent, light, dark }`，
  即两套完整调色板。终端与浏览器因此共用同一份配色数据，客户端不需要读取配置目录，
  也不需要在各自语言里再内置一份色值。
- **校验**：`PUT /api/config` 校验引用存在且明暗属性匹配（浅色不得引用深色调色板），
  强调色须在 `ACCENTS` 内；调色板写入时逐令牌校验为 `#rgb`/`#rrggbb`，id 须为安全 slug
  （同时决定文件名，故拒绝路径分隔符）。
- **前端**：调色板令牌由 `stores/theme.ts` 在运行时写入 `<html>` 的行内 CSS 变量，
  样式表内只定义语义别名层（`--ink`/`--paper`/`--accent`…）供组件引用，
  因此不依赖任何第三方配色包，也不随调色板数量增长而注入多张样式表。

---

## 9. 命令行入口与 Web 前端工程

### 9.1 统一 `oma` 命令行入口 (`crates/oma`)
- `oma daemon`：独立启动后台 Daemon 服务（默认监听 `127.0.0.1:17431`，
  取自 `settings.json` 的 `server.host` / `server.port`，`--addr` 可覆盖）；
- `oma web`：启动 web 客户端（仅内嵌前端静态服务，**不会**顺便拉起 Daemon，也不再依赖 Vite/pnpm）；
  构建产物经 `rust-embed` 内嵌进二进制（release 下不依赖任何外部目录），
  仅打印 `web 监听地址: http://…`（不打印 token），需 `--open` 才自动打开浏览器；
  监听地址与端口未由 `--host/--port` 指定时读自 `client.json` 的 `web`，
  并向页面提供同源接口 `GET/PUT /api/client/config`（见 9.3）；
- `oma tui`：启动 tui 客户端（连接 Daemon 并进入 Ratatui 终端交互界面）；
  连接来源优先级为 `--addr/--token` > `--connection <名称>` > `client.json` 活动连接
  > 默认地址，界面内 `Ctrl+O` 可在已保存连接间切换（回写 `client.json` 的 `active`）；
- `oma status` / `oma help [子命令]`：查看服务端状态 / 打印帮助；
- 不带子命令（`oma`）：等价于 `oma -h`，仅打印帮助，不自动启动任何界面；
- **输出全中文**：clap 的固定文案（`Usage:`/`Options:`/`Commands:` 标题、`[default: …]`
  与 `[OPTIONS]` 占位符、内建 `-h/--help`、`-V/--version`、`help` 子命令、解析错误）
  都是硬编码英文且没有 i18n 接口，故 `crates/oma/src/cli.rs` 在派生出的命令树上统一改写：
  自建帮助项与 `help` 子命令、用 `help_template` 接管版式、按 `ErrorKind` 与上下文
  重渲染解析错误（默认值现读自参数本身，不与帮助文案各写一份）。

### 9.2 原生 Web 前端工程 (`web/`)
- 技术选型：**Vue 3 + TypeScript**；UI 控件全部来自内部组件库 `@waittide/ui`
  （本地 `link:` 引入），不引入任何第三方 UI 框架；
- 工程约束：
  1. 包管理器统一使用 **pnpm**；
  2. 通知统一走 **vue-sonner**；
  3. 图标统一使用 **vue-icons-plus**（禁止用文字/字符充当图标）；
  4. 不使用浏览器原生控件（`<select>`/`<dialog>`/`<details>` 等），一律用 `@waittide/ui`
     的 `UiSelect` / `UiModal` / `UiInput` 等，以保证跨浏览器呈现一致；
     仅附件上传保留隐藏的 `<input type="file">` 作为「文件选择器通道」；
  5. 调色板不内置在前端：由服务端 `GET /api/palettes` 与 `Ready.active_theme` 提供，
     运行时注入 CSS 变量（见 8.1）。
- 具备功能：
  1. 会话列表管理与工作区选择；
  2. 树状对话流展示、Markdown 渲染、Thinking 思维链折叠；**流式中的那条回复与持久化
     消息一样带模型标签与用量行**——模型标签取轮次开始时记录的模型（回退当前模型），
     用量行取服务端每次模型请求后下发的**该次请求**的用量（`usage_updated` / 追赶快照），
     与随后回读到的这条助手消息上的数字完全一致；**每条助手消息的耗时取该次请求的墙钟**
     （`usage.duration_ms`，服务端测量），思考段与每次工具调用各自在折叠按钮左侧显示
     自己的耗时（`thinking.duration_ms` / 回执里的 `duration_ms`）；
  3. Tool 执行过程与参数/Diff 展示；
  4. 分支切换与回溯（`SwitchBranch`, `ForkAndRun`）与历史树弹层；
  5. 三栏工作区外壳（侧栏 / 消息区 / 右侧面板）与顶部工具栏，
     宽度与折叠状态可拖拽并持久化；右侧面板的关闭按钮位于面板标题栏最右端，
     与顶栏的展开 / 折叠按钮同图标；未选中会话时消息区显示空态主按钮「新建会话」；
  6. 设置面板（连接、外观/主题与调色板、语言、默认参数、提供商、预设、技能），
     组合器内含 预设 / 模型 两个选择器；「默认参数」页的默认 Agent 选择器列出三层
     全部预设（bundled / global / project）；
     其中「连接」页读写 `oma web` 同源接口的 `client.json`：列表可切换、增删连接，
     保存时 upsert 当前连接并置为 `active`；接口不可用（如 vite dev）时退回 localStorage；
  7. 界面 i18n 支持简体中文 / 繁体中文 / English / 日本語，缺键回退为键名；
     首次访问时按浏览器语言自动选择（`zh-Hans` / `zh-Hant` / `ja` / 其余为 `en`）；
  8. 用户消息操作区提供「复制」（与代码块复制共用降级到 `execCommand` 的剪贴板实现）；
  9. 轮次完成等需要人参与的事件**仅在失焦时**发出通知：
     应用内走 vue-sonner，并补一条浏览器系统通知；
     页面聚焦时不提示（消息已在眼前）；
  10. 右侧面板与顶部工具栏的部分入口（文件树/预览、Git 变更、分支）
      仍为占位，待后续分期接入；顶栏不做会话导出与系统提示词入口
      （`GET /api/system-prompt` 端点保留，供外部客户端调用）。

### 9.3 客户端本地配置 `client.json`
- 路径：`<配置目录>/oma/client.json`，与 Daemon 的 `settings.json` 分离，属「这台机器上的客户端」信息；
- 内容：`web` 段（`host`/`port`，仅 `oma web` 启动时使用）、`connections` 连接列表
  （`name`/`url`/`token`，用户可在客户端保存与切换）、`active`（当前活动连接名，空则取列表首个）；
- 读写：`oma web` 提供同源接口 `GET/PUT /api/client/config`（写入前校验名称/地址非空且不重名，
  经写锁串行化后原子落盘）；TUI 只读取连接列表，切换后回写 `active`；

---

## 10. 文档与实现的一致性说明

本规格书描述的是**当前实现**。以下为与早期草案不同、以代码为准的决策：

| 事项 | 决策与理由 |
|---|---|
| 鉴权传输 | REST 仅接受 `Authorization: Bearer`；WebSocket 握手额外接受 `?token=`，因为浏览器无法为 WS 请求设置自定义头。 |
| 前端静态资源 | `oma web` 提供界面：资产由 `rust-embed` 内嵌进二进制，debug 下从 `web/dist` 实时读取（改前端只需 `pnpm build`），release 下真正内嵌。Daemon 不直出资产、保持 Headless。 |
| 上下文压缩 | 第一阶段只替换 `ToolResult` 内容（保持配对），第二阶段按**完整轮次**丢弃前缀；旧的「按消息条数切半」会切出孤儿回执或以 assistant 开头，长会话下必然被厂商 API 拒绝。 |
| 内置工具集 | 只保留 `read` / `write` / `edit` / `shell`：检索与目录列举统一交给 `shell` 调用外部命令，不再为 `find` / `grep` / `ls` 维护各自的实现。 |
| 输出截断 | 统一走 `truncate` 模块（2000 行 / 50KB 双上限，先到者生效），不再每个工具一套字符数上限；截断必须给出可操作的后续动作（续读的 offset / 缩小 pattern），否则模型只能瞎猜。 |
| 会话标识 | `session_id` 会被拼接进文件系统路径，因此全局校验为 `[A-Za-z0-9_-]{1,128}`；附件名同样只允许安全字符并丢弃任何目录成分。 |
| 版本号来源 | `Cargo.toml` 的 `version` **不会**被 CI 自动递增，只在无标签的分支 / PR 构建里作为回退值被读取；正式版用人工推送的语义化标签（`v0.2.0`），每日快照用日期标签（`v2026.09.17`）。构建标识与制品名由 `.github/workflows/build.yml` 的 `meta` job 统一计算（标签优先），不再存在单独的发布工作流。 |
| shell 执行环境 | 解释器取 `$SHELL`（兜底 `/bin/sh`），环境取自 Daemon 启动时对登录 shell 的一次快照（`$SHELL -lic`），而非写死的 `sh` + 继承 Daemon 进程环境：后者既读不到 rc 文件里的 alias / `export`，环境也未必是用户终端的。 |
| 会话存储并发 | 每会话两套连接池：写池 `max_connections = 1` 严格串行、读池只读并行，读写互不阻塞；库以 WAL 打开并设 `busy_timeout`，因此同数据目录上的多个实例能互相看到最新提交。 |
| 错误分类 | 存储层返回 `StorageError`、房间返回 `RoomError`，HTTP 状态码由类型映射，不再依赖错误文案匹配。 |
| 配置校验 | `OmaConfig` 启用 `deny_unknown_fields`：拼错的键名（或前端字段映射错误）在 `PUT /api/config` 直接 400，不再「保存成功但配置没变」；启动时配置文件解析失败即报错退出，而非静默回退默认值。 |
| 能力裁剪 | 仅剩 MCP 之外的三项仍是删除状态：子代理 `task`、`ask` 提问、熔断器（均按最小内核原则移除）。**已应用户要求恢复**：Agent 预设（v2.5）、内置 MCP（v2.7）。 |
| 技能发现 | 按 `<root>/<name>/SKILL.md` 三层发现（global/agent/project），同名时更具体的一层覆盖更宽泛的一层；删除技能会连同其目录内的 `scripts/` 等资源一并移除（id 经严格校验，不可穿越）。技能与预设是两套独立机制：预设决定「以什么角色、能用哪些工具运行」，技能只是一段按需读取的知识，同名也不会互相覆盖。 |
| skill frontmatter 容错 | 技能的 YAML 字段全部可选且忽略未知键：用户目录里存在只有 `description` 与自有键的文件时，名称即目录名，不应因严格解析而整条不可用。 |
| 工具命名与 `edit` 形态 | 终端执行工具名为 `shell`。`edit` 放弃 `{path, edits[]}` 结构化替换，改为 apply_patch 形态——单个 `input` 字符串承载 `*** Begin Patch` 信封，可一次增 / 删 / 改 / 改名多个文件并整包原子落盘：多文件改动不再需要多次调用，也不会出现「改到一半失败、各文件状态不一致」。 |
| 默认预设 | 内置 `Task` 删除，默认改为 `Build`：`Build` 不声明 `tools`（= 不限制，拥有全部工具），承担「装完即用」的默认角色；无配置文件时写入 `default_agent: build`，会话库列的 `active_agent` 默认值同步为 `build`。 |
| 调色板默认 | 中性色调色板全部删除，内置 4 套全部为 Catppuccin 族（浅色 `latte`，深色 `frappe` / `macchiato` / `mocha`），默认深色 `mocha` + 浅色 `latte`。 |
| `server` 配置字段 | 监听地址由单个字符串字段拆成 `host` + `port`：设置面板本就分两个字段编辑，拆开后前后端不必再做「host:port」字符串的拼装与解析；`oma daemon --addr` 仍可覆盖。 |
| 前端外壳 | 删除底部状态栏（模型 / 推理 / 上下文占用 / 队列 / 工作区路径）与顶栏「导出为 markdown」「系统提示词」两个入口：这几处信息与入口在侧栏、设置面板、组合器中已各有归属，重复展示只增加维护面；`GET /api/system-prompt` 端点保留，供外部客户端取用。右侧面板关闭按钮移到面板标题栏最右端并与顶栏展开 / 折叠同图标，空态主按钮固定为「新建会话」。 |
| 内置终端 | **不做**：右侧面板只保留文件 / 变更 / 历史树三页，不提供终端标签页，也不引入 pty 通道（原先的占位标签页与文案已删除）。工作区里的命令执行由 Agent 的 `shell` 工具承担；面板开一个交互式 shell 等于把「任意命令执行」直接交给任何持 token 的客户端，与本项目「内核只做最小必要能力」的口径冲突。 |
| 数据兼容与迁移 | **不做**：存储层不保留任何旧格式兼容或补列迁移——老库缺列、`context_usage` 冒号串、坏 `blocks_json` / `role` / `usage_json` 一律硬报错（错误信息带缺失列名或消息 id），而不是自动补列、退化成 `None` 或兜底成空内容。理由：这些兼容分支只服务于「曾经存在过的格式」，保留它们会让「静默降级」在界面上看起来像正常历史，排障成本远高于让用户重建数据；协议层的 `#[serde(default)]`、前端可选字段与未知事件忽略不属于此类，它们是字段可选与前向兼容，保留。 |
| 用量口径 | 助手消息的 `usage` 只记**产生它的那一次模型请求**（含该次请求的整个提示侧，故缓存命中通常占大头）；整轮总量由 `TurnFinished.usage` 给出，不落到任何单条消息上。早期实现把「本轮累计值」逐条写进消息，于是长工具循环里最后一条会显示成整轮之和（实测上千万输入），被正确地读成了「这个工具调用消耗了这么多 token」。 |
| 耗时口径 | **谁的时间就记在谁身上**，且一律由服务端测量：每条助手消息记该次模型请求的墙钟（`usage.duration_ms`，发起到流读完）；每段思维链记这一段思考的耗时（`thinking.duration_ms`，首帧思考增量到该段结束）；每次工具调用记执行耗时（回执里的 `duration_ms`）。三者各自显示在自己的位置（消息底部统计行 / 折叠按钮左侧），不再用「整轮墙钟挂最后一条」的近似——那种做法要求前端拿消息 `created_at` 相减，即「提问到收尾」的累积值，贴在中间那条上会被读成这条消息本身的耗时。 |
| 设置面板结构 | 提供商页：提供商配置为单个带底色容器（标题在其内），模型配置为容器外分区标题，其下每个模型各自一个容器；预设页的工具授权用多选下拉（标签可逐个移除），选项来自 `GET /api/tools`。 |
