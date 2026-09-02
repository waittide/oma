use std::path::{Path, PathBuf};
use std::sync::Arc;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use oma_config::OmaConfig;
use oma_daemon::{create_router, resolve_or_create_token, DaemonState};
use oma_mcp::McpManager;
use oma_storage::StorageManager;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser)]
#[command(name = "oma", about = "Oma: Multi-client Collaborative AI Agent", version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 独立启动后台 Daemon 服务
    Daemon {
        #[arg(long, default_value = "127.0.0.1:17431")]
        addr: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 启动 Daemon 并打开 Web 交互界面
    Web {
        #[arg(long, default_value = "127.0.0.1:17431")]
        addr: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        dev: bool,
        #[arg(long, default_value = "5173")]
        port: u16,
    },
    /// 连接 Daemon 启动 TUI 终端客户端
    Tui {
        #[arg(long, default_value = "127.0.0.1:17431")]
        addr: String,
        #[arg(long)]
        token: Option<String>,
    },
    /// 查看 Daemon 服务端运行状态
    Status {
        #[arg(long, default_value = "127.0.0.1:17431")]
        addr: String,
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

async fn start_daemon(
    addr: &str,
    token_opt: Option<&str>,
    config_opt: Option<&Path>,
) -> Result<()> {
    let data_dir = get_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    let token = resolve_or_create_token(token_opt, &data_dir)?;

    let config = if let Some(cp) = config_opt {
        OmaConfig::load_from_file(cp).unwrap_or_else(|_| OmaConfig::load_or_default())
    } else {
        OmaConfig::load_or_default()
    };

    let storage = StorageManager::new(&data_dir).await?;
    let mcp = Arc::new(McpManager::new());
    for (name, cfg) in &config.mcp_servers {
        mcp.register_server(name, cfg.clone());
    }

    let state = DaemonState::new(token.clone(), storage, config, mcp);
    let app = create_router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to {}", addr))?;

    println!("┌────────────────────────────────────────────────────────────┐");
    println!("│ Oma Core Daemon v0.1.0                                     │");
    println!("│ Listening:   http://{:<38} │", addr);
    println!("│ Auth Token:  {:<45} │", token);
    println!("└────────────────────────────────────────────────────────────┘");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn run_web(
    addr: &str,
    token_opt: Option<&str>,
    dev: bool,
    port: u16,
) -> Result<()> {
    let data_dir = get_data_dir();
    let token = resolve_or_create_token(token_opt, &data_dir)?;

    // 1. 探测 Daemon 是否已经在运行
    let test_url = format!("http://{}/api/server/status", addr);
    let client = reqwest::Client::new();
    let is_running = client
        .get(&test_url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if !is_running {
        println!("🚀 Oma Daemon 未在 {} 运行，正在本地启动后台服务...", addr);
        let addr_clone = addr.to_string();
        let token_clone = token.clone();
        tokio::spawn(async move {
            if let Err(e) = start_daemon(&addr_clone, Some(&token_clone), None).await {
                eprintln!("Daemon error: {}", e);
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    let web_url = if dev {
        format!("http://localhost:{}?token={}", port, token)
    } else {
        format!("http://localhost:{}?token={}", port, token)
    };

    println!("✨ Oma Web Client Ready: {}", web_url);
    println!("👉 请在浏览器中访问该链接以进入 Oma 协同工作台。");

    if dev {
        let web_dir = Path::new("web");
        if web_dir.exists() {
            println!("📦 正在启动 Vite 前端开发服务器 (npm run dev)...");
            let mut child = tokio::process::Command::new("npm")
                .arg("run")
                .arg("dev")
                .current_dir(web_dir)
                .spawn()?;
            let _ = child.wait().await;
        }
    } else {
        // 尝试自动拉起默认浏览器
        let _ = tokio::process::Command::new("xdg-open").arg(&web_url).spawn();
        // 保持挂起运行
        tokio::signal::ctrl_c().await?;
    }

    Ok(())
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
        Some(Commands::Web { addr, token, dev, port }) => {
            run_web(&addr, token.as_deref(), dev, port).await?;
        }
        Some(Commands::Tui { addr, token }) => {
            println!("Oma TUI mode connecting to http://{} (token: {:?})", addr, token);
            println!("💡 TUI 模块将在第一期 Web 端完成后全面打通交互。");
        }
        Some(Commands::Status { addr, token }) => {
            run_status(&addr, token.as_deref()).await?;
        }
        None => {
            // 默认敲 oma 直接打开 web
            run_web("127.0.0.1:17431", None, false, 5173).await?;
        }
    }

    Ok(())
}
