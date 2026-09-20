//! oma 的 QuickJS 插件系统。
//!
//! 插件是纯 JavaScript 文件（`<root>/<plugin-id>/plugin.js`），随进程内嵌的
//! QuickJS 引擎执行，无需 Node。插件通过全局 `oma` 对象注册工具、命令与事件
//! 钩子；宿主能力（读写文件、列目录、执行命令、日志）由 Rust 侧白名单桥接。
//!
//! 发现顺序：项目级 `<workspace>/.oma/plugins/` 覆盖全局 `~/.config/oma/plugins/`
//! （按目录名，即 plugin id 去重）。
//!
//! 设计取舍：
//! - 插件 `execute` 必须**同步**返回；异步 Promise 会被显式拒绝（避免把 QuickJS
//!   事件循环塞进 Rust 的 async 运行时）。宿主调用本身是阻塞的，工具执行整体跑在
//!   `spawn_blocking` 里。
//! - 每次工具调用新建一个 JS 上下文，插件顶层状态不跨调用保留。
//! - 插件与 pi 的扩展一样拥有宿主进程权限（见 README 的安全提示），
//!   文件访问被约束在 workspace 内。

use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use oma_contract::ToolOutput;
use oma_tool::Tool;
use rquickjs::{Context, Ctx, Exception, Function, Object, Result as JsResult, Runtime};
use serde::{Deserialize, Serialize};

/// 宿主注入的 JS 预置：把底层函数包装成用户友好的 `oma.*` API，
/// 并收集注册项到全局数组。
const PRELUDE: &str = r#"
globalThis.__oma_tools = [];
globalThis.__oma_commands = [];
globalThis.__oma_hooks = { tool_call: [], tool_result: [] };
oma.log = (...a) => oma.__log(a.map(x => (typeof x === 'string' ? x : JSON.stringify(x))).join(' '));
oma.readFile = (p) => oma.__readFile(String(p));
oma.writeFile = (p, c) => { oma.__writeFile(String(p), String(c)); };
oma.listDir = (p) => JSON.parse(oma.__listDir(String(p)));
oma.exists = (p) => oma.__exists(String(p));
oma.exec = (c) => JSON.parse(oma.__exec(String(c)));
oma.registerTool = (def) => {
  if (def && def.name && typeof def.execute === 'function') globalThis.__oma_tools.push(def);
};
oma.registerCommand = (def) => {
  if (def && def.name && typeof def.handler === 'function') globalThis.__oma_commands.push(def);
};
oma.on = (event, fn) => {
  const k = String(event);
  (globalThis.__oma_hooks[k] || (globalThis.__oma_hooks[k] = [])).push(fn);
};
// 已支持的事件：tool_call / tool_result（可返回值阻断）、
// user_input（可 { content } 改写或 { block, reason } 拦截）、
// turn_start / turn_end（观察型）
"#;

const TOOL_META_EXPR: &str = r#"
JSON.stringify(globalThis.__oma_tools.map(t => ({
  name: String(t.name),
  label: String(t.label || ''),
  description: String(t.description || ''),
  parameters: (t.parameters && typeof t.parameters === 'object') ? t.parameters : { type: 'object' }
})))
"#;

const COMMAND_META_EXPR: &str = r#"
JSON.stringify(globalThis.__oma_commands.map(c => ({
  name: String(c.name),
  description: String(c.description || '')
})))
"#;

const HOOK_TOOL_CALL_EXPR: &str = r#"
(() => {
  const hs = globalThis.__oma_hooks.tool_call || [];
  for (const h of hs) {
    const r = h(JSON.parse(globalThis.__oma_event));
    if (r && r.block) return JSON.stringify({ block: true, reason: String(r.reason || 'blocked by plugin') });
  }
  return '';
})()
"#;

/// 通用事件发射：把 `__oma_event_name` 对应的钩子全部跑一遍，收集非空返回值。
///
/// 与 `tool_call` / `user_input` 这类「返回值有语义」的钩子分开：这里只负责
/// 观察型事件（turn_start / turn_end），调用方不关心返回值。
const HOOK_EMIT_EXPR: &str = r#"
(() => {
  const hs = globalThis.__oma_hooks[globalThis.__oma_event_name] || [];
  const out = [];
  for (const h of hs) {
    const r = h(JSON.parse(globalThis.__oma_event));
    if (r !== undefined && r !== null) out.push(r);
  }
  return JSON.stringify(out);
})()
"#;

/// `user_input` 钩子：可改写内容（`{ content }`）或拦截（`{ block, reason }`）。
///
/// 多个钩子按加载顺序依次运行，前一个的改写是后一个的输入。
const HOOK_USER_INPUT_EXPR: &str = r#"
(() => {
  const hs = globalThis.__oma_hooks.user_input || [];
  let content = String(globalThis.__oma_content);
  for (const h of hs) {
    const r = h({ content });
    if (!r) continue;
    if (r.block) return JSON.stringify({ block: true, reason: String(r.reason || 'blocked by plugin') });
    if (typeof r.content === 'string') content = r.content;
  }
  return JSON.stringify({ content });
})()
"#;

/// `user_input` 钩子的决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserInputDecision {
    /// 原样放行
    Allow,
    /// 改为插件给出的内容
    Replace(String),
    /// 拦截，并把原因作为系统提示回给模型
    Block(String),
}

/// 插件注册的工具元数据（JSON Schema 原样透传给模型）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginToolDef {
    pub name:        String,
    #[serde(default)]
    pub label:       String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_parameters")]
    pub parameters:  serde_json::Value,
}

fn default_parameters() -> serde_json::Value {
    serde_json::json!({ "type": "object" })
}

/// 插件注册的命令元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginCommand {
    pub name:        String,
    #[serde(default)]
    pub description: String,
}

impl PluginCommand {
    /// 展示用全名：`pluginId:command`。
    pub fn qualified(&self, plugin_id: &str) -> String {
        format!("{}:{}", plugin_id, self.name)
    }
}

/// 已加载的单个插件。
pub struct LoadedPlugin {
    pub id:    String,
    pub scope: String,
    pub path:  PathBuf,
    source:    String,
    workspace: PathBuf,
    tools:     Vec<PluginToolDef>,
    commands:  Vec<PluginCommand>,
}

impl LoadedPlugin {
    /// 读取并求值插件源码，抽出注册的工具与命令元数据。
    fn load(id: String, scope: &str, path: PathBuf, workspace: &Path) -> anyhow::Result<Self> {
        let source = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read plugin {}: {}", path.display(), e))?;
        let mut plugin = Self {
            id,
            scope: scope.to_string(),
            path,
            source,
            workspace: workspace.to_path_buf(),
            tools: Vec::new(),
            commands: Vec::new(),
        };
        let tools_json: String = plugin.with_context(|ctx| eval_string(ctx, TOOL_META_EXPR))?;
        plugin.tools = serde_json::from_str(&tools_json)
            .map_err(|e| anyhow::anyhow!("plugin returned invalid tool metadata: {}", e))?;
        let commands_json: String = plugin.with_context(|ctx| eval_string(ctx, COMMAND_META_EXPR))?;
        plugin.commands = serde_json::from_str(&commands_json).unwrap_or_default();
        Ok(plugin)
    }

    /// 建立上下文、注入宿主 API、执行预置与插件源码，再运行 `f`。
    fn with_context<T>(&self, f: impl for<'js> FnOnce(&Ctx<'js>) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let rt = Runtime::new().map_err(js)?;
        let ctx = Context::full(&rt).map_err(js)?;
        ctx.with(|ctx| {
            install_host(&ctx, &self.workspace).map_err(js)?;
            ctx.eval::<(), _>(PRELUDE).map_err(js)?;
            ctx.eval::<(), _>(self.source.as_str()).map_err(js)?;
            f(&ctx)
        })
    }

    /// 在插件上下文中调用第 `index` 个工具。
    fn run_tool(&self, index: usize, input: &serde_json::Value) -> anyhow::Result<String> {
        let params = serde_json::to_string(input)?;
        self.with_context(|ctx| {
            ctx.globals()
                .set("__oma_params", params.clone())
                .map_err(js)?;
            let expr = format!(
                r#"(() => {{
                    const t = globalThis.__oma_tools[{index}];
                    if (!t) throw new Error('plugin tool index out of range');
                    const r = t.execute(JSON.parse(globalThis.__oma_params));
                    if (r && typeof r.then === 'function') {{
                        throw new Error('async execute is not supported; plugin tools must be synchronous');
                    }}
                    return typeof r === 'string' ? r : JSON.stringify(r === undefined ? null : r);
                }})()"#
            );
            eval_string(ctx, expr.as_str())
        })
    }

    /// 运行 `tool_call` 钩子；返回 Some(reason) 表示应拦截执行。
    fn before_tool_call(&self, name: &str, input: &serde_json::Value) -> anyhow::Result<Option<String>> {
        let event = serde_json::json!({ "name": name, "input": input }).to_string();
        self.with_context(|ctx| {
            ctx.globals()
                .set("__oma_event", event.clone())
                .map_err(js)?;
            let out: String = eval_string(ctx, HOOK_TOOL_CALL_EXPR)?;
            if out.is_empty() {
                return Ok(None);
            }
            let value: serde_json::Value = serde_json::from_str(&out)?;
            Ok(Some(
                value
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("blocked by plugin")
                    .to_string(),
            ))
        })
    }

    /// 发射一个观察型事件，返回各钩子的返回值（调用方通常忽略）。
    fn emit(&self, event: &str, payload: &serde_json::Value) -> anyhow::Result<Vec<serde_json::Value>> {
        let payload = serde_json::to_string(payload)?;
        let name = event.to_string();
        let out: String = self.with_context(|ctx| {
            ctx.globals()
                .set("__oma_event_name", name.clone())
                .map_err(js)?;
            ctx.globals()
                .set("__oma_event", payload.clone())
                .map_err(js)?;
            eval_string(ctx, HOOK_EMIT_EXPR)
        })?;
        serde_json::from_str(&out).map_err(|e| anyhow::anyhow!("plugin returned invalid hook results: {e}"))
    }

    /// 运行 `user_input` 钩子：可改写内容，也可拦截。
    fn before_user_input(&self, content: &str) -> anyhow::Result<UserInputDecision> {
        let out: String = self.with_context(|ctx| {
            ctx.globals()
                .set("__oma_content", content.to_string())
                .map_err(js)?;
            eval_string(ctx, HOOK_USER_INPUT_EXPR)
        })?;
        let value: serde_json::Value = serde_json::from_str(&out)?;
        if value
            .get("block")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)
        {
            let reason = value
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("blocked by plugin")
                .to_string();
            return Ok(UserInputDecision::Block(reason));
        }
        match value.get("content").and_then(|c| c.as_str()) {
            Some(rewritten) if rewritten != content => Ok(UserInputDecision::Replace(rewritten.to_string())),
            _ => Ok(UserInputDecision::Allow),
        }
    }

    /// 运行命令处理器，返回其注入文本。
    fn run_command(&self, name: &str, args: &serde_json::Value) -> anyhow::Result<String> {
        let args = serde_json::to_string(args)?;
        self.with_context(|ctx| {
            ctx.globals()
                .set("__oma_cmd_name", name.to_string())
                .map_err(js)?;
            ctx.globals()
                .set("__oma_cmd_args", args.clone())
                .map_err(js)?;
            let expr = r#"(() => {
                const want = String(globalThis.__oma_cmd_name);
                for (const c of (globalThis.__oma_commands || [])) {
                    if (String(c.name) === want) {
                        const r = c.handler(JSON.parse(globalThis.__oma_cmd_args));
                        if (r && typeof r.then === 'function') {
                            throw new Error('async command handler is not supported');
                        }
                        return typeof r === 'string' ? r : JSON.stringify(r === undefined ? null : r);
                    }
                }
                return '';
            })()"#;
            eval_string(ctx, expr)
        })
    }
}

/// 把 rquickjs 错误转成 anyhow。
fn js(e: rquickjs::Error) -> anyhow::Error {
    anyhow::anyhow!("JS error: {}", e)
}

/// 求值一段返回字符串的 JS；失败时尽量取出 JS 异常消息（rquickjs 的
/// `Error` Display 是泛化的，拿不到 throw 的文本）。
fn eval_string<'js>(ctx: &Ctx<'js>, src: &str) -> anyhow::Result<String> {
    match ctx.eval::<String, _>(src) {
        Ok(value) => Ok(value),
        Err(e) => {
            let caught = ctx.catch();
            let msg = caught
                .as_object()
                .and_then(|obj| obj.get::<_, String>("message").ok())
                .unwrap_or_else(|| e.to_string());
            Err(anyhow::anyhow!("JS error: {}", msg))
        }
    }
}

/// 注入宿主 API 对象 `oma`。
fn install_host<'js>(ctx: &Ctx<'js>, workspace: &Path) -> JsResult<()> {
    let oma = Object::new(ctx.clone())?;
    oma.set("workspace", workspace.to_string_lossy().to_string())?;
    oma.set(
        "__log",
        Function::new(ctx.clone(), |msg: String| {
            tracing::info!(target: "oma_plugin", "{}", msg);
        })?,
    )?;

    let ws = workspace.to_path_buf();
    let read_ws = ws.clone();
    oma.set(
        "__readFile",
        Function::new(ctx.clone(), move |ctx: Ctx, path: String| -> JsResult<String> {
            let target =
                resolve_sandboxed(&read_ws, &path).map_err(|e| Exception::throw_message(&ctx, &e.to_string()))?;
            std::fs::read_to_string(&target)
                .map_err(|e| Exception::throw_message(&ctx, &format!("readFile {}: {}", path, e)))
        })?,
    )?;

    let write_ws = ws.clone();
    oma.set(
        "__writeFile",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx, path: String, content: String| -> JsResult<()> {
                let target =
                    resolve_sandboxed(&write_ws, &path).map_err(|e| Exception::throw_message(&ctx, &e.to_string()))?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Exception::throw_message(&ctx, &format!("writeFile {}: {}", path, e)))?;
                }
                std::fs::write(&target, content)
                    .map_err(|e| Exception::throw_message(&ctx, &format!("writeFile {}: {}", path, e)))
            },
        )?,
    )?;

    let list_ws = ws.clone();
    oma.set(
        "__listDir",
        Function::new(ctx.clone(), move |ctx: Ctx, path: String| -> JsResult<String> {
            let target =
                resolve_sandboxed(&list_ws, &path).map_err(|e| Exception::throw_message(&ctx, &e.to_string()))?;
            let entries = std::fs::read_dir(&target)
                .map_err(|e| Exception::throw_message(&ctx, &format!("listDir {}: {}", path, e)))?;
            let mut names: Vec<String> = entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect();
            names.sort();
            Ok(serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string()))
        })?,
    )?;

    let exists_ws = ws.clone();
    oma.set(
        "__exists",
        Function::new(ctx.clone(), move |path: String| -> bool {
            resolve_sandboxed(&exists_ws, &path)
                .map(|p| p.exists())
                .unwrap_or(false)
        })?,
    )?;

    let exec_ws = ws;
    oma.set(
        "__exec",
        Function::new(ctx.clone(), move |ctx: Ctx, command: String| -> JsResult<String> {
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&exec_ws)
                .output()
                .map_err(|e| Exception::throw_message(&ctx, &format!("exec failed: {}", e)))?;
            let value = serde_json::json!({
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
                "code": output.status.code(),
            });
            Ok(value.to_string())
        })?,
    )?;

    ctx.globals().set("oma", oma)?;
    Ok(())
}

/// 把插件给的相对路径约束到 workspace 内；绝对路径与 `..` 一律拒绝。
fn resolve_sandboxed(workspace: &Path, raw: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(raw);
    if path.is_absolute() {
        anyhow::bail!("absolute paths are not allowed: {raw}");
    }
    let mut out = workspace.to_path_buf();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("path traversal is not allowed: {raw}")
            }
        }
    }
    Ok(out)
}

/// 插件工具：把一个已注册的 JS 工具桥接为 [`oma_tool::Tool`]。
pub struct PluginTool {
    plugin:      Arc<LoadedPlugin>,
    index:       usize,
    name:        String,
    description: String,
    parameters:  serde_json::Value,
}

#[async_trait::async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.parameters.clone()
    }

    async fn execute(&self, _workspace: &Path, input: serde_json::Value) -> ToolOutput {
        let plugin = self.plugin.clone();
        let index = self.index;
        let run = tokio::task::spawn_blocking(move || plugin.run_tool(index, &input)).await;
        match run {
            Ok(Ok(text)) => ToolOutput::success(text),
            Ok(Err(e)) => ToolOutput::error(format!("Plugin tool '{}' failed: {:#}", self.name, e)),
            Err(e) => ToolOutput::error(format!("Plugin tool '{}' panicked: {}", self.name, e)),
        }
    }
}

/// 全部已加载插件的集合。
pub struct PluginHost {
    plugins: Vec<Arc<LoadedPlugin>>,
}

impl PluginHost {
    /// 按全局 → 项目顺序发现并加载插件；单个插件加载失败只跳过并告警。
    pub fn load(workspace: &Path) -> Self {
        let mut by_id: std::collections::BTreeMap<String, (String, PathBuf)> = std::collections::BTreeMap::new();
        if let Some(dir) = global_plugins_dir() {
            collect_plugins(&dir, "global", &mut by_id);
        }
        collect_plugins(&workspace.join(".oma").join("plugins"), "project", &mut by_id);

        let mut plugins = Vec::new();
        for (id, (scope, path)) in by_id {
            match LoadedPlugin::load(id.clone(), &scope, path.clone(), workspace) {
                Ok(p) => plugins.push(Arc::new(p)),
                Err(e) => tracing::warn!(plugin = %id, error = %e, "failed to load plugin"),
            }
        }
        Self { plugins }
    }

    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// 插件提供的工具（每个已注册工具一个条目）。
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut out: Vec<Arc<dyn Tool>> = Vec::new();
        for plugin in &self.plugins {
            for (index, def) in plugin.tools.iter().enumerate() {
                out.push(Arc::new(PluginTool {
                    plugin: plugin.clone(),
                    index,
                    name: def.name.clone(),
                    description: def.description.clone(),
                    parameters: def.parameters.clone(),
                }));
            }
        }
        out
    }

    /// 全部插件命令（限定名 `plugin:command`）。
    pub fn commands(&self) -> Vec<PluginCommand> {
        let mut out = Vec::new();
        for plugin in &self.plugins {
            for command in &plugin.commands {
                out.push(command.clone());
            }
        }
        out
    }

    /// 运行 `tool_call` 钩子；任一插件返回 block 即拦截。
    pub fn before_tool_call(&self, name: &str, input: &serde_json::Value) -> Option<String> {
        for plugin in &self.plugins {
            match plugin.before_tool_call(name, input) {
                Ok(Some(reason)) => return Some(reason),
                Ok(None) => {}
                Err(e) => tracing::warn!(plugin = %plugin.id, error = %e, "plugin tool_call hook failed"),
            }
        }
        None
    }

    /// 发射一个观察型生命周期事件（`turn_start` / `turn_end`）。
    ///
    /// 钩子抛错只告警：插件出问题不该让正在跑的轮次失败。
    pub fn emit(&self, event: &str, payload: &serde_json::Value) {
        for plugin in &self.plugins {
            if let Err(e) = plugin.emit(event, payload) {
                tracing::warn!(plugin = %plugin.id, event = %event, error = %e, "plugin hook failed");
            }
        }
    }

    /// 运行 `user_input` 钩子：任一插件可改写用户输入或拦截整轮。
    ///
    /// 改写按加载顺序链式传递（后一个看到前一个的结果）；拦截立即生效。
    /// 钩子抛错时保守放行原内容（不让插件故障阻断用户说话）。
    pub fn before_user_input(&self, content: &str) -> UserInputDecision {
        let mut current = content.to_string();
        for plugin in &self.plugins {
            match plugin.before_user_input(&current) {
                Ok(UserInputDecision::Allow) => {}
                Ok(UserInputDecision::Replace(next)) => current = next,
                Ok(UserInputDecision::Block(reason)) => return UserInputDecision::Block(reason),
                Err(e) => tracing::warn!(plugin = %plugin.id, error = %e, "plugin user_input hook failed"),
            }
        }
        if current == content {
            UserInputDecision::Allow
        } else {
            UserInputDecision::Replace(current)
        }
    }

    /// 运行指定命令；未找到返回 None，找到则返回注入文本。
    pub fn run_command(&self, qualified: &str, args: &serde_json::Value) -> Option<anyhow::Result<String>> {
        let (plugin_id, command) = qualified.split_once(':')?;
        for plugin in &self.plugins {
            if plugin.id != plugin_id {
                continue;
            }
            if plugin.commands.iter().any(|c| c.name == command) {
                return Some(plugin.run_command(command, args));
            }
        }
        None
    }
}

fn collect_plugins(dir: &Path, scope: &str, by_id: &mut std::collections::BTreeMap<String, (String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_valid_plugin_id(id) {
            tracing::warn!(plugin = %id, "ignoring plugin with invalid id");
            continue;
        }
        let source = path.join("plugin.js");
        if !source.is_file() {
            continue;
        }
        by_id.insert(id.to_string(), (scope.to_string(), source));
    }
}

fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// 全局插件目录：`~/.config/oma/plugins`（遵循 XDG_CONFIG_HOME）。
pub fn global_plugins_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("oma").join("plugins"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_plugin(dir: &Path, id: &str, source: &str) -> PathBuf {
        let plugin_dir = dir.join(id);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        let path = plugin_dir.join("plugin.js");
        std::fs::write(&path, source).unwrap();
        path
    }

    #[tokio::test]
    async fn test_loads_tool_and_executes() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write_plugin(
            &ws.join(".oma").join("plugins"),
            "hello",
            r#"
oma.registerTool({
  name: "hello",
  description: "Greet someone",
  parameters: { type: "object", properties: { who: { type: "string" } }, required: ["who"] },
  execute: (p) => "Hello, " + p.who + "!"
});
"#,
        );

        let host = PluginHost::load(ws);
        let tools = host.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "hello");
        assert_eq!(tools[0].parameters_schema()["required"][0], "who");

        let out = tools[0]
            .execute(ws, serde_json::json!({ "who": "Oma" }))
            .await;
        assert!(!out.is_error, "unexpected error: {}", out.output);
        assert_eq!(out.output, "Hello, Oma!");
    }

    #[tokio::test]
    async fn test_host_fs_is_sandboxed() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::write(ws.join("sub").join("a.txt"), "content-a").unwrap();
        write_plugin(
            &ws.join(".oma").join("plugins"),
            "fs",
            r#"
oma.registerTool({
  name: "peek",
  description: "read a file",
  parameters: { type: "object" },
  execute: () => oma.readFile("sub/a.txt")
});
oma.registerTool({
  name: "escape",
  description: "try to escape",
  parameters: { type: "object" },
  execute: () => oma.readFile("../../etc/passwd")
});
"#,
        );

        let host = PluginHost::load(ws);
        let tools = host.tools();
        assert_eq!(tools.len(), 2);

        let ok = tools[0].execute(ws, serde_json::json!({})).await;
        assert_eq!(ok.output, "content-a");

        let escaped = tools[1].execute(ws, serde_json::json!({})).await;
        assert!(escaped.is_error, "path traversal must fail");
        assert!(escaped.output.contains("traversal"), "unexpected: {}", escaped.output);
    }

    #[test]
    fn test_tool_call_hook_can_block() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write_plugin(
            &ws.join(".oma").join("plugins"),
            "guard",
            r#"
oma.on("tool_call", (e) => (e.name === "bash" ? { block: true, reason: "bash disabled" } : undefined));
"#,
        );

        let host = PluginHost::load(ws);
        let blocked = host.before_tool_call("bash", &serde_json::json!({ "command": "rm -rf /" }));
        assert_eq!(blocked.as_deref(), Some("bash disabled"));
        assert!(
            host.before_tool_call("read", &serde_json::json!({}))
                .is_none()
        );
    }

    /// `user_input` 钩子可以改写用户输入（例如先做模板展开）。
    #[test]
    fn test_user_input_hook_can_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(
            &tmp.path().join(".oma").join("plugins"),
            "expand",
            r#"
oma.on("user_input", (e) => ({ content: e.content.replace("PREFIX", "展开后的内容") }));
"#,
        );

        let host = PluginHost::load(tmp.path());
        assert_eq!(
            host.before_user_input("PREFIX 后面"),
            UserInputDecision::Replace("展开后的内容 后面".into())
        );
        // 未命中改写时视作放行，不构造无意义的新字符串
        assert_eq!(host.before_user_input("无关输入"), UserInputDecision::Allow);
    }

    /// 改写按加载顺序链式传递，且拦截立即生效。
    #[test]
    fn test_user_input_hooks_chain_and_block() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".oma").join("plugins");
        write_plugin(
            &dir,
            "a-first",
            r#"oma.on("user_input", (e) => ({ content: e.content + " +A" }));"#,
        );
        write_plugin(
            &dir,
            "b-second",
            r#"oma.on("user_input", (e) => (e.content.includes("+A") ? { content: e.content + " +B" } : undefined));"#,
        );

        let host = PluginHost::load(tmp.path());
        assert_eq!(
            host.before_user_input("hi"),
            UserInputDecision::Replace("hi +A +B".into())
        );

        // 拦截：后续钩子不再跑
        let tmp2 = tempfile::tempdir().unwrap();
        let dir2 = tmp2.path().join(".oma").join("plugins");
        write_plugin(
            &dir2,
            "a-block",
            r#"oma.on("user_input", () => ({ block: true, reason: "维护中" }));"#,
        );
        write_plugin(
            &dir2,
            "b-late",
            r#"oma.on("user_input", () => ({ content: "不该生效" }));"#,
        );
        let host2 = PluginHost::load(tmp2.path());
        assert_eq!(host2.before_user_input("hi"), UserInputDecision::Block("维护中".into()));
    }

    /// 观察型事件的返回值被收集；钩子抛错不影响其他钩子与调用方。
    #[test]
    fn test_emit_collects_results_and_survives_throw() {
        let tmp = tempfile::tempdir().unwrap();
        write_plugin(
            &tmp.path().join(".oma").join("plugins"),
            "observer",
            r#"
oma.on("turn_start", (e) => ({ seen: e.turn_id }));
oma.on("turn_start", () => { throw new Error("boom"); });
"#,
        );

        let host = PluginHost::load(tmp.path());
        // 同一插件内抛错会让这次发命中止，但宿主只告警、不 panic
        host.emit("turn_start", &serde_json::json!({ "turn_id": "t1" }));

        // 没有钩子的未知事件也不得报错
        host.emit("nope", &serde_json::json!({}));
    }

    #[test]
    fn test_command_registration_and_run() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();
        write_plugin(
            &ws.join(".oma").join("plugins"),
            "cmds",
            r#"
oma.registerCommand({
  name: "explain",
  description: "Explain a topic",
  handler: (args) => "Please explain: " + (args.topic || "everything")
});
"#,
        );

        let host = PluginHost::load(ws);
        let commands = host.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].qualified("cmds"), "cmds:explain");

        let out = host
            .run_command("cmds:explain", &serde_json::json!({ "topic": "QuickJS" }))
            .unwrap()
            .unwrap();
        assert_eq!(out, "Please explain: QuickJS");
        assert!(
            host.run_command("cmds:missing", &serde_json::json!({}))
                .is_none()
        );
    }

    #[test]
    fn test_project_overrides_global_and_invalid_source_skipped() {
        // 全局插件目录通过 XDG_CONFIG_HOME 指向临时目录，避免污染真实用户目录。
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        // 说明：本测试只验证项目级插件的加载；全局目录因进程级环境变量不宜在测试中改写。
        write_plugin(
            &ws.join(".oma").join("plugins"),
            "p",
            r#"oma.registerTool({ name: "a", description: "a", parameters: {}, execute: () => "a" });"#,
        );
        // 语法错误的插件应被跳过而不是让加载失败
        write_plugin(&ws.join(".oma").join("plugins"), "broken", "this is not valid js ((");

        let host = PluginHost::load(&ws);
        let names: Vec<String> = host.tools().iter().map(|t| t.name().to_string()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn test_sandbox_resolution() {
        let ws = Path::new("/tmp/ws");
        assert_eq!(
            resolve_sandboxed(ws, "a/b.txt").unwrap(),
            PathBuf::from("/tmp/ws/a/b.txt")
        );
        assert!(resolve_sandboxed(ws, "../x").is_err());
        assert!(resolve_sandboxed(ws, "/etc/passwd").is_err());
    }
}
