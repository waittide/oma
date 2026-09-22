use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use futures_util::StreamExt;
use oma_contract::{Block, ChatMessage, ModelCapability, Role, StopReason};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

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
                    ..
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
///
/// `echo_reasoning` 为真（模型声明了 thinking 能力）时，带 `tool_calls` 的
/// assistant 消息必须把思维链原样带回：DeepSeek 等推理接口在 thinking 模式下
/// 用 `reasoning_content` 校验历史，缺失时整轮请求被拒
/// （`The reasoning_content in the thinking mode must be passed back to the API.`）。
/// 未记录到思维链时补空串占位，同样满足校验。
fn build_openai_messages(
    messages: &[ChatMessage],
    system_prompt: Option<&str>,
    echo_reasoning: bool,
) -> Vec<serde_json::Value> {
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
                let mut reasoning = String::new();
                let mut tool_calls = Vec::new();
                for b in &msg.content {
                    match b {
                        Block::Text { text } => text_parts.push(text.clone()),
                        Block::Thinking { thinking } => reasoning.push_str(thinking),
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
                // 兼容接口要求 assistant 消息至少带 `content` 或 `tool_calls`：
                // 思维链尚未结束就被打断（用户中止、上游断流）会留下只有 thinking
                // 的消息，此时必须补空串占位，否则整轮请求被上游拒
                // （`Invalid assistant message: content or tool_calls must be set`）。
                // 带 tool_calls 的消息按惯例省略 content，保持与各厂商示例一致。
                if !text_parts.is_empty() || tool_calls.is_empty() {
                    obj["content"] = serde_json::Value::String(text_parts.join("\n"));
                }
                // 思维链回传（见函数文档）：只在当前模型声明 thinking 时下发，
                // 换成不支持推理的厂商时不能凭空多出该字段
                if echo_reasoning && (!reasoning.is_empty() || !tool_calls.is_empty()) {
                    obj["reasoning_content"] = serde_json::Value::String(reasoning);
                }
                if !tool_calls.is_empty() {
                    obj["tool_calls"] = serde_json::Value::Array(tool_calls);
                }
                openai_messages.push(obj);
            }
            Role::User => {
                // 工具回执：role: tool 的 content 只能是字符串，图片无法内联，
                // 因此把回执发完后另起一条带图的用户消息（紧跟其后，保持配对顺序）
                if is_tool_result_message(msg) {
                    for b in &msg.content {
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
                    push_tool_image_message(&mut openai_messages, &tool_result_images(msg), |m, d| {
                        serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": image_data_url(m, d) }
                        })
                    });
                    continue;
                }

                // 普通用户消息：带图使用 parts 数组形态，纯文本仍走字符串形态
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

    openai_messages
}

/// 当前用户消息是否为「纯工具回执」（含紧跟其后的图片）
fn is_tool_result_message(msg: &ChatMessage) -> bool {
    msg.role == Role::User
        && msg
            .content
            .first()
            .is_some_and(|b| matches!(b, Block::ToolResult { .. }))
}

/// 回执里附带的图片，按出现顺序（一条消息可能对应多个工具调用）
fn tool_result_images(msg: &ChatMessage) -> Vec<(&str, &str)> {
    msg.content
        .iter()
        .filter_map(|b| match b {
            Block::Image { mime_type, data } => Some((mime_type.as_str(), data.as_str())),
            _ => None,
        })
        .collect()
}

/// 把「工具回执里带回的图片」补成一条紧随其后的用户消息。
///
/// 仅用于回执无法携带图片的协议（completion / response / google）：各厂商都允许
/// 在工具结果之后追加用户消息，图片放这里既不破坏 tool_call↔tool_result 的配对，
/// 也能让模型在同一轮里看到图。返回 true 表示确实追加了消息。
fn push_tool_image_message(
    out: &mut Vec<serde_json::Value>,
    images: &[(&str, &str)],
    build_part: impl Fn(&str, &str) -> serde_json::Value,
) -> bool {
    if images.is_empty() {
        return false;
    }
    let parts: Vec<serde_json::Value> = images.iter().map(|(m, d)| build_part(m, d)).collect();
    out.push(serde_json::json!({ "role": "user", "content": parts }));
    true
}

/// 图片块 → 可直接投递的 data URL（已是 URL/data URI 时原样透传）
fn image_data_url(mime_type: &str, data: &str) -> String {
    if data.starts_with("http://") || data.starts_with("https://") || data.starts_with("data:") {
        data.to_string()
    } else {
        format!("data:{};base64,{}", mime_type, data)
    }
}

/// 剥掉可能存在的 data URI 前缀：内联图片在不同厂商协议里分别要 base64 或 URL
fn base64_payload(data: &str) -> &str {
    data.rsplit_once(",").map_or(
        data,
        |(prefix, rest)| {
            if prefix.starts_with("data:") { rest } else { data }
        },
    )
}

/// Anthropic 图片内容块
fn anthropic_image_block(mime_type: &str, data: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": mime_type,
            "data": base64_payload(data)
        }
    })
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
        /// 提示侧（输入）总量：统一含缓存读写，与各厂商口径对齐
        input_tokens:       usize,
        output_tokens:      usize,
        /// 缓存命中的输入 token（Anthropic `cache_read_input_tokens` /
        /// OpenAI `prompt_tokens_details.cached_tokens` / Google `cachedContentTokenCount`）
        cache_read_tokens:  usize,
        /// 写入缓存的输入 token（Anthropic `cache_creation_input_tokens`）
        cache_write_tokens: usize,
    },
    Done {
        stop_reason: StopReason,
    },
    Error(String),
}

/// 配置文件中的模型条目（ProviderConfig.models）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    pub id:            String,
    #[serde(default = "model_entry_default_name")]
    pub name:          String,
    #[serde(default = "model_entry_default_context")]
    pub context_len:   usize,
    /// 模型能力集合；未声明时仅文本输入与文本输出
    #[serde(default = "oma_contract::default_model_capabilities")]
    pub capabilities:  BTreeSet<ModelCapability>,
    /// 最大输出 Token 数；None 时各协议使用内置默认
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output:    Option<usize>,
    /// 推理等级 → 厂商自定义字符串；未配置的等级回退为等级名本身
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_map: BTreeMap<String, String>,
    /// 该模型附加的请求头：同名覆盖 Provider 级配置
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers:       BTreeMap<String, String>,
    /// 该模型附加的请求体字段：递归深合并，覆盖 Provider 级配置
    #[serde(
        default = "empty_json_object",
        deserialize_with = "deserialize_json_body",
        skip_serializing_if = "is_empty_json_object"
    )]
    pub body:          serde_json::Value,
}

fn model_entry_default_name() -> String {
    String::new()
}
fn model_entry_default_context() -> usize {
    128_000
}
fn empty_json_object() -> serde_json::Value {
    serde_json::json!({})
}

/// 空请求体不写入配置文件：模型条目数量多，每项都带 `body = {}` 徒增噪声。
fn is_empty_json_object(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|o| o.is_empty())
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
#[serde(deny_unknown_fields)]
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
    pub id:               String,
    pub name:             String,
    pub context_len:      usize,
    /// 模型能力集合；驱动图片编码与思考参数下发
    #[serde(default = "oma_contract::default_model_capabilities")]
    pub capabilities:     BTreeSet<ModelCapability>,
    #[serde(default)]
    pub max_output:       Option<usize>,
    /// 实际下发给厂商的推理参数；由会话等级经 `reasoning_map` 解析后填入
    #[serde(default)]
    pub reasoning_effort: String,
    /// 推理等级 → 厂商自定义字符串；未配置的等级回退为等级名本身
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reasoning_map:    BTreeMap<String, String>,
    #[serde(default)]
    pub headers:          BTreeMap<String, String>,
    #[serde(default = "empty_json_object", deserialize_with = "deserialize_json_body")]
    pub body:             serde_json::Value,
}

impl ModelConfig {
    /// 是否具备某项能力
    pub fn has(&self, capability: ModelCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// 能否直接接收图片输入（驱动图片编码与 `read` 内联判断）
    pub fn supports_image_input(&self) -> bool {
        self.has(ModelCapability::ImageInput)
    }

    /// 是否支持思考（决定是否下发 reasoning 类参数）
    pub fn supports_thinking(&self) -> bool {
        self.has(ModelCapability::Thinking)
    }
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

    /// 针对各厂商组装请求并开始流式接收
    ///
    /// 传输层中断（网关/厂商中途断流）由 [`relay`] 在内容下发之前透明重发，
    /// 调用方只在重发穷尽或内容已下发时收到 `ProviderStreamEvent::Error`。
    pub async fn send_stream(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> Result<mpsc::Receiver<ProviderStreamEvent>> {
        let (tx, rx) = mpsc::channel(128);

        let Some(api) = ApiType::parse(&self.config.api_type) else {
            let _ = tx
                .send(ProviderStreamEvent::Error(format!(
                    "Unsupported api_type: {}",
                    self.config.api_type
                )))
                .await;
            return Ok(rx);
        };

        let plan = self.build_plan(api, messages, system_prompt, tools, model);
        tokio::spawn(relay(plan, tx));
        Ok(rx)
    }

    /// 按协议组装请求：请求体在此一次性序列化，重发时复用同一份字节
    fn build_plan(
        &self,
        api: ApiType,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> RequestPlan {
        let builder = match api {
            ApiType::Anthropic => self.build_anthropic_request(messages, system_prompt, tools, model),
            ApiType::OpenAi => self.build_openai_request(messages, system_prompt, tools, model),
            ApiType::Responses => self.build_responses_request(messages, system_prompt, tools, model),
            ApiType::Gemini => self.build_gemini_request(messages, system_prompt, tools, model),
        };
        RequestPlan { api, builder }
    }

    // -------------------------------------------------------------
    // Anthropic Messages API
    // -------------------------------------------------------------
    fn build_anthropic_request(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> reqwest::RequestBuilder {
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
            let mut i = 0;
            while i < msg.content.len() {
                match &msg.content[i] {
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
                        content_blocks.push(anthropic_image_block(mime_type, data));
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
                        ..
                    } => {
                        // 紧跟在回执后的图片属于该回执（如 read 读图片）：
                        // Anthropic 允许 tool_result.content 为内容块数组，
                        // 图片因此可以留在原位，不必另起一条用户消息。
                        let mut inner = vec![serde_json::json!({
                            "type": "text",
                            "text": content
                        })];
                        let mut j = i + 1;
                        while let Some(Block::Image { mime_type, data }) = msg.content.get(j) {
                            inner.push(anthropic_image_block(mime_type, data));
                            j += 1;
                        }
                        i = j;
                        content_blocks.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": inner,
                            "is_error": is_error
                        }));
                        continue;
                    }
                }
                i += 1;
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

        req.json(&body)
    }

    // -------------------------------------------------------------
    // OpenAI / DeepSeek Chat Completion API
    // -------------------------------------------------------------
    fn build_openai_request(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> reqwest::RequestBuilder {
        let url = format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'));
        let api_key = self.config.resolved_api_key();

        let openai_messages = build_openai_messages(messages, system_prompt, model.supports_thinking());

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

        req.json(&body)
    }

    // -------------------------------------------------------------
    // Google Gemini API
    // -------------------------------------------------------------
    fn build_gemini_request(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> reqwest::RequestBuilder {
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

        self.client.post(&url).json(&body)
    }

    // -------------------------------------------------------------
    // OpenAI Responses API
    // -------------------------------------------------------------
    fn build_responses_request(
        &self,
        messages: &[ChatMessage],
        system_prompt: Option<&str>,
        tools: &[serde_json::Value],
        model: &ModelConfig,
    ) -> reqwest::RequestBuilder {
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
                    // 工具回执：function_call_output 的 output 只能是字符串，
                    // 图片另起一条带 input_image 的用户消息
                    if is_tool_result_message(msg) {
                        for b in &msg.content {
                            if let Block::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                                ..
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
                        push_tool_image_message(&mut input, &tool_result_images(msg), |m, d| {
                            serde_json::json!({
                                "type": "input_image",
                                "image_url": image_data_url(m, d)
                            })
                        });
                        continue;
                    }

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

        req.json(&body)
    }
}

// =========================================================================
// 流式请求驱动：传输层中断的透明重发
// =========================================================================

/// 传输层中断时的尝试次数上限（首次 + 重发）
const STREAM_MAX_ATTEMPTS: usize = 3;
/// 重发前的等待：给网关换渠道、上游恢复留出时间
const STREAM_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// 厂商协议：决定请求组装方式与响应解析器
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiType {
    Anthropic,
    OpenAi,
    Responses,
    Gemini,
}

impl ApiType {
    /// 配置里的 `api_type` 取值
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "anthropic" => Some(Self::Anthropic),
            "completion" => Some(Self::OpenAi),
            "response" => Some(Self::Responses),
            "google" => Some(Self::Gemini),
            _ => None,
        }
    }

    /// 错误信息前缀：让用户看得出是哪一侧协议报的错
    fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::Responses => "Responses",
            Self::Gemini => "Gemini",
        }
    }
}

/// 组装完成、可重复发送的流式请求
struct RequestPlan {
    api:     ApiType,
    builder: reqwest::RequestBuilder,
}

/// 一次尝试的结局
enum Attempt {
    /// 请求已结束：正常收尾，或厂商返回了错误正文（重发无意义）
    Settled,
    /// 传输层中断且尚未下发任何内容：可以安全重发
    Retryable(String),
    /// 传输层中断但已下发过内容：重发会造成重复，不得重发
    Interrupted(String),
}

/// 发送请求并驱动响应，传输层中断时在**内容下发之前**重发
///
/// 网关与厂商偶发在响应中途断开连接，且常发生在正文尚未开始的第一个分片之后
/// （排查中实测某聚合网关的主渠道断流率接近三成）。这类中断与请求内容无关，
/// 重发即可恢复；而一旦有增量下发，它已经流向界面与历史，重发只会造成重复，
/// 故只报错不重发。
async fn relay(plan: RequestPlan, tx: mpsc::Sender<ProviderStreamEvent>) {
    for attempt in 1..=STREAM_MAX_ATTEMPTS {
        match attempt_once(&plan, &tx).await {
            Attempt::Settled => return,
            Attempt::Interrupted(err) => {
                let _ = tx.send(ProviderStreamEvent::Error(err)).await;
                return;
            }
            Attempt::Retryable(err) if attempt == STREAM_MAX_ATTEMPTS => {
                let _ = tx
                    .send(ProviderStreamEvent::Error(format!(
                        "Stream failed before any content after {} attempts: {}",
                        attempt, err
                    )))
                    .await;
                return;
            }
            Attempt::Retryable(_) => tokio::time::sleep(STREAM_RETRY_DELAY).await,
        }
    }
}

/// 单次尝试：发送、校验状态、转发增量，并回报本次连接是否中途断开
async fn attempt_once(plan: &RequestPlan, tx: &mpsc::Sender<ProviderStreamEvent>) -> Attempt {
    // 请求体是 Bytes：克隆只增加引用计数，重发不会重新序列化
    let Some(builder) = plan.builder.try_clone() else {
        return Attempt::Interrupted("request body cannot be replayed".to_string());
    };

    let resp = match builder.send().await {
        Ok(resp) => resp,
        Err(e) => return Attempt::Retryable(e.to_string()),
    };

    if !resp.status().is_success() {
        // 鉴权、参数类错误重发也不会改变结果：直接透出厂商正文
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let _ = tx
            .send(ProviderStreamEvent::Error(format!(
                "{} API error {}: {}",
                plan.api.label(),
                status,
                body
            )))
            .await;
        return Attempt::Settled;
    }

    let api = plan.api;
    let (inner_tx, mut inner_rx) = mpsc::channel(128);
    let (end_tx, end_rx) = oneshot::channel();
    tokio::spawn(async move {
        let _ = end_tx.send(run_parser(api, resp, inner_tx).await);
    });

    // Anthropic 在首帧就上报用量：在确认不会重发之前先押着，
    // 否则被重发的尝试会把同一份用量重复计入。
    let mut deferred: Vec<ProviderStreamEvent> = Vec::new();
    let mut content_seen = false;

    while let Some(event) = inner_rx.recv().await {
        match event {
            ProviderStreamEvent::Usage { .. } if !content_seen => deferred.push(event),
            ProviderStreamEvent::TextDelta(_)
            | ProviderStreamEvent::ThinkingDelta(_)
            | ProviderStreamEvent::ToolCall { .. } => {
                content_seen = true;
                for pending in deferred.drain(..) {
                    if tx.send(pending).await.is_err() {
                        return Attempt::Settled;
                    }
                }
                if tx.send(event).await.is_err() {
                    return Attempt::Settled;
                }
            }
            other => {
                if tx.send(other).await.is_err() {
                    return Attempt::Settled;
                }
            }
        }
    }

    match end_rx.await.ok().flatten() {
        None => Attempt::Settled,
        Some(err) if content_seen => Attempt::Interrupted(format!("Stream interrupted mid-response: {}", err)),
        Some(err) => Attempt::Retryable(err),
    }
}

/// 解析任务：返回传输层中断的原因（`None` 表示读到流正常结束）
async fn run_parser(api: ApiType, resp: reqwest::Response, tx: mpsc::Sender<ProviderStreamEvent>) -> Option<String> {
    let result = match api {
        ApiType::Anthropic => parse_anthropic_sse(resp.bytes_stream(), tx).await,
        ApiType::OpenAi => parse_openai_sse(resp.bytes_stream(), tx).await,
        ApiType::Responses => parse_responses_sse(resp.bytes_stream(), tx).await,
        ApiType::Gemini => parse_gemini_sse(resp.bytes_stream(), tx).await,
    };
    result.err()
}

// =========================================================================
// SSE 解析器集合 (手写状态机)
// =========================================================================

/// Anthropic SSE 解析
///
/// 返回传输层中断原因（`None` 语义由调用方按 `Result` 处理）：厂商在流内上报的
/// 错误仍以 `ProviderStreamEvent::Error` 下发，两者不可混淆——前者可重发。
pub async fn parse_anthropic_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>) -> Result<(), String>
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
            Err(e) => return Err(e.to_string()),
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
                        // Anthropic 的 input_tokens 不含缓存部分：上下文占用必须把
                        // cache_creation/cache_read 一并计入，否则开启 caching 后严重低估。
                        // 两者同时单独上报，供界面展示缓存命中情况。
                        let field = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                        let cache_read_tokens = field("cache_read_input_tokens");
                        let cache_write_tokens = field("cache_creation_input_tokens");
                        let input_tokens = field("input_tokens") + cache_write_tokens + cache_read_tokens;
                        let _ = tx
                            .send(ProviderStreamEvent::Usage {
                                input_tokens,
                                output_tokens: 0,
                                cache_read_tokens,
                                cache_write_tokens,
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
                                cache_read_tokens: 0,
                                cache_write_tokens: 0,
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

    Ok(())
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

/// 带未读数据关闭连接会导致内核发送 RST，网关（如 LLMGate）会把这条连接记成「客户端中断」。
/// 因此读到结束标记（如 `data: [DONE]`）后，把剩余数据在后台排空读完，以正常 FIN 关闭。
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 后台排空流中的剩余字节，读到 EOF 或超时后正常释放连接
fn drain_stream<S, E>(mut stream: S)
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin + Send + 'static,
    E: Send + 'static,
{
    tokio::spawn(async move {
        let drain_fut = async { while let Some(_chunk) = stream.next().await {} };
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, drain_fut).await;
    });
}

pub async fn parse_openai_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>) -> Result<(), String>
where
    S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin + Send + 'static,
    E: std::fmt::Display + Send + 'static,
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
            Err(e) => return Err(e.to_string()),
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
                    drain_stream(stream);
                    return Ok(());
                }

                let val: serde_json::Value = match serde_json::from_str(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // 统计 Usage
                if let Some(usage) = val.get("usage") {
                    // prompt_tokens 已包含缓存命中部分（与 Anthropic 口径不同，
                    // 这里不得再加 prompt_tokens_details.cached_tokens，否则重复计数）；
                    // cached_tokens 仅用于展示缓存命中量。
                    let input_tokens = usage
                        .get("prompt_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let output_tokens = usage
                        .get("completion_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let cached = usage
                        .pointer("/prompt_tokens_details/cached_tokens")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let _ = tx
                        .send(ProviderStreamEvent::Usage {
                            input_tokens,
                            output_tokens,
                            cache_read_tokens: cached,
                            cache_write_tokens: 0,
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

    Ok(())
}

/// OpenAI Responses API SSE 解析（api_type = "response"）
pub async fn parse_responses_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>) -> Result<(), String>
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
            Err(e) => return Err(e.to_string()),
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
                            let cached = usage
                                .pointer("/input_tokens_details/cached_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            let _ = tx
                                .send(ProviderStreamEvent::Usage {
                                    input_tokens,
                                    output_tokens,
                                    cache_read_tokens: cached,
                                    cache_write_tokens: 0,
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

    Ok(())
}

/// Google Gemini SSE 解析
pub async fn parse_gemini_sse<S, E>(mut stream: S, tx: mpsc::Sender<ProviderStreamEvent>) -> Result<(), String>
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
            Err(e) => return Err(e.to_string()),
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
                    let cached = usage
                        .get("cachedContentTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let _ = tx
                        .send(ProviderStreamEvent::Usage {
                            input_tokens,
                            output_tokens,
                            cache_read_tokens: cached,
                            cache_write_tokens: 0,
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, atomic::AtomicUsize};

    use super::*;

    /// 测试用能力集：文本输入/输出 + 思考 + 图像输入，与旧默认（vision/thinking 均 true）等价
    fn test_capabilities() -> BTreeSet<ModelCapability> {
        [
            ModelCapability::Thinking,
            ModelCapability::TextInput,
            ModelCapability::TextOutput,
            ModelCapability::ImageInput,
        ]
        .into_iter()
        .collect()
    }

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
    fn test_null_body_normalizes_to_object() {
        // 前端 PUT 可能带回 body: null，必须规范为空对象，否则构造请求体时会拿到 null
        let missing: ProviderConfig =
            serde_json::from_str(r#"{"api_type":"completion","base_url":"https://api.example.com/v1","api_key":"k"}"#)
                .unwrap();
        assert_eq!(missing.body, serde_json::json!({}));

        let null_cfg: ProviderConfig = serde_json::from_str(
            r#"{"api_type":"completion","base_url":"https://api.example.com/v1","api_key":"k","body":null}"#,
        )
        .unwrap();
        assert_eq!(null_cfg.body, serde_json::json!({}));

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
            model:      None,
            usage:      None,
        }
    }

    /// 工具回执 + 图片：与 read 读图片后写入历史的形态一致
    fn tool_result_with_image() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                id:         "a1".into(),
                parent_id:  None,
                role:       Role::Assistant,
                content:    vec![Block::ToolUse {
                    id:    "call_1".into(),
                    name:  "read".into(),
                    input: serde_json::json!({ "path": "shot.png" }),
                }],
                created_at: 0,
                model:      None,
                usage:      None,
            },
            ChatMessage {
                id:         "u1".into(),
                parent_id:  Some("a1".into()),
                role:       Role::User,
                content:    vec![
                    Block::ToolResult {
                        tool_use_id: "call_1".into(),
                        content:     "mime: image/png".into(),
                        is_error:    false,
                        duration_ms: None,
                    },
                    Block::Image {
                        mime_type: "image/png".into(),
                        data:      "QUJD".into(),
                    },
                ],
                created_at: 0,
                model:      None,
                usage:      None,
            },
        ]
    }

    /// Anthropic 原生支持：图片留在 tool_result.content 数组里，
    /// 既不拆消息也不破坏 tool_use ↔ tool_result 配对。
    #[test]
    fn test_anthropic_tool_result_carries_image_inline() {
        let msgs = tool_result_with_image();
        let block = anthropic_image_block("image/png", "QUJD");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "QUJD", "raw base64 must not be re-wrapped");

        // data URI 形式输入要剥掉前缀，否则厂商会报非法 base64
        let stripped = anthropic_image_block("image/png", "data:image/png;base64,QUJD");
        assert_eq!(stripped["source"]["data"], "QUJD");

        // 回执消息里必须仍然只有一条 user 消息（图片不能另起一条）
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].content.len(), 2);
    }

    /// OpenAI 兼容协议：role: tool 只能带字符串，图片改为紧跟其后的用户消息。
    #[test]
    fn test_openai_tool_images_become_followup_user_message() {
        let msgs = build_openai_messages(&tool_result_with_image(), None, false);
        assert_eq!(msgs.len(), 3, "tool message plus image follow-up: {msgs:#?}");
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[1]["role"], "tool");
        assert_eq!(msgs[1]["tool_call_id"], "call_1");
        assert_eq!(msgs[1]["content"], "mime: image/png");
        assert_eq!(msgs[2]["role"], "user", "images ride a trailing user message");
        assert_eq!(msgs[2]["content"][0]["type"], "image_url");
        assert_eq!(msgs[2]["content"][0]["image_url"]["url"], "data:image/png;base64,QUJD");
    }

    /// 没有图片回执时不得多出空白的用户消息（避免无谓的上下文污染）。
    #[test]
    fn test_openai_tool_result_without_image_stays_compact() {
        let mut msgs = tool_result_with_image();
        msgs[1]
            .content
            .retain(|b| !matches!(b, Block::Image { .. }));
        let built = build_openai_messages(&msgs, None, false);
        assert_eq!(built.len(), 2, "{built:#?}");
        assert_eq!(built[1]["role"], "tool");
    }

    /// Gemini：图片与 functionResponse 落进同一轮 parts（同角色合并），无需拆分。
    #[test]
    fn test_gemini_tool_result_image_shares_turn() {
        let body = build_gemini_body(&tool_result_with_image(), None, &[], &model_for_tests());
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2, "{contents:#?}");
        let user_parts = contents[1]["parts"].as_array().unwrap();
        assert!(
            user_parts
                .iter()
                .any(|p| p.get("functionResponse").is_some()),
            "functionResponse must survive: {user_parts:#?}"
        );
        let inline = user_parts
            .iter()
            .find_map(|p| p.get("inlineData"))
            .expect("image must be inlined into the same user turn");
        assert_eq!(inline["mimeType"], "image/png");
        assert_eq!(inline["data"], "QUJD");
    }

    fn model_for_tests() -> ModelConfig {
        ModelConfig {
            id:               "m".into(),
            name:             "m".into(),
            context_len:      1000,
            capabilities:     test_capabilities(),
            max_output:       None,
            reasoning_effort: String::new(),
            reasoning_map:    BTreeMap::new(),
            headers:          BTreeMap::new(),
            body:             serde_json::json!({}),
        }
    }

    /// OpenAI 兼容路径此前只 join 文本块，图片被静默丢弃。
    #[test]
    fn test_openai_messages_keep_images() {
        let msgs = build_openai_messages(&[user_with_image()], None, false);
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
            model:      None,
            usage:      None,
        };
        let tool_result = ChatMessage {
            id:         "r1".into(),
            parent_id:  Some("a1".into()),
            role:       Role::User,
            content:    vec![Block::ToolResult {
                tool_use_id: "call_1".into(),
                content:     "file body".into(),
                is_error:    false,
                duration_ms: None,
            }],
            created_at: 0,
            model:      None,
            usage:      None,
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
                id:               "gemini-pro".into(),
                name:             "gemini-pro".into(),
                context_len:      1000,
                capabilities:     test_capabilities(),
                max_output:       Some(256),
                reasoning_effort: String::new(),
                reasoning_map:    BTreeMap::new(),
                headers:          BTreeMap::new(),
                body:             serde_json::json!({}),
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
        parse_gemini_sse(stream, tx).await.unwrap();

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
                input_tokens:       7,
                output_tokens:      3,
                cache_read_tokens:  0,
                cache_write_tokens: 0,
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
            parse_openai_sse(stream, tx).await.unwrap();
        });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }
        assert_eq!(events[0], ProviderStreamEvent::ThinkingDelta("第一段".into()));
        assert_eq!(events[1], ProviderStreamEvent::ThinkingDelta("第二段".into()));
        assert_eq!(events[2], ProviderStreamEvent::TextDelta("答复".into()));
    }

    /// Anthropic 的 input_tokens 不含缓存：上下文占用必须是三者之和，
    /// 否则开启 prompt caching 后进度条严重低估。
    #[tokio::test]
    async fn test_anthropic_usage_includes_cache_tokens() {
        let sse_data = "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":1000,\"cache_creation_input_tokens\":500,\"cache_read_input_tokens\":2000}}}\n\n\
data: [DONE]\n\n";

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);
        tokio::spawn(async move {
            parse_anthropic_sse(stream, tx).await.unwrap();
        });

        let mut usage = None;
        while let Some(ev) = rx.recv().await {
            if let ProviderStreamEvent::Usage { input_tokens, .. } = ev {
                usage = Some(input_tokens);
            }
        }
        assert_eq!(usage, Some(3500), "input + cache_creation + cache_read");
    }

    /// 无缓存字段时退化为纯 input_tokens。
    #[tokio::test]
    async fn test_anthropic_usage_without_cache_fields() {
        let sse_data = "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":777}}}\n\n\
data: [DONE]\n\n";

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);
        tokio::spawn(async move {
            parse_anthropic_sse(stream, tx).await.unwrap();
        });

        let mut usage = None;
        while let Some(ev) = rx.recv().await {
            if let ProviderStreamEvent::Usage { input_tokens, .. } = ev {
                usage = Some(input_tokens);
            }
        }
        assert_eq!(usage, Some(777));
    }

    /// OpenAI 的 prompt_tokens 本身含缓存，不得再叠加 cached_tokens（会重复计数）。
    #[tokio::test]
    async fn test_openai_usage_does_not_double_count_cache() {
        let sse_data = "data: {\"usage\":{\"prompt_tokens\":1200,\"completion_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":800}},\"choices\":[{\"delta\":{}}]}\n\n\
data: [DONE]\n\n";

        let stream = futures_util::stream::iter(vec![Ok::<_, String>(bytes::Bytes::from(sse_data))]);
        let (tx, mut rx) = mpsc::channel(10);
        tokio::spawn(async move {
            parse_openai_sse(stream, tx).await.unwrap();
        });

        let mut usage = None;
        while let Some(ev) = rx.recv().await {
            if let ProviderStreamEvent::Usage { input_tokens, .. } = ev {
                usage = Some(input_tokens);
            }
        }
        assert_eq!(usage, Some(1200), "prompt_tokens already includes cached tokens");
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
            parse_openai_sse(stream, tx).await.unwrap();
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
            parse_openai_sse(stream, tx).await.unwrap();
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
        tokio::spawn(async move { parse_openai_sse(stream, tx).await.unwrap() });

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
    async fn test_parse_openai_sse_drains_remaining_stream_on_done() {
        // 构造流：先给出包含 [DONE] 的分片，再给出若干后续分片
        let chunks = vec![
            Ok::<_, String>(bytes::Bytes::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n",
            )),
            Ok::<_, String>(bytes::Bytes::from("data: [DONE]\n\n")),
            Ok::<_, String>(bytes::Bytes::from("extra chunk 1")),
            Ok::<_, String>(bytes::Bytes::from("extra chunk 2")),
            Ok::<_, String>(bytes::Bytes::from("extra chunk 3")),
        ];
        let total_chunks = chunks.len();
        let consumed = Arc::new(AtomicUsize::new(0));
        let consumed_clone = Arc::clone(&consumed);

        let stream = futures_util::stream::iter(chunks).map(move |item| {
            consumed_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            item
        });

        let (tx, mut rx) = mpsc::channel(10);
        // parse_openai_sse 遇到 [DONE] 会在后台排空并立即返回 Ok
        let res = parse_openai_sse(stream, tx).await;
        assert!(res.is_ok(), "parse_openai_sse 应该返回 Ok");

        // 事件中应该恰好只有一个 TextDelta 和一个 Done，没有多余事件
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], ProviderStreamEvent::TextDelta("hello".to_string()));
        assert_eq!(
            events[1],
            ProviderStreamEvent::Done {
                stop_reason: StopReason::EndTurn,
            }
        );

        // 轮询检查后台排空：稍等片刻后所有后续分片都应被消费完
        let start = std::time::Instant::now();
        while consumed.load(std::sync::atomic::Ordering::SeqCst) < total_chunks {
            if start.elapsed() > std::time::Duration::from_secs(2) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(
            consumed.load(std::sync::atomic::Ordering::SeqCst),
            total_chunks,
            "流中的所有后续分片都应该在后台被排空消费"
        );
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
            r#"data: {"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_9","name":"bash"}}"#,
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
            parse_responses_sse(stream, tx).await.unwrap();
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
                name:  "bash".into(),
                input: serde_json::json!({ "cmd": "ls -la" }),
            }
        );
        assert_eq!(
            events[4],
            ProviderStreamEvent::Usage {
                input_tokens:       100,
                output_tokens:      42,
                cache_read_tokens:  0,
                cache_write_tokens: 0,
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
        parse_responses_sse(stream, tx).await.unwrap();
        let mut ev = rx.recv().await.unwrap();
        assert_eq!(
            ev,
            ProviderStreamEvent::Usage {
                input_tokens:       1,
                output_tokens:      5,
                cache_read_tokens:  0,
                cache_write_tokens: 0,
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
        parse_responses_sse(stream, tx).await.unwrap();
        assert_eq!(rx.recv().await.unwrap(), ProviderStreamEvent::Error("boom".into()));
        // response.failed → Error 事件，随后的 completed 事件先带 Usage 再收尾
        assert_eq!(
            rx.recv().await.unwrap(),
            ProviderStreamEvent::Usage {
                input_tokens:       0,
                output_tokens:      0,
                cache_read_tokens:  0,
                cache_write_tokens: 0,
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
            capabilities: test_capabilities(),
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

    // ---------- 传输层中断的透明重发 ----------

    fn completion_config(base_url: &str) -> ProviderConfig {
        ProviderConfig {
            api_type: "completion".into(),
            base_url: base_url.to_string(),
            api_key:  "test-key".into(),
            headers:  BTreeMap::new(),
            body:     serde_json::json!({}),
            models:   Vec::new(),
        }
    }

    fn user_text(text: &str) -> Vec<ChatMessage> {
        vec![ChatMessage {
            id:         "u1".into(),
            parent_id:  None,
            role:       Role::User,
            content:    vec![Block::Text { text: text.into() }],
            created_at: 0,
            model:      None,
            usage:      None,
        }]
    }

    fn sse_event(json: &str) -> String {
        format!("data: {}\n\n", json)
    }

    /// 拼一个 chunked 响应；`terminated` 为假时不写终止分片，
    /// 复现网关在响应中途断开（连接关闭，响应体被截断）的形态。
    fn chunked_response(chunks: &[String], terminated: bool) -> String {
        let mut out =
            String::from("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n");
        for data in chunks {
            out.push_str(&format!("{:x}\r\n{}\r\n", data.len(), data));
        }
        if terminated {
            out.push_str("0\r\n\r\n");
        }
        out
    }

    fn role_chunk() -> String {
        sse_event(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#)
    }

    fn text_chunk(text: &str) -> String {
        sse_event(&format!(r#"{{"choices":[{{"delta":{{"content":"{text}"}}}}]}}"#))
    }

    fn usage_chunk() -> String {
        sse_event(r#"{"choices":[{"delta":{}}],"usage":{"prompt_tokens":10,"completion_tokens":1}}"#)
    }

    fn finish_chunk() -> String {
        sse_event(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
    }

    /// 起一个按脚本回放的 HTTP 服务：第 n 次连接回放 `scripts[n]`，
    /// 用尽后重复最后一个脚本。返回 base_url 与实际接受的连接数。
    async fn spawn_scripted_server(scripts: Vec<String>) -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let index = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let script = scripts
                    .get(index)
                    .or_else(|| scripts.last())
                    .cloned()
                    .unwrap_or_default();
                // 请求体不参与回放，读走即可（否则关闭连接会让客户端看到 RST）
                let mut buf = [0u8; 65536];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(script.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        (format!("http://{addr}/v1"), seen)
    }

    async fn drain_events(rx: &mut mpsc::Receiver<ProviderStreamEvent>) -> Vec<ProviderStreamEvent> {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    /// 网关在正文开始前断流（只发出角色分片）：重发后应拿到完整结果，用户无感。
    #[tokio::test]
    async fn test_stream_retries_when_broken_before_any_content() {
        let broken = chunked_response(&[role_chunk()], false);
        let complete = chunked_response(
            &[
                role_chunk(),
                text_chunk("hi"),
                finish_chunk(),
                "data: [DONE]\n\n".to_string(),
            ],
            true,
        );
        let (base_url, connections) = spawn_scripted_server(vec![broken, complete]).await;

        let provider = UniversalProvider::new(completion_config(&base_url));
        let mut rx = provider
            .send_stream(&user_text("hi"), None, &[], &model_for_tests())
            .await
            .unwrap();
        let events = drain_events(&mut rx).await;

        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2, "断流必须重发");
        assert_eq!(
            events,
            vec![
                ProviderStreamEvent::TextDelta("hi".into()),
                ProviderStreamEvent::Done {
                    stop_reason: StopReason::EndTurn,
                },
            ]
        );
    }

    /// 已经下发过内容后断流：不得重发（增量已流向界面，重发只会重复），
    /// 但错误信息要指明是中途断流而非毫无头绪的传输错误。
    #[tokio::test]
    async fn test_stream_does_not_retry_after_content_delivered() {
        let broken_midway = chunked_response(&[role_chunk(), text_chunk("hi")], false);
        let complete = chunked_response(&[role_chunk(), text_chunk("hi"), finish_chunk()], true);
        let (base_url, connections) = spawn_scripted_server(vec![broken_midway, complete]).await;

        let provider = UniversalProvider::new(completion_config(&base_url));
        let mut rx = provider
            .send_stream(&user_text("hi"), None, &[], &model_for_tests())
            .await
            .unwrap();
        let events = drain_events(&mut rx).await;

        assert_eq!(
            connections.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "有内容下发后不得重发"
        );
        assert_eq!(events[0], ProviderStreamEvent::TextDelta("hi".into()));
        match &events[1] {
            ProviderStreamEvent::Error(msg) => assert!(msg.contains("mid-response"), "{msg}"),
            other => panic!("expected an error event, got {other:?}"),
        }
    }

    /// 重发不得让失败尝试的用量被计入两次（Anthropic 在首帧就上报用量，
    /// 该字段必须在确认不重发之后才下发）。
    #[tokio::test]
    async fn test_stream_retry_does_not_duplicate_usage() {
        let broken = chunked_response(&[usage_chunk(), role_chunk()], false);
        let complete = chunked_response(&[usage_chunk(), text_chunk("hi"), finish_chunk()], true);
        let (base_url, connections) = spawn_scripted_server(vec![broken, complete]).await;

        let provider = UniversalProvider::new(completion_config(&base_url));
        let mut rx = provider
            .send_stream(&user_text("hi"), None, &[], &model_for_tests())
            .await
            .unwrap();
        let events = drain_events(&mut rx).await;

        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 2, "断流必须重发");
        let usages: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ProviderStreamEvent::Usage { .. }))
            .collect();
        assert_eq!(usages.len(), 1, "被重发的尝试不得把用量计入: {events:?}");
        assert!(events.contains(&ProviderStreamEvent::TextDelta("hi".into())));
    }

    // ---------- 推理模型的历史回传 ----------

    fn assistant_with_thinking(thinking: Option<&str>) -> ChatMessage {
        let mut content = Vec::new();
        if let Some(t) = thinking {
            content.push(Block::Thinking { thinking: t.into() });
        }
        content.push(Block::ToolUse {
            id:    "call_1".into(),
            name:  "read".into(),
            input: serde_json::json!({ "path": "a.rs" }),
        });
        ChatMessage {
            id: "a1".into(),
            parent_id: Some("u1".into()),
            role: Role::Assistant,
            content,
            created_at: 0,
            model: None,
            usage: None,
        }
    }

    /// 推理接口（DeepSeek thinking 模式）要求带 tool_calls 的 assistant 消息
    /// 把思维链原样带回，缺失时整轮请求被拒。
    #[test]
    fn test_openai_echoes_reasoning_content_for_thinking_models() {
        let messages = vec![user_text("q").remove(0), assistant_with_thinking(Some("先读文件"))];
        let built = build_openai_messages(&messages, None, true);
        assert_eq!(built[1]["reasoning_content"], "先读文件");
        assert_eq!(built[1]["tool_calls"][0]["id"], "call_1");
    }

    /// 没记录到思维链时补空串：校验只要求字段存在。
    #[test]
    fn test_openai_reasoning_content_placeholder_without_recorded_thinking() {
        let messages = vec![user_text("q").remove(0), assistant_with_thinking(None)];
        let built = build_openai_messages(&messages, None, true);
        assert_eq!(built[1]["reasoning_content"], "");
    }

    /// 未声明 thinking 能力的模型不下发该字段：多数兼容接口并不认识它。
    #[test]
    fn test_openai_reasoning_content_skipped_for_non_thinking_models() {
        let messages = vec![user_text("q").remove(0), assistant_with_thinking(Some("先读文件"))];
        let built = build_openai_messages(&messages, None, false);
        assert!(built[1].get("reasoning_content").is_none(), "{built:#?}");
    }

    /// 思维链输出到一半被打断时，历史里留下的是既无正文也无工具调用的
    /// assistant 消息。此时必须补空串 `content`，否则下一次发言整轮被上游拒
    /// （`Invalid assistant message: content or tool_calls must be set`）。
    #[test]
    fn test_openai_fills_content_for_interrupted_thinking() {
        let interrupted = ChatMessage {
            id:         "a1".into(),
            parent_id:  Some("u1".into()),
            role:       Role::Assistant,
            content:    vec![Block::Thinking {
                thinking: "想到一半就被打断".into(),
            }],
            created_at: 0,
            model:      None,
            usage:      None,
        };
        let messages = vec![user_text("q").remove(0), interrupted, user_text("继续").remove(0)];

        let built = build_openai_messages(&messages, None, true);
        assert_eq!(built[1]["content"], "");
        assert_eq!(built[1]["reasoning_content"], "想到一半就被打断");
        assert!(built[1].get("tool_calls").is_none(), "{built:#?}");
    }
}
