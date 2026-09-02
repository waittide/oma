use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use anyhow::{Context, Result};
use oma_contract::ToolOutput;
use oma_tool::Tool;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// MCP 服务器配置定义
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerConfig {
    Local {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
    },
    Remote {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
}

/// JSON-RPC 请求
#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
}

/// JSON-RPC 通知
#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub params: serde_json::Value,
}

/// JSON-RPC 响应
#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

/// MCP 导出的工具元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
}

/// 本地 Stdio 客户端连接
struct LocalMcpProcess {
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    _child: Child,
}

/// 单个 MCP 客户端连接抽象
pub struct McpClient {
    name: String,
    config: McpServerConfig,
    req_counter: AtomicU64,
    local_proc: Mutex<Option<LocalMcpProcess>>,
    tools_cache: RwLock<Vec<McpToolInfo>>,
    http_client: reqwest::Client,
}

impl McpClient {
    pub fn new(name: impl Into<String>, config: McpServerConfig) -> Self {
        Self {
            name: name.into(),
            config,
            req_counter: AtomicU64::new(1),
            local_proc: Mutex::new(None),
            tools_cache: RwLock::new(Vec::new()),
            http_client: reqwest::Client::new(),
        }
    }

    /// 初始化握手并拉取工具列表
    pub async fn connect_and_discover(&self) -> Result<Vec<McpToolInfo>> {
        match &self.config {
            McpServerConfig::Local { command, args, env } => {
                let mut proc_guard = self.local_proc.lock().await;
                if proc_guard.is_none() {
                    let mut cmd = Command::new(command);
                    cmd.args(args);
                    for (k, v) in env {
                        cmd.env(k, v);
                    }
                    cmd.stdin(std::process::Stdio::piped());
                    cmd.stdout(std::process::Stdio::piped());
                    cmd.stderr(std::process::Stdio::null());

                    let mut child = cmd.spawn().with_context(|| format!("Failed to spawn local MCP server '{}'", self.name))?;
                    let stdin = child.stdin.take().context("Failed to take stdin")?;
                    let stdout = child.stdout.take().context("Failed to take stdout")?;
                    let reader = BufReader::new(stdout);

                    *proc_guard = Some(LocalMcpProcess {
                        stdin,
                        reader,
                        _child: child,
                    });
                }

                let proc = proc_guard.as_mut().unwrap();

                // 1. 发送 initialize 请求
                let init_id = self.req_counter.fetch_add(1, Ordering::SeqCst);
                let init_req = JsonRpcRequest {
                    jsonrpc: "2.0".into(),
                    id: init_id,
                    method: "initialize".into(),
                    params: serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": { "name": "oma", "version": "0.1.0" }
                    }),
                };
                let mut line = serde_json::to_string(&init_req)?;
                line.push('\n');
                proc.stdin.write_all(line.as_bytes()).await?;
                proc.stdin.flush().await?;

                let mut resp_line = String::new();
                proc.reader.read_line(&mut resp_line).await?;

                // 2. 发送 initialized 通知
                let notif = JsonRpcNotification {
                    jsonrpc: "2.0".into(),
                    method: "notifications/initialized".into(),
                    params: serde_json::json!({}),
                };
                let mut notif_line = serde_json::to_string(&notif)?;
                notif_line.push('\n');
                proc.stdin.write_all(notif_line.as_bytes()).await?;
                proc.stdin.flush().await?;

                // 3. 发送 tools/list 请求
                let list_id = self.req_counter.fetch_add(1, Ordering::SeqCst);
                let list_req = JsonRpcRequest {
                    jsonrpc: "2.0".into(),
                    id: list_id,
                    method: "tools/list".into(),
                    params: serde_json::json!({}),
                };
                let mut list_line = serde_json::to_string(&list_req)?;
                list_line.push('\n');
                proc.stdin.write_all(list_line.as_bytes()).await?;
                proc.stdin.flush().await?;

                let mut list_resp_line = String::new();
                proc.reader.read_line(&mut list_resp_line).await?;
                let list_resp: JsonRpcResponse = serde_json::from_str(&list_resp_line)
                    .with_context(|| format!("Invalid JSON-RPC response from '{}': {}", self.name, list_resp_line))?;

                let tools = if let Some(res) = list_resp.result {
                    if let Some(t_array) = res.get("tools").and_then(|v| v.as_array()) {
                        serde_json::from_value(serde_json::Value::Array(t_array.clone())).unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                *self.tools_cache.write() = tools.clone();
                Ok(tools)
            }
            McpServerConfig::Remote { url, headers } => {
                // Remote HTTP/SSE 工具发现（请求 GET /tools 或 POST rpc）
                let mut req = self.http_client.post(url).header("content-type", "application/json");
                for (k, v) in headers {
                    req = req.header(k, v);
                }

                let list_req = JsonRpcRequest {
                    jsonrpc: "2.0".into(),
                    id: 1,
                    method: "tools/list".into(),
                    params: serde_json::json!({}),
                };

                let resp = req.json(&list_req).send().await.context("Failed to connect to remote MCP server")?;
                let list_resp: JsonRpcResponse = resp.json().await?;

                let tools = if let Some(res) = list_resp.result {
                    if let Some(t_array) = res.get("tools").and_then(|v| v.as_array()) {
                        serde_json::from_value(serde_json::Value::Array(t_array.clone())).unwrap_or_default()
                    } else {
                        Vec::new()
                    }
                } else {
                    Vec::new()
                };

                *self.tools_cache.write() = tools.clone();
                Ok(tools)
            }
        }
    }

    /// 执行远程/本地工具调用 (tools/call)
    pub async fn call_tool(&self, tool_name: &str, arguments: serde_json::Value) -> ToolOutput {
        match &self.config {
            McpServerConfig::Local { .. } => {
                let mut proc_guard = self.local_proc.lock().await;
                let Some(proc) = proc_guard.as_mut() else {
                    return ToolOutput::error("Local MCP server is not connected.");
                };

                let call_id = self.req_counter.fetch_add(1, Ordering::SeqCst);
                let call_req = JsonRpcRequest {
                    jsonrpc: "2.0".into(),
                    id: call_id,
                    method: "tools/call".into(),
                    params: serde_json::json!({
                        "name": tool_name,
                        "arguments": arguments
                    }),
                };

                let mut req_str = match serde_json::to_string(&call_req) {
                    Ok(s) => s,
                    Err(e) => return ToolOutput::error(e.to_string()),
                };
                req_str.push('\n');

                if let Err(e) = proc.stdin.write_all(req_str.as_bytes()).await {
                    return ToolOutput::error(format!("Failed to write to MCP stdin: {}", e));
                }
                if let Err(e) = proc.stdin.flush().await {
                    return ToolOutput::error(format!("Failed to flush MCP stdin: {}", e));
                }

                let mut resp_line = String::new();
                if let Err(e) = proc.reader.read_line(&mut resp_line).await {
                    return ToolOutput::error(format!("Failed to read MCP response: {}", e));
                }

                let resp: JsonRpcResponse = match serde_json::from_str(&resp_line) {
                    Ok(r) => r,
                    Err(e) => return ToolOutput::error(format!("Invalid MCP JSON response: {}", e)),
                };

                if let Some(err) = resp.error {
                    return ToolOutput::error(err.to_string());
                }

                if let Some(res) = resp.result {
                    let is_error = res.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                    let text = if let Some(contents) = res.get("content").and_then(|v| v.as_array()) {
                        contents
                            .iter()
                            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        res.to_string()
                    };
                    if is_error {
                        ToolOutput::error(text)
                    } else {
                        ToolOutput::success(text)
                    }
                } else {
                    ToolOutput::success("")
                }
            }
            McpServerConfig::Remote { url, headers } => {
                let mut req = self.http_client.post(url).header("content-type", "application/json");
                for (k, v) in headers {
                    req = req.header(k, v);
                }

                let call_req = JsonRpcRequest {
                    jsonrpc: "2.0".into(),
                    id: 1,
                    method: "tools/call".into(),
                    params: serde_json::json!({
                        "name": tool_name,
                        "arguments": arguments
                    }),
                };

                let resp = match req.json(&call_req).send().await {
                    Ok(r) => r,
                    Err(e) => return ToolOutput::error(format!("HTTP error calling MCP: {}", e)),
                };

                let rpc_resp: JsonRpcResponse = match resp.json().await {
                    Ok(r) => r,
                    Err(e) => return ToolOutput::error(format!("Failed to parse MCP JSON: {}", e)),
                };

                if let Some(err) = rpc_resp.error {
                    return ToolOutput::error(err.to_string());
                }

                if let Some(res) = rpc_resp.result {
                    let is_error = res.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
                    let text = if let Some(contents) = res.get("content").and_then(|v| v.as_array()) {
                        contents
                            .iter()
                            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        res.to_string()
                    };
                    if is_error {
                        ToolOutput::error(text)
                    } else {
                        ToolOutput::success(text)
                    }
                } else {
                    ToolOutput::success("")
                }
            }
        }
    }
}

/// 将 MCP 工具包装为 `oma_tool::Tool` 的适配器
pub struct McpToolWrapper {
    namespaced_name: String,
    original_tool_name: String,
    description: String,
    input_schema: serde_json::Value,
    client: Arc<McpClient>,
}

#[async_trait::async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.input_schema.clone()
    }

    async fn execute(&self, _workspace: &Path, input: serde_json::Value) -> ToolOutput {
        self.client.call_tool(&self.original_tool_name, input).await
    }
}

/// 全局 MCP 管理中心
#[derive(Default)]
pub struct McpManager {
    clients: RwLock<BTreeMap<String, Arc<McpClient>>>,
}

impl McpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册服务器配置
    pub fn register_server(&self, name: impl Into<String>, config: McpServerConfig) {
        let name = name.into();
        let client = Arc::new(McpClient::new(name.clone(), config));
        self.clients.write().insert(name, client);
    }

    /// 发现并获取所有 MCP 服务器的包装工具集
    pub async fn create_all_tools(&self) -> Vec<Arc<dyn Tool>> {
        let clients: Vec<Arc<McpClient>> = self.clients.read().values().cloned().collect();
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();

        for client in clients {
            if let Ok(discovered) = client.connect_and_discover().await {
                for t in discovered {
                    let namespaced_name = format!("mcp__{}__{}", client.name, t.name);
                    let desc = t.description.unwrap_or_default();
                    let wrapper = McpToolWrapper {
                        namespaced_name,
                        original_tool_name: t.name,
                        description: desc,
                        input_schema: t.input_schema,
                        client: client.clone(),
                    };
                    tools.push(Arc::new(wrapper));
                }
            }
        }

        tools
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_config_serde() {
        let local = McpServerConfig::Local {
            command: "uvx".into(),
            args: vec!["mcp-server".into()],
            env: BTreeMap::new(),
        };
        let json = serde_json::to_string(&local).unwrap();
        assert!(json.contains("\"type\":\"local\""));

        let de: McpServerConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(de, McpServerConfig::Local { .. }));
    }

    #[test]
    fn test_json_rpc_messages() {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: 1,
            method: "tools/list".into(),
            params: serde_json::json!({}),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"method\":\"tools/list\""));

        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            result: Some(serde_json::json!({
                "tools": [{
                    "name": "calc",
                    "description": "Calculate expression",
                    "inputSchema": { "type": "object" }
                }]
            })),
            error: None,
        };
        let resp_s = serde_json::to_string(&resp).unwrap();
        let de: JsonRpcResponse = serde_json::from_str(&resp_s).unwrap();
        assert!(de.result.is_some());
    }
}
