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

/// 统一 Provider 流式事件
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderStreamEvent {
    ThinkingDelta(String),
    TextDelta(String),
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    Usage {
        input_tokens: usize,
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
    pub id: String,
    #[serde(default = "model_entry_default_name")]
    pub name: String,
    #[serde(default = "model_entry_default_context")]
    pub context_len: usize,
    #[serde(default = "model_entry_default_true")]
    pub supports_vision: bool,
    #[serde(default = "model_entry_default_true")]
    pub supports_thinking: bool,
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

/// Provider 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub api_type: String, // "anthropic" | "completion" | "response" | "google"
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: serde_json::Value,
    /// 可选模型清单；为空时按请求的 model id 动态合成 ModelConfig
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelEntry>,
}

/// Model 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub id: String,
    pub name: String,
    pub context_len: usize,
    pub supports_vision: bool,
    pub supports_thinking: bool,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: serde_json::Value,
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

impl UniversalProvider {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            client: reqwest::Client::new(),
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
                self.stream_anthropic(messages, system_prompt, tools, model, tx).await?;
            }
            "completion" => {
                self.stream_openai_completion(messages, system_prompt, tools, model, tx).await?;
            }
            "response" => {
                self.stream_responses(messages, system_prompt, tools, model, tx).await?;
            }
            "google" => {
                self.stream_google(messages, system_prompt, tools, model, tx).await?;
            }
            other => {
                let _ = tx.send(ProviderStreamEvent::Error(format!("Unsupported api_type: {}", other))).await;
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
                    Block::ToolResult { tool_use_id, content, is_error } => {
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
            "max_tokens": 4096,
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

        let resp = req.json(&body).send().await.context("Failed to send request to Anthropic")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx.send(ProviderStreamEvent::Error(format!("Anthropic API error {}: {}", status, err_text))).await;
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

        // 组装 OpenAI 格式的 messages（解构 ToolResult 为 role: "tool"）
        let mut openai_messages = Vec::new();
        if let Some(sys) = system_prompt {
            openai_messages.push(serde_json::json!({
                "role": "system",
                "content": sys
            }));
        }

        for msg in messages {
            match msg.role {
                Role::System => {
                    let text = msg.content.iter().filter_map(|b| match b {
                        Block::Text { text } => Some(text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n");
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
                    let tool_results: Vec<&Block> = msg.content.iter().filter(|b| matches!(b, Block::ToolResult { .. })).collect();
                    if !tool_results.is_empty() && tool_results.len() == msg.content.len() {
                        for b in tool_results {
                            if let Block::ToolResult { tool_use_id, content, .. } = b {
                                openai_messages.push(serde_json::json!({
                                    "role": "tool",
                                    "tool_call_id": tool_use_id,
                                    "content": content
                                }));
                            }
                        }
                    } else {
                        let text = msg.content.iter().filter_map(|b| match b {
                            Block::Text { text } => Some(text.as_str()),
                            _ => None,
                        }).collect::<Vec<_>>().join("\n");
                        openai_messages.push(serde_json::json!({
                            "role": "user",
                            "content": text
                        }));
                    }
                }
            }
        }

        let mut body = serde_json::json!({
            "model": model.id,
            "stream": true,
            "messages": openai_messages
        });

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

        let resp = req.json(&body).send().await.context("Failed to send request to OpenAI/DeepSeek")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx.send(ProviderStreamEvent::Error(format!("API error {}: {}", status, err_text))).await;
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
        _tools: &[serde_json::Value],
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

        let mut contents = Vec::new();
        for msg in messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "model",
                Role::System => continue,
            };
            let text = msg.content.iter().filter_map(|b| match b {
                Block::Text { text } => Some(text.as_str()),
                _ => None,
            }).collect::<Vec<_>>().join("\n");

            contents.push(serde_json::json!({
                "role": role,
                "parts": [{ "text": text }]
            }));
        }

        let mut body = serde_json::json!({
            "contents": contents
        });

        if let Some(sys) = system_prompt {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{ "text": sys }]
            });
        }

        deep_merge_json(&mut body, &self.config.body);
        deep_merge_json(&mut body, &model.body);

        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx.send(ProviderStreamEvent::Error(format!("Gemini API error {}: {}", status, err_text))).await;
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
                    let text = msg.content.iter().filter_map(|b| match b {
                        Block::Text { text } => Some(text.as_str()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n");
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
                    let tool_results: Vec<&Block> = msg.content.iter().filter(|b| matches!(b, Block::ToolResult { .. })).collect();
                    if !tool_results.is_empty() && tool_results.len() == msg.content.len() {
                        for b in tool_results {
                            if let Block::ToolResult { tool_use_id, content, is_error } = b {
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
                                    let data_url = if data.starts_with("http://") || data.starts_with("https://") || data.starts_with("data:") {
                                        data.clone()
                                    } else {
                                        format!("data:{};base64,{}", mime_type, data)
                                    };
                                    content_parts.push(serde_json::json!({
                                        "type": "input_image",
                                        "image_url": data_url
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

        let resp = req.json(&body).send().await.context("Failed to send request to Responses API")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err_text = resp.text().await.unwrap_or_default();
            let _ = tx.send(ProviderStreamEvent::Error(format!("Responses API error {}: {}", status, err_text))).await;
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
                        let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let _ = tx.send(ProviderStreamEvent::Usage {
                            input_tokens,
                            output_tokens: 0,
                        }).await;
                    }
                }
                "content_block_start" => {
                    if let Some(cb) = val.get("content_block") {
                        let block_type = cb.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if block_type == "tool_use" {
                            current_tool_id = cb.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            current_tool_name = cb.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            current_tool_args.clear();
                        }
                    }
                }
                "content_block_delta" => {
                    if let Some(delta) = val.get("delta") {
                        let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if delta_type == "text_delta" {
                            if let Some(text) = delta.get("text").and_then(|v| v.as_str()) {
                                let _ = tx.send(ProviderStreamEvent::TextDelta(text.to_string())).await;
                            }
                        } else if delta_type == "thinking_delta" {
                            if let Some(th) = delta.get("thinking").and_then(|v| v.as_str()) {
                                let _ = tx.send(ProviderStreamEvent::ThinkingDelta(th.to_string())).await;
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
                        let _ = tx.send(ProviderStreamEvent::ToolCall {
                            id: std::mem::take(&mut current_tool_id),
                            name: std::mem::take(&mut current_tool_name),
                            input: parsed_args,
                        }).await;
                        current_tool_args.clear();
                    }
                }
                "message_delta" => {
                    if let Some(usage) = val.get("usage") {
                        let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let _ = tx.send(ProviderStreamEvent::Usage {
                            input_tokens: 0,
                            output_tokens,
                        }).await;
                    }
                }
                "message_stop" => {
                    let _ = tx.send(ProviderStreamEvent::Done {
                        stop_reason: StopReason::EndTurn,
                    }).await;
                }
                _ => {}
            }
        }
    }
}

/// OpenAI / DeepSeek SSE 解析
pub async fn parse_openai_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = String::new();

    struct ToolCallAcc {
        id: String,
        name: String,
        arguments: String,
    }
    let mut tool_calls: BTreeMap<usize, ToolCallAcc> = BTreeMap::new();

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
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();

                if data == "[DONE]" {
                    // 推送所有累积的工具调用
                    for (_, tc) in std::mem::take(&mut tool_calls) {
                        let parsed = match serde_json::from_str(&tc.arguments) {
                            Ok(v) => v,
                            Err(e) => serde_json::json!({
                                "_parse_error": e.to_string(),
                                "_raw": tc.arguments
                            }),
                        };
                        let _ = tx.send(ProviderStreamEvent::ToolCall {
                            id: tc.id,
                            name: tc.name,
                            input: parsed,
                        }).await;
                    }
                    let _ = tx.send(ProviderStreamEvent::Done { stop_reason: StopReason::EndTurn }).await;
                    return;
                }

                let val: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // 统计 Usage
                if let Some(usage) = val.get("usage") {
                    let input_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let output_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let _ = tx.send(ProviderStreamEvent::Usage { input_tokens, output_tokens }).await;
                }

                if let Some(choice) = val.get("choices").and_then(|c| c.get(0)) {
                    if let Some(delta) = choice.get("delta") {
                        // 1. 深度求索思维链 reasoning_content
                        if let Some(thinking) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                            if !thinking.is_empty() {
                                let _ = tx.send(ProviderStreamEvent::ThinkingDelta(thinking.to_string())).await;
                            }
                        }

                        // 2. 普通文本内容 content
                        if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                let _ = tx.send(ProviderStreamEvent::TextDelta(text.to_string())).await;
                            }
                        }

                        // 3. 工具切片累计 tool_calls
                        if let Some(tc_array) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                            for tc_item in tc_array {
                                let idx = tc_item.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                                let entry = tool_calls.entry(idx).or_insert_with(|| ToolCallAcc {
                                    id: String::new(),
                                    name: String::new(),
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
                            let _ = tx.send(ProviderStreamEvent::ToolCall {
                                id: tc.id,
                                name: tc.name,
                                input: parsed,
                            }).await;
                        }

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
                let Some(data) = line.strip_prefix("data:") else { continue };
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
                            let _ = tx.send(ProviderStreamEvent::TextDelta(text.to_string())).await;
                        }
                    }
                    // 思维链摘要增量
                    "response.reasoning_summary_text.delta" => {
                        if let Some(th) = val.get("delta").and_then(|v| v.as_str()) {
                            let _ = tx.send(ProviderStreamEvent::ThinkingDelta(th.to_string())).await;
                        }
                    }
                    // function_call 输出项开始（携带完整 call_id 与 name）
                    "response.output_item.added" => {
                        if val.get("item").and_then(|i| i.get("type")).and_then(|v| v.as_str()) == Some("function_call") {
                            let call_id = val.pointer("/item/call_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let name = val.pointer("/item/name").and_then(|v| v.as_str()).unwrap_or("").to_string();
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
                            let final_args = val.get("arguments").and_then(|v| v.as_str()).unwrap_or(&args).to_string();
                            let parsed = match serde_json::from_str(&final_args) {
                                Ok(v) => v,
                                Err(e) => serde_json::json!({
                                    "_parse_error": e.to_string(),
                                    "_raw": final_args
                                }),
                            };
                            let _ = tx.send(ProviderStreamEvent::ToolCall {
                                id: call_id,
                                name,
                                input: parsed,
                            }).await;
                        }
                    }
                    // 整个响应完成（含 usage 与 output 数组）
                    "response.completed" => {
                        saw_done = true;
                        if let Some(usage) = val.pointer("/response/usage") {
                            let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let _ = tx.send(ProviderStreamEvent::Usage { input_tokens, output_tokens }).await;
                        }
                        let stop_reason = val.pointer("/response/incomplete_details/reason")
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
                        let msg = val.pointer("/response/error/message")
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
        let _ = tx.send(ProviderStreamEvent::Done { stop_reason: StopReason::EndTurn }).await;
    }
}

/// Google Gemini SSE 解析
pub async fn parse_gemini_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut buffer = String::new();

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
                let Some(data) = line.strip_prefix("data:") else { continue };
                let val: serde_json::Value = match serde_json::from_str(data.trim()) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(candidates) = val.get("candidates").and_then(|v| v.as_array()) {
                    for cand in candidates {
                        if let Some(parts) = cand.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                            for part in parts {
                                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                    let _ = tx.send(ProviderStreamEvent::TextDelta(text.to_string())).await;
                                }
                            }
                        }
                    }
                }

                if let Some(usage) = val.get("usageMetadata") {
                    let input_tokens = usage.get("promptTokenCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let output_tokens = usage.get("candidatesTokenCount").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let _ = tx.send(ProviderStreamEvent::Usage { input_tokens, output_tokens }).await;
                }
            }
        }
    }

    let _ = tx.send(ProviderStreamEvent::Done { stop_reason: StopReason::EndTurn }).await;
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
        assert_eq!(events[3], ProviderStreamEvent::ToolCall {
            id: "call_1".into(),
            name: "read".into(),
            input: serde_json::json!({ "path": "src/lib.rs" }),
        });
        assert_eq!(events[4], ProviderStreamEvent::Done { stop_reason: StopReason::EndTurn });
    }

    #[tokio::test]
    async fn test_parse_responses_sse_stream() {
        let sse_data = concat!(
            r#"data: {"type":"response.reasoning_summary_text.delta","delta":"thinking hard"}"#, "\n\n",
            r#"data: {"type":"response.output_text.delta","delta":"Hello "}"#, "\n\n",
            r#"data: {"type":"response.output_text.delta","delta":"world"}"#, "\n\n",
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_9","name":"shell"}}"#, "\n\n",
            r#"data: {"type":"response.function_call_arguments.delta","delta":"{\"cmd\":"}"#, "\n\n",
            r#"data: {"type":"response.function_call_arguments.delta","delta":"\"ls\"}"}"#, "\n\n",
            r#"data: {"type":"response.function_call_arguments.done","arguments":"{\"cmd\":\"ls -la\"}"}"#, "\n\n",
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":42},"incomplete_details":null}}"#, "\n\n",
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
        assert_eq!(events[3], ProviderStreamEvent::ToolCall {
            id: "call_9".into(),
            name: "shell".into(),
            input: serde_json::json!({ "cmd": "ls -la" }),
        });
        assert_eq!(events[4], ProviderStreamEvent::Usage { input_tokens: 100, output_tokens: 42 });
        assert_eq!(events[5], ProviderStreamEvent::Done { stop_reason: StopReason::EndTurn });
    }

    #[tokio::test]
    async fn test_parse_responses_sse_incomplete_and_error() {
        // max_output_tokens 不完整结束 → MaxTokens
        let sse_max = concat!(
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":5},"incomplete_details":{"reason":"max_output_tokens"}}}"#, "\n\n",
        );
        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_max))]);
        let (tx, mut rx) = mpsc::channel(4);
        parse_responses_sse(stream, tx).await;
        let mut ev = rx.recv().await.unwrap();
        assert_eq!(ev, ProviderStreamEvent::Usage { input_tokens: 1, output_tokens: 5 });
        ev = rx.recv().await.unwrap();
        assert_eq!(ev, ProviderStreamEvent::Done { stop_reason: StopReason::MaxTokens });
        assert!(rx.recv().await.is_none());

        // response.failed → Error 事件
        let sse_fail = concat!(
            r#"data: {"type":"response.failed","response":{"error":{"message":"boom"}}}"#, "\n\n",
            r#"data: {"type":"response.completed","response":{"usage":{"input_tokens":0,"output_tokens":0},"incomplete_details":null}}"#, "\n\n",
        );
        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_fail))]);
        let (tx, mut rx) = mpsc::channel(4);
        parse_responses_sse(stream, tx).await;
        assert_eq!(rx.recv().await.unwrap(), ProviderStreamEvent::Error("boom".into()));
        // response.failed → Error 事件，随后的 completed 事件先带 Usage 再收尾
        assert_eq!(rx.recv().await.unwrap(), ProviderStreamEvent::Usage { input_tokens: 0, output_tokens: 0 });
        assert_eq!(rx.recv().await.unwrap(), ProviderStreamEvent::Done { stop_reason: StopReason::EndTurn });
    }
}
