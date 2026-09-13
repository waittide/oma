use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use axum::{
    Router,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use clap::{CommandFactory, Parser, Subcommand};
use oma_config::OmaConfig;
use oma_daemon::{DaemonState, create_router, resolve_or_create_token};
use oma_mcp::McpManager;
use oma_storage::StorageManager;
use rust_embed::Embed;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const DEFAULT_ADDR: &str = "127.0.0.1:17431";
/// 前端默认监听端口：独立于 Daemon 端口，避免两者互相抢占
const DEFAULT_WEB_PORT: u16 = 5173;
const INDEX_HTML: &str = "index.html";

/// 内嵌的前端构建产物。
///
/// debug 构建下 rust-embed 直接从磁盘读取 `web/dist`，因此改完前端只要跑一次
/// `pnpm build` 即可生效，无需重新编译 Rust；release 构建则把资产写进二进制，
/// 部署时不再依赖任何外部目录，也不需要目标机器安装 node。
#[derive(Embed)]
#[folder = "$OMA_WEB_DIST"]
struct WebAssets;

#[derive(Parser)]
#[command(
    name = "oma",
    about = "Oma: Multi-client Collaborative AI Agent",
    long_about = "Oma: Multi-client Collaborative AI Agent\n\n不带子命令时等价于 `oma -h`，不会自动启动任何界面。",
    version = "0.1.0"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 独立启动后台 Daemon 服务（仅 API，界面由 `oma web` 提供）
    Daemon {
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr:   String,
        #[arg(long)]
        token:  Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 启动内嵌前端静态服务（不启动 Daemon）
    Web {
        /// 前端监听地址
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// 前端监听端口
        #[arg(long, default_value_t = DEFAULT_WEB_PORT)]
        port: u16,
        /// 就绪后自动调用浏览器打开页面（默认仅打印访问地址）
        #[arg(long)]
        open: bool,
    },
    /// 连接 Daemon 启动 TUI 终端客户端
    Tui {
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr:      String,
        #[arg(long)]
        token:     Option<String>,
        /// 目标工作区；缺省为当前目录
        #[arg(long)]
        workspace: Option<String>,
    },
    /// 查看 Daemon 服务端运行状态
    Status {
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr:  String,
        #[arg(long)]
        token: Option<String>,
    },
}

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
        .unwrap_or_else(|| get_data_dir().join("config.toml"))
}

/// 加载配置；文件存在但解析失败时报错退出。
///
/// 静默回退到默认配置会丢掉全部 provider 与模型设置，比直接启动失败更难排查。
fn load_config(path: &Path) -> Result<OmaConfig> {
    if path.exists() {
        OmaConfig::load_from_file(path).with_context(|| format!("Invalid config file {}", path.display()))
    } else {
        Ok(OmaConfig::default())
    }
}

async fn start_daemon(addr: &str, token_opt: Option<&str>, config_opt: Option<&Path>) -> Result<()> {
    let data_dir = get_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let token = resolve_or_create_token(token_opt, &data_dir)?;
    let config_path = config_path(config_opt);
    let config = load_config(&config_path)?;

    let storage = StorageManager::new(&data_dir).await?;
    let mcp = Arc::new(McpManager::new());
    mcp.sync_servers(&config.mcp_servers);
    // 后台预热 MCP 工具发现：不阻塞监听端口，也不拖慢首次打开会话
    {
        let mcp = mcp.clone();
        tokio::spawn(async move { mcp.warm_up().await });
    }

    let state = DaemonState::new(token.clone(), storage, config, config_path, mcp);
    let app = create_router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    println!("Oma Daemon v{} 已启动", env!("CARGO_PKG_VERSION"));
    println!("  API 地址:  http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

/// 启动内嵌前端的静态服务。
///
/// 只提供界面本身：连接哪个 Daemon、用什么 token 由用户在设置的「连接」页填写，
/// 因此这里不需要也不应该知道 Daemon 的任何信息。
async fn run_web(host: &str, port: u16, open: bool) -> Result<()> {
    if WebAssets::iter().next().is_none() {
        anyhow::bail!("前端资产缺失：请在 web/ 目录执行 `pnpm build` 后重新编译（当前二进制内没有任何文件）");
    }

    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .with_context(|| format!("Failed to bind to {host}:{port}"))?;
    // 端口传 0 时由系统分配，打印真实端口而不是用户传的 0
    let actual = listener.local_addr()?;

    let app = Router::new().fallback(web_asset_handler);
    let url = format!("http://{}", format_host(host, actual.port()));

    println!("Oma Web v{} 已启动", env!("CARGO_PKG_VERSION"));
    println!("  界面地址:  {}", url);

    if open {
        let _ = tokio::process::Command::new("xdg-open").arg(&url).spawn();
    }

    axum::serve(listener, app).await?;
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
    let data_dir = get_data_dir();
    let token = resolve_or_create_token(token_opt, &data_dir)?;

    let url = format!("http://{}/api/server/status", addr);
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .context("Failed to connect to Oma Daemon")?;

    if resp.status().is_success() {
        let json: serde_json::Value = resp.json().await?;
        println!("Oma Daemon is healthy on {}:", addr);
        println!("{}", serde_json::to_string_pretty(&json)?);
    } else {
        eprintln!("Oma Daemon returned status: {}", resp.status());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Daemon { addr, token, config }) => {
            start_daemon(&addr, token.as_deref(), config.as_deref()).await?;
        }
        Some(Commands::Web { host, port, open }) => {
            run_web(&host, port, open).await?;
        }
        Some(Commands::Tui { addr, token, workspace }) => {
            let data_dir = get_data_dir();
            let token = resolve_or_create_token(token.as_deref(), &data_dir)?;
            let workspace = match workspace {
                Some(w) => w,
                None => std::env::current_dir()?.to_string_lossy().to_string(),
            };
            oma_tui::run(&addr, &token, &workspace).await?;
        }
        Some(Commands::Status { addr, token }) => {
            run_status(&addr, token.as_deref()).await?;
        }
        None => {
            // 默认不启动任何界面，仅打印帮助，交由用户显式选择 `oma tui` / `oma web`
            Cli::command().print_help()?;
            println!();
        }
    }

    Ok(())
}
