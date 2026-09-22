//! ls 工具：列目录内容。
//!
//! 按名排序、目录名带 `/` 后缀、默认 500 条上限、字节上限用统一的 50KB 截断。
//! 这里用本地文件系统调用，不依赖外部二进制。

use std::path::Path;

use oma_contract::ToolOutput;
use serde::Deserialize;

use crate::{
    Tool, resolve_path,
    truncate::{self, DEFAULT_MAX_BYTES},
};

/// 默认条目数上限
const DEFAULT_LIMIT: usize = 500;

pub struct LsTool;

#[derive(Debug, Deserialize)]
struct LsInput {
    #[serde(default)]
    path:  String,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait::async_trait]
impl Tool for LsTool {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn description(&self) -> &'static str {
        "List directory contents. Returns entries sorted alphabetically, with '/' suffix for \
         directories. Includes dotfiles. Output is truncated to 500 entries or 50KB (whichever is \
         hit first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list (default: workspace root)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (default: 500)"
                }
            }
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: LsInput = match serde_json::from_value(input) {
            Ok(value) => value,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for ls: {e}")),
        };

        let raw = if input.path.is_empty() {
            "."
        } else {
            input.path.as_str()
        };
        let dir = resolve_path(workspace, raw);
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);

        let metadata = match tokio::fs::metadata(&dir).await {
            Ok(meta) => meta,
            Err(_) => return ToolOutput::error(format!("Path not found: {}", dir.display())),
        };
        if !metadata.is_dir() {
            return ToolOutput::error(format!("Not a directory: {}", dir.display()));
        }

        let read = match tokio::fs::read_dir(&dir).await {
            Ok(read) => read,
            Err(e) => return ToolOutput::error(format!("Cannot read directory: {e}")),
        };

        let mut names: Vec<String> = Vec::new();
        let mut entries = read;
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => names.push(entry.file_name().to_string_lossy().into_owned()),
                Ok(None) => break,
                Err(e) => return ToolOutput::error(format!("Cannot read directory: {e}")),
            }
        }
        // 大小写不敏感排序
        names.sort_by_key(|name| name.to_lowercase());

        let mut results: Vec<String> = Vec::new();
        let mut limit_reached = false;
        for name in names {
            if results.len() >= limit {
                limit_reached = true;
                break;
            }
            // 拿不到类型的条目直接跳过
            let Ok(meta) = tokio::fs::symlink_metadata(dir.join(&name)).await else {
                continue;
            };
            let suffix = if meta.is_dir() { "/" } else { "" };
            results.push(format!("{name}{suffix}"));
        }

        if results.is_empty() {
            return ToolOutput::success("(empty directory)");
        }

        let raw_output = results.join("\n");
        let truncation = truncate::truncate_head_with(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut output = truncation.content;

        let mut notices: Vec<String> = Vec::new();
        if limit_reached {
            notices.push(format!(
                "{limit} entries limit reached. Use limit={} for more",
                limit * 2
            ));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", truncate::format_size(DEFAULT_MAX_BYTES)));
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }

        ToolOutput::success(output)
    }
}
