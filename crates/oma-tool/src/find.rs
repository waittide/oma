//! find 工具：调用外部 `fd` 按 glob 查找文件。
//!
//! `--hidden`、`.gitignore` 感知、含 `/` 的 pattern 走 `--full-path` 并自动补
//! `**/` 前缀、结果相对搜索根展示。

use std::{path::Path, process::Stdio};

use oma_contract::ToolOutput;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    Tool,
    binaries::{ExternalTool, ensure_tool},
    resolve_path,
    truncate::{self, DEFAULT_MAX_BYTES},
};

/// 默认结果条数上限
const DEFAULT_LIMIT: usize = 1000;

pub struct FindTool;

#[derive(Debug, Deserialize)]
struct FindInput {
    pattern: String,
    #[serde(default)]
    path:    Option<String>,
    #[serde(default)]
    limit:   Option<usize>,
}

#[async_trait::async_trait]
impl Tool for FindTool {
    fn name(&self) -> &'static str {
        "find"
    }

    fn description(&self) -> &'static str {
        "Search for files by glob pattern. Returns matching file paths relative to the search \
         directory. Respects .gitignore. Output is truncated to 1000 results or 50KB (whichever is \
         hit first)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files, e.g. '*.rs', '**/*.json', or 'src/**/*.spec.ts'"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: workspace root)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 1000)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: FindInput = match serde_json::from_value(input) {
            Ok(value) => value,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for find: {e}")),
        };

        let search_path = resolve_path(workspace, input.path.as_deref().unwrap_or("."));
        if !search_path.exists() {
            return ToolOutput::error(format!("Path not found: {}", search_path.display()));
        }
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT);

        let fd = match ensure_tool(ExternalTool::Fd).await {
            Ok(path) => path,
            Err(e) => return ToolOutput::error(format!("fd is not available and could not be downloaded: {e}")),
        };

        let mut cmd = tokio::process::Command::new(fd);
        cmd.arg("--glob").arg("--color=never").arg("--hidden");

        // fd 在非 git 仓库里默认**不**读 .gitignore，加 --no-require-git 补上；
        // 在仓库内则保留 fd 的 git 感知行为，让父级 .gitignore 在嵌套仓库处停下。
        if !inside_git_repo(&search_path) {
            cmd.arg("--no-require-git");
        }
        cmd.arg("--max-results").arg(limit.to_string());

        // fd --glob 默认只匹配文件名；带路径的 pattern 需要 --full-path，
        // 而该模式下匹配的是完整路径，因此要补 `**/` 前缀才可能命中。
        let mut pattern = input.pattern.clone();
        if pattern.contains('/') {
            cmd.arg("--full-path");
            if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
                pattern = format!("**/{pattern}");
            }
            // fd 在 Windows 上按原生分隔符匹配完整路径
            #[cfg(windows)]
            {
                pattern = pattern.replace('/', "[/\\\\]");
            }
        }
        cmd.arg("--").arg(&pattern).arg(&search_path);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return ToolOutput::error(format!("Failed to run fd: {e}")),
        };

        let mut stderr_pipe = child.stderr.take().expect("stderr is piped");
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr_pipe, &mut buf).await;
            buf
        });

        let stdout = child.stdout.take().expect("stdout is piped");
        let mut lines = BufReader::new(stdout).lines();

        let mut results: Vec<String> = Vec::new();
        let mut read_error: Option<String> = None;
        loop {
            match lines.next_line().await {
                Ok(Some(raw)) => {
                    let line = raw.trim_end_matches('\r').trim();
                    if line.is_empty() {
                        continue;
                    }
                    results.push(relativize(line, &search_path));
                }
                Ok(None) => break,
                Err(e) => {
                    read_error = Some(e.to_string());
                    break;
                }
            }
        }

        drop(lines);
        let status = child.wait().await;
        let stderr = stderr_task.await.unwrap_or_default();

        if let Some(e) = read_error {
            return ToolOutput::error(format!("Failed to read fd output: {e}"));
        }

        // fd 非零退出但有输出（例如个别目录不可读）时仍然展示结果
        let code = status.as_ref().ok().and_then(|s| s.code());
        if !matches!(code, Some(0)) && results.is_empty() {
            let head = stderr.trim();
            let message = if head.is_empty() {
                let code = code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                format!("fd exited with code {code}")
            } else {
                head.to_string()
            };
            return ToolOutput::error(message);
        }

        if results.is_empty() {
            return ToolOutput::success("No files found matching pattern");
        }

        let limit_reached = results.len() >= limit;
        let raw_output = results.join("\n");
        // 条数已被 --max-results 限制，这里只可能撞上字节上限
        let truncation = truncate::truncate_head_with(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut output = truncation.content;

        let mut notices: Vec<String> = Vec::new();
        if limit_reached {
            notices.push(format!(
                "{limit} results limit reached. Use limit={} for more, or refine pattern",
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

/// 结果相对搜索根展示，并统一成正斜杠。
fn relativize(result: &str, search_path: &Path) -> String {
    let path = Path::new(result);
    if !path.is_absolute() {
        return result.replace('\\', "/");
    }
    match path.strip_prefix(search_path) {
        Ok(relative) => {
            let shown = relative.to_string_lossy().replace('\\', "/");
            if shown.is_empty() {
                result.replace('\\', "/")
            } else {
                shown
            }
        }
        Err(_) => result.replace('\\', "/"),
    }
}

/// 从搜索根向上找 `.git`，判断是否位于 git 仓库内。
fn inside_git_repo(search_path: &Path) -> bool {
    let mut current = search_path.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return true;
        }
        match current.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => current = parent.to_path_buf(),
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 绝对路径按搜索根相对化，相对路径原样（分隔符归一）。
    #[test]
    fn results_are_relativized_against_search_root() {
        assert_eq!(relativize("/work/src/main.rs", Path::new("/work")), "src/main.rs");
        assert_eq!(relativize("src/main.rs", Path::new("/work")), "src/main.rs");
        // 不在搜索根下的绝对路径保持原样，避免伪造出 `..` 相对路径
        assert_eq!(relativize("/other/x.rs", Path::new("/work")), "/other/x.rs");
    }

    /// 自己就是 git 仓库 / 位于仓库子目录，都算「在仓库内」。
    #[test]
    fn git_repo_detection_walks_upwards() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(tmp.path().join(".git")).unwrap();

        assert!(inside_git_repo(tmp.path()));
        assert!(inside_git_repo(&nested));

        let outside = tempfile::tempdir().unwrap();
        assert!(!inside_git_repo(outside.path()));
    }
}
