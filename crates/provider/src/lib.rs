use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use oma_contract::{Block, ChatMessage, Role, StopReason};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// 三级递归合并 JSON 辅助函数
pub fn deep_merge_json(target: &mut serde_json::Value, source: &serde_json::Value) {
    match (target, source) {
        (serde_json::Value::Object(target_map), serde_json::Value::Object(source_map)) => {
            for (k, v) in source_map {
                deep_merge_json(target_map.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (target_slot, source_val) => {
            *target_slot = source_val.clone();
        }
    }
}

/// 组装 Gemini streamGenerateContent 请求体
/// （工具声明、图片内联、tool_use/tool_result 往返映射均在此完成）
fn build_gemini_body(
    messages: &[ChatMessage],
    system_prompt: Option<&str>,
    tools: &[serde_json::Value],
    model: &ModelConfig,
) -> serde_json::Value {
    // tool_use_id → 函数名：functionResponse 需要名称而非 id
    let mut tool_names: BTreeMap<String, String> = BTreeMap::new();
    for msg in messages {
        for b in &msg.content {
            if let Block::ToolUse { id, name, .. } = b {
                tool_names.insert(id.clone(), name.clone());
            }
        }
    }

    let mut contents = Vec::new();
    let mut pending_parts: Vec<serde_json::Value> = Vec::new();
    let mut pending_role = "user";

    // Gemini 要求同一轮次的 parts 合并进单个 content；连续同角色消息需合并
    for msg in messages {
        let role = match msg.role {
            Role::User => "user",
            Role::Assistant => "model",
            // System 通过 systemInstruction 下发
            Role::System => continue,
        };

        let mut parts = Vec::new();
        for b in &msg.content {
            match b {
                Block::Text { text } => {
                    if !text.is_empty() {
                        parts.push(serde_json::json!({ "text": text }));
                    }
                }
                Block::Thinking { .. } => {} // Gemini 不回传思维链
                Block::Image { mime_type, data } => {
                    // 内联 base64 → inlineData；远程/已编码 URL 交由上层内联处理
                    let (mime, payload) = match data.strip_prefix("data:") {
                        Some(rest) => match rest.split_once(";base64,") {
                            Some((m, p)) => (m.to_string(), p.to_string()),
                            None => (mime_type.clone(), data.clone()),
                        },
                        None => (mime_type.clone(), data.clone()),
                    };
                    parts.push(serde_json::json!({
                        "inlineData": { "mimeType": mime, "data": payload }
                    }));
                }
                Block::ToolUse { name, input, .. } => {
                    parts.push(serde_json::json!({
                        "functionCall": { "name": name, "args": input }
                    }));
                }
                Block::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let name = tool_names
                        .get(tool_use_id)
                        .cloned()
                        .unwrap_or_else(|| tool_use_id.clone());
                    let response = if *is_error {
                        serde_json::json!({ "error": content })
                    } else {
                        serde_json::json!({ "result": content })
                    };
                    parts.push(serde_json::json!({
                        "functionResponse": { "name": name, "response": response }
                    }));
                }
            }
        }

        if parts.is_empty() {
            continue;
        }
        if !pending_parts.is_empty() && role == pending_role {
            pending_parts.extend(parts);
        } else {
            if !pending_parts.is_empty() {
                contents.push(serde_json::json!({
                    "role": pending_role,
                    "parts": std::mem::take(&mut pending_parts)
                }));
            }
            pending_parts = parts;
            pending_role = role;
        }
    }
    if !pending_parts.is_empty() {
        contents.push(serde_json::json!({
            "role": pending_role,
            "parts": pending_parts
        }));
    }

    let mut body = serde_json::json!({
        "contents": contents
    });
    if let Some(n) = model.max_output {
        body["generationConfig"] = serde_json::json!({ "maxOutputTokens": n });
    }

    // 工具声明（Gemini 的 functionDeclarations 形态）
    if !tools.is_empty() {
        let declarations: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t["name"],
                    "description": t["description"],
                    "parameters": t["parameters"]
                })
            })
            .collect();
        body["tools"] = serde_json::json!([{ "functionDeclarations": declarations }]);
    }

    if let Some(sys) = system_prompt {
        body["systemInstruction"] = serde_json::json!({
            "parts": [{ "text": sys }]
        });
    }

    body
}

/// 组装 OpenAI / DeepSeek Chat Completions 的 messages 数组
/// （ToolResult 解构为独立 role: "tool" 消息，带图消息使用 parts 数组形态）
fn build_openai_messages(messages: &[ChatMessage], system_prompt: Option<&str>) -> Vec<serde_json::Value> {
    let mut openai_messages = Vec::new();
    // 组装 OpenAI 格式的 messages（解构 ToolResult 为 role: "tool"）
    if let Some(sys) = system_prompt {
        openai_messages.push(serde_json::json!({
            "role": "system",
            "content": sys
        }));
    }

    for msg in messages {
        match msg.role {
            Role::System => {
                let text = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        Block::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                openai_messages.push(serde_json::json!({
                    "role": "system",
                    "content": text
                }));
            }
            Role::Assistant => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                for b in &msg.content {
                    match b {
                        Block::Text { text } => text_parts.push(text.clone()),
                        Block::Thinking { .. } => {}
                        Block::ToolUse { id, name, input } => {
                            tool_calls.push(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": input.to_string()
                                }
                            }));
                        }
                        _ => {}
                    }
                }

                let mut obj = serde_json::json!({
                    "role": "assistant",
                });
                if !text_parts.is_empty() {
                    obj["content"] = serde_json::Value::String(text_parts.join("\n"));
                }
                if !tool_calls.is_empty() {
                    obj["tool_calls"] = serde_json::Value::Array(tool_calls);
                }
                openai_messages.push(obj);
            }
            Role::User => {
                // 若 User 消息全为 ToolResult，则解构为独立的 role: "tool" 消息
                let tool_results: Vec<&Block> = msg
                    .content
                    .iter()
                    .filter(|b| matches!(b, Block::ToolResult { .. }))
                    .collect();
                if !tool_results.is_empty() && tool_results.len() == msg.content.len() {
                    for b in tool_results {
                        if let Block::ToolResult {
                            tool_use_id, content, ..
                        } = b
                        {
                            openai_messages.push(serde_json::json!({
                                "role": "tool",
                                "tool_call_id": tool_use_id,
                                "content": content
                            }));
                        }
                    }
                } else {
                    // 带图消息使用 parts 数组形态，纯文本仍走字符串形态
                    let mut parts = Vec::new();
                    for b in &msg.content {
                        match b {
                            Block::Text { text } => parts.push(serde_json::json!({
                                "type": "text",
                                "text": text
                            })),
                            Block::Image { mime_type, data } => parts.push(serde_json::json!({
                                "type": "image_url",
                                "image_url": { "url": image_data_url(mime_type, data) }
                            })),
                            _ => {}
                        }
                    }
                    let has_image = msg.content.iter().any(|b| matches!(b, Block::Image { .. }));
                    if has_image {
                        openai_messages.push(serde_json::json!({
                            "role": "user",
                            "content": parts
                        }));
                    } else {
                        let text = parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("\n");
                        openai_messages.push(serde_json::json!({
                            "role": "user",
                            "content": text
                        }));
                    }
                }
            }
        }
    }

    openai_messages
}

/// 图片块 → 可直接投递的 data URL（已是 URL/data URI 时原样透传）
fn image_data_url(mime_type: &str, data: &str) -> String {
    if data.starts_with("http://") || data.starts_with("https://") || data.starts_with("data:") {
        data.to_string()
    } else {
        format!("data:{};base64,{}", mime_type, data)
    }
}

/// 统一 Provider 流式事件
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderStreamEvent {
    ThinkingDelta(String),
    TextDelta(String),
    ToolCall {
        id:    String,
        name:  String,
        input: serde_json::Value,
    },
    Usage {
        input_tokens:  usize,
        output_tokens: usize,
    },
    Done {
        stop_reason: StopReason,
    },
    Error(String),
}

/// 配置文件中的模型条目（ProviderConfig.models）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id:                String,
    #[serde(default = "model_entry_default_name")]
    pub name:              String,
    #[serde(default = "model_entry_default_context")]
    pub context_len:       usize,
    #[serde(default = "model_entry_default_true")]
    pub supports_vision:   bool,
    #[serde(default = "model_entry_default_true")]
    pub supports_thinking: bool,
    /// 最大输出 Token 数；None 时各协议使用内置默认
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:        Option<usize>,
    /// 推理等级 → 厂商自定义字符串；未配置的等级回退为等级名本身
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_map:     BTreeMap<String, String>,
    /// 支持的输入模态: text / image / video
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_types:       Vec<String>,
}

fn model_entry_default_name() -> String {
    String::new()
}
fn model_entry_default_context() -> usize {
    128_000
}
fn model_entry_default_true() -> bool {
    true
}
fn empty_json_object() -> serde_json::Value {
    serde_json::json!({})
}

/// 请求体预设：TOML 无法表达 null，反序列化时将缺失/null 规范为空对象
fn deserialize_json_body<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(serde_json::Value::Null) => serde_json::Value::Object(Default::default()),
        Some(v) => v,
    })
}

/// Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_type: String, // "anthropic" | "completion" | "response" | "google"
    pub base_url: String,
    pub api_key:  String,
    #[serde(default)]
    pub headers:  BTreeMap<String, String>,
    #[serde(default = "empty_json_object", deserialize_with = "deserialize_json_body")]
    pub body:     serde_json::Value,
    /// 可选模型清单：为空时按请求的 model id 动态合成 ModelConfig
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models:   Vec<ModelEntry>,
}

/// Model 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id:                String,
    pub name:              String,
    pub context_len:       usize,
    pub supports_vision:   bool,
    pub supports_thinking: bool,
    #[serde(default)]
    pub max_output:        Option<usize>,
    /// 实际下发给厂商的推理参数；由会话等级经 `reasoning_map` 解析后填入
    #[serde(default)]
    pub reasoning_effort:  String,
    /// 推理等级 → 厂商自定义字符串；未配置的等级回退为等级名本身
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_map:     BTreeMap<String, String>,
    #[serde(default)]
    pub headers:           BTreeMap<String, String>,
    #[serde(default = "empty_json_object", deserialize_with = "deserialize_json_body")]
    pub body:              serde_json::Value,
}

impl ProviderConfig {
    /// 解析 api_key，支持 `env:KEY_NAME` 语法
    pub fn resolved_api_key(&self) -> String {
        if let Some(env_var) = self.api_key.strip_prefix("env:") {
            std::env::var(env_var).unwrap_or_default()
        } else {
            self.api_key.clone()
        }
    }
}

impl ModelConfig {
    /// 将推理等级解析为发给厂商的字符串。
    ///
    /// - 定制了映射：取映射值；映射到空串表示「该等级下不下发该字段」
    /// - 未定制映射：直接用等级名本身（如 `medium`）
    ///
    /// 调用方保证 `level` 是 `REASONING_LEVELS` 中的具体等级（非空）。
    pub fn resolve_reasoning_effort(&self, level: &str) -> Option<String> {
        debug_assert!(!level.is_empty(), "reasoning level must be a concrete level");
        if self.reasoning_map.is_empty() {
            return Some(level.to_string());
        }
        match self.reasoning_map.get(level) {
            // 映射到空串 = 显式关闭该等级
            Some(mapped) if mapped.is_empty() => None,
            Some(mapped) => Some(mapped.clone()),
            // 未配置该等级的映射：按约定回退为等级名
            None => Some(level.to_string()),
        }
    }
}

/// 统一 LLM Provider Trait
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> Result<mpsc::Receiver<ProviderStreamEvent>>;
}

/// 通用 Multi-Provider 实现
pub struct UniversalProvider {
    client: reqwest::Client,
    config: ProviderConfig,
}

/// 进程级共享 HTTP 客户端：连接池与 TLS 会话在多次请求间复用。
/// 每次 `UniversalProvider::new` 都新建客户端会让 agent loop 的每一轮
/// 都重新建连、重新握手 TLS。
fn shared_http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .pool_idle_timeout(std::time::Duration::from_secs(90))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        })
        .clone()
}

impl UniversalProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            client: shared_http_client(),
            config,
        }
    }

    /// 针对各厂商组装并发送请求
    pub async fn send_stream(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> Result<mpsc::Receiver<ProviderStreamEvent>> {
        let (tx, rx) = mpsc::channel(128);

        match self.config.api_type.as_str() {
            "anthropic" => {
                self.stream_anthropic(messages, system_prompt, tools, model, tx)
                    .await?;
            }
            "completion" => {
                self.stream_openai_completion(messages, system_prompt, tools, model, tx)
                    .await?;
            }
            "response" => {
                self.stream_responses(messages, system_prompt, tools, model, tx)
                    .await?;
            }
            "google" => {
                self.stream_google(messages, system_prompt, tools, model, tx)
                    .await?;
            }
            other => {
                let _ = tx
                    .send(ProviderStreamEvent::Error(format!("Unsupported api_type: {}", other)))
                    .await;
            }
        }

        Ok(rx)
    }

    // -------------------------------------------------------------
    // Anthropic Messages API
    // -------------------------------------------------------------
    async fn stream_anthropic(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
        tx: mpsc::Sender<ProviderStreamEvent>,
    ) -> Result<()> {
        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));
        let api_key = self.config.resolved_api_key();

        // 组装 Anthropic 格式的 messages
        let mut anthropic_messages = Vec::new();
        for msg in messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => continue, // System 提示词通过顶层参数传递
            };

            let mut content_blocks = Vec::new();
            for b in &msg.content {
                match b {
                    Block::Text { text } => {
                        content_blocks.push(serde_json::json!({
                            "type": "text",
                            "text": text
                        }));
                    }
                    Block::Thinking { thinking } => {
                        content_blocks.push(serde_json::json!({
                            "type": "thinking",
                            "thinking": thinking
                        }));
                    }
                    Block::Image { mime_type, data } => {
                        content_blocks.push(serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": mime_type,
                                "data": data
                            }
                        }));
                    }
                    Block::ToolUse { id, name, input } => {
                        content_blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                    Block::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        content_blocks.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content,
                            "is_error": is_error
                        }));
                    }
                }
            }

            if !content_blocks.is_empty() {
                anthropic_messages.push(serde_json::json!({
                    "role": role,
                    "content": content_blocks
                }));
            }
        }

        // 构造三级覆盖后的 Body
        let mut body = serde_json::json!({
            "model": model.id,
            "max_tokens": model.max_output.unwrap_or(4096),
            "stream": true,
            "messages": anthropic_messages
        });

        if let Some(sys) = system_prompt {
            body["system"] = serde_json::Value::String(sys.to_string());
        }

        if !tools.is_empty() {
            let anthropic_tools: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t["name"],
                        "description": t["description"],
                        "input_schema": t["parameters"]
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(anthropic_tools);
        }

        deep_merge_json(&mut body, &self.config.body);
        deep_merge_json(&mut body, &model.body);

        let mut req = self
            .client
            .post(&url)
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        // 合并 Headers
        for (k, v) in &self.config.headers {
            req = req.header(k, v);
        }
        for (k, v) in &model.headers {
            req = req.header(k, v);
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Anthropic")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(ProviderStreamEvent::Error(format!(
                    "Anthropic API error {}: {}",
                    status, err_text
                )))
                .await;
            return Ok(());
        }

        // 启动后台解析 SSE
        tokio::spawn(async move {
            parse_anthropic_sse(resp.bytes_stream(), tx).await;
        });

        Ok(())
    }

    // -------------------------------------------------------------
    // OpenAI / DeepSeek Chat Completion API
    // -------------------------------------------------------------
    async fn stream_openai_completion(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
        tx: mpsc::Sender<ProviderStreamEvent>,
    ) -> Result<()> {
        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));
        let api_key = self.config.resolved_api_key();

        let openai_messages = build_openai_messages(messages, system_prompt);

        let mut body = serde_json::json!({
            "model": model.id,
            "stream": true,
            "messages": openai_messages
        });
        if let Some(n) = model.max_output {
            body["max_tokens"] = serde_json::json!(n);
        }
        if !model.reasoning_effort.is_empty() {
            body["reasoning_effort"] = serde_json::json!(model.reasoning_effort);
        }

        if !tools.is_empty() {
            let openai_tools: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t["name"],
                            "description": t["description"],
                            "parameters": t["parameters"]
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(openai_tools);
        }

        deep_merge_json(&mut body, &self.config.body);
        deep_merge_json(&mut body, &model.body);

        let mut req = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("content-type", "application/json");

        for (k, v) in &self.config.headers {
            req = req.header(k, v);
        }
        for (k, v) in &model.headers {
            req = req.header(k, v);
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .context("Failed to send request to OpenAI/DeepSeek")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(ProviderStreamEvent::Error(format!(
                    "API error {}: {}",
                    status, err_text
                )))
                .await;
            return Ok(());
        }

        tokio::spawn(async move {
            parse_openai_sse(resp.bytes_stream(), tx).await;
        });

        Ok(())
    }

    // -------------------------------------------------------------
    // Google Gemini API
    // -------------------------------------------------------------
    async fn stream_google(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
        tx: mpsc::Sender<ProviderStreamEvent>,
    ) -> Result<()> {
        let api_key = self.config.resolved_api_key();
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.config.base_url.trim_end_matches('/'),
            model.id,
            api_key
        );

        let mut body = build_gemini_body(messages, system_prompt, tools, model);

        deep_merge_json(&mut body, &self.config.body);
        deep_merge_json(&mut body, &model.body);

        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(ProviderStreamEvent::Error(format!(
                    "Gemini API error {}: {}",
                    status, err_text
                )))
                .await;
            return Ok(());
        }

        tokio::spawn(async move {
            parse_gemini_sse(resp.bytes_stream(), tx).await;
        });
        Ok(())
    }
    async fn stream_responses(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
        tx: mpsc::Sender<ProviderStreamEvent>,
    ) -> Result<()> {
        let url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));
        let api_key = self.config.resolved_api_key();

        // 组装 Responses input 条目（ToolResult 解构为 function_call_output）
        let mut input = Vec::new();
        if let Some(sys) = system_prompt {
            input.push(serde_json::json!({
                "role": "system",
                "content": [{ "type": "input_text", "text": sys }]
            }));
        }

        for msg in messages {
            match msg.role {
                Role::System => {
                    let text = msg
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            Block::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    input.push(serde_json::json!({
                        "role": "system",
                        "content": [{ "type": "input_text", "text": text }]
                    }));
                }
                Role::Assistant => {
                    let mut text_parts = Vec::new();
                    for b in &msg.content {
                        match b {
                            Block::Text { text } => text_parts.push(text.clone()),
                            Block::Thinking { .. } => {}
                            Block::ToolUse { id, name, input: args } => {
                                input.push(serde_json::json!({
                                    "type": "function_call",
                                    "call_id": id,
                                    "name": name,
                                    "arguments": args.to_string()
                                }));
                            }
                            _ => {}
                        }
                    }
                    if !text_parts.is_empty() {
                        input.push(serde_json::json!({
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": text_parts.join("\n") }]
                        }));
                    }
                }
                Role::User => {
                    let tool_results: Vec<&Block> = msg
                        .content
                        .iter()
                        .filter(|b| matches!(b, Block::ToolResult { .. }))
                        .collect();
                    if !tool_results.is_empty() && tool_results.len() == msg.content.len() {
                        for b in tool_results {
                            if let Block::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } = b
                            {
                                let output = if *is_error {
                                    serde_json::json!({ "error": content }).to_string()
                                } else {
                                    content.clone()
                                };
                                input.push(serde_json::json!({
                                    "type": "function_call_output",
                                    "call_id": tool_use_id,
                                    "output": output
                                }));
                            }
                        }
                    } else {
                        let mut content_parts = Vec::new();
                        for b in &msg.content {
                            match b {
                                Block::Text { text } => content_parts.push(serde_json::json!({
                                    "type": "input_text",
                                    "text": text
                                })),
                                Block::Image { mime_type, data } => {
                                    content_parts.push(serde_json::json!({
                                        "type": "input_image",
                                        "image_url": image_data_url(mime_type, data)
                                    }));
                                }
                                _ => {}
                            }
                        }
                        if content_parts.is_empty() {
                            content_parts.push(serde_json::json!({ "type": "input_text", "text": "" }));
                        }
                        input.push(serde_json::json!({
                            "role": "user",
                            "content": content_parts
                        }));
                    }
                }
            }
        }

        let mut body = serde_json::json!({
            "model": model.id,
            "stream": true,
            "input": input
        });
        if let Some(n) = model.max_output {
            body["max_output_tokens"] = serde_json::json!(n);
        }
        if !model.reasoning_effort.is_empty() {
            body["reasoning"] = serde_json::json!({ "effort": model.reasoning_effort });
        }

        if !tools.is_empty() {
            let responses_tools: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t["name"],
                        "description": t["description"],
                        "parameters": t["parameters"]
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(responses_tools);
        }

        deep_merge_json(&mut body, &self.config.body);
        deep_merge_json(&mut body, &model.body);

        let mut req = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("content-type", "application/json");

        for (k, v) in &self.config.headers {
            req = req.header(k, v);
        }
        for (k, v) in &model.headers {
            req = req.header(k, v);
        }

        let resp = req
            .json(&body)
            .send()
            .await
            .context("Failed to send request to Responses API")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx
                .send(ProviderStreamEvent::Error(format!(
                    "Responses API error {}: {}",
                    status, err_text
                )))
                .await;
            return Ok(());
        }

        tokio::spawn(async move {
            parse_responses_sse(resp.bytes_stream(), tx).await;
        });

        Ok(())
    }
}

// =========================================================================
// SSE 解析器集合 (手写状态机)
// =========================================================================

/// Anthropic SSE 解析
pub async fn parse_anthropic_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = String::new();
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_args = String::new();
    let mut end_stop_reason = StopReason::EndTurn;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ProviderStreamEvent::Error(e.to_string())).await;
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let message = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            let mut event_type = "";
            let mut data = "";

            for line in message.lines() {
                if let Some(ev) = line.strip_prefix("event:") {
                    event_type = ev.trim();
                } else if let Some(d) = line.strip_prefix("data:") {
                    data = d.trim();
                }
            }

            if data.is_empty() {
                continue;
            }

            let val: serde_json::Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match event_type {
                "message_start" => {
                    if let Some(usage) = val.get("message").and_then(|m| m.get("usage")) {
                        let input_tokens = usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize;
                        let _ = tx
                            .send(ProviderStreamEvent::Usage {
                                input_tokens,
                                output_tokens: 0,
                            })
                            .await;
                    }
                }
                "content_block_start" => {
                    if let Some(cb) = val.get("content_block") {
                        let block_type = cb.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if block_type == "tool_use" {
                            current_tool_id = cb
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            current_tool_name = cb
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            current_tool_args.clear();
                        }
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = val.get("delta") {
                        let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if delta_type == "text_delta" {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                let _ = tx
                                    .send(ProviderStreamEvent::TextDelta(text.to_string()))
                                    .await;
                            }
                        } else if delta_type == "thinking_delta" {
                            if let Some(th) = delta.get("thinking").and_then(|v| v.as_str()) {
                                let _ = tx
                                    .send(ProviderStreamEvent::ThinkingDelta(th.to_string()))
                                    .await;
                            }
                        } else if delta_type == "input_json_delta" {
                            if let Some(partial) = delta.get("partial_json").and_then(|v| v.as_str()) {
                                current_tool_args.push_str(partial);
                            }
                        }
                    }
                }
                "content_block_stop" => {
                    if !current_tool_id.is_empty() {
                        let parsed_args = match serde_json::from_str(&current_tool_args) {
                            Ok(v) => v,
                            Err(e) => serde_json::json!({
                                "_parse_error": e.to_string(),
                                "_raw": current_tool_args
                            }),
                        };
                        let _ = tx
                            .send(ProviderStreamEvent::ToolCall {
                                id:    std::mem::take(&mut current_tool_id),
                                name:  std::mem::take(&mut current_tool_name),
                                input: parsed_args,
                            })
                            .await;
                        current_tool_args.clear();
                    }
                }
                "message_delta" => {
                    if let Some(sr) = val.pointer("/delta/stop_reason").and_then(|v| v.as_str()) {
                        end_stop_reason = match sr {
                            "tool_use" => StopReason::ToolUse,
                            "max_tokens" => StopReason::MaxTokens,
                            _ => StopReason::EndTurn,
                        };
                    }
                    if let Some(usage) = val.get("usage") {
                        let output_tokens = usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as usize;
                        let _ = tx
                            .send(ProviderStreamEvent::Usage {
                                input_tokens: 0,
                                output_tokens,
                            })
                            .await;
                    }
                }
                "message_stop" => {
                    let _ = tx
                        .send(ProviderStreamEvent::Done {
                            stop_reason: end_stop_reason,
                        })
                        .await;
                }
                _ => {}
            }
        }
    }
}

/// OpenAI / DeepSeek SSE 解析
/// 从 completion 流的 `delta` 中提取思维链增量。
///
/// 字段名随上游而异：DeepSeek 官方用 `reasoning_content`，OpenRouter 及多数
/// 聚合网关用 `reasoning`，另有上游只在 `reasoning_details[]` 里给文本。
/// 同一段内容常被上述字段同时携带（内容一致），故按优先级取其一，避免重复拼接。
fn extract_reasoning_delta(delta: &serde_json::Value) -> Option<String> {
    for key in ["reasoning_content", "reasoning"] {
        if let Some(t) = delta.get(key).and_then(|v| v.as_str()) {
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    let parts: Vec<&str> = delta
        .get("reasoning_details")?
        .as_array()?
        .iter()
        .filter_map(|d| d.get("text").and_then(|v| v.as_str()))
        .collect();
    if parts.is_empty() { None } else { Some(parts.concat()) }
}

pub async fn parse_openai_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = String::new();

    struct ToolCallAcc {
        id:        String,
        name:      String,
        arguments: String,
    }
    let mut tool_calls: BTreeMap<usize, ToolCallAcc> = BTreeMap::new();
    let mut done_sent = false;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ProviderStreamEvent::Error(e.to_string())).await;
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let message = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in message.lines() {
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    // finish_reason 分片已发出 Done 时不得再次覆盖 stop_reason
                    if !done_sent {
                        let has_tools = !tool_calls.is_empty();
                        for (_, tc) in std::mem::take(&mut tool_calls) {
                            let parsed = match serde_json::from_str(&tc.arguments) {
                                Ok(v) => v,
                                Err(e) => serde_json::json!({
                                    "_parse_error": e.to_string(),
                                    "_raw": tc.arguments
                                }),
                            };
                            let _ = tx
                                .send(ProviderStreamEvent::ToolCall {
                                    id:    tc.id,
                                    name:  tc.name,
                                    input: parsed,
                                })
                                .await;
                        }
                        let _ = tx
                            .send(ProviderStreamEvent::Done {
                                stop_reason: if has_tools {
                                    StopReason::ToolUse
                                } else {
                                    StopReason::EndTurn
                                },
                            })
                            .await;
                    }
                    return;
                }

                let val: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // 统计 Usage
                if let Some(usage) = val.get("usage") {
                    let input_tokens = usage
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let output_tokens = usage
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let _ = tx
                        .send(ProviderStreamEvent::Usage {
                            input_tokens,
                            output_tokens,
                        })
                        .await;
                }

                if let Some(choice) = val.get("choices").and_then(|c| c.get(0)) {
                    if let Some(delta) = choice.get("delta") {
                        // 1. 思维链增量
                        if let Some(thinking) = extract_reasoning_delta(delta) {
                            let _ = tx.send(ProviderStreamEvent::ThinkingDelta(thinking)).await;
                        }

                        // 2. 普通文本内容 content
                        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                let _ = tx
                                    .send(ProviderStreamEvent::TextDelta(text.to_string()))
                                    .await;
                            }
                        }

                        // 3. 工具切片累计 tool_calls
                        if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc_item in tc_array {
                                let idx = tc_item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                let entry = tool_calls.entry(idx).or_insert_with(|| ToolCallAcc {
                                    id:        String::new(),
                                    name:      String::new(),
                                    arguments: String::new(),
                                });

                                if let Some(id) = tc_item.get("id").and_then(|v| v.as_str()) {
                                    entry.id = id.to_string();
                                }
                                if let Some(func) = tc_item.get("function") {
                                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                        entry.name.push_str(name);
                                    }
                                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                        entry.arguments.push_str(args);
                                    }
                                }
                            }
                        }
                    }

                    if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                        let stop_reason = match finish_reason {
                            "tool_calls" => StopReason::ToolUse,
                            "length" => StopReason::MaxTokens,
                            _ => StopReason::EndTurn,
                        };

                        // 若在 finish_reason 处触发，推送累积工具调用
                        for (_, tc) in std::mem::take(&mut tool_calls) {
                            let parsed = match serde_json::from_str(&tc.arguments) {
                                Ok(v) => v,
                                Err(e) => serde_json::json!({
                                    "_parse_error": e.to_string(),
                                    "_raw": tc.arguments
                                }),
                            };
                            let _ = tx
                                .send(ProviderStreamEvent::ToolCall {
                                    id:    tc.id,
                                    name:  tc.name,
                                    input: parsed,
                                })
                                .await;
                        }

                        done_sent = true;
                        let _ = tx.send(ProviderStreamEvent::Done { stop_reason }).await;
                    }
                }
            }
        }
    }
}

/// OpenAI Responses API SSE 解析（api_type = "response"）
pub async fn parse_responses_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = String::new();

    // 累积中的 function_call 输出项：output_index -> (call_id, name, arguments)
    let mut current_call: Option<(String, String, String)> = None;
    let mut saw_done = false;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ProviderStreamEvent::Error(e.to_string())).await;
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let message = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in message.lines() {
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();

                let val: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let event_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match event_type {
                    // 文本增量
                    "response.output_text.delta" => {
                        if let Some(text) = val.get("delta").and_then(|v| v.as_str()) {
                            let _ = tx
                                .send(ProviderStreamEvent::TextDelta(text.to_string()))
                                .await;
                        }
                    }
                    // 思维链增量（摘要与全文两种事件形态）
                    "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                        if let Some(th) = val.get("delta").and_then(|v| v.as_str()) {
                            let _ = tx
                                .send(ProviderStreamEvent::ThinkingDelta(th.to_string()))
                                .await;
                        }
                    }
                    // function_call 输出项开始（携带完整 call_id 与 name）
                    "response.output_item.added" => {
                        if val
                            .get("item")
                            .and_then(|i| i.get("type"))
                            .and_then(|v| v.as_str())
                            == Some("function_call")
                        {
                            let call_id = val
                                .pointer("/item/call_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = val
                                .pointer("/item/name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            current_call = Some((call_id, name, String::new()));
                        }
                    }
                    // function_call 参数流式切片
                    "response.function_call_arguments.delta" => {
                        if let Some((_, _, args)) = current_call.as_mut() {
                            if let Some(partial) = val.get("delta").and_then(|v| v.as_str()) {
                                args.push_str(partial);
                            }
                        }
                    }
                    // function_call 参数流结束，产出完整 ToolCall
                    "response.function_call_arguments.done" => {
                        if let Some((call_id, name, args)) = current_call.take() {
                            // done 事件若携带完整 arguments 则以服务端值为准
                            let final_args = val
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or(&args)
                                .to_string();
                            let parsed = match serde_json::from_str(&final_args) {
                                Ok(v) => v,
                                Err(e) => serde_json::json!({
                                    "_parse_error": e.to_string(),
                                    "_raw": final_args
                                }),
                            };
                            let _ = tx
                                .send(ProviderStreamEvent::ToolCall {
                                    id: call_id,
                                    name,
                                    input: parsed,
                                })
                                .await;
                        }
                    }
                    // 整个响应完成（含 usage 与 output 数组）
                    "response.completed" => {
                        saw_done = true;
                        if let Some(usage) = val.pointer("/response/usage") {
                            let input_tokens = usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            let output_tokens = usage
                                .get("output_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            let _ = tx
                                .send(ProviderStreamEvent::Usage {
                                    input_tokens,
                                    output_tokens,
                                })
                                .await;
                        }
                        let stop_reason = val
                            .pointer("/response/incomplete_details/reason")
                            .and_then(|v| v.as_str())
                            .map(|r| match r {
                                "max_output_tokens" => StopReason::MaxTokens,
                                _ => StopReason::EndTurn,
                            })
                            .unwrap_or(StopReason::EndTurn);
                        let _ = tx.send(ProviderStreamEvent::Done { stop_reason }).await;
                    }
                    // 服务端错误事件
                    "response.failed" | "error" => {
                        let msg = val
                            .pointer("/response/error/message")
                            .or_else(|| val.get("message"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown Responses API error")
                            .to_string();
                        let _ = tx.send(ProviderStreamEvent::Error(msg)).await;
                    }
                    _ => {}
                }
            }
        }
    }

    // 流中断且未收到 completed：强制收尾，避免调用方悬挂等待
    if !saw_done {
        let _ = tx
            .send(ProviderStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
            })
            .await;
    }
}

/// Google Gemini SSE 解析
pub async fn parse_gemini_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = String::new();
    // Gemini 的 functionCall 不携带调用 ID，自行合成稳定标识供回传时配对
    let mut call_seq = 0usize;
    let mut tool_calls = 0usize;

    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(ProviderStreamEvent::Error(e.to_string())).await;
                return;
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find("\n\n") {
            let message = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();

            for line in message.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let val: serde_json::Value = match serde_json::from_str(data.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(candidates) = val.get("candidates").and_then(|v| v.as_array()) {
                    for cand in candidates {
                        if let Some(parts) = cand
                            .get("content")
                            .and_then(|c| c.get("parts"))
                            .and_then(|p| p.as_array())
                        {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        let _ = tx
                                            .send(ProviderStreamEvent::TextDelta(text.to_string()))
                                            .await;
                                    }
                                }
                                // 函数调用：Gemini 以完整对象下发，直接转换为统一事件
                                if let Some(call) = part.get("functionCall") {
                                    let name = call
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    if name.is_empty() {
                                        continue;
                                    }
                                    let args = call
                                        .get("args")
                                        .cloned()
                                        .unwrap_or_else(|| serde_json::json!({}));
                                    call_seq += 1;
                                    tool_calls += 1;
                                    let _ = tx
                                        .send(ProviderStreamEvent::ToolCall {
                                            id: format!("gemini_call_{}", call_seq),
                                            name,
                                            input: args,
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                }

                if let Some(usage) = val.get("usageMetadata") {
                    let input_tokens = usage
                        .get("promptTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let output_tokens = usage
                        .get("candidatesTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let _ = tx
                        .send(ProviderStreamEvent::Usage {
                            input_tokens,
                            output_tokens,
                        })
                        .await;
                }
            }
        }
    }

    let _ = tx
        .send(ProviderStreamEvent::Done {
            stop_reason: if tool_calls > 0 {
                StopReason::ToolUse
            } else {
                StopReason::EndTurn
            },
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deep_merge_json() {
        let mut target = serde_json::json!({
            "a": 1,
            "nested": {
                "x": "foo",
                "y": 10
            }
        });
        let source = serde_json::json!({
            "b": 2,
            "nested": {
                "y": 20,
                "z": "bar"
            }
        });
        deep_merge_json(&mut target, &source);

        assert_eq!(target["a"], 1);
        assert_eq!(target["b"], 2);
        assert_eq!(target["nested"]["x"], "foo");
        assert_eq!(target["nested"]["y"], 20);
        assert_eq!(target["nested"]["z"], "bar");
    }

    #[test]
    fn test_null_body_normalizes_to_object_and_toml_roundtrip() {
        // 前端 PUT 可能带回 body: null；TOML 不支持 null，必须规范为空对象后再序列化
        let missing: ProviderConfig =
            serde_json::from_str(r#"{"api_type":"completion","base_url":"https://api.example.com/v1","api_key":"k"}"#)
                .unwrap();
        assert_eq!(missing.body, serde_json::json!({}));

        let null_cfg: ProviderConfig = serde_json::from_str(
            r#"{"api_type":"completion","base_url":"https://api.example.com/v1","api_key":"k","body":null}"#,
        )
        .unwrap();
        assert_eq!(null_cfg.body, serde_json::json!({}));

        let toml_str = toml::to_string_pretty(&null_cfg).unwrap();
        let back: ProviderConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(back.body, serde_json::json!({}));

        // 非空预设原样保留
        let rich: ProviderConfig = serde_json::from_str(
            r#"{"api_type":"completion","base_url":"https://api.example.com/v1","api_key":"k","body":{"temperature":0.7}}"#,
        )
        .unwrap();
        assert_eq!(rich.body["temperature"], 0.7);
    }

    #[test]
    fn test_image_data_url() {
        assert_eq!(image_data_url("image/png", "AAAA"), "data:image/png;base64,AAAA");
        assert_eq!(image_data_url("image/png", "https://x/y.png"), "https://x/y.png");
        assert_eq!(
            image_data_url("image/png", "data:image/png;base64,AAAA"),
            "data:image/png;base64,AAAA"
        );
    }

    fn user_with_image() -> ChatMessage {
        ChatMessage {
            id:         "m1".into(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![
                Block::Text { text: "look".into() },
                Block::Image {
                    mime_type: "image/png".into(),
                    data:      "QUJD".into(),
                },
            ],
            created_at: 0,
        }
    }

    /// OpenAI 兼容路径此前只 join 文本块，图片被静默丢弃。
    #[test]
    fn test_openai_messages_keep_images() {
        let msgs = build_openai_messages(&[user_with_image()], None);
        let content = &msgs[0]["content"];
        assert!(content.is_array(), "image messages must use parts form: {}", content);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,QUJD");
    }

    /// Gemini 此前完全忽略工具声明与图片，且不解析 functionCall。
    #[test]
    fn test_gemini_body_maps_tools_images_and_calls() {
        let assistant_call = ChatMessage {
            id:         "a1".into(),
            parent_id:  Some("m1".into()),
            role:       Role::Assistant,
            content:    vec![Block::ToolUse {
                id:    "call_1".into(),
                name:  "read".into(),
                input: serde_json::json!({ "path": "x.rs" }),
            }],
            created_at: 0,
        };
        let tool_result = ChatMessage {
            id:         "r1".into(),
            parent_id:  Some("a1".into()),
            role:       Role::User,
            content:    vec![Block::ToolResult {
                tool_use_id: "call_1".into(),
                content:     "file body".into(),
                is_error:    false,
            }],
            created_at: 0,
        };
        let tools = vec![serde_json::json!({
            "name": "read",
            "description": "Read a file",
            "parameters": { "type": "object" }
        })];

        let body = build_gemini_body(
            &[user_with_image(), assistant_call, tool_result],
            Some("sys"),
            &tools,
            &ModelConfig {
                id:                "gemini-pro".into(),
                name:              "gemini-pro".into(),
                context_len:       1000,
                supports_vision:   true,
                supports_thinking: true,
                max_output:        Some(256),
                reasoning_effort:  String::new(),
                reasoning_map:     BTreeMap::new(),
                headers:           BTreeMap::new(),
                body:              serde_json::json!({}),
            },
        );

        // 工具声明必须下发
        assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "read");
        // 图片 → inlineData（剥离 data URI 前缀）
        assert_eq!(body["contents"][0]["parts"][1]["inlineData"]["data"], "QUJD");
        // assistant 工具调用 → functionCall
        assert_eq!(body["contents"][1]["parts"][0]["functionCall"]["name"], "read");
        // 工具回执 → functionResponse，且使用函数名而非 id
        assert_eq!(body["contents"][2]["parts"][0]["functionResponse"]["name"], "read");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"]["result"],
            "file body"
        );
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "sys");
    }

    /// Gemini 的 functionCall 必须转换为统一 ToolCall，并把 stop_reason 提升为 ToolUse，
    /// 否则 agent loop 会跳过工具执行退化成纯聊天。
    #[tokio::test]
    async fn test_parse_gemini_sse_function_call() {
        let sse_data = concat!(
            r#"data: {"candidates":[{"content":{"parts":[{"text":"calling"}]}}]}"#,
            "\n\n",
            r#"data: {"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"a.rs"}}}]}}]}"#,
            "\n\n",
            r#"data: {"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3}}"#,
            "\n\n",
        );

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(16);
        parse_gemini_sse(stream, tx).await;

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events[0], ProviderStreamEvent::TextDelta("calling".into()));
        assert_eq!(
            events[1],
            ProviderStreamEvent::ToolCall {
                id:    "gemini_call_1".into(),
                name:  "read".into(),
                input: serde_json::json!({ "path": "a.rs" }),
            }
        );
        assert_eq!(
            events[2],
            ProviderStreamEvent::Usage {
                input_tokens:  7,
                output_tokens: 3,
            }
        );
        assert_eq!(
            events[3],
            ProviderStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
            }
        );
    }

    /// 多数聚合网关（OpenRouter 形态）把思维链放在 `reasoning` 而非
    /// `reasoning_content`；两者必须都能进入 ThinkingDelta。
    #[tokio::test]
    async fn test_parse_openai_sse_reasoning_field_variants() {
        let sse_data = "data: {\"choices\":[{\"delta\":{\"reasoning\":\"第一段\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"reasoning\":\"第二段\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"答复\"}}]}\n\n\
data: [DONE]\n\n";

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);
        tokio::spawn(async move {
            parse_openai_sse(stream, tx).await;
        });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events[0], ProviderStreamEvent::ThinkingDelta("第一段".into()));
        assert_eq!(events[1], ProviderStreamEvent::ThinkingDelta("第二段".into()));
        assert_eq!(events[2], ProviderStreamEvent::TextDelta("答复".into()));
    }

    /// 同一段内容同时出现在 `reasoning` 与 `reasoning_details` 时不得重复拼接；
    /// 仅有 `reasoning_details` 时须能取到文本。
    #[tokio::test]
    async fn test_parse_openai_sse_reasoning_details_fallback() {
        let sse_data = "data: {\"choices\":[{\"delta\":{\"reasoning\":\"一次\",\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"一次\",\"index\":0}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"仅详情\",\"index\":0}]}}]}\n\n\
data: [DONE]\n\n";

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);
        tokio::spawn(async move {
            parse_openai_sse(stream, tx).await;
        });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events[0], ProviderStreamEvent::ThinkingDelta("一次".into()), "不得重复");
        assert_eq!(
            events[1],
            ProviderStreamEvent::ThinkingDelta("仅详情".into()),
            "无 reasoning 时回退到 reasoning_details"
        );
    }

    #[tokio::test]
    async fn test_parse_openai_sse_stream() {
        let sse_data = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"I think...\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"world!\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"src/lib.rs\\\"}\"}}]}}]}\n\n\
data: [DONE]\n\n";

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);

        tokio::spawn(async move {
            parse_openai_sse(stream, tx).await;
        });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }

        assert_eq!(events[0], ProviderStreamEvent::ThinkingDelta("I think...".into()));
        assert_eq!(events[1], ProviderStreamEvent::TextDelta("Hello ".into()));
        assert_eq!(events[2], ProviderStreamEvent::TextDelta("world!".into()));
        assert_eq!(
            events[3],
            ProviderStreamEvent::ToolCall {
                id:    "call_1".into(),
                name:  "read".into(),
                input: serde_json::json!({ "path": "src/lib.rs" }),
            }
        );
        // 累积的工具调用随收尾下发；存在未消费的 tool_calls 时 stop_reason 必须为 ToolUse，
        // 否则 Agent Loop 会跳过工具执行
        assert_eq!(
            events[4],
            ProviderStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
            }
        );
    }

    #[tokio::test]
    async fn test_parse_openai_sse_done_does_not_override_finish_reason() {
        // 服务端先推 finish_reason: tool_calls，再推 [DONE]：只允许一次 Done，且不得被覆盖为 EndTurn
        let sse_data = concat!(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{}"}}]}}]}"#,
            "\n\n",
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "\n\n",
            "data: [DONE]\n\n",
        );

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);
        tokio::spawn(async move { parse_openai_sse(stream, tx).await });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[1],
            ProviderStreamEvent::Done {
                stop_reason: StopReason::ToolUse,
            }
        ));
    }

    #[tokio::test]
    async fn test_parse_responses_sse_stream() {
        let sse_data = concat!(
            r#"data: {"type":"response.reasoning_summary_text.delta","delta":"thinking hard"}"#,
            "\n\n",
            r#"data: {"type":"response.output_text.delta","delta":"Hello "}"#,
            "\n\n",
            r#"data: {"type":"response.output_text.delta","delta":"world"}"#,
            "\n\n",
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_9","name":"shell"}}"#,
            "\n\n",
            r#"data: {"type":"response.function_call_arguments.delta","delta":"{\"cmd\":"}"#,
            "\n\n",
            r#"data: {"type":"response.function_call_arguments.delta","delta":"\"ls\"}"}"#,
            "\n\n",
            r#"data: {"type":"response.function_call_arguments.done","arguments":"{\"cmd\":\"ls -la\"}"}"#,
            "\n\n",
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":42},"incomplete_details":null}}"#,
            "\n\n",
        );

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);

        tokio::spawn(async move {
            parse_responses_sse(stream, tx).await;
        });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }

        assert_eq!(events[0], ProviderStreamEvent::ThinkingDelta("thinking hard".into()));
        assert_eq!(events[1], ProviderStreamEvent::TextDelta("Hello ".into()));
        assert_eq!(events[2], ProviderStreamEvent::TextDelta("world".into()));
        assert_eq!(
            events[3],
            ProviderStreamEvent::ToolCall {
                id:    "call_9".into(),
                name:  "shell".into(),
                input: serde_json::json!({ "cmd": "ls -la" }),
            }
        );
        assert_eq!(
            events[4],
            ProviderStreamEvent::Usage {
                input_tokens:  100,
                output_tokens: 42,
            }
        );
        assert_eq!(
            events[5],
            ProviderStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
            }
        );
    }

    #[tokio::test]
    async fn test_parse_responses_sse_incomplete_and_error() {
        // max_output_tokens 不完整结束 → MaxTokens
        let sse_max = concat!(
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":5},"incomplete_details":{"reason":"max_output_tokens"}}}"#,
            "\n\n",
        );
        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_max))]);
        let (tx, mut rx) = mpsc::channel(4);
        parse_responses_sse(stream, tx).await;
        let mut ev = rx.recv().await.unwrap();
        assert_eq!(
            ev,
            ProviderStreamEvent::Usage {
                input_tokens:  1,
                output_tokens: 5,
            }
        );
        ev = rx.recv().await.unwrap();
        assert_eq!(
            ev,
            ProviderStreamEvent::Done {
                stop_reason: StopReason::MaxTokens,
            }
        );
        assert!(rx.recv().await.is_none());

        // response.failed → Error 事件
        let sse_fail = concat!(
            r#"data: {"type":"response.failed","response":{"error":{"message":"boom"}}}"#,
            "\n\n",
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":0,"output_tokens":0},"incomplete_details":null}}"#,
            "\n\n",
        );
        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_fail))]);
        let (tx, mut rx) = mpsc::channel(4);
        parse_responses_sse(stream, tx).await;
        assert_eq!(rx.recv().await.unwrap(), ProviderStreamEvent::Error("boom".into()));
        // response.failed → Error 事件，随后的 completed 事件先带 Usage 再收尾
        assert_eq!(
            rx.recv().await.unwrap(),
            ProviderStreamEvent::Usage {
                input_tokens:  0,
                output_tokens: 0,
            }
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            ProviderStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
            }
        );
    }

    // ---------- 推理等级映射 ----------

    fn model_with(map: &[(&str, &str)]) -> ModelConfig {
        let mut reasoning_map = BTreeMap::new();
        for (k, v) in map {
            reasoning_map.insert(k.to_string(), v.to_string());
        }
        ModelConfig {
            id: "m".into(),
            name: "m".into(),
            context_len: 1000,
            supports_vision: true,
            supports_thinking: true,
            max_output: None,
            reasoning_effort: String::new(),
            reasoning_map,
            headers: BTreeMap::new(),
            body: serde_json::json!({}),
        }
    }

    /// 未定制映射：等级名直接下发。
    #[test]
    fn test_reasoning_passthrough_without_map() {
        let m = model_with(&[]);
        assert_eq!(m.resolve_reasoning_effort("medium").as_deref(), Some("medium"));
        assert_eq!(m.resolve_reasoning_effort("ultra").as_deref(), Some("ultra"));
    }

    /// 定制映射：命中取自定义字符串，未命中回退等级名。
    #[test]
    fn test_reasoning_map_overrides_and_falls_back() {
        let m = model_with(&[("low", "think-low"), ("high", "think-ultra")]);
        assert_eq!(m.resolve_reasoning_effort("low").as_deref(), Some("think-low"));
        assert_eq!(m.resolve_reasoning_effort("high").as_deref(), Some("think-ultra"));
        // 未配置映射的等级按等级名下发，不因存在映射表而丢失
        assert_eq!(m.resolve_reasoning_effort("medium").as_deref(), Some("medium"));
    }

    /// 映射到空串 = 该等级显式关闭，不下发字段。
    #[test]
    fn test_reasoning_map_empty_disables() {
        let m = model_with(&[("minimal", ""), ("ultra", "  ")]);
        assert!(m.resolve_reasoning_effort("minimal").is_none());
        // 空白视为有值的自定义字符串（不清洗，原样透传）
        assert_eq!(m.resolve_reasoning_effort("ultra").as_deref(), Some("  "));
    }
}
