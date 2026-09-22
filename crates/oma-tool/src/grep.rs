//! grep 工具：调用外部 `ripgrep` 搜索文件内容。
//!
//! 参数映射、`--json` 流式解析、`context` 行的回读渲染、上限与截断提示都在这里
//! 处理。之所以不自己遍历文件，是因为 `.gitignore` / 隐藏文件语义只有 `rg` 能
//! 保证一致。

use std::{collections::HashMap, path::Path, process::Stdio};

use oma_contract::ToolOutput;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{
    Tool,
    binaries::{ExternalTool, ensure_tool},
    resolve_path,
    truncate::{self, DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH},
};

/// 默认匹配条数上限
const DEFAULT_LIMIT: usize = 100;

pub struct GrepTool;

#[derive(Debug, Deserialize)]
struct GrepInput {
    pattern:     String,
    #[serde(default)]
    path:        Option<String>,
    #[serde(default)]
    glob:        Option<String>,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    literal:     bool,
    #[serde(default)]
    context:     Option<usize>,
    #[serde(default)]
    limit:       Option<usize>,
}

/// rg `--json` 输出里的一条匹配
struct Match {
    path: String,
    line: usize,
    /// `context == 0` 时可直接用这一行，省掉一次回读
    text: Option<String>,
}

#[async_trait::async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents for a pattern. Returns matching lines with file paths and line numbers. \
         Respects .gitignore. Output is truncated to 100 matches or 50KB (whichever is hit first). \
         Long lines are truncated to 500 chars."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Search pattern (regex or literal string)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (default: workspace root)"
                },
                "glob": {
                    "type": "string",
                    "description": "Filter files by glob pattern, e.g. '*.rs' or '**/*.spec.ts'"
                },
                "ignore_case": {
                    "type": "boolean",
                    "description": "Case-insensitive search (default: false)"
                },
                "literal": {
                    "type": "boolean",
                    "description": "Treat pattern as literal string instead of regex (default: false)"
                },
                "context": {
                    "type": "integer",
                    "description": "Number of lines to show before and after each match (default: 0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default: 100)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: GrepInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for grep: {e}")),
        };
        if input.pattern.trim().is_empty() {
            return ToolOutput::error("pattern must not be empty");
        }

        let search_path = resolve_path(workspace, input.path.as_deref().unwrap_or("."));
        let is_directory = match tokio::fs::metadata(&search_path).await {
            Ok(meta) => meta.is_dir(),
            Err(_) => return ToolOutput::error(format!("Path not found: {}", search_path.display())),
        };

        let rg = match ensure_tool(ExternalTool::Rg).await {
            Ok(path) => path,
            Err(e) => {
                return ToolOutput::error(format!(
                    "ripgrep (rg) is not available and could not be downloaded: {e}"
                ));
            }
        };

        let context_lines = input.context.filter(|c| *c > 0).unwrap_or(0);
        let limit = input.limit.unwrap_or(DEFAULT_LIMIT).max(1);

        let mut cmd = tokio::process::Command::new(rg);
        cmd.arg("--json")
            .arg("--line-number")
            .arg("--color=never")
            .arg("--hidden");
        if input.ignore_case {
            cmd.arg("--ignore-case");
        }
        if input.literal {
            cmd.arg("--fixed-strings");
        }
        if let Some(glob) = input.glob.as_deref().filter(|g| !g.is_empty()) {
            cmd.arg("--glob").arg(glob);
        }
        cmd.arg("--").arg(&input.pattern).arg(&search_path);
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return ToolOutput::error(format!("Failed to run ripgrep: {e}")),
        };

        // stderr 必须与 stdout 并行消费，否则管道写满会让子进程卡住
        let mut stderr_pipe = child.stderr.take().expect("stderr is piped");
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr_pipe, &mut buf).await;
            buf
        });

        let stdout = child.stdout.take().expect("stdout is piped");
        let mut lines = BufReader::new(stdout).lines();

        let mut matches: Vec<Match> = Vec::new();
        let mut match_count = 0usize;
        let mut limit_reached = false;
        let mut killed_due_to_limit = false;

        loop {
            match lines.next_line().await {
                Ok(Some(raw)) => {
                    if raw.trim().is_empty() {
                        continue;
                    }
                    let Ok(event) = serde_json::from_str::<serde_json::Value>(&raw) else {
                        continue;
                    };
                    if event.get("type").and_then(|t| t.as_str()) != Some("match") {
                        continue;
                    }

                    match_count += 1;
                    let data = &event["data"];
                    let path = data["path"]["text"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let line = data["line_number"].as_u64().unwrap_or(0) as usize;
                    let text = data["lines"]["text"].as_str().map(str::to_string);
                    if !path.is_empty() && line > 0 {
                        matches.push(Match { path, line, text });
                    }

                    if match_count >= limit {
                        limit_reached = true;
                        let _ = child.start_kill();
                        killed_due_to_limit = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = child.start_kill();
                    let stderr = stderr_task.await.unwrap_or_default();
                    return ToolOutput::error(format!("Failed to read ripgrep output: {e}{}", detail(&stderr)));
                }
            }
        }

        // 关闭读端再等子进程，避免它在 EOF 前退出不了
        drop(lines);
        let status = child.wait().await;
        let stderr = stderr_task.await.unwrap_or_default();

        if !killed_due_to_limit {
            let code = status.as_ref().ok().and_then(|s| s.code());
            if !matches!(code, Some(0) | Some(1)) {
                let head = stderr.trim();
                let message = if head.is_empty() {
                    let code = code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    format!("ripgrep exited with code {code}")
                } else {
                    head.to_string()
                };
                return ToolOutput::error(message);
            }
        }

        if match_count == 0 {
            return ToolOutput::success("No matches found");
        }

        let mut output_lines: Vec<String> = Vec::new();
        let mut lines_truncated = false;
        let mut file_cache: HashMap<String, Vec<String>> = HashMap::new();

        for entry in &matches {
            let relative = format_path(&search_path, is_directory, &entry.path);
            let inline = if context_lines == 0 {
                entry.text.as_deref().map(sanitize_line)
            } else {
                None
            };

            match inline {
                Some(text) => {
                    let (text, cut) = truncate::truncate_line(&text, GREP_MAX_LINE_LENGTH);
                    lines_truncated |= cut;
                    output_lines.push(format!("{relative}:{}: {text}", entry.line));
                }
                None => {
                    let block = render_block(
                        &search_path,
                        is_directory,
                        &entry.path,
                        entry.line,
                        context_lines,
                        &mut file_cache,
                        &mut lines_truncated,
                    )
                    .await;
                    output_lines.extend(block);
                }
            }
        }

        let raw_output = output_lines.join("\n");
        // 条数已被 limit 限制，这里只可能撞上字节上限
        let truncation = truncate::truncate_head_with(&raw_output, usize::MAX, DEFAULT_MAX_BYTES);
        let mut output = truncation.content;

        let mut notices: Vec<String> = Vec::new();
        if limit_reached {
            notices.push(format!(
                "{limit} matches limit reached. Use limit={} for more, or refine pattern",
                limit * 2
            ));
        }
        if truncation.truncated {
            notices.push(format!("{} limit reached", truncate::format_size(DEFAULT_MAX_BYTES)));
        }
        if lines_truncated {
            notices.push(format!(
                "Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"
            ));
        }
        if !notices.is_empty() {
            output.push_str(&format!("\n\n[{}]", notices.join(". ")));
        }

        ToolOutput::success(output)
    }
}

/// 展示用路径：目录内搜索时相对搜索根，否则退化为文件名。
fn format_path(search_path: &Path, is_directory: bool, file: &str) -> String {
    let path = Path::new(file);
    if is_directory {
        if let Ok(relative) = path.strip_prefix(search_path) {
            let shown = relative.to_string_lossy().replace('\\', "/");
            if !shown.is_empty() && !shown.starts_with("..") {
                return shown;
            }
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string())
}

/// rg 给的行文本带换行；去掉行尾换行与 `\r`。
fn sanitize_line(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "")
        .trim_end_matches('\n')
        .to_string()
}

/// 读文件并渲染匹配行（含前后文），行号前缀格式：
/// 命中行 `path:12: text`，上下文行 `path-12- text`。
async fn render_block(
    search_path: &Path,
    is_directory: bool,
    file: &str,
    line: usize,
    context: usize,
    cache: &mut HashMap<String, Vec<String>>,
    lines_truncated: &mut bool,
) -> Vec<String> {
    let relative = format_path(search_path, is_directory, file);

    if !cache.contains_key(file) {
        let lines = match tokio::fs::read_to_string(file).await {
            Ok(content) => content
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .split('\n')
                .map(str::to_string)
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        };
        cache.insert(file.to_string(), lines);
    }
    let file_lines = cache.get(file).expect("just inserted");

    if file_lines.is_empty() {
        return vec![format!("{relative}:{line}: (unable to read file)")];
    }

    let start = line.saturating_sub(context).max(1);
    let end = (line + context).min(file_lines.len());
    let mut block = Vec::new();
    for current in start..=end {
        let text = file_lines
            .get(current - 1)
            .map(String::as_str)
            .unwrap_or("");
        let (text, cut) = truncate::truncate_line(text, GREP_MAX_LINE_LENGTH);
        *lines_truncated |= cut;
        if current == line {
            block.push(format!("{relative}:{current}: {text}"));
        } else {
            block.push(format!("{relative}-{current}- {text}"));
        }
    }
    block
}

/// 把 stderr 拼成 `: detail` 形式，空则省略。
fn detail(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(": {trimmed}")
    }
}
