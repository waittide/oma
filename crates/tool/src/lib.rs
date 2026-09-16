use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
use oma_contract::ToolImage;
pub use oma_contract::ToolOutput;
use serde::Deserialize;

pub mod image;

/// 最大工具输出字符数限制
pub const RESULT_MAX_CHARS: usize = 24_000;

/// 工具输出截断保护函数
pub fn truncate_output(output: &str) -> String {
    if output.chars().count() <= RESULT_MAX_CHARS {
        output.to_string()
    } else {
        let truncated: String = output.chars().take(RESULT_MAX_CHARS).collect();
        format!("{}\n... [截断以节省上下文]", truncated)
    }
}

/// 路径安全解析辅助函数（不做沙盒隔离，相对路径按工作区解析，绝对路径直通）
pub fn resolve_path(workspace: &Path, raw_path: &str) -> PathBuf {
    let p = Path::new(raw_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    }
}

/// 非图片二进制文件的 MIME 判定（仅靠扩展名与文件头特征）。
/// 用于告诉模型「这是个二进制文件」而不是直接报 UTF-8 错误。
fn binary_mime(path: &str, bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"%PDF-") {
        return Some("application/pdf");
    }
    if bytes.starts_with(b"\x1F\x8B") {
        return Some("application/gzip");
    }
    if bytes.starts_with(b"PK\x03\x04") {
        return Some("application/zip");
    }
    if bytes.starts_with(&[0x7F, b'E', b'L', b'F']) {
        return Some("application/x-elf");
    }
    if bytes.contains(&0) {
        return Some("application/octet-stream");
    }
    // 扩展名已知但没有 NUL 字节：仍按文本处理，交给 UTF-8 校验
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    match ext.as_str() {
        "woff" | "woff2" => Some("font/woff"),
        "ttf" => Some("font/ttf"),
        "zip" => Some("application/zip"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

/// 标准 base64（无外部依赖，图片内联用）
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 子 Agent 执行委托 Trait
#[async_trait::async_trait]
pub trait SubagentRunner: Send + Sync {
    async fn run_subagent(&self, agent: &str, prompt: &str) -> Result<String, String>;
}

/// 向用户提问的委托 Trait：实现方负责广播问题、等待作答并格式化结果。
#[async_trait::async_trait]
pub trait AskRunner: Send + Sync {
    /// 返回可直接回交给模型的文本；用户取消或超时时返回 Err。
    async fn ask_questions(&self, questions: Vec<oma_contract::AskQuestion>) -> Result<String, String>;
}

/// 统一 Tool Trait
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput;

    /// 带运行环境的执行入口；默认忽略环境，与 [`Tool::execute`] 一致。
    ///
    /// 只有需要感知模型能力的工具（如 `read`）才需要覆写。
    async fn execute_with(&self, workspace: &Path, input: serde_json::Value, _ctx: &dyn ToolContext) -> ToolOutput {
        self.execute(workspace, input).await
    }
}

/// 工具执行期可用的运行环境。
///
/// 工具本身不应感知会话与模型配置；确实需要感知的（当前只有 `read` 判断
/// 「要不要把图片交给模型」）通过这一层显式传入，避免把 Trait 扩散成大杂烩。
pub trait ToolContext: Send + Sync {
    /// 当前模型能否直接接收图片输入（即具备图像输入能力）
    fn supports_image_input(&self) -> bool;
}

/// 默认上下文：视为不具备图像输入能力，工具据此只回元数据文本。
pub struct NoImageInput;

impl ToolContext for NoImageInput {
    fn supports_image_input(&self) -> bool {
        false
    }
}

// ==========================================
// 1. Read Tool
// ==========================================
/// read 返回给模型的图片体积上限（base64 后）。
///
/// 超过上限的图片即使模型支持视觉也不内联：单张图就能吃掉整个窗口，
/// 降级为元数据至少让模型知道文件存在且规模多大。
pub const READ_IMAGE_MAX_BYTES: usize = 5 * 1024 * 1024;

pub struct ReadTool;

#[derive(Debug, Deserialize)]
struct ReadInput {
    path:   String,
    #[serde(default = "default_offset")]
    offset: usize,
    #[serde(default = "default_limit")]
    limit:  usize,
}

fn default_offset() -> usize {
    1
}
fn default_limit() -> usize {
    1000
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read file content with line slicing. Image files return the image itself when the active \
         model can view images, otherwise a metadata summary (dimensions, channels, alpha, MIME)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "1-based starting line number (default: 1), text files only"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 1000), text files only"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        self.execute_with(workspace, input, &NoImageInput).await
    }

    async fn execute_with(&self, workspace: &Path, input: serde_json::Value, ctx: &dyn ToolContext) -> ToolOutput {
        let input: ReadInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for read: {}", e)),
        };

        let target_path = resolve_path(workspace, &input.path);
        if !target_path.exists() {
            return ToolOutput::error(format!("File not found: {:?}", target_path));
        }

        // 先按字节读：图片是二进制，按文本读会直接失败
        let bytes = match tokio::fs::read(&target_path).await {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("Failed to read file: {}", e)),
        };

        if let Some(meta) = image::parse(&bytes) {
            return read_image(&meta, &bytes, ctx);
        }

        // 已识别的二进制格式（能看出 MIME 但解析不出元数据）：说明读的不是文本
        if let Some(mime) = binary_mime(&input.path, &bytes) {
            return ToolOutput::success(format!(
                "Binary file (not readable as text): {} bytes, MIME {}",
                bytes.len(),
                mime
            ));
        }

        let content = match String::from_utf8(bytes) {
            Ok(c) => c,
            Err(_) => {
                return ToolOutput::error(format!("File is not valid UTF-8 text: {:?}", target_path));
            }
        };

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let start_idx = input.offset.saturating_sub(1);
        if start_idx >= total_lines && total_lines > 0 {
            return ToolOutput::error(format!(
                "Offset {} is out of range. File has {} lines.",
                input.offset, total_lines
            ));
        }

        let slice = lines
            .into_iter()
            .skip(start_idx)
            .take(input.limit)
            .enumerate()
            .map(|(i, line)| format!("{:4} | {}", start_idx + i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");

        ToolOutput::success(truncate_output(&slice))
    }
}

/// 按「模型能不能看图」选择图片回给模型，或降级成元数据块。
///
/// 元数据块保证模型总能知道文件是什么、多大、什么色彩模式，即使它看不见图。
fn read_image(meta: &image::ImageMeta, bytes: &[u8], ctx: &dyn ToolContext) -> ToolOutput {
    let meta_block = format!(
        "mime: {}\ndimensions: {}x{}\nchannels: {}\nalpha: {}\ncolor_mode: {}\nsize_bytes: {}",
        meta.mime_type,
        meta.width,
        meta.height,
        meta.channels,
        if meta.has_alpha { "yes" } else { "no" },
        meta.color_mode(),
        bytes.len(),
    );

    if !ctx.supports_image_input() {
        return ToolOutput::success(format!(
            "Image metadata (the active model cannot view images):\n{meta_block}"
        ));
    }
    if bytes.len() > READ_IMAGE_MAX_BYTES {
        return ToolOutput::success(format!(
            "Image metadata (too large to attach; over {READ_IMAGE_MAX_BYTES} bytes):\n{meta_block}"
        ));
    }

    ToolOutput::success_with_images(
        format!(
            "Image attached below. Read it from the tool result image, then answer based on what it shows.\n{meta_block}"
        ),
        vec![ToolImage {
            mime_type: meta.mime_type.clone(),
            data:      base64_encode(bytes),
        }],
    )
}

// ==========================================
// 2. Write Tool
// ==========================================
pub struct WriteTool;

#[derive(Debug, Deserialize)]
struct WriteInput {
    path:    String,
    content: String,
}

#[async_trait::async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Overwrite or create a file with new content."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Full content to write into the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: WriteInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for write: {}", e)),
        };

        let target_path = resolve_path(workspace, &input.path);
        if let Some(parent) = target_path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return ToolOutput::error(format!("Failed to create parent directories: {}", e));
            }
        }

        if let Err(e) = tokio::fs::write(&target_path, &input.content).await {
            return ToolOutput::error(format!("Failed to write file: {}", e));
        }

        let bytes_len = input.content.len();
        let lines_count = input.content.lines().count();
        ToolOutput::success(format!(
            "Successfully wrote {} bytes ({} lines) to {:?}",
            bytes_len, lines_count, target_path
        ))
    }
}

// ==========================================
// 3. Edit Tool (Multi-Hunk Atomic Replacement + Diff)
// ==========================================
pub struct EditTool;

#[derive(Debug, Clone, Deserialize)]
pub struct EditHunk {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Debug, Deserialize)]
struct EditInput {
    path:  String,
    edits: Vec<EditHunk>,
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Perform atomic multi-hunk replacement on a file with overlap checking and unified diff."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "edits": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": { "type": "string", "description": "Exact text to replace" },
                            "new_text": { "type": "string", "description": "New replacement text" }
                        },
                        "required": ["old_text", "new_text"]
                    },
                    "description": "List of non-overlapping edits to apply"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: EditInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for edit: {}", e)),
        };

        if input.edits.is_empty() {
            return ToolOutput::error("No edits provided.");
        }

        let target_path = resolve_path(workspace, &input.path);
        if !target_path.exists() {
            return ToolOutput::error(format!("File not found: {:?}", target_path));
        }

        let original_content = match tokio::fs::read_to_string(&target_path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to read file for editing: {}", e)),
        };

        // 1. 原始基准定位与唯一性约束
        struct MatchSpan {
            start:    usize,
            end:      usize,
            new_text: String,
        }

        let mut spans = Vec::with_capacity(input.edits.len());
        for (i, hunk) in input.edits.iter().enumerate() {
            let matches: Vec<(usize, &str)> = original_content.match_indices(&hunk.old_text).collect();
            if matches.is_empty() {
                return ToolOutput::error(format!(
                    "Edit #{}: old_text not found in file: {:?}",
                    i + 1,
                    hunk.old_text
                ));
            }
            if matches.len() > 1 {
                return ToolOutput::error(format!(
                    "Edit #{}: old_text matched {} times in file (must be unique). Please include more context around the change.",
                    i + 1,
                    matches.len()
                ));
            }
            let start = matches[0].0;
            let end = start + hunk.old_text.len();
            spans.push(MatchSpan {
                start,
                end,
                new_text: hunk.new_text.clone(),
            });
        }

        // 2. 按起始位置升序排序并执行重叠区间检测 (Overlap Detection)
        spans.sort_by_key(|s| s.start);
        for i in 0..spans.len().saturating_sub(1) {
            if spans[i].end > spans[i + 1].start {
                return ToolOutput::error(format!(
                    "Edit ranges overlap between hunk starting at byte {} and hunk starting at byte {}. Please merge into a single edit.",
                    spans[i].start,
                    spans[i + 1].start
                ));
            }
        }

        // 3. 全量原子替换拼装新文件
        let mut new_content = String::with_capacity(original_content.len());
        let mut last_idx = 0;
        for span in spans {
            new_content.push_str(&original_content[last_idx..span.start]);
            new_content.push_str(&span.new_text);
            last_idx = span.end;
        }
        new_content.push_str(&original_content[last_idx..]);

        // 4. 写回目标文件
        if let Err(e) = tokio::fs::write(&target_path, &new_content).await {
            return ToolOutput::error(format!("Failed to write edited file: {}", e));
        }

        // 5. 产出 Unified Diff
        let diff = similar::TextDiff::from_lines(&original_content, &new_content);
        let unified_diff = diff
            .unified_diff()
            .header("original", "modified")
            .to_string();

        ToolOutput::success(truncate_output(&format!(
            "Successfully applied {} edit(s) to {:?}.\nUnified Diff:\n{}",
            input.edits.len(),
            target_path,
            unified_diff
        )))
    }
}

// ==========================================
// 4. Shell Tool (libc::killpg Process Group & Timeout)
// ==========================================

/// 用户登录 shell 的环境快照。
///
/// 会话内执行的命令理应「跟用户交互终端一样」，但这两点都不是默认继承
/// （`Command` 一路继承父进程环境）能拿到的：
///   - 继承的只是**守护进程**的环境，而守护进程常由服务管理器拉起，只有最小环境；
///   - rc 文件（`.zshrc` / `.bashrc`）里的 `export` 只在登录/交互 shell 中才会执行。
///
/// 因此守护进程启动时（[`init_shell_env`]）用 `$SHELL` 跑一次登录 shell，索取它
/// 最终的完整环境并缓存。rc 里的 `export` 与 `PATH` 由此对之后每个 `shell` 命令生效，
/// 又不必在每条命令上重跑登录 shell（那会把 banner、提示符、rc 报错混进命令输出）。
/// 快照在进程生命周期内固定。
struct ShellEnv {
    /// 命令解释器（`$SHELL`，兜底 `/bin/sh`）
    program: PathBuf,
    /// 登录 shell 报告的环境变量
    vars:    Vec<(OsString, OsString)>,
}

/// 进程级快照槽位。`None` 表示采集失败（或从未采集），
/// 此时 `shell` 工具退回直接继承守护进程环境，不会因快照缺失而不可用。
static SHELL_ENV: std::sync::OnceLock<Option<ShellEnv>> = std::sync::OnceLock::new();

/// 采集用户登录 shell 环境，幂等；应在守护进程开始服务前调用一次。
///
/// 只认第一次调用：后续会话（乃至后续重建的房间）共用同一份进程级快照。
pub fn init_shell_env() {
    let _ = SHELL_ENV.set(capture_login_shell_env());
}

/// 命令解释器：取 `$SHELL`，为空或不是文件时兜底 `/bin/sh`。
/// 未加 `-l` / `-i` 不适用于此处的执行路径（环境已由快照提供）。
fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// 跑一次 `$SHELL -lic 'command env'` 并解析输出。
///
/// `-i` 是必需的：zsh/bash 只在交互模式下读 `.zshrc` / `.bashrc`，而用户的
/// `export` 绝大多数就写在那里，只加 `-l` 会整片漏掉。
/// 用 `command env` 而非裸 `env`，避免 rc 里的同名 alias/函数把 `env` 顶掉。
/// 输出中的 banner（`.zshrc` 里的 `echo` 之类）不含 `=`，解析时会被自然丢弃。
fn capture_login_shell_env() -> Option<ShellEnv> {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    let program = login_shell();
    let mut child = std::process::Command::new(&program)
        .arg("-lic")
        .arg("command env")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    // 先起读取线程：环境变量总量可能超过管道缓冲区，等进程退出后再读会死锁
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });

    // 轮询等待而非阻塞 wait：用户 rc 若卡在等输入或外网请求上，
    // 不能让守护进程启动无限期挂住，超时即放弃快照（退回继承）。
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    let buf = reader.join().ok()?;
    let vars = parse_env(&buf);
    if vars.is_empty() {
        return None;
    }
    Some(ShellEnv { program, vars })
}

/// 解析 `KEY=VALUE` 行：变量名必须是合法的 shell 标识符，
/// 其余行（banner、提示符残留、空行）一律丢弃。
fn parse_env(buf: &[u8]) -> Vec<(OsString, OsString)> {
    String::from_utf8_lossy(buf)
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let mut name = key.bytes();
            let first = name.next()?;
            if !(first.is_ascii_alphabetic() || first == b'_') {
                return None;
            }
            if !name.all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return None;
            }
            Some((OsString::from(key), OsString::from(value)))
        })
        .collect()
}

pub struct ShellTool {
    timeout_secs: u64,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self { timeout_secs: 60 }
    }
}

impl ShellTool {
    pub fn new(timeout_secs: u64) -> Self {
        Self { timeout_secs }
    }
}

#[derive(Debug, Deserialize)]
struct ShellInput {
    command: String,
}

/// 进程组守卫：无论正常返回、超时还是调用方取消（future 被 drop），
/// 都会向整个进程组发送 SIGKILL。仅设置 `kill_on_drop` 只能杀掉直接子进程，
/// 命令派生的孙进程会残留。
struct ProcessGroupGuard {
    #[cfg(unix)]
    pid: Option<i32>,
}

impl ProcessGroupGuard {
    fn new(pid: Option<u32>) -> Self {
        #[cfg(unix)]
        {
            Self {
                pid: pid.map(|p| p as i32),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            Self {}
        }
    }

    /// 进程已正常回收，解除守卫
    fn disarm(&mut self) {
        #[cfg(unix)]
        {
            self.pid = None;
        }
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            #[allow(unsafe_code)]
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
}

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Execute a shell command with process group management and timeout."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: ShellInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for shell: {}", e)),
        };

        // 解释器与环境取自登录 shell 快照（见 `init_shell_env`）：
        // 有快照就以它为准（rc 里的 export / PATH 一并生效，被 rc unset 的变量
        // 也不再漏下来）；采集失败则退回既有行为——继承守护进程环境。
        let snapshot = SHELL_ENV.get().and_then(|s| s.as_ref());
        let program = snapshot
            .map(|s| s.program.clone())
            .unwrap_or_else(login_shell);

        let mut cmd = tokio::process::Command::new(program);
        cmd.arg("-c").arg(&input.command);
        // 工作目录就是当前会话的工作区：模型不用先 cd，相对路径也直接落在工作区
        cmd.current_dir(workspace);
        if let Some(snapshot) = snapshot {
            cmd.env_clear();
            cmd.envs(snapshot.vars.iter().cloned());
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // 直接子进程随 future 释放被杀，配合进程组守卫覆盖孙进程
        cmd.kill_on_drop(true);

        // 进程组隔离 (Unix 下设置独立 process group)
        #[cfg(unix)]
        #[allow(unsafe_code)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to spawn shell command: {}", e)),
        };

        // 守卫覆盖超时与取消两条路径；正常回收后解除
        let mut guard = ProcessGroupGuard::new(child.id());

        let timeout_duration = std::time::Duration::from_secs(self.timeout_secs);
        let run_result = tokio::time::timeout(timeout_duration, child.wait_with_output()).await;

        match run_result {
            Ok(Ok(output)) => {
                guard.disarm();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = format!("{}{}", stdout, stderr);
                let truncated = truncate_output(&combined);
                if output.status.success() {
                    ToolOutput::success(truncated)
                } else {
                    ToolOutput::error(format!(
                        "Command exited with code {:?}:\n{}",
                        output.status.code(),
                        truncated
                    ))
                }
            }
            Ok(Err(e)) => {
                guard.disarm();
                ToolOutput::error(format!("Failed to wait for process: {}", e))
            }
            Err(_) => ToolOutput::error(format!(
                "Command timed out after {} seconds and was killed.",
                self.timeout_secs
            )),
        }
    }
}

// ==========================================
// 5. Task Tool (Subagent Delegation)
// ==========================================

/// 延迟绑定槽：SessionRoom 需要持有工具注册表，而 subagent runner 又需要持有 SessionRoom，
/// 构成构造期环路；用一次性槽位在房间建成后回填 runner 解环。
#[derive(Default)]
pub struct RunnerSlot {
    subagent: std::sync::OnceLock<Arc<dyn SubagentRunner>>,
    ask:      std::sync::OnceLock<Arc<dyn AskRunner>>,
}

impl RunnerSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, runner: Arc<dyn SubagentRunner>) -> Result<()> {
        self.subagent
            .set(runner)
            .map_err(|_| anyhow::anyhow!("Subagent runner is already bound"))
    }

    pub fn get(&self) -> Option<&Arc<dyn SubagentRunner>> {
        self.subagent.get()
    }

    /// 回填提问 runner（与 subagent runner 同为一次性槽位，避免环状引用）
    pub fn set_ask(&self, runner: Arc<dyn AskRunner>) -> Result<()> {
        self.ask
            .set(runner)
            .map_err(|_| anyhow::anyhow!("Ask runner is already bound"))
    }

    pub fn get_ask(&self) -> Option<&Arc<dyn AskRunner>> {
        self.ask.get()
    }
}

// ==========================================
// 6. Ask Tool（向用户提问）
// ==========================================

pub struct AskTool {
    runner: Arc<RunnerSlot>,
}

impl AskTool {
    pub fn new(runner: Arc<RunnerSlot>) -> Self {
        Self { runner }
    }
}

#[derive(Debug, Deserialize)]
struct AskInput {
    questions: Vec<oma_contract::AskQuestion>,
}

#[async_trait::async_trait]
impl Tool for AskTool {
    fn name(&self) -> &'static str {
        "ask"
    }

    fn description(&self) -> &'static str {
        "Ask the user to choose among options when the right course of action is ambiguous. \
         Use `is_multi: true` to allow multiple selections; the UI always adds an \"Other\" free-form \
         entry, so never add one yourself. Prefer resolving ambiguity from the repository first."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to ask; group related ones together",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Stable identifier used in the answer" },
                            "question": { "type": "string", "description": "Question text" },
                            "options": {
                                "type": "array",
                                "minItems": 1,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "Short option label" },
                                        "description": { "type": "string", "description": "Optional tradeoffs/explanations" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "is_multi": { "type": "boolean", "description": "Allow multiple selections" },
                            "recommended": { "type": "integer", "description": "Zero-based recommended option index" }
                        },
                        "required": ["id", "question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, _workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: AskInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for ask: {}", e)),
        };
        if input.questions.is_empty() {
            return ToolOutput::error("questions must not be empty");
        }
        let Some(runner) = self.runner.get_ask() else {
            return ToolOutput::error("Ask runner is not configured in this runtime.");
        };

        match runner.ask_questions(input.questions).await {
            Ok(text) => ToolOutput::success(truncate_output(&text)),
            Err(e) => ToolOutput::error(format!("Ask failed: {}", e)),
        }
    }
}

pub struct TaskTool {
    runner: Arc<RunnerSlot>,
}

impl TaskTool {
    pub fn new(runner: Arc<RunnerSlot>) -> Self {
        Self { runner }
    }
}

#[derive(Debug, Deserialize)]
struct TaskInput {
    agent:  String,
    prompt: String,
}

#[async_trait::async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &'static str {
        "Delegate a sub-task to a specialized subagent (e.g. explore, review, plan)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Name of the target subagent (e.g. explore, review)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Task instruction for the subagent"
                }
            },
            "required": ["agent", "prompt"]
        })
    }

    async fn execute(&self, _workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: TaskInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for task: {}", e)),
        };

        let Some(runner) = self.runner.get() else {
            return ToolOutput::error("Subagent runner is not configured in this runtime.");
        };

        match runner.run_subagent(&input.agent, &input.prompt).await {
            Ok(result) => ToolOutput::success(truncate_output(&result)),
            Err(e) => ToolOutput::error(format!("Subagent failed: {}", e)),
        }
    }
}

// ==========================================
// 6. Tool Registry
// ==========================================
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// 内置工具集（不含 MCP）。
    ///
    /// 「有哪些内置工具」只在此处定义，房间装配与工具清单接口共用，
    /// 避免两处各列一份而悄悄漂移。`runner_slot` 由调用方注入：
    /// 房间需要自己的槽位以便回填 subagent runner。
    pub fn with_builtins(runner_slot: Arc<RunnerSlot>) -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(ReadTool));
        reg.register(Arc::new(WriteTool));
        reg.register(Arc::new(EditTool));
        reg.register(Arc::new(ShellTool::default()));
        reg.register(Arc::new(TaskTool::new(runner_slot.clone())));
        reg.register(Arc::new(AskTool::new(runner_slot)));
        reg
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.push(tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.iter().find(|t| t.name() == name).cloned()
    }

    pub fn list(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }

    /// 导出为大模型工具调用定义 (OpenAI / Anthropic 兼容)
    ///
    /// `allowed` 为空表示不限制；否则仅导出白名单内的工具（Agent 模板的 tools 声明）。
    pub fn to_definitions(&self, allowed: &[String]) -> Vec<serde_json::Value> {
        self.tools
            .iter()
            .filter(|t| allowed.is_empty() || allowed.iter().any(|a| a == t.name()))
            .map(|t| {
                serde_json::json!({
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters_schema()
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 可切换图像输入能力的测试上下文
    struct Ctx(bool);
    impl ToolContext for Ctx {
        fn supports_image_input(&self) -> bool {
            self.0
        }
    }

    /// 最小 PNG：签名 + IHDR（宽 2、高 1、色彩类型 2 = RGB）
    fn png_fixture() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&2u32.to_be_bytes());
        v.extend_from_slice(&1u32.to_be_bytes());
        v.extend_from_slice(&[8, 2, 0, 0, 0]);
        v
    }

    #[tokio::test]
    async fn test_read_and_write() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let write_tool = WriteTool;
        let out = write_tool
            .execute(
                ws,
                serde_json::json!({
                    "path": "test.txt",
                    "content": "line 1\nline 2\nline 3\n"
                }),
            )
            .await;
        assert!(!out.is_error, "{}", out.output);

        let read_tool = ReadTool;
        let out_read = read_tool
            .execute(
                ws,
                serde_json::json!({
                    "path": "test.txt",
                    "offset": 2,
                    "limit": 2
                }),
            )
            .await;
        assert!(!out_read.is_error);
        assert!(out_read.output.contains("line 2"));
        assert!(out_read.output.contains("line 3"));
        assert!(!out_read.output.contains("line 1"));

        Ok(())
    }

    /// 视觉模型：read 顺手把图片带回去，元数据仍随文本一起给出。
    #[tokio::test]
    async fn test_read_image_for_vision_model() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        tokio::fs::write(tmp.path().join("shot.png"), png_fixture()).await?;

        let out = ReadTool
            .execute_with(tmp.path(), serde_json::json!({ "path": "shot.png" }), &Ctx(true))
            .await;

        assert!(!out.is_error, "{}", out.output);
        assert_eq!(out.images.len(), 1, "vision model must receive the image");
        assert_eq!(out.images[0].mime_type, "image/png");
        assert!(!out.images[0].data.is_empty());
        // 元数据与图片同在，模型既能看图也能引用尺寸
        assert!(out.output.contains("dimensions: 2x1"), "{}", out.output);
        assert!(out.output.contains("mime: image/png"), "{}", out.output);
        assert!(out.output.contains("alpha: no"), "{}", out.output);
        Ok(())
    }

    /// 非视觉模型：只给元数据块，不携带任何图片数据。
    #[tokio::test]
    async fn test_read_image_metadata_only_without_vision() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let bytes = png_fixture();
        tokio::fs::write(tmp.path().join("shot.png"), &bytes).await?;

        let out = ReadTool
            .execute_with(tmp.path(), serde_json::json!({ "path": "shot.png" }), &Ctx(false))
            .await;

        assert!(!out.is_error, "{}", out.output);
        assert!(out.images.is_empty(), "non-vision model must not get image bytes");
        assert!(out.output.contains("cannot view images"), "{}", out.output);
        for field in [
            "dimensions: 2x1",
            "channels: 3",
            "alpha: no",
            "color_mode: RGB",
            "mime: image/png",
        ] {
            assert!(out.output.contains(field), "missing {field} in: {}", out.output);
        }
        assert!(
            out.output.contains(&format!("size_bytes: {}", bytes.len())),
            "{}",
            out.output
        );
        // 不能把图片内容当作文本回传
        assert!(!out.output.contains("base64"), "{}", out.output);
        Ok(())
    }

    /// 裸 `execute` 不得透出图片：默认上下文视为看不见图（TUI 等回退路径）。
    #[tokio::test]
    async fn test_read_image_plain_execute_stays_text_only() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        tokio::fs::write(tmp.path().join("shot.png"), png_fixture()).await?;

        let out = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "shot.png" }))
            .await;
        assert!(!out.is_error);
        assert!(out.images.is_empty());
        assert!(out.output.contains("dimensions: 2x1"));
        Ok(())
    }

    /// 二进制非图片文件给出 MIME 与体积，而不是 UTF-8 报错。
    #[tokio::test]
    async fn test_read_binary_file_reports_mime() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        tokio::fs::write(tmp.path().join("doc.pdf"), b"%PDF-1.7\x00\x01binary").await?;
        tokio::fs::write(tmp.path().join("blob.bin"), [0u8, 159, 146, 150]).await?;

        let pdf = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "doc.pdf" }))
            .await;
        assert!(!pdf.is_error, "{}", pdf.output);
        assert!(pdf.output.contains("application/pdf"), "{}", pdf.output);

        // 无已知文件头但含 NUL：仍应识别为二进制而不是报“非 UTF-8”错误
        let blob = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "blob.bin" }))
            .await;
        assert!(!blob.is_error, "{}", blob.output);
        assert!(blob.output.contains("Binary file"), "{}", blob.output);
        Ok(())
    }

    /// 普通文本读取行为不变（分片、行号、错误分支）。
    #[tokio::test]
    async fn test_read_text_regressions() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        tokio::fs::write(tmp.path().join("a.txt"), "one\ntwo\nthree\n").await?;

        let all = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "a.txt" }))
            .await;
        assert!(all.output.contains("   1 | one"));
        assert!(all.output.contains("   3 | three"));

        let missing = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "nope.txt" }))
            .await;
        assert!(missing.is_error);
        assert!(missing.output.contains("File not found"));

        let out_of_range = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "a.txt", "offset": 99 }))
            .await;
        assert!(out_of_range.is_error);
        assert!(out_of_range.output.contains("out of range"));
        Ok(())
    }

    #[tokio::test]
    async fn test_edit_multi_hunk_and_overlap() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let file_path = ws.join("sample.rs");
        tokio::fs::write(&file_path, "fn foo() {}\nfn bar() {}\nfn baz() {}\n").await?;

        let edit_tool = EditTool;

        // 1. 唯一性失败测试
        let out_dup = edit_tool
            .execute(
                ws,
                serde_json::json!({
                    "path": "sample.rs",
                    "edits": [
                        { "old_text": "fn", "new_text": "pub fn" }
                    ]
                }),
            )
            .await;
        assert!(out_dup.is_error);
        assert!(out_dup.output.contains("matched 3 times"));

        // 2. 重叠区间检测测试
        let out_overlap = edit_tool
            .execute(
                ws,
                serde_json::json!({
                    "path": "sample.rs",
                    "edits": [
                        { "old_text": "fn foo() {}", "new_text": "fn f() {}" },
                        { "old_text": "foo()", "new_text": "f()" }
                    ]
                }),
            )
            .await;
        assert!(out_overlap.is_error);
        assert!(out_overlap.output.contains("overlap"));

        // 3. 正常双 Hunk 原子替换与 Diff
        let out_ok = edit_tool
            .execute(
                ws,
                serde_json::json!({
                    "path": "sample.rs",
                    "edits": [
                        { "old_text": "fn foo() {}", "new_text": "pub fn foo() { 1; }" },
                        { "old_text": "fn baz() {}", "new_text": "pub fn baz() { 3; }" }
                    ]
                }),
            )
            .await;
        assert!(!out_ok.is_error, "{}", out_ok.output);
        assert!(out_ok.output.contains("Unified Diff:"));

        let final_content = tokio::fs::read_to_string(&file_path).await?;
        assert_eq!(final_content, "pub fn foo() { 1; }\nfn bar() {}\npub fn baz() { 3; }\n");

        Ok(())
    }

    /// 超时必须杀掉整个进程组：`sh -c "sleep & wait"` 会派生孙进程，
    /// 仅杀直接子进程会让后台命令变成孤儿继续运行。
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_timeout_kills_process_group() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let marker = tmp.path().join("leaked.txt");
        // 子进程在超时之后才写入标记文件：若进程组未被杀死，文件就会出现
        let command = format!("(sleep 2; echo leaked > {}) & sleep 30", marker.display());

        let shell = ShellTool::new(1);
        let out = shell
            .execute(tmp.path(), serde_json::json!({ "command": command }))
            .await;
        assert!(out.is_error, "timeout must be reported as an error: {}", out.output);
        assert!(out.output.contains("timed out"), "{}", out.output);

        // 等待超过后台子进程的计划写入时间，确认它已随进程组一并终止
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "background child survived the timeout: it was not killed with the process group"
        );
        Ok(())
    }

    /// 取消（future 被 drop）同样必须回收进程组。
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shell_cancel_kills_process_group() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let marker = tmp.path().join("cancelled.txt");
        let command = format!("(sleep 2; echo leaked > {}) & sleep 30", marker.display());

        let shell = ShellTool::new(60);
        let workspace = tmp.path().to_path_buf();
        let task = tokio::spawn(async move {
            shell
                .execute(&workspace, serde_json::json!({ "command": command }))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        task.abort();
        let _ = task.await;

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        assert!(
            !marker.exists(),
            "background child survived cancellation: process group was not killed on drop"
        );
        Ok(())
    }

    /// 未绑定 runner 时必须报错而不是静默返回成功，否则模型会拿到空答案。
    #[tokio::test]
    async fn test_ask_without_runner_reports_error() -> Result<()> {
        let ask = AskTool::new(Arc::new(RunnerSlot::new()));
        let out = ask
            .execute(
                Path::new("/tmp"),
                serde_json::json!({
                    "questions": [{
                        "id": "q1",
                        "question": "pick one",
                        "options": [{ "label": "a" }, { "label": "b" }]
                    }]
                }),
            )
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("Ask runner is not configured"));
        Ok(())
    }

    /// 空 questions 由 schema 层与运行时双重拦截。
    #[tokio::test]
    async fn test_ask_rejects_empty_questions() -> Result<()> {
        let ask = AskTool::new(Arc::new(RunnerSlot::new()));
        let out = ask
            .execute(Path::new("/tmp"), serde_json::json!({ "questions": [] }))
            .await;
        assert!(out.is_error);
        Ok(())
    }

    /// 参数缺失必填字段时给出可读错误，而不是 panic。
    #[tokio::test]
    async fn test_ask_rejects_missing_question_fields() -> Result<()> {
        let ask = AskTool::new(Arc::new(RunnerSlot::new()));
        let out = ask
            .execute(Path::new("/tmp"), serde_json::json!({ "questions": [{ "id": "q1" }] }))
            .await;
        assert!(out.is_error);
        assert!(out.output.contains("Invalid arguments for ask"));
        Ok(())
    }

    #[tokio::test]
    async fn test_shell_execution() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let shell = ShellTool::new(5);
        let out = shell
            .execute(
                ws,
                serde_json::json!({
                    "command": "echo 'hello shell'"
                }),
            )
            .await;
        assert!(!out.is_error);
        assert!(out.output.contains("hello shell"));

        Ok(())
    }

    /// 命令的工作目录必须落在会话工作区，而不是守护进程自己的 cwd。
    #[tokio::test]
    async fn test_shell_runs_in_workspace() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let shell = ShellTool::new(5);
        let out = shell
            .execute(ws, serde_json::json!({ "command": "pwd" }))
            .await;
        assert!(!out.is_error, "{}", out.output);
        // macOS 下 /tmp 是指向 /private/tmp 的符号链接，两边都归一后再比
        let reported = std::fs::canonicalize(out.output.trim())?;
        assert_eq!(reported, std::fs::canonicalize(ws)?);

        Ok(())
    }

    /// 环境快照解析：只收合法标识符开头的 `KEY=VALUE`，
    /// banner（rc 里的 `echo`）与非法行（空 key、数字开头、无等号）全部丢弃。
    #[test]
    fn test_parse_env_filters_noise() {
        let vars = parse_env(b"PATH=/usr/bin\n\xe6\xac\xa2\xe8\xbf\x8e\xe5\x9b\x9e\xe6\x9d\xa5\n=empty\n1NUM=x\nOK=1\nEMPTY=\nno_equals\n");
        let got: Vec<(String, String)> = vars
            .into_iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("OK".to_string(), "1".to_string()),
                ("EMPTY".to_string(), String::new()),
            ]
        );
    }
}
