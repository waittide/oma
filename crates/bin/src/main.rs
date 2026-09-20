use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use cli::Commands;
use oma_config::{ClientConfig, OmaConfig};
use oma_daemon::{DaemonState, create_router, resolve_token};
use oma_storage::StorageManager;
use rust_embed::Embed;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod cli;

const DEFAULT_ADDR: &str = "127.0.0.1:17431";
const INDEX_HTML: &str = "index.html";

/// 内嵌的前端构建产物。
///
/// debug 构建下 rust-embed 直接从磁盘读取 `web/dist`，因此改完前端只要跑一次
/// `pnpm build` 即可生效，无需重新编译 Rust；release 构建则把资产写进二进制，
/// 部署时不再依赖任何外部目录，也不需要目标机器安装 node。
#[derive(Embed)]
#[folder = "$OMA_WEB_DIST"]
struct WebAssets;

fn get_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oma")
}

fn config_path(config_opt: Option<&Path>) -> PathBuf {
    config_opt
        .map(PathBuf::from)
        .or_else(OmaConfig::config_path)
        .unwrap_or_else(|| get_data_dir().join("settings.json"))
}

/// 加载配置；文件存在但解析失败时报错退出。
///
/// 静默回退到默认配置会丢掉全部 provider 与模型设置，比直接启动失败更难排查。
fn load_config(path: &Path) -> Result<OmaConfig> {
    if path.exists() {
        OmaConfig::load_from_file(path).with_context(|| format!("配置文件 {} 内容无效", path.display()))
    } else {
        Ok(OmaConfig::default())
    }
}

/// 取生效的 token，并在配置缺省时把默认值落盘。
///
/// 落盘是为了让用户能直接看到、并自行改成强密钥；否则「默认 admin」只存在于
/// 代码里，用户既不知情也无从修改。仅在原先没有值时写入，不覆盖用户已设的值。
fn resolve_and_persist_token(cli_token: Option<&str>, config: &mut OmaConfig, path: &Path) -> Result<String> {
    if config.server.token.is_empty() {
        config.server.token = oma_config::DEFAULT_AUTH_TOKEN.to_string();
        config
            .save_to_file_atomic(path)
            .with_context(|| format!("无法把默认 token 写入 {}", path.display()))?;
    }
    Ok(resolve_token(cli_token, config))
}

/// 只读取 token，不修改配置（供 tui / status 这类客户端使用）
fn read_token(cli_token: Option<&str>, config_path: &Path) -> Result<String> {
    let config = load_config(config_path)?;
    Ok(resolve_token(cli_token, &config))
}

/// 守护进程的信号策略。
///
/// - `SIGHUP`：传统上表示「控制终端已关闭」。脚本后台启动、或 SSH 会话结束时
///   内核会发这个信号，默认动作是**终止进程**；作为服务必须忽略它，否则
///   `oma daemon &` 之类的启动方式会随终端一起悄无声息地死掉。
/// - `SIGTERM` / `SIGINT`：作为正常的关闭请求，优雅退出。
#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    // 安装失败时不能直接退出：那会让进程回到「SIGHUP 终止」的默认行为，
    // 反而比不做任何处理更容易被悄悄杀掉
    let mut hup = match signal(SignalKind::hangup()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot install SIGHUP handler");
            return std::future::pending().await;
        }
    };
    let mut term = signal(SignalKind::terminate()).ok();
    let mut int = signal(SignalKind::interrupt()).ok();

    loop {
        tokio::select! {
            _ = hup.recv() => {
                tracing::info!("收到 SIGHUP（终端已关闭），继续在后台运行");
            }
            // 两个 Option 用 match 而非 unwrap：流未装上时对应分支整体不参与
            Some(_) = recv_opt(term.as_mut()), if term.is_some() => {
                tracing::info!("收到 SIGTERM，正在停止");
                return;
            }
            Some(_) = recv_opt(int.as_mut()), if int.is_some() => {
                tracing::info!("收到 SIGINT，正在停止");
                return;
            }
        }
    }
}

/// 包一层便于在 `select!` 中对 `Option<Signal>` 取流
#[cfg(unix)]
async fn recv_opt(sig: Option<&mut tokio::signal::unix::Signal>) -> Option<()> {
    match sig {
        Some(s) => s.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn start_daemon(addr: &str, token_opt: Option<&str>, config_opt: Option<&Path>) -> Result<()> {
    let data_dir = get_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let config_path = config_path(config_opt);
    let mut config = load_config(&config_path)?;
    let token = resolve_and_persist_token(token_opt, &mut config, &config_path)?;
    let config_paths = oma_config::ConfigPaths::from_settings_file(&config_path);

    // 采集用户登录 shell 环境（`$SHELL` + rc 里的 `export`）：之后所有会话的
    // `shell` 命令都以这份快照为准。耗时取决于用户 rc，丢到阻塞线程池，
    // 不占用运行时工作线程；采集失败不回退为错误，工具会退回继承本进程环境。
    let _ = tokio::task::spawn_blocking(oma_tool::init_shell_env).await;

    let storage = StorageManager::new(&data_dir).await?;

    let state = DaemonState::new(token.clone(), storage, config, config_paths);
    let app = create_router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("无法监听 {addr}"))?;

    // 与 `oma web` 保持同一种输出形式：只报监听地址，不打印 token，
    // 避免凭证留在终端回滚、CI 日志或 screen/tmux 记录里。
    tracing::info!("daemon 监听地址: http://{}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// 启动内嵌前端的静态服务。
///
/// 只提供界面本身：连接哪个 Daemon、用什么 token 由用户在设置的「连接」页填写，
/// 因此这里不需要也不应该知道 Daemon 的任何信息。另提供一个同源的
/// `/api/client/config` 给界面读写本机的 client.json（连接列表与绑定地址）。
async fn run_web(host: Option<&str>, port: Option<u16>, open: bool) -> Result<()> {
    if WebAssets::iter().next().is_none() {
        anyhow::bail!("前端资产缺失：请在 web/ 目录执行 `pnpm build` 后重新编译（当前二进制内没有任何文件）");
    }

    // 绑定地址优先取命令行参数；未给出时回落到 client.json，再缺省则为内置默认值
    let client_path = client_config_path();
    let client = load_client_config(&client_path)?;
    let host = host.map(str::to_string).unwrap_or(client.web.host.clone());
    let port = port.unwrap_or(client.web.port);

    let listener = tokio::net::TcpListener::bind((host.as_str(), port))
        .await
        .with_context(|| format!("无法监听 {host}:{port}"))?;
    // 端口传 0 时由系统分配，打印真实端口而不是用户传的 0
    let actual = listener.local_addr()?;

    let state = Arc::new(ClientConfigState {
        path: client_path,
        lock: tokio::sync::Mutex::new(()),
    });
    let app = Router::new()
        .route(
            "/api/client/config",
            axum::routing::get(get_client_config).put(put_client_config),
        )
        .with_state(state)
        .fallback(web_asset_handler);
    let url = format!("http://{}", format_host(&host, actual.port()));

    // 只报监听地址：这是用户唯一需要的信息；不打印 token，避免凭证进入
    // 终端回滚、CI 日志或 screen/tmux 记录。
    tracing::info!("web 监听地址: {}", url);

    if open {
        let _ = tokio::process::Command::new("xdg-open").arg(&url).spawn();
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// client.json 的标准路径（定位不到配置目录时回落到数据目录）
fn client_config_path() -> PathBuf {
    ClientConfig::config_path().unwrap_or_else(|| get_data_dir().join("client.json"))
}

/// 加载 client.json；文件存在但解析失败时报错退出，理由与 settings.json 一致。
fn load_client_config(path: &Path) -> Result<ClientConfig> {
    ClientConfig::load_from_file(path).with_context(|| format!("客户端配置文件 {} 内容无效", path.display()))
}

/// `oma web` 读写 client.json 所需的共享状态。
///
/// `lock` 串行化写入：浏览器可能并发调用 PUT，无锁时两次原子替换可能
/// 相互覆盖（后者读到旧的临时文件状态），造成连接列表丢失。
struct ClientConfigState {
    path: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

/// GET /api/client/config：每次从磁盘重读，手改文件后刷新页面即可生效。
async fn get_client_config(State(state): State<Arc<ClientConfigState>>) -> Response {
    match load_client_config(&state.path) {
        Ok(cfg) => Json(cfg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// PUT /api/client/config：校验后原子落盘，并回读返回规范化结果。
async fn put_client_config(State(state): State<Arc<ClientConfigState>>, Json(cfg): Json<ClientConfig>) -> Response {
    if let Err(e) = validate_client_config(&cfg) {
        return (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response();
    }
    let _guard = state.lock.lock().await;
    match cfg.save_to_file_atomic(&state.path) {
        Ok(()) => Json(cfg).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// 校验连接列表：名称与地址非空，名称不得重复（active 以名称为键）。
fn validate_client_config(cfg: &ClientConfig) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for conn in &cfg.connections {
        if conn.name.trim().is_empty() {
            anyhow::bail!("连接名称不能为空");
        }
        if conn.url.trim().is_empty() {
            anyhow::bail!("连接「{}」的地址不能为空", conn.name);
        }
        if !seen.insert(conn.name.as_str()) {
            anyhow::bail!("连接名称「{}」重复", conn.name);
        }
    }
    Ok(())
}

/// 绑定 `0.0.0.0` 时打印 localhost：用户要访问的是可点击的地址
fn format_host(host: &str, port: u16) -> String {
    let host = match host {
        "0.0.0.0" | "::" => "localhost",
        other => other,
    };
    format!("{host}:{port}")
}

/// 静态资源处理：命中文件则直出，未命中且不像文件路径时回落到 SPA 入口。
async fn web_asset_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // 本服务不含完整 API：若收到这类请求，说明前端把访问地址配到了
    // `oma web` 自己头上。回 404 并说清楚原因，比回一个 HTML 页面好排查得多。
    if path.starts_with("api/") || path == "ws" {
        return (
            StatusCode::NOT_FOUND,
            "此端口只提供前端静态资源与客户端连接配置；请把界面上的「访问地址」指向 oma daemon",
        )
            .into_response();
    }

    if path.is_empty() || path == INDEX_HTML {
        return index_html();
    }

    if let Some(content) = WebAssets::get(path) {
        return (
            [
                (header::CONTENT_TYPE, content.metadata.mimetype().to_string()),
                // 构建产物带内容哈希文件名，可长期缓存；入口 index.html 不缓存
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
            ],
            content.data,
        )
            .into_response();
    }

    // 含扩展名的路径视为真实资源缺失，返回 404；否则交给前端路由
    if path.contains('.') {
        return (StatusCode::NOT_FOUND, "404 Not Found").into_response();
    }
    index_html()
}

fn index_html() -> Response {
    match WebAssets::get(INDEX_HTML) {
        Some(content) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                // 入口必须每次回源校验：否则前端发版后用户会一直卡在旧页面
                (header::CACHE_CONTROL, "no-cache"),
            ],
            content.data,
        )
            .into_response(),
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "前端入口缺失：请重新执行 `pnpm build` 并编译",
        )
            .into_response(),
    }
}

async fn run_status(addr: &str, token_opt: Option<&str>) -> Result<()> {
    let token = read_token(token_opt, &config_path(None))?;

    let url = format!("http://{}/api/server/status", addr);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .context("无法连接 Oma Daemon")?;

    if resp.status().is_success() {
        let json: serde_json::Value = resp.json().await?;
        println!("Oma Daemon 运行正常（{addr}）:");
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        eprintln!("Oma Daemon 返回状态: {}", resp.status());
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // 不用 `main() -> Result`：那会让 anyhow 打出英文的 `Error: ` 前缀
    if let Err(err) = run().await {
        eprintln!("错误: {err:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = cli::parse();

    match args.command {
        Some(Commands::Daemon { addr, token, config }) => {
            start_daemon(&addr, token.as_deref(), config.as_deref()).await?;
        }
        Some(Commands::Web { host, port, open }) => {
            run_web(host.as_deref(), port, open).await?;
        }
        Some(Commands::Tui {
            addr,
            token,
            connection,
            workspace,
        }) => {
            let workspace = match workspace {
                Some(w) => w,
                None => std::env::current_dir()?.to_string_lossy().to_string(),
            };
            run_tui(addr, token, connection, workspace).await?;
        }
        Some(Commands::Status { addr, token }) => {
            run_status(&addr, token.as_deref()).await?;
        }
        Some(Commands::Help { subcommand }) => cli::print_help(subcommand.as_deref())?,
        None => {
            // 默认不启动任何界面，仅打印帮助，交由用户显式选择 `oma daemon` / `oma web` / `oma tui`
            cli::print_help(None)?;
        }
    }

    Ok(())
}

/// 启动 TUI：连接来源优先级为 命令行 > client.json 指定连接 > 活动连接 > 默认地址。
///
/// TUI 内切换连接后回写 client.json 的 active，下次启动仍在同一连接上。
async fn run_tui(
    addr: Option<String>,
    token: Option<String>,
    connection: Option<String>,
    workspace: String,
) -> Result<()> {
    let path = client_config_path();
    let mut client = load_client_config(&path)?;

    // 显式指定的连接名必须存在，避免静默换到别的地址
    let named = match connection.as_deref() {
        Some(name) => Some(
            client
                .connections
                .iter()
                .find(|c| c.name == name)
                .with_context(|| format!("client.json 中不存在名为「{name}」的连接"))?,
        ),
        None => None,
    };

    let addr = addr
        .or_else(|| named.map(|c| c.url.clone()))
        .or_else(|| client.active_connection().map(|c| c.url.clone()))
        .unwrap_or_else(|| DEFAULT_ADDR.to_string());

    let token = match token {
        Some(t) => t,
        // token 优先取与地址匹配的连接；否则退回命名/活动连接，最后才读 settings.json
        None => client
            .connections
            .iter()
            .find(|c| c.url == addr)
            .or(named)
            .or_else(|| client.active_connection())
            .map(|c| c.token.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or(read_token(None, &config_path(None))?),
    };

    let active = client
        .connections
        .iter()
        .find(|c| c.url == addr)
        .map(|c| c.name.clone());

    let options = oma_tui::TuiOptions {
        addr,
        token,
        workspace,
        connections: client
            .connections
            .iter()
            .map(|c| oma_tui::TuiConnection {
                name:  c.name.clone(),
                url:   c.url.clone(),
                token: c.token.clone(),
            })
            .collect(),
        active,
    };

    if let Some(name) = oma_tui::run(options).await? {
        if client.active != name {
            client.active = name;
            client
                .save_to_file_atomic(&path)
                .with_context(|| format!("无法把活动连接写入 {}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use oma_config::Connection;

    use super::*;

    /// 资产自带 MIME 与强缓存；含扩展名的缺失资源必须是 404，不能回落成 HTML。
    #[tokio::test]
    async fn test_serves_index_and_assets() {
        let resp = index_html();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-cache",
            "入口页必须每次回源校验，否则前端发版后用户会卡在旧页面"
        );

        let missing = web_asset_handler("/definitely-missing.js".parse().unwrap()).await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }

    /// SPA 路由必须回落入口；API 路径必须 404，避免用户把访问地址配到 web 端口上时
    /// 拿到一个看似正常的 HTML 页面而难排查。
    #[tokio::test]
    async fn test_spa_fallback_and_api_rejection() {
        let deep = web_asset_handler("/sessions/abc".parse().unwrap()).await;
        assert_eq!(deep.status(), StatusCode::OK);

        for path in ["/api/server/status", "/api/config", "/ws"] {
            let resp = web_asset_handler(path.parse().unwrap()).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{path} must not fall back");
            let body = to_bytes(resp.into_body(), 4096).await.unwrap();
            let text = String::from_utf8_lossy(&body);
            assert!(text.contains("oma daemon"), "{path} should explain where the API lives");
        }
    }

    #[test]
    fn test_format_host_replaces_unspecified_bind() {
        // 绑定 0.0.0.0/:: 时打印的应是可点击的 localhost
        assert_eq!(format_host("0.0.0.0", 5173), "localhost:5173");
        assert_eq!(format_host("::", 5173), "localhost:5173");
        assert_eq!(format_host("127.0.0.1", 5173), "127.0.0.1:5173");
    }

    /// client.json 校验：空名称、空地址与重名都要拒绝，
    /// 否则 active 按名称引用时会歧义。
    #[test]
    fn test_validate_client_config() {
        let conn = |name: &str, url: &str| Connection {
            name:  name.into(),
            url:   url.into(),
            token: String::new(),
        };

        let ok = ClientConfig {
            connections: vec![conn("a", "http://127.0.0.1:17431"), conn("b", "http://10.0.0.5:17431")],
            ..ClientConfig::default()
        };
        assert!(validate_client_config(&ok).is_ok());

        let empty_name = ClientConfig {
            connections: vec![conn("  ", "http://x")],
            ..ClientConfig::default()
        };
        assert!(validate_client_config(&empty_name).is_err());

        let empty_url = ClientConfig {
            connections: vec![conn("a", "")],
            ..ClientConfig::default()
        };
        assert!(validate_client_config(&empty_url).is_err());

        let duplicated = ClientConfig {
            connections: vec![conn("a", "http://x"), conn("a", "http://y")],
            ..ClientConfig::default()
        };
        assert!(validate_client_config(&duplicated).is_err());
    }
}
