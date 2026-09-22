use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
use anyhow::Result;
use oma_contract::ToolImage;
pub use oma_contract::ToolOutput;
use serde::Deserialize;

pub mod apply_patch;
pub mod image;
pub mod ls;
pub mod truncate;

use std::path::Component;

/// 最大工具输出字符数限制。
///
/// 现在只剩 `edit` 的 diff 在用（作为超大 diff 的兜底）；
/// `read` / `shell` 与 `ls` 已统一走 [`truncate`] 的行数 + 字节双上限。
pub const RESULT_MAX_CHARS: usize = 24_000;

/// 工具输出截断保护函数（字符数口径）。
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

/// `edit`（apply_patch）专用的路径解析：只接受工作区内的相对路径。
///
/// 段落头里的路径是模型写的，绝对路径与 `..` 逃逸一律拒绝——写文件这件事
/// 不该由模型指哪打哪；`.` / 重复分隔符按字典序归一，与
/// [`resolve_path`] 对正常相对路径的结果一致。
fn resolve_patch_path(workspace: &Path, raw_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(raw_path);
    if raw_path.is_empty() {
        return Err("patch path must not be empty".to_string());
    }
    if path.is_absolute() {
        return Err(format!(
            "patch paths must be relative to the workspace, got {raw_path:?}"
        ));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "patch path {raw_path:?} escapes the workspace; patches must stay inside it"
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "patch paths must be relative to the workspace, got {raw_path:?}"
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(format!("patch path {raw_path:?} does not name a file"));
    }
    Ok(workspace.join(normalized))
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
    #[serde(default)]
    limit:  Option<usize>,
}

fn default_offset() -> usize {
    1
}

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
        "Read file content with line slicing. Text output is truncated to 2000 lines or 50KB \
         (whichever is hit first); the notice tells you the offset to continue from. Image files \
         return the image itself when the active model can view images, otherwise a metadata \
         summary (dimensions, channels, alpha, MIME)."
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
                    "description": "Maximum number of lines to read (default: 2000 lines or 50KB, whichever comes first), text files only"
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

        // 行号前缀是 oma 自有的展示形式，只在这里拼接；
        // 截断后的行数与源文件行一一对应，提示里的行号因此仍指向文件真实行号。
        let numbered = |line_no: usize, text: &str| format!("{line_no:4} | {text}");

        // 用户显式给了 limit 就先按它切片，否则交给 truncate_head 双上限决定
        let (selected, user_limited): (Vec<(usize, &str)>, Option<usize>) = match input.limit {
            Some(limit) => {
                let end = (start_idx + limit).min(total_lines);
                let selected = lines[start_idx..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| (start_idx + i + 1, *line))
                    .collect();
                let count = end - start_idx;
                (selected, Some(count))
            }
            None => (
                lines[start_idx..]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| (start_idx + i + 1, *line))
                    .collect(),
                None,
            ),
        };

        let slice = selected
            .iter()
            .map(|(no, line)| numbered(*no, line))
            .collect::<Vec<_>>()
            .join("\n");
        let truncation = truncate::truncate_head(&slice);
        let start_display = start_idx + 1;

        let output = if truncation.first_line_exceeds_limit {
            // 单行就超过字节上限：直接给出可操作的 shell 回退
            let first_line_size = lines.get(start_idx).map(|l| l.len()).unwrap_or(0);
            format!(
                "[Line {start_display} is {}, exceeds {} limit. Use shell: sed -n '{}p' {} | head -c {}]",
                truncate::format_size(first_line_size),
                truncate::format_size(truncate::DEFAULT_MAX_BYTES),
                start_display,
                input.path,
                truncate::DEFAULT_MAX_BYTES
            )
        } else if truncation.truncated {
            let end_display = start_display + truncation.output_lines - 1;
            let next_offset = end_display + 1;
            let mut text = truncation.content.clone();
            if truncation.truncated_by == Some(truncate::TruncatedBy::Lines) {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_display}-{end_display} of {total_lines}. Use offset={next_offset} to continue.]"
                ));
            } else {
                text.push_str(&format!(
                    "\n\n[Showing lines {start_display}-{end_display} of {total_lines} ({} limit). Use offset={next_offset} to continue.]",
                    truncate::format_size(truncate::DEFAULT_MAX_BYTES)
                ));
            }
            text
        } else if let Some(read_lines) = user_limited.filter(|n| start_idx + n < total_lines) {
            let remaining = total_lines - (start_idx + read_lines);
            let next_offset = start_idx + read_lines + 1;
            format!(
                "{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]",
                truncation.content
            )
        } else {
            truncation.content.clone()
        };

        ToolOutput::success(output)
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
// 3. Edit Tool (apply_patch Envelope)
// ==========================================
/// `edit`：一次 `*** Begin Patch` envelope，落在工作区内的若干文件上。
///
/// 语义与文案见 [`apply_patch`] 模块；这里只负责 IO 与安全：
/// 先把 envelope 里每个文件的新内容全部算出来，再统一落盘，
/// 任何一段失败都不会留下「改了一半」的工作区。
pub struct EditTool;

#[derive(Debug, Deserialize)]
struct EditInput {
    /// `*** Begin Patch` … `*** End Patch` 全文。
    input: String,
}

/// 规划好的一次落盘动作。
struct PlannedFile {
    /// 段落头里的路径（摘要、diff 头、报错都用它）
    display:   String,
    /// 实际写入/删除的绝对路径
    absolute:  PathBuf,
    /// 改名前的内容（新建或原本不存在时为空串）
    before:    String,
    /// 改后的内容；`None` 表示删除该文件
    after:     Option<String>,
    /// 改名时要移除的源文件
    move_from: Option<PathBuf>,
    /// 摘要里用的操作标记
    op:        apply_patch::FileOp,
}

/// 按路径分组：同一路径的多个段落按出现顺序依次作用，其余保持 envelope 顺序。
fn group_entries(entries: Vec<apply_patch::PatchEntry>) -> Vec<Vec<apply_patch::PatchEntry>> {
    let mut groups: Vec<Vec<apply_patch::PatchEntry>> = Vec::new();
    for entry in entries {
        match groups.iter_mut().find(|group| group[0].path == entry.path) {
            Some(group) => group.push(entry),
            None => groups.push(vec![entry]),
        }
    }
    groups
}

/// 单文件版本的统一 diff（`a/` `b/` 头按内容存在与否落到 `/dev/null`）。
fn unified_diff(plan: &PlannedFile) -> String {
    let after = plan.after.as_deref().unwrap_or_default();
    if plan.before == after {
        return String::new();
    }
    let old_header = if plan.before.is_empty() {
        "/dev/null".to_string()
    } else {
        format!("a/{}", plan.display)
    };
    let new_header = if plan.after.is_none() {
        "/dev/null".to_string()
    } else {
        format!("b/{}", plan.display)
    };
    similar::TextDiff::from_lines(&plan.before, after)
        .unified_diff()
        .header(&old_header, &new_header)
        .to_string()
}

/// 把一组同路径段落规划成一次落盘；只读不写。
async fn plan_group(
    workspace: &Path,
    group: &[apply_patch::PatchEntry],
) -> Result<PlannedFile, apply_patch::PatchError> {
    let display = group[0].path.clone();
    let absolute = resolve_patch_path(workspace, &display).map_err(apply_patch::PatchError::new)?;

    if absolute.is_dir() {
        return Err(apply_patch::PatchError::new(format!(
            "{display} is a directory, not a file"
        )));
    }
    let mut current = if absolute.is_file() {
        match tokio::fs::read_to_string(&absolute).await {
            Ok(text) => Some(text),
            Err(error) => {
                return Err(apply_patch::PatchError::new(format!(
                    "Failed to read {display}: {error}"
                )));
            }
        }
    } else {
        None
    };
    let original = current.clone();
    let mut rename: Option<String> = None;

    for entry in group {
        match entry.op {
            apply_patch::FileOp::Add => {
                if current.is_some() {
                    return Err(apply_patch::PatchError::new(format!(
                        "Cannot create {display}: file already exists. Use *** Update File to modify \
                         it in place."
                    )));
                }
                current = Some(entry.body.clone().unwrap_or_default());
            }
            apply_patch::FileOp::Delete => {
                if current.take().is_none() {
                    return Err(apply_patch::PatchError::new(format!("File not found: {display}")));
                }
            }
            apply_patch::FileOp::Update => {
                let Some(text) = current.as_deref() else {
                    return Err(apply_patch::PatchError::new(format!("File not found: {display}")));
                };
                let body = entry.body.as_deref().unwrap_or_default();
                let hunks = apply_patch::parse_hunks(body)?;
                current = Some(apply_patch::apply_hunks(text, &display, &hunks)?);
                if let Some(destination) = &entry.rename {
                    if *destination == display {
                        return Err(apply_patch::PatchError::new("rename path is the same as source path"));
                    }
                    let destination_path =
                        resolve_patch_path(workspace, destination).map_err(apply_patch::PatchError::new)?;
                    if destination_path.exists() {
                        return Err(apply_patch::PatchError::new(format!(
                            "Cannot rename {display} to {destination}: destination already exists."
                        )));
                    }
                    rename = Some(destination.clone());
                }
            }
        }
    }

    let op = group.last().expect("patch groups are never empty").op;
    let move_from = rename.as_ref().map(|_| absolute.clone());
    let (absolute, display) = match &rename {
        Some(destination) => (
            resolve_patch_path(workspace, destination).map_err(apply_patch::PatchError::new)?,
            destination.clone(),
        ),
        None => (absolute, display),
    };
    Ok(PlannedFile {
        display,
        absolute,
        before: original.unwrap_or_default(),
        after: current,
        move_from,
        op,
    })
}

/// 落盘阶段已经写过哪些文件（写失败时用来告诉模型工作区处于什么状态）。
fn partial_write_error(display: &str, error: &std::io::Error, written: &[String]) -> String {
    if written.is_empty() {
        format!("Failed to apply patch to {display}: {error}")
    } else {
        format!(
            "Failed to apply patch to {display}: {error}\nAlready written before the failure (the \
             workspace is partially updated): {}",
            written.join(", ")
        )
    }
}

#[async_trait::async_trait]
impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Apply a `*** Begin Patch` envelope: add, delete, update (with optional `*** Move to:`) one \
         or more files atomically, then report a unified diff."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "Full patch envelope: `*** Begin Patch`, then file sections \
                                    (`*** Add File: <path>`, `*** Delete File: <path>`, \
                                    `*** Update File: <path>` with `@@` hunks), then \
                                    `*** End Patch`. Paths are relative to the workspace."
                }
            },
            "required": ["input"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: EditInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for edit: {}", e)),
        };

        let entries = match apply_patch::parse_envelope(&input.input) {
            Ok(entries) => entries,
            Err(error) => return ToolOutput::error(error.to_string()),
        };
        if entries.is_empty() {
            return ToolOutput::error("No files were modified.");
        }

        let groups = group_entries(entries);
        let multiple_files = groups.len() > 1;

        // 阶段一：全部算好，不碰磁盘（失败即整体放弃，工作区保持原样）
        let mut planned = Vec::with_capacity(groups.len());
        for group in &groups {
            match plan_group(workspace, group).await {
                Ok(plan) => planned.push(plan),
                Err(error) => {
                    return ToolOutput::error(if multiple_files {
                        format!("[{}]: {error}\n{}", group[0].path, apply_patch::ATOMICITY_NOTICE)
                    } else {
                        error.to_string()
                    });
                }
            }
        }

        // 阶段二：落盘
        let mut written: Vec<String> = Vec::new();
        for plan in &planned {
            if let Some(after) = &plan.after {
                if let Some(parent) = plan.absolute.parent() {
                    if let Err(error) = tokio::fs::create_dir_all(parent).await {
                        return ToolOutput::error(partial_write_error(&plan.display, &error, &written));
                    }
                }
                if let Err(error) = tokio::fs::write(&plan.absolute, after).await {
                    return ToolOutput::error(partial_write_error(&plan.display, &error, &written));
                }
            } else if let Err(error) = tokio::fs::remove_file(&plan.absolute).await {
                return ToolOutput::error(partial_write_error(&plan.display, &error, &written));
            }
            if let Some(source) = &plan.move_from {
                if let Err(error) = tokio::fs::remove_file(source).await {
                    return ToolOutput::error(partial_write_error(&plan.display, &error, &written));
                }
            }
            written.push(plan.display.clone());
        }

        // 阶段三：摘要 + 统一 diff
        let operations = planned
            .iter()
            .map(|plan| (plan.op, plan.display.as_str()))
            .collect::<Vec<_>>();
        let mut text = apply_patch::format_summary(&operations);
        let mut diffs = String::new();
        for plan in &planned {
            let diff = unified_diff(plan);
            if !diff.is_empty() {
                diffs.push_str(&diff);
            }
        }
        if !diffs.is_empty() {
            text.push_str("\n\nUnified Diff:\n");
            text.push_str(&diffs);
        }
        ToolOutput::success(truncate_output(&text))
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
    let mut cmd = std::process::Command::new(&program);
    cmd.arg("-lic")
        .arg("command env")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    // 这次采集必须跑在自己的会话里（setsid，无控制终端）。
    //
    // `-i` 的 zsh 会为作业控制把**终端的前台进程组**改成自己（tcsetpgrp），
    // 子进程退出后终端的 TPGID 就永久停在一个已死的进程组上，从此键盘
    // Ctrl+C / Ctrl+Z 送不到该终端里的任何进程——守护进程自己也就再也杀不掉。
    // 脱离控制终端后交互 shell 无从抢占，而 `.zshrc` / `.bashrc` 仍会照读。
    #[cfg(unix)]
    #[allow(unsafe_code)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut child = cmd.spawn().ok()?;

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

/// 命令输出格式化：保留末尾（错误与最终结果在末尾），
/// 用 2000 行 / 50KB 双上限，并在截断时给出可读的范围提示。
fn format_command_output(combined: &str) -> String {
    if combined.is_empty() {
        return "(no output)".to_string();
    }

    let truncation = truncate::truncate_tail(combined);
    if !truncation.truncated {
        return truncation.content;
    }

    let start_line = truncation.total_lines - truncation.output_lines + 1;
    let end_line = truncation.total_lines;
    let mut text = truncation.content;
    if truncation.last_line_partial {
        text.push_str(&format!(
            "\n\n[Showing last {} of line {end_line}.]",
            truncate::format_size(truncation.output_bytes)
        ));
    } else if truncation.truncated_by == Some(truncate::TruncatedBy::Lines) {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {}.]",
            truncation.total_lines
        ));
    } else {
        text.push_str(&format!(
            "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit).]",
            truncation.total_lines,
            truncate::format_size(truncate::DEFAULT_MAX_BYTES)
        ));
    }
    text
}

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Execute a shell command. It already runs in the workspace root (the working directory is \
         set for you), so do not prefix commands with `cd`. Process group management with timeout; \
         returns stdout and stderr. Output is truncated to the last 2000 lines or 50KB (whichever \
         is hit first)."
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
                let truncated = format_command_output(&combined);
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
// 5. Tool Registry
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
    /// 避免两处各列一份而悄悄漂移。
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register(Arc::new(ReadTool));
        reg.register(Arc::new(WriteTool));
        reg.register(Arc::new(EditTool));
        reg.register(Arc::new(ShellTool::default()));
        reg.register(Arc::new(crate::ls::LsTool));
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

    /// read 默认不再固定取 1000 行，而是行数（2000）+ 字节（50KB）双上限，
    /// 并给出可操作的续读提示。
    #[tokio::test]
    async fn test_read_truncation_notice_and_continue_offset() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        // 超过 2000 行，truncate_head 的行数上限先到
        let many: String = (1..=2500).map(|n| format!("line {n}\n")).collect();
        tokio::fs::write(tmp.path().join("many.txt"), many).await?;

        let out = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "many.txt" }))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("2000 | line 2000"), "{}", out.output);
        assert!(!out.output.contains("line 2001"), "只应给前 2000 行");
        assert!(
            out.output
                .contains("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"),
            "{}",
            out.output
        );

        // 按提示续读，拿到剩下的部分
        let next = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "many.txt", "offset": 2001 }))
            .await;
        assert!(next.output.contains("2001 | line 2001"), "{}", next.output);
        assert!(next.output.contains("2500 | line 2500"), "{}", next.output);

        // 显式 limit 提前停下时，提示剩下的行数
        let limited = ReadTool
            .execute(tmp.path(), serde_json::json!({ "path": "many.txt", "limit": 10 }))
            .await;
        assert!(!limited.is_error, "{}", limited.output);
        assert!(limited.output.contains("10 | line 10"), "{}", limited.output);
        assert!(
            limited
                .output
                .contains("[2490 more lines in file. Use offset=11 to continue.]"),
            "{}",
            limited.output
        );
        Ok(())
    }

    /// shell 保留输出末尾（错误与最终结果），超限时给出范围提示。
    #[tokio::test]
    async fn test_shell_keeps_tail_of_long_output() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let out = ShellTool::default()
            .execute(tmp.path(), serde_json::json!({ "command": "seq 1 3000" }))
            .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("3000"), "末尾必须保留：{}", out.output);
        assert!(!out.output.contains("\n1\n"), "开头的行应被丢弃");
        assert!(
            out.output.contains("[Showing lines 1001-3000 of 3000.]"),
            "{}",
            out.output
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // edit（apply_patch envelope）
    //
    // 用例覆盖 envelope / hunk 的各类场景，函数名即场景名。
    // ------------------------------------------------------------------

    /// 在工作区里跑一次 `edit`，返回工具输出。
    async fn run_patch(ws: &Path, patch: &str) -> crate::ToolOutput {
        EditTool
            .execute(ws, serde_json::json!({ "input": patch }))
            .await
    }

    fn write_file(path: &Path, content: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }

    fn read_file(path: &Path) -> Result<String> {
        Ok(std::fs::read_to_string(path)?)
    }

    /// 001_add_file：新建文件，并回一份 unified diff。
    #[tokio::test]
    async fn test_apply_patch_add_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Add File: bar.md\n+This is a new file\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("bar.md"))?, "This is a new file\n");
        assert!(out.output.contains("A bar.md"), "{}", out.output);
        assert!(out.output.contains("+This is a new file"), "{}", out.output);
        Ok(())
    }

    /// 003_multiple_chunks + 021_update_file_deletion_only：一个段落里多个 hunk。
    #[tokio::test]
    async fn test_apply_patch_multiple_chunks() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("multi.txt"), "line1\nline2\nline3\nline4\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: multi.txt\n@@\n-line2\n+changed2\n@@\n-line4\n\
             +changed4\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("multi.txt"))?, "line1\nchanged2\nline3\nchanged4\n");

        write_file(&ws.join("lines.txt"), "line1\nline2\nline3\n")?;
        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: lines.txt\n@@\n line1\n-line2\n line3\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("lines.txt"))?, "line1\nline3\n");
        Ok(())
    }

    /// 004_move_to_new_directory：`*** Move to:` 会建目录、搬内容、删源文件，兄弟文件不动。
    #[tokio::test]
    async fn test_apply_patch_move_to_new_directory() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("old/name.txt"), "old content\n")?;
        write_file(&ws.join("old/other.txt"), "unrelated file\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n\
             -old content\n+new content\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert!(!ws.join("old/name.txt").exists(), "源文件必须已被移除");
        assert_eq!(read_file(&ws.join("renamed/dir/name.txt"))?, "new content\n");
        assert_eq!(read_file(&ws.join("old/other.txt"))?, "unrelated file\n");
        Ok(())
    }

    /// 005_rejects_empty_patch：只有 envelope 没有段落时拒绝，不改任何文件。
    #[tokio::test]
    async fn test_apply_patch_rejects_empty_patch() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("foo.txt"), "stable\n")?;

        let out = run_patch(ws, "*** Begin Patch\n*** End Patch").await;
        assert!(out.is_error, "{}", out.output);
        assert_eq!(out.output, "No files were modified.");
        assert_eq!(read_file(&ws.join("foo.txt"))?, "stable\n");
        Ok(())
    }

    /// 006_rejects_missing_context：上下文对不上时报出期望内容，文件保持原样。
    #[tokio::test]
    async fn test_apply_patch_rejects_missing_context() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("modify.txt"), "line1\nline2\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: modify.txt\n@@\n-missing\n+changed\n*** End Patch",
        )
        .await;
        assert!(out.is_error, "{}", out.output);
        assert!(
            out.output
                .contains("Failed to find expected lines in modify.txt:\nmissing"),
            "{}",
            out.output
        );
        assert_eq!(read_file(&ws.join("modify.txt"))?, "line1\nline2\n");
        Ok(())
    }

    /// 007_rejects_missing_file_delete / 009_requires_existing_file_for_update：
    /// 目标不存在时 delete 与 update 都拒绝。
    #[tokio::test]
    async fn test_apply_patch_requires_existing_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("foo.txt"), "stable\n")?;

        let deleted = run_patch(ws, "*** Begin Patch\n*** Delete File: missing.txt\n*** End Patch").await;
        assert!(deleted.is_error, "{}", deleted.output);
        assert_eq!(deleted.output, "File not found: missing.txt");

        let updated = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch",
        )
        .await;
        assert!(updated.is_error, "{}", updated.output);
        assert_eq!(updated.output, "File not found: missing.txt");
        assert_eq!(read_file(&ws.join("foo.txt"))?, "stable\n");
        Ok(())
    }

    /// 008_rejects_empty_update_hunk：`*** Update File:` 后没有 hunk 直接拒绝。
    #[tokio::test]
    async fn test_apply_patch_rejects_empty_update_hunk() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("foo.txt"), "stable\n")?;

        let out = run_patch(ws, "*** Begin Patch\n*** Update File: foo.txt\n*** End Patch").await;
        assert!(out.is_error, "{}", out.output);
        assert!(
            out.output
                .contains("Update file hunk for path 'foo.txt' is empty"),
            "{}",
            out.output
        );
        assert_eq!(read_file(&ws.join("foo.txt"))?, "stable\n");
        Ok(())
    }

    /// 010_move_rejects_existing_destination：改名目标已存在时拒绝，两边都不动。
    #[tokio::test]
    async fn test_apply_patch_move_rejects_existing_destination() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("old/name.txt"), "from\n")?;
        write_file(&ws.join("renamed/dir/name.txt"), "existing\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/dir/name.txt\n@@\n\
             -from\n+new\n*** End Patch",
        )
        .await;
        assert!(out.is_error, "{}", out.output);
        assert!(
            out.output
                .contains("Cannot rename old/name.txt to renamed/dir/name.txt: destination already exists."),
            "{}",
            out.output
        );
        assert_eq!(read_file(&ws.join("old/name.txt"))?, "from\n");
        assert_eq!(read_file(&ws.join("renamed/dir/name.txt"))?, "existing\n");
        Ok(())
    }

    /// 011_add_rejects_existing_file：Add 不覆盖已存在的文件。
    #[tokio::test]
    async fn test_apply_patch_add_rejects_existing_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("duplicate.txt"), "old content\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Add File: duplicate.txt\n+new content\n*** End Patch",
        )
        .await;
        assert!(out.is_error, "{}", out.output);
        assert!(
            out.output.contains(
                "Cannot create duplicate.txt: file already exists. Use *** Update File to modify it \
                 in place."
            ),
            "{}",
            out.output
        );
        assert_eq!(read_file(&ws.join("duplicate.txt"))?, "old content\n");
        Ok(())
    }

    /// 012_delete_directory_fails：Delete 只针对文件，目录拒绝。
    #[tokio::test]
    async fn test_apply_patch_delete_directory_fails() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("dir/foo.txt"), "stable\n")?;

        let out = run_patch(ws, "*** Begin Patch\n*** Delete File: dir\n*** End Patch").await;
        assert!(out.is_error, "{}", out.output);
        assert!(out.output.contains("dir is a directory, not a file"), "{}", out.output);
        assert_eq!(read_file(&ws.join("dir/foo.txt"))?, "stable\n");
        Ok(())
    }

    /// 013_rejects_invalid_hunk_header：不认识的段落头直接拒绝。
    #[tokio::test]
    async fn test_apply_patch_rejects_invalid_hunk_header() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("foo.txt"), "stable\n")?;

        let out = run_patch(ws, "*** Begin Patch\n*** Frobnicate File: foo\n*** End Patch").await;
        assert!(out.is_error, "{}", out.output);
        assert!(out.output.contains("is not a valid hunk header"), "{}", out.output);
        assert_eq!(read_file(&ws.join("foo.txt"))?, "stable\n");
        Ok(())
    }

    /// 014_update_file_appends_trailing_newline：替换后仍以换行结尾。
    #[tokio::test]
    async fn test_apply_patch_appends_trailing_newline() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("no_newline.txt"), "no newline at end\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: no_newline.txt\n@@\n-no newline at end\n+first line\n\
             +second line\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("no_newline.txt"))?, "first line\nsecond line\n");
        Ok(())
    }

    /// 016_pure_addition_update_chunk：只有 `+` 行的段落追加到文件末尾。
    #[tokio::test]
    async fn test_apply_patch_pure_addition_chunk() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("input.txt"), "line1\nline2\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: input.txt\n@@\n+added line 1\n+added line 2\n\
             *** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(
            read_file(&ws.join("input.txt"))?,
            "line1\nline2\nadded line 1\nadded line 2\n"
        );
        Ok(())
    }

    /// 017/018/020：envelope 标记与段落头两侧的空白不影响解析。
    #[tokio::test]
    async fn test_apply_patch_whitespace_padded_markers() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let cases = [
            // 017_whitespace_padded_hunk_header
            "*** Begin Patch\n  *** Update File: foo.txt\n@@\n-old\n+new\n*** End Patch",
            // 018_whitespace_padded_patch_markers
            " *** Begin Patch\n*** Update File: foo.txt\n@@\n-one\n+two\n*** End Patch ",
            // 020_whitespace_padded_patch_marker_lines
            "*** Begin Patch \n*** Update File: foo.txt\n@@\n-one\n+two\n *** End Patch",
        ];
        for (index, patch) in cases.iter().enumerate() {
            let expected = if index == 0 { "new" } else { "two" };
            write_file(&ws.join("foo.txt"), "old\n")?;
            if index > 0 {
                write_file(&ws.join("foo.txt"), "one\n")?;
            }
            let out = run_patch(ws, patch).await;
            assert!(!out.is_error, "case {index}: {}", out.output);
            assert_eq!(read_file(&ws.join("foo.txt"))?, format!("{expected}\n"), "case {index}");
        }
        Ok(())
    }

    /// 019_unicode_simple：Unicode 内容按行精确匹配。
    #[tokio::test]
    async fn test_apply_patch_unicode() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("foo.txt"), "line1\nnaïve café\nline3\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: foo.txt\n@@\n line1\n-naïve café\n+naïve café ✅\n\
             *** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("foo.txt"))?, "line1\nnaïve café ✅\nline3\n");
        Ok(())
    }

    /// 020_delete_file_success：删除段落只删目标文件。
    #[tokio::test]
    async fn test_apply_patch_delete_file_success() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("keep.txt"), "keep\n")?;
        write_file(&ws.join("obsolete.txt"), "obsolete\n")?;

        let out = run_patch(ws, "*** Begin Patch\n*** Delete File: obsolete.txt\n*** End Patch").await;
        assert!(!out.is_error, "{}", out.output);
        assert!(out.output.contains("D obsolete.txt"), "{}", out.output);
        assert!(!ws.join("obsolete.txt").exists());
        assert_eq!(read_file(&ws.join("keep.txt"))?, "keep\n");
        Ok(())
    }

    /// 022_update_file_end_of_file_marker：`*** End of File` 要求匹配落在文件末尾。
    #[tokio::test]
    async fn test_apply_patch_end_of_file_marker() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("tail.txt"), "first\nsecond\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: tail.txt\n@@\n first\n-second\n+second updated\n\
             *** End of File\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("tail.txt"))?, "first\nsecond updated\n");

        // 同样两行，但文件里后面还有内容：`*** End of File` 必须拒绝
        write_file(&ws.join("tail.txt"), "first\nsecond\nthird\n")?;
        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: tail.txt\n@@\n first\n-second\n+second updated\n\
             *** End of File\n*** End Patch",
        )
        .await;
        assert!(out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("tail.txt"))?, "first\nsecond\nthird\n");
        Ok(())
    }

    /// core.json：一个 envelope 里的 update + delete + add 一起落地。
    #[tokio::test]
    async fn test_apply_patch_multiple_operations() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("a.txt"), "old\n")?;
        write_file(&ws.join("remove.txt"), "bye\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** Delete File: remove.txt\n\
             *** Add File: added.txt\n+created\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("a.txt"))?, "new\n");
        assert_eq!(read_file(&ws.join("added.txt"))?, "created\n");
        assert!(!ws.join("remove.txt").exists());
        for marker in ["M a.txt", "D remove.txt", "A added.txt"] {
            assert!(out.output.contains(marker), "missing {marker} in {}", out.output);
        }
        // 成功输出带统一 diff：新增文件从 /dev/null 起，修改文件两侧都带路径
        assert!(out.output.contains("\n\nUnified Diff:\n"), "{}", out.output);
        assert!(out.output.contains("--- a/a.txt"), "{}", out.output);
        assert!(out.output.contains("+++ b/a.txt"), "{}", out.output);
        assert!(out.output.contains("--- /dev/null"), "{}", out.output);
        assert!(out.output.contains("-old"), "{}", out.output);
        assert!(out.output.contains("+new"), "{}", out.output);
        assert!(out.output.contains("+created"), "{}", out.output);
        Ok(())
    }

    /// core.json：多文件 envelope 失败即整体放弃，已算好的文件也不落盘。
    #[tokio::test]
    async fn test_apply_patch_multi_file_failure_is_atomic() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("a.txt"), "old\n")?;
        write_file(&ws.join("b.txt"), "value\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** Update File: b.txt\n@@\n\
             -missing\n+changed\n*** End Patch",
        )
        .await;
        assert!(out.is_error, "{}", out.output);
        assert!(out.output.starts_with("[b.txt]: "), "{}", out.output);
        assert!(out.output.contains(apply_patch::ATOMICITY_NOTICE), "{}", out.output);
        assert_eq!(read_file(&ws.join("a.txt"))?, "old\n");
        assert_eq!(read_file(&ws.join("b.txt"))?, "value\n");
        Ok(())
    }

    /// delete 之后再 add 同一路径 = 原子替换（不需要两次调用）。
    #[tokio::test]
    async fn test_apply_patch_delete_then_add_replaces_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        write_file(&ws.join("a.txt"), "old\n")?;

        let out = run_patch(
            ws,
            "*** Begin Patch\n*** Delete File: a.txt\n*** Add File: a.txt\n+new\n*** End Patch",
        )
        .await;
        assert!(!out.is_error, "{}", out.output);
        assert_eq!(read_file(&ws.join("a.txt"))?, "new\n");
        Ok(())
    }

    /// 路径必须留在工作区内：绝对路径与 `..` 逃逸直接拒绝，不写盘。
    #[tokio::test]
    async fn test_apply_patch_rejects_path_escape() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        let outside = tmp.path().join("outside.txt");
        write_file(&outside, "untouched\n")?;

        let absolute = run_patch(
            ws,
            &format!(
                "*** Begin Patch\n*** Update File: {}\n@@\n-untouched\n+touched\n*** End Patch",
                outside.display()
            ),
        )
        .await;
        assert!(absolute.is_error, "{}", absolute.output);
        assert!(
            absolute.output.contains("patch paths must be relative"),
            "{}",
            absolute.output
        );

        let escaping = run_patch(
            ws,
            "*** Begin Patch\n*** Add File: ../escaped.txt\n+oops\n*** End Patch",
        )
        .await;
        assert!(escaping.is_error, "{}", escaping.output);
        assert!(escaping.output.contains("escapes the workspace"), "{}", escaping.output);
        assert_eq!(read_file(&outside)?, "untouched\n");
        assert!(!tmp.path().join("escaped.txt").exists());
        Ok(())
    }

    /// 参数只认 `input`：旧的 `{path, edits}` 形式必须被拒绝（不留双入口）。
    #[tokio::test]
    async fn test_edit_requires_apply_patch_input() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        write_file(&tmp.path().join("a.txt"), "old\n")?;

        let out = EditTool
            .execute(
                tmp.path(),
                serde_json::json!({
                    "path": "a.txt",
                    "edits": [{ "old_text": "old", "new_text": "new" }]
                }),
            )
            .await;
        assert!(out.is_error, "{}", out.output);
        assert!(out.output.contains("Invalid arguments for edit"), "{}", out.output);
        assert_eq!(read_file(&tmp.path().join("a.txt"))?, "old\n");
        Ok(())
    }

    /// envelope 头缺失/结尾缺失都拒绝，并指出缺哪一行。
    #[tokio::test]
    async fn test_apply_patch_requires_envelope() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();

        let no_begin = run_patch(ws, "*** Add File: a.txt\n+x\n").await;
        assert!(no_begin.is_error, "{}", no_begin.output);
        assert!(
            no_begin
                .output
                .contains("The first line of the patch must be '*** Begin Patch'"),
            "{}",
            no_begin.output
        );

        let no_end = run_patch(ws, "*** Begin Patch\n*** Add File: a.txt\n+x\n").await;
        assert!(no_end.is_error, "{}", no_end.output);
        assert!(
            no_end
                .output
                .contains("The last line of the patch must be '*** End Patch'"),
            "{}",
            no_end.output
        );
        assert!(!ws.join("a.txt").exists(), "解析失败不该留下文件");
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

    /// 内置工具集：edit / ls / read / shell / write。
    #[test]
    fn test_builtin_tool_set() {
        let reg = ToolRegistry::with_builtins();
        let mut names: Vec<&str> = reg.list().iter().map(|t| t.name()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["edit", "ls", "read", "shell", "write"]);

        // 下发给模型的参数契约：edit 只认 apply_patch 的 `input`，shell 仍只认 `command`
        let defs = reg.to_definitions(&[]);
        let required = |name: &str| {
            defs.iter()
                .find(|d| d["name"] == name)
                .unwrap_or_else(|| panic!("missing definition for {name}"))["parameters"]["required"]
                .clone()
        };
        assert_eq!(required("edit"), serde_json::json!(["input"]));
        assert_eq!(required("shell"), serde_json::json!(["command"]));
    }

    /// ls 不依赖外部二进制，直接验证输出形态（目录带 `/` 后缀、无体积列）。
    #[tokio::test]
    async fn test_ls_lists_entries_sorted() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join("src"))?;
        std::fs::write(ws.join("README.md"), "# title\n")?;

        let ls = crate::ls::LsTool.execute(ws, serde_json::json!({})).await;
        assert!(!ls.is_error, "{}", ls.output);
        assert!(ls.output.contains("src/"), "{}", ls.output);
        assert!(ls.output.contains("README.md"), "{}", ls.output);
        // 条目行不附体积（只给名字与 `/` 后缀）
        assert!(!ls.output.contains(" B"), "{}", ls.output);

        let empty = tempfile::tempdir()?;
        let ls = crate::ls::LsTool
            .execute(empty.path(), serde_json::json!({}))
            .await;
        assert!(ls.output.contains("(empty directory)"));
        Ok(())
    }
}
