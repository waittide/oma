# Oma

[简体中文](README.md) · [English](README.en.md) · [日本語](README.ja.md)

> 多端协同的 AI Agent 工作台：一个 Rust 内核 Daemon，同时服务终端、浏览器与桌面客户端。

[![License](https://img.shields.io/badge/license-MIT-89b4fa.svg)](LICENSE)
[![build](https://github.com/waittide/oma/actions/workflows/build.yml/badge.svg)](https://github.com/waittide/oma/actions/workflows/build.yml)
[![Rust](https://img.shields.io/badge/rust-nightly-cba6f7.svg)](rust-toolchain.toml)
[![Vue](https://img.shields.io/badge/Vue-3-42b883.svg)](web/package.json)
[![Spec](https://img.shields.io/badge/技术规格书-42KB-f9e2af.svg)](docs/multi_client_agent_spec.md)

Oma 把一个完整的编码 Agent 拆成两层：**无头的 Daemon 内核**（会话、模型、工具、持久化全部在 Rust 侧）
和**可替换的客户端**（终端 TUI、Web 控制台，桌面端在规划中）。
多个客户端可以同时接入同一个会话——你在终端里发起的一轮对话，浏览器上能看到同样的流式输出、同样的工具调用卡片，
审批弹窗在任意一端点过就全体生效。

界面与命令行以**简体中文为第一语言**，Web 端内置简体中文 / 繁體中文 / English / 日本語四套文案。

![Web 客户端：工具调用、思考过程与代码块](docs/images/web-chat.png)

---

## 目录

- [特性](#特性)
- [截图](#截图)
- [架构](#架构)
- [快速开始](#快速开始)
- [配置](#配置)
- [内置工具](#内置工具)
- [目录结构](#目录结构)
- [文档](#文档)
- [许可证](#许可证)

---

## 特性

### 多端协同

- **单 Daemon 单端口**：默认 `127.0.0.1:17431`，一个端口同时承载 WebSocket（`/ws`）与 REST（`/api/*`）。
- **会话房间（Session Room）**：每个会话一个房间，事件经 `tokio::broadcast` 广播给全部在线客户端，
  新接入的客户端会收到进行中轮次的追平快照（catch-up），不会看到半截对话。
- **命令 FIFO 与级联取消**：同会话指令进入串行队列；一次 `cancel` 会中断当前轮次并清空排队指令。

### Agent 运行引擎

- **回合制 Agent Loop**：思考、正文、工具调用、工具结果全部建模为结构化 Content Block，随历史落库。
- **会话树而非线性列表**：消息带 `parent_id`，可以从历史任意节点分叉重开，Web 端有专门的历史树视图。
- **两级上下文管理**：按模型声明的 `context_len` 计算 70% 阈值，先做工具结果截断、再做两阶段压缩；
  权威 Token 锚点会抑制不必要的压缩，避免把已知的准确值覆盖成启发式估算。

### 模型与工具

- **四种协议归一化**（手写 SSE 状态机）：Anthropic Messages、OpenAI / DeepSeek Chat Completions、
  Responses、Google Gemini。工具调用与多模态图片在四种协议下统一成同一套 `Block`。
- **请求头 / 请求体三级递归合并**：Provider 级 $\prec$ Model 级 $\prec$ 推理等级覆盖，可注入厂商私有字段。
- **七个内置工具**：`read`（支持读图片）、`write`、`edit`（原子多段替换 + 统一 diff）、
  `bash`（进程组守卫 + 可配置超时）、`ls`、`find`（glob）、`grep`（正则 / 字面量搜索）。
  工具集与 pi 对齐：内核只保留最小可用的读写与检索能力。
- **可配置的展示能力**：模型能力（思考 / 文本 / 图片输入输出 / 音频）逐模型声明，
  界面据此决定是否内联图片、是否展示思考开关。

### 客户端

- **Web（`web/`）**：Vue 3 + TypeScript，手写 CSS，**零外部 UI / CSS 库**；
  深色浅色各四套 Catppuccin 调色板、四语言文案、Markdown 渲染、历史树、消息导航栏、附件与图片预览。
- **TUI（`crates/oma-tui`）**：基于 Ratatui 的终端客户端，流式渲染、CJK 折行。
- **CLI**：`oma daemon | web | tui | status`，帮助与解析错误全部中文化。
- **零运行时依赖**：前端构建产物在编译期经 `rust-embed` 内嵌进二进制，发布时不需要目标机器安装 Node。

---

## 截图

### Web 控制台

会话主界面：思考折叠、工具调用卡片、工具回执、代码块一键复制。

![Web 会话主界面](docs/images/web-chat.png)

### 终端客户端

![TUI](docs/images/tui.png)

---

## 架构

```mermaid
flowchart TB
    subgraph Clients["客户端矩阵"]
        direction LR
        TUI["CLI / TUI<br/>crates/oma-tui · Ratatui"]
        WEB["Web<br/>web/ · Vue 3 + TS"]
        DESK["桌面端<br/>Tauri（规划中）"]
    end

    subgraph Daemon["Oma Daemon · 单端口 17431"]
        direction TB
        GW["Axum 网关<br/>REST / WebSocket · Bearer 鉴权 · CORS"]
        ROOM["Session Room 调度<br/>命令 FIFO · 级联取消 · 事件广播"]
        ENGINE["Agent 运行引擎<br/>回合循环 · 上下文压缩"]
        subgraph Sub["子系统"]
            direction LR
            PROV["oma-provider<br/>流式归一化"]
            TOOL["oma-tool<br/>七个工具"]
            STORE["oma-storage<br/>JSONL 会话树"]
            CONF["oma-config<br/>settings.json"]
        end
    end

    VENDORS["模型厂商<br/>Anthropic · OpenAI · Responses · Gemini"]

    TUI -->|"Bearer token<br/>ws + rest"| GW
    WEB --> GW
    DESK --> GW
    GW --> ROOM --> ENGINE
    ENGINE --> PROV --> VENDORS
    ENGINE --> TOOL
    ENGINE --> STORE
    CONF -.->|"启动期加载"| ENGINE
```

### Crates 职责

| Crate / 目录 | 职责 |
|---|---|
| `crates/oma-contract` | 纯类型与协议契约（`Role`、`Block`、`ClientMessage`、`ServerMessage`、`AgentEvent` 等），零重依赖 |
| `crates/oma-storage` | JSONL 会话树持久化：每会话一个 `session.jsonl`（pi 风格 id/parentId 树）+ `attachments/` 目录 |
| `crates/oma-provider` | 手写 SSE 状态机，归一化四家流式协议（含工具调用与多模态） |
| `crates/oma-tool` | 七个内置工具与输出截断 |
| `crates/oma-plugin` | QuickJS 插件：以 JS 注册工具/命令/事件钩子，host API 白名单桥接 |
| `crates/oma-config` | `settings.json` / `models.json` 解析、系统提示词、调色板与项目级覆盖 |
| `crates/oma-runtime` | Agent Loop、会话房间、命令队列、级联取消、上下文压缩 |
| `crates/oma-daemon` | Axum HTTP / WebSocket 网关、Bearer 鉴权中间件、REST 路由 |
| `crates/oma-client` | 纯 Rust 客户端 SDK：`OmaClient`（事件流与指令）与 `SessionApi`（会话管理） |
| `crates/oma-tui` | Ratatui 终端客户端 |
| `crates/oma` | 统一可执行文件 `oma`：`daemon` / `web` / `tui` / `status` |
| `web/` | Vue 3 + TypeScript + 手写 CSS 的 Web 客户端 |

---

## 快速开始

### 环境要求

| 依赖 | 版本 | 说明 |
|---|---|---|
| Rust | nightly | 仓库内 `rust-toolchain.toml` 已固定为 nightly（`edition = "2024"`） |
| Node.js | ≥ 20 | 仅构建前端需要；运行时不需要 Node |
| pnpm | ≥ 9 | 前端包管理器 |

平台：Linux 与 macOS 为日常开发环境；Windows 具备对应 `cfg` 回退分支，但未做验证。

### 直接下载预编译产物

不想自己搭工具链时，直接用 GitHub Actions 构建好的二进制：

| 方式 | 位置 | 说明 |
|---|---|---|
| 每日快照 | [Releases](https://github.com/waittide/oma/releases) | 每天 00:00 自动发布日期版本，如 `v2026.09.17`。标记为 pre-release，不会占用 Latest |
| 正式版本 | 同上 | 手工推送语义化标签，如 `v0.2.0`，成为 Releases 页的 Latest |
| 某次构建 | [Actions](https://github.com/waittide/oma/actions/workflows/build.yml) → 选中一条 run → 页面底部 **Artifacts** | 每次 push / PR 都会构建，产物默认保留 90 天 |
| 手动触发 | 同一页面 → **Run workflow** | 不改代码即可重新构建并取产物 |

发布分两条互不干扰的轨道：每日快照只打日期标签、不动源码，供随时取用最新构建；
正式版本由人工维护语义化标签，用来标记有意义的里程碑。二进制的自报版本形如
`0.2.0+1a2b3c4`（版本号 + 提交短 hash），任何一份产物都能对应回具体提交：

```bash
oma --version    # oma 0.2.0+1a2b3c4
oma status       # 运行中 Daemon 的版本，取自 /api/server/status
```

仓库为公开仓库，Artifacts 无需登录即可下载。产物命名 `oma-<版本>-<target>.tar.gz`（Windows 为 `.zip`），
解包后是自包含的 `oma` 可执行文件 + `README.md` + `LICENSE`——前端资产已在编译期内嵌，**运行时不需要 Node，也不需要 `web/dist`**：

| 平台 | target |
|---|---|
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux aarch64 | `aarch64-unknown-linux-gnu` |
| macOS Apple Silicon | `aarch64-apple-darwin` |
| macOS Intel | `x86_64-apple-darwin` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

```bash
tar xzf oma-0.1.0-x86_64-unknown-linux-gnu.tar.gz
cd oma-0.1.0-x86_64-unknown-linux-gnu
./oma daemon     # 终端 A：内核
./oma web        # 终端 B：Web 控制台
```

### 构建

前端资产会在 `cargo build` 时经 `rust-embed` 内嵌进二进制，**必须先构建前端**：

```bash
# 1) 前端
cd web
pnpm install
pnpm build
cd ..

# 2) Rust
cargo build --release
```

产物为 `target/release/oma`（自包含，发布时无需携带 `web/dist`）。

### 运行

```bash
./target/release/oma daemon     # 启动内核，默认监听 127.0.0.1:17431
./target/release/oma web        # 启动 Web 控制台，默认 http://127.0.0.1:5173
./target/release/oma tui        # 终端客户端（复用当前工作区最近的会话）
./target/release/oma status     # 查看 Daemon 运行状态
```

首次使用：

1. 打开 Web 控制台，进入 **设置 → 连接**，填入地址与 Token（默认 `http://127.0.0.1:17431` / `admin`）；
2. 进入 **设置 → 提供商**，添加 Provider（`api_type` 与 `base_url`）与其下的模型；
3. 新建工作区会话，选择模型后即可对话。

> Daemon 默认只监听回环地址。若要暴露到局域网或公网，**务必先把 `settings.json` 里的 `server.token` 改成强密钥**。

### 前端开发

```bash
cd web
pnpm dev        # Vite 开发服务器，/api 与 /ws 自动代理到 127.0.0.1:17431
pnpm test       # 单元级校验脚本（流式分段、子代理渲染）
pnpm test:e2e   # 端到端：真实 Daemon + 假厂商 SSE 服务
```

`cargo build` 的 debug 构建下，`rust-embed` 直接从磁盘读取 `web/dist`：
改完前端跑一次 `pnpm build` 即可生效，无需重新编译 Rust。

---

## 配置

配置文件位于 `~/.config/oma/`（遵循 `XDG_CONFIG_HOME`），数据目录为 `~/.local/share/oma/`：

```text
~/.config/oma/
├── settings.json       # 常规设置：默认模型、推理等级、theme、server
├── models.json         # 提供商与模型清单（providers）
├── skills/             # oma 自身技能（<id>/SKILL.md）
├── plugins/            # 全局 QuickJS 插件（<id>/plugin.js）
└── themes/             # 自定义调色板（*.json）
~/.local/share/oma/
└── sessions/<id>/             # 每个会话一个目录
    ├── session.jsonl          # pi 风格的 JSONL 会话树（首行 header + 条目带 id/parentId）
    └── attachments/           # 会话附件
```

> 旧版 `config.toml` / `client.toml` 会在首次启动时自动迁移为 `settings.json` + `models.json` /
> `client.json`，原文件备份为 `*.toml.bak`。

最小示例（`settings.json`）：

```json
{
  "default_model": "my_anthropic/claude-3-7-sonnet",
  "default_reasoning_level": "medium",
  "server": { "listen_addr": "127.0.0.1:17431", "token": "admin" }
}
```

提供商与模型（`models.json`）：

```json
{
  "providers": {
    "my_anthropic": {
      "api_type": "anthropic",
      "base_url": "https://api.anthropic.com",
      "api_key": "env:ANTHROPIC_API_KEY",
      "models": [
        {
          "id": "claude-3-7-sonnet",
          "name": "Claude 3.7 Sonnet",
          "context_len": 200000,
          "capabilities": ["thinking", "text_input", "text_output", "image_input"]
        }
      ]
    }
  }
}
```

`api_type` 取值 `anthropic | completion | response | google`；`api_key` 支持 `env:VAR` 间接引用。

Token 优先级：命令行 `--token` > 环境变量 `OMA_AUTH_TOKEN` > 配置文件。
配置文件缺省时，Daemon 启动会补写默认值 `admin` 并落盘，方便直接查看与修改。

Agent 预设已移除：oma 使用与 pi 相同的单一内置系统提示词，不再有角色/预设概念；
需要额外行为请使用技能或插件。

---

## 内置工具

| 工具 | 说明 |
|---|---|
| `read` | 按行区间读文件；图片在模型支持视觉时直接内联，否则返回尺寸 / 通道 / MIME 摘要 |
| `write` | 覆盖或创建文件 |
| `edit` | 原子多段替换，带重叠检查与统一 diff |
| `bash` | 执行命令，独立进程组 + 兜底 `killpg`，超时可配置 |
| `ls` | 列出目录条目 |
| `find` | 按 glob 模式查找文件（支持 `**` 跨目录） |
| `grep` | 按正则 / 字面量搜索文件内容 |

工具集与 pi 内核保持一致：内核只保留最小可用的读写与检索能力。

---

## 插件（QuickJS）

插件是纯 JavaScript 文件，由进程内嵌的 QuickJS 引擎执行（无需 Node），通过全局 `oma`
对象注册工具、命令与事件钩子。放于 `<workspace>/.oma/plugins/<id>/plugin.js`（项目级）
或 `~/.config/oma/plugins/<id>/plugin.js`（全局），同名时项目级覆盖全局。

```js
oma.registerTool({
  name: "word_count",
  description: "Count words in a file",
  parameters: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
  execute: (p) => String(oma.readFile(p.path).split(/\s+/).filter(Boolean).length),
});

oma.on("tool_call", (e) =>
  e.name === "bash" && /rm\s+-rf/.test(e.input.command || "")
    ? { block: true, reason: "destructive bash command" }
    : undefined);

oma.registerCommand({ name: "explain", description: "Explain a topic", handler: (a) => `Please explain: ${a.topic}` });
```

宿主 API：

| API | 说明 |
|---|---|
| `oma.log(...)` | 写日志 |
| `oma.readFile(path)` / `oma.writeFile(path, content)` | 读写文本文件（拒绝绝对路径与 `..`，限 workspace 内） |
| `oma.listDir(path)` / `oma.exists(path)` | 列目录 / 判断存在 |
| `oma.exec(cmd)` | 在 workspace 下执行命令，返回 `{stdout, stderr, code}` |
| `oma.registerTool(def)` | 注册工具（`execute` 必须同步返回） |
| `oma.registerCommand(def)` | 注册命令（限定名 `plugin:command`） |
| `oma.on(event, fn)` | 事件钩子，当前支持 `tool_call`（返回 `{block:true, reason}` 可拦截） |

插件工具会随会话装配进工具注册表，并由 `GET /api/tools?workspace=...` 以 `kind: "plugin"` 列出。

> **安全**：插件与 pi 的扩展一样拥有宿主进程权限（文件访问被限在 workspace 内），安装前请审查源码。

---

## 目录结构

```text
oma/
├── crates/
│   ├── oma/          # 统一命令行入口 oma
│   ├── oma-client/   # Rust 客户端 SDK
│   ├── oma-config/   # 配置解析与系统提示词
│   ├── oma-contract/ # 协议与数据模型
│   ├── oma-daemon/   # Axum 网关
│   ├── oma-provider/ # 模型厂商流式适配
│   ├── oma-runtime/  # Agent 运行引擎
│   ├── oma-storage/  # JSONL 会话树持久化
│   ├── oma-tool/     # 内置工具
│   └── oma-tui/      # 终端客户端
├── docs/
│   ├── multi_client_agent_spec.md   # 技术规格书（协议、DDL、配置、接口全量定义）
│   └── images/                      # README 截图
├── web/              # Vue 3 + TypeScript Web 客户端
└── Cargo.toml        # workspace 定义
```

---

## 文档

- [技术规格书](docs/multi_client_agent_spec.md)：架构拓扑、WebSocket 契约、JSONL 会话格式、
  配置规范、REST 接口、插件扩展机制、实现一致性说明。

---

## 许可证

[MIT](LICENSE) © 2026 waittide
