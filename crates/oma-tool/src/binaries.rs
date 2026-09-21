//! 外部检索二进制的解析与安装，与 pi `utils/tools-manager.ts` 行为一致。
//!
//! `find` / `grep` 不自己实现文件遍历与匹配，而是调用 `fd` / `ripgrep`：
//! 只有这样才能得到与 pi 一致的 `.gitignore` 语义、隐藏文件语义与匹配性能。
//!
//! 解析顺序（与 pi 相同）：
//!   1. oma 自己的二进制目录 `<数据目录>/bin`（先前下载留下的）
//!   2. 系统 `PATH`（`fd` 兼容 Debian 的 `fdfind`）
//!   3. 从 GitHub Releases 下载解压到 `<数据目录>/bin`
//!
//! 设置 `OMA_OFFLINE=1` 可跳过第 3 步（与 pi 的 `PI_OFFLINE` 对应），
//! 便于离线环境与测试。

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};

use oma_config::dirs_data_dir;

/// 版本查询（只读一次重定向头）的超时
const NETWORK_TIMEOUT: Duration = Duration::from_secs(10);
/// 下载归档的超时
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// 需要外部二进制的工具
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExternalTool {
    Fd,
    Rg,
}

/// 单个工具的发布信息（对应 pi `TOOLS` 表里的一项）
struct ToolSpec {
    /// GitHub 仓库，形如 `sharkdp/fd`
    repo:         &'static str,
    /// 归档内的二进制名
    binary:       &'static str,
    /// 系统 PATH 中可能出现的命令名（首个命中即用）
    system_names: &'static [&'static str],
    /// release tag 前缀（fd 是 `v`，ripgrep 没有）
    tag_prefix:   &'static str,
}

const FD_SPEC: ToolSpec = ToolSpec {
    repo:         "sharkdp/fd",
    binary:       "fd",
    system_names: &["fd", "fdfind"],
    tag_prefix:   "v",
};

const RG_SPEC: ToolSpec = ToolSpec {
    repo:         "BurntSushi/ripgrep",
    binary:       "rg",
    system_names: &["rg"],
    tag_prefix:   "",
};

impl ExternalTool {
    fn spec(self) -> &'static ToolSpec {
        match self {
            Self::Fd => &FD_SPEC,
            Self::Rg => &RG_SPEC,
        }
    }

    /// 面向日志/报错的完整名字（与 pi 的 `ToolConfig.name` 一致）
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Fd => "fd",
            Self::Rg => "ripgrep",
        }
    }
}

/// oma 存放外部二进制的目录：`<数据目录>/bin`。
pub fn bin_dir() -> PathBuf {
    dirs_data_dir().join("bin")
}

/// 二进制名的平台后缀。
fn binary_file_name(spec: &ToolSpec) -> String {
    if cfg!(windows) {
        format!("{}.exe", spec.binary)
    } else {
        spec.binary.to_string()
    }
}

/// 在 `PATH` 中查找可用命令；`--version` 能跑起来即视为存在（与 pi 相同）。
fn command_exists(cmd: &str) -> bool {
    std::process::Command::new(cmd)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// 只做本地解析，不触发下载：先看 oma 的 bin 目录，再看系统 PATH。
pub fn installed_path(tool: ExternalTool) -> Option<String> {
    let spec = tool.spec();

    let local = bin_dir().join(binary_file_name(spec));
    if local.is_file() {
        return Some(local.to_string_lossy().into_owned());
    }

    spec.system_names
        .iter()
        .find(|name| command_exists(name))
        .map(|name| (*name).to_string())
}

/// 进程内缓存：解析结果（含下载所得的路径）。
fn cache() -> &'static tokio::sync::Mutex<HashMap<ExternalTool, String>> {
    static CACHE: OnceLock<tokio::sync::Mutex<HashMap<ExternalTool, String>>> = OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()))
}

/// 单飞锁：避免多个会话同时首次使用某个工具时重复下载。
fn install_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// 解析工具路径，必要时下载安装。
///
/// 返回可直接交给 `Command::new` 的路径或命令名。
pub async fn ensure_tool(tool: ExternalTool) -> Result<String, String> {
    if let Some(path) = cache().lock().await.get(&tool).cloned() {
        return Ok(path);
    }

    let _guard = install_lock().lock().await;
    // 等锁期间别人可能已经装好了
    if let Some(path) = cache().lock().await.get(&tool).cloned() {
        return Ok(path);
    }

    let path = resolve_or_install(tool).await?;
    cache().lock().await.insert(tool, path.clone());
    Ok(path)
}

async fn resolve_or_install(tool: ExternalTool) -> Result<String, String> {
    if let Some(path) = installed_path(tool) {
        return Ok(path);
    }

    if offline_mode() {
        return Err(format!(
            "{} not found. Offline mode enabled, skipping download.",
            tool.display_name()
        ));
    }

    // Android/Termux 上 Linux 二进制跑不起来（Bionic libc 不兼容），只能由用户装。
    #[cfg(target_os = "android")]
    {
        let pkg = match tool {
            ExternalTool::Fd => "fd",
            ExternalTool::Rg => "ripgrep",
        };
        return Err(format!(
            "{} not found. Install with: pkg install {}",
            tool.display_name(),
            pkg
        ));
    }

    #[cfg(not(target_os = "android"))]
    {
        download_tool(tool)
            .await
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| format!("Failed to download {}: {e}", tool.display_name()))
    }
}

/// 是否启用离线模式（`OMA_OFFLINE=1|true|yes`）。
fn offline_mode() -> bool {
    parse_offline(std::env::var("OMA_OFFLINE").ok().as_deref())
}

fn parse_offline(value: Option<&str>) -> bool {
    match value {
        Some(v) => v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"),
        None => false,
    }
}

/// 归档文件名（对应 pi 的 `getAssetName`）。
fn asset_name(binary: &str, version: &str, os: &str, arch: &str) -> Option<String> {
    let arch = match arch {
        "aarch64" | "x86_64" => arch,
        _ => return None,
    };
    match os {
        "macos" => Some(format!("{binary}-v{version}-{arch}-apple-darwin.tar.gz")),
        "linux" => Some(format!("{binary}-v{version}-{arch}-unknown-linux-musl.tar.gz")),
        "windows" => Some(format!("{binary}-v{version}-{arch}-pc-windows-msvc.zip")),
        _ => None,
    }
}

/// 用一个「跟随重定向但只看头」的客户端解析最新版本号。
///
/// 不用 api.github.com：匿名 API 配额（60 次/小时/IP）在共享出口 IP 上经常已被耗尽，
/// 而 releases 页面的重定向不占用配额，且与二进制下载同源。
async fn latest_version(repo: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(NETWORK_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(format!("https://github.com/{repo}/releases/latest"))
        .header(reqwest::header::USER_AGENT, "oma-coding-agent")
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .filter(|_| response.status().is_redirection())
        .ok_or_else(|| format!("Failed to resolve latest {repo} release: HTTP {}", response.status()))?;

    if !location.contains("/releases/tag/") {
        return Err(format!(
            "Failed to resolve latest {repo} release: unexpected redirect to {location}"
        ));
    }
    location
        .rsplit('/')
        .next()
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.trim_start_matches('v').to_string())
        .ok_or_else(|| format!("Failed to resolve latest {repo} release: empty tag in {location}"))
}

async fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "oma-coding-agent")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("Download failed with HTTP {}: {url}", response.status()));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    tokio::fs::write(dest, &bytes)
        .await
        .map_err(|e| e.to_string())
}

/// 下载归档 → 解压 → 把二进制挪进 `bin_dir`。
async fn download_tool(tool: ExternalTool) -> Result<PathBuf, String> {
    let spec = tool.spec();
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);

    let version = latest_version(spec.repo).await?;
    let asset =
        asset_name(spec.binary, &version, os, arch).ok_or_else(|| format!("Unsupported platform: {os}/{arch}"))?;

    let dir = bin_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;

    let url = format!(
        "https://github.com/{}/releases/download/{}{}/{}",
        spec.repo, spec.tag_prefix, version, asset
    );
    let archive = dir.join(&asset);
    download_file(&url, &archive).await?;

    // 解压到唯一临时目录：fd 与 rg 可能并发下载，共用固定目录会互相踩。
    let extract_dir = dir.join(format!(
        "extract_tmp_{}_{}_{}",
        spec.binary,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));

    let outcome = install_from_archive(tool, &asset, &archive, &extract_dir).await;

    // 无论成败都清理下载物与临时目录
    let _ = tokio::fs::remove_file(&archive).await;
    let _ = tokio::fs::remove_dir_all(&extract_dir).await;

    outcome
}

async fn install_from_archive(
    tool: ExternalTool,
    asset: &str,
    archive: &Path,
    extract_dir: &Path,
) -> Result<PathBuf, String> {
    let spec = tool.spec();
    tokio::fs::create_dir_all(extract_dir)
        .await
        .map_err(|e| e.to_string())?;

    if asset.ends_with(".tar.gz") {
        run_tool(
            "tar",
            &["xzf", &archive.to_string_lossy(), "-C", &extract_dir.to_string_lossy()],
        )
        .await?;
    } else if asset.ends_with(".zip") {
        extract_zip(archive, extract_dir).await?;
    } else {
        return Err(format!("Unsupported archive format: {asset}"));
    }

    let name = binary_file_name(spec);
    let stem = asset.trim_end_matches(".tar.gz").trim_end_matches(".zip");
    let candidates = [extract_dir.join(stem).join(&name), extract_dir.join(&name)];
    let found = match candidates.iter().find(|p| p.is_file()) {
        Some(p) => p.clone(),
        // 有的归档把文件放在版本号子目录里，找不到就递归搜一次
        None => find_file_recursively(extract_dir, &name)
            .await
            .ok_or_else(|| {
                format!(
                    "Binary not found in archive: expected {name} under {}",
                    extract_dir.display()
                )
            })?,
    };

    let target = bin_dir().join(&name);
    tokio::fs::rename(&found, &target)
        .await
        .map_err(|e| format!("Failed to install {}: {e}", target.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| format!("Failed to mark {} executable: {e}", target.display()))?;
    }

    Ok(target)
}

/// zip 解压：Windows 上没有 unzip，改用系统 tar，再退回 PowerShell。
async fn extract_zip(archive: &Path, extract_dir: &Path) -> Result<(), String> {
    let archive = archive.to_string_lossy().to_string();
    let dir = extract_dir.to_string_lossy().to_string();
    let mut failures: Vec<String> = Vec::new();

    if cfg!(windows) {
        if run_tool("tar", &["xf", &archive, "-C", &dir]).await.is_ok() {
            return Ok(());
        }
        failures.push("tar failed".to_string());

        let script = "& { param($archive, $destination) $ErrorActionPreference = 'Stop'; \
                      Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force }";
        match run_tool(
            "powershell.exe",
            &[
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                script,
                &archive,
                &dir,
            ],
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                failures.push(e);
                Err(format!("Failed to extract {archive}: {}", failures.join("; ")))
            }
        }
    } else {
        if run_tool("unzip", &["-q", &archive, "-d", &dir])
            .await
            .is_ok()
        {
            return Ok(());
        }
        failures.push("unzip failed".to_string());
        match run_tool("tar", &["xf", &archive, "-C", &dir]).await {
            Ok(_) => Ok(()),
            Err(e) => {
                failures.push(e);
                Err(format!("Failed to extract {archive}: {}", failures.join("; ")))
            }
        }
    }
}

/// 跑一个解压命令，非零退出即报错（带 stderr）。
async fn run_tool(program: &str, args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("{program}: {e}"))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {:?}", output.status.code())
    };
    Err(format!("{program}: {detail}"))
}

/// 广度优先找一个文件名，用于归档把二进制埋在子目录里的情形。
async fn find_file_recursively(root: &Path, name: &str) -> Option<PathBuf> {
    let mut queue = std::collections::VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                queue.push_back(path);
            } else if entry.file_name() == name {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_match_release_layout() {
        assert_eq!(
            asset_name("fd", "10.2.0", "linux", "x86_64").as_deref(),
            Some("fd-v10.2.0-x86_64-unknown-linux-musl.tar.gz")
        );
        assert_eq!(
            asset_name("ripgrep", "14.1.1", "macos", "aarch64").as_deref(),
            Some("ripgrep-v14.1.1-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_name("rg", "14.1.1", "windows", "x86_64").as_deref(),
            Some("rg-v14.1.1-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(asset_name("fd", "1.0.0", "linux", "mips"), None);
        assert_eq!(asset_name("fd", "1.0.0", "freebsd", "x86_64"), None);
    }

    /// 离线开关只认显式的真值。
    #[test]
    fn offline_flag_parsing() {
        for value in ["1", "true", "TRUE", "Yes"] {
            assert!(parse_offline(Some(value)), "{value} should enable offline mode");
        }
        for value in ["0", "false", "", "no"] {
            assert!(!parse_offline(Some(value)), "{value} should keep downloads enabled");
        }
        assert!(!parse_offline(None));
    }

    #[test]
    fn bin_dir_sits_under_data_dir() {
        assert!(bin_dir().ends_with("oma/bin"));
    }

    /// 真实下载链路（需联网）。手动跑：
    /// `cargo test -p oma-tool --lib -- --ignored fetch_missing_tool`
    #[tokio::test]
    #[ignore = "需要联网，手动触发"]
    async fn fetch_missing_tool_end_to_end() {
        let path = ensure_tool(ExternalTool::Fd)
            .await
            .expect("fd should resolve or download");
        let version = tokio::process::Command::new(&path)
            .arg("--version")
            .output()
            .await
            .expect("the resolved binary should run");
        assert!(version.status.success());
        assert!(String::from_utf8_lossy(&version.stdout).contains("fd"));
    }
}
