use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Result;
pub use oma_contract::ToolOutput;
use serde::Deserialize;

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

/// 子 Agent 执行委托 Trait
#[async_trait::async_trait]
pub trait SubagentRunner: Send + Sync {
    async fn run_subagent(&self, agent: &str, prompt: &str) -> Result<String, String>;
}

/// 统一 Tool Trait
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput;
}

// ==========================================
// 1. Read Tool
// ==========================================
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
        "Read file content with line slicing."
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
                    "description": "1-based starting line number (default: 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 1000)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let input: ReadInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for read: {}", e)),
        };

        let target_path = resolve_path(workspace, &input.path);
        if !target_path.exists() {
            return ToolOutput::error(format!("File not found: {:?}", target_path));
        }

        let content = match tokio::fs::read_to_string(&target_path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("Failed to read file: {}", e)),
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
    path:     String,
    #[serde(default)]
    edits:    Vec<EditHunk>,
    // 兼容单对象参数传参
    #[serde(default)]
    old_text: Option<String>,
    #[serde(default)]
    new_text: Option<String>,
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
        let mut input: EditInput = match serde_json::from_value(input) {
            Ok(v) => v,
            Err(e) => return ToolOutput::error(format!("Invalid arguments for edit: {}", e)),
        };

        // 归一化：若模型传入单块 old_text / new_text
        if input.edits.is_empty() {
            if let (Some(old_t), Some(new_t)) = (input.old_text.take(), input.new_text.take()) {
                input.edits.push(EditHunk {
                    old_text: old_t,
                    new_text: new_t,
                });
            }
        }

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

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&input.command);
        cmd.current_dir(workspace);
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
                    ToolOutput {
                        output:   format!("Command exited with code {:?}:\n{}", output.status.code(), truncated),
                        is_error: true,
                    }
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
pub struct RunnerSlot(std::sync::OnceLock<Arc<dyn SubagentRunner>>);

impl RunnerSlot {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, runner: Arc<dyn SubagentRunner>) -> Result<()> {
        self.0
            .set(runner)
            .map_err(|_| anyhow::anyhow!("Subagent runner is already bound"))
    }

    pub fn get(&self) -> Option<&Arc<dyn SubagentRunner>> {
        self.0.get()
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
}
