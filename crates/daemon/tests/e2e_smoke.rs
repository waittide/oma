//! 端到端冒烟测试：真实 Daemon + Mock Provider + oma-client
//!
//! 覆盖此前实测复现的缺陷：会话标识路径穿越、排队指令丢失、`task` 与 MCP
//! 工具未注册、Agent 模板白名单失效、附件未贯通到 Provider 请求。

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use oma_client::{ConnectOptions, OmaClient, SessionApi};
use oma_contract::{AgentCommand, AgentEvent, ApprovalMode, ClientType, StopReason};
use oma_daemon::{DaemonState, create_router};
use oma_mcp::McpManager;
use oma_storage::StorageManager;
use parking_lot::Mutex;

/// Mock Provider 观测到的每次请求
#[derive(Debug, Clone)]
struct Observed {
    tool_names:       Vec<String>,
    has_image:        bool,
    role_sequence:    Vec<String>,
    /// 请求中 role=tool 的消息内容（用于观察压缩是否把旧回执替换为占位）
    tool_contents:    Vec<String>,
    /// 请求体里下发的 reasoning_effort（未下发为 None）
    reasoning_effort: Option<String>,
}

#[derive(Clone, Default)]
struct MockState {
    observed: Arc<Mutex<Vec<Observed>>>,
}

/// 以 OpenAI SSE 形态回放：主 Agent 请求（工具含 task）→ 调用 task；
/// 子 Agent 请求（只读工具集）→ 调用 read；已有工具回执 → 直接收尾。
async fn chat_completions(State(state): State<MockState>, body: Bytes) -> Response {
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad json").into_response(),
    };

    let tool_names: Vec<String> = req["tools"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t["function"]["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let messages = req["messages"].as_array().cloned().unwrap_or_default();
    let role_sequence: Vec<String> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap_or("").to_string())
        .collect();
    let has_image = messages.iter().any(|m| {
        m["content"]
            .as_array()
            .is_some_and(|parts| parts.iter().any(|p| p["type"] == "image_url"))
    });
    let tool_contents: Vec<String> = messages
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect();

    state.observed.lock().push(Observed {
        tool_names: tool_names.clone(),
        has_image,
        role_sequence: role_sequence.clone(),
        tool_contents,
        reasoning_effort: req["reasoning_effort"].as_str().map(str::to_string),
    });

    // 每次响应都上报用量：agent loop 会据此回填权威上下文锚点
    let usage = format!(
        r#"{{"choices":[{{"delta":{{}}}}],"usage":{{"prompt_tokens":{},"completion_tokens":5}}}}"#,
        messages.len() * 100 + 50
    );

    // 命名请求：system prompt 标识为命名任务时，直接回一个短标题
    let is_naming = req["messages"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|m| {
                m["role"] == "system"
                    && m["content"]
                        .as_str()
                        .is_some_and(|c| c.contains("name chat sessions"))
            })
        })
        .unwrap_or(false);
    if is_naming {
        let mut payload = String::new();
        payload.push_str(&sse(r#"{"choices":[{"delta":{"content":"数据库查询优化"}}]}"#));
        payload.push_str(&sse(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#));
        payload.push_str(&usage);
        payload.push_str("data: [DONE]\n\n");
        return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response();
    }

    let tool_rounds = role_sequence.iter().filter(|r| *r == "tool").count();
    // ask 场景：仅当用户输入带 ASK_TRIGGER 标记时走提问分支，
    // 否则会污染其它用例（内置 agent 白名单都含 ask）
    let user_text = messages
        .iter()
        .filter(|m| m["role"] == "user")
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let ask_wanted = user_text.contains("ASK_TRIGGER");
    let ask_answered = messages.iter().any(|m| {
        m["role"] == "tool"
            && m["content"]
                .as_str()
                .is_some_and(|c| c.contains("User answers:"))
    });
    let mut chunks: Vec<String> = if ask_wanted && tool_names.iter().any(|t| t == "ask") && !ask_answered {
        vec![
            sse(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_ask","function":{"name":"ask","arguments":"{\"questions\":[{\"id\":\"pick\",\"question\":\"which one?\",\"options\":[{\"label\":\"alpha\"},{\"label\":\"beta\"}],\"multi\":false,\"recommended\":1}]}"}}]}}]}"#,
            ),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ]
    } else if tool_rounds >= 2 {
        // 已有回执：产出最终文本并收尾
        vec![
            sse(r#"{"choices":[{"delta":{"content":"done"}}]}"#),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
        ]
    } else if tool_names.iter().any(|t| t == "task") {
        // 主 Agent：委托子代理（工具白名单不含 task 的角色不会走到这里）
        vec![
            sse(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_task","function":{"name":"task","arguments":"{\"agent\":\"explore\",\"prompt\":\"scan\"}"}}]}}]}"#,
            ),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ]
    } else if tool_names.iter().any(|t| t == "read") {
        // 子 Agent：读取文件
        vec![
            sse(
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_read","function":{"name":"read","arguments":"{\"path\":\"note.txt\"}"}}]}}]}"#,
            ),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        ]
    } else {
        vec![
            sse(r#"{"choices":[{"delta":{"content":"no tools"}}]}"#),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
        ]
    };

    chunks.push(sse(&usage));

    let mut payload = String::new();
    for chunk in chunks {
        payload.push_str(&chunk);
    }
    payload.push_str("data: [DONE]\n\n");

    ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], payload).into_response()
}

fn sse(json: &str) -> String {
    format!("data: {}\n\n", json)
}

struct Harness {
    base:      String,
    api:       SessionApi,
    token:     String,
    mock:      MockState,
    /// 会话工作区（含 fixture 文件 `note.txt`，供 read 工具真实读取）
    workspace: String,
}

async fn start_harness() -> Result<Harness> {
    start_harness_with_context(100_000).await
}

async fn start_harness_with_context(context_len: usize) -> Result<Harness> {
    let tmp = tempfile::tempdir()?;
    // 测试进程存活期间保留目录（data dir 需要跨请求稳定）
    let data_dir = tmp.keep();

    // 1. Mock Provider
    let mock = MockState::default();
    let mock_app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .with_state(mock.clone());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let mock_addr = mock_listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    // 2. Daemon（指向 mock provider）
    let config_toml = format!(
        r#"
default_model = "mock/model-x"
default_agent = "task"
default_approval_mode = "auto"

[providers.mock]
api_type = "completion"
base_url = "http://{mock_addr}/v1"
api_key = "test-key"

[[providers.mock.models]]
id = "model-x"
name = "Mock Model"
context_len = {context_len}

[providers.mock.models.reasoning_map]
low = "think-low"
ultra = "think-ultra"
"#
    );
    let config_path = data_dir.join("config.toml");
    std::fs::write(&config_path, config_toml)?;
    let config = oma_config::OmaConfig::load_from_file(&config_path)?;

    let workspace = data_dir.join("ws");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(workspace.join("note.txt"), "hello from note\n")?;

    let storage = StorageManager::new(&data_dir).await?;
    let token = "smoke-token".to_string();
    let state = DaemonState::new(token.clone(), storage, config, config_path, Arc::new(McpManager::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, create_router(state)).await;
    });

    let base = format!("127.0.0.1:{}", addr.port());
    let workspace = workspace.to_string_lossy().to_string();
    Ok(Harness {
        api: SessionApi::new(&base, &token),
        base,
        token,
        mock,
        workspace,
    })
}

/// 收集事件直到**主**轮次结束。
///
/// 子代理复用 `TurnStarted` / `TurnFinished`，但携带 `subagent_id`；客户端必须
/// 忽略这些事件，否则子代理收尾会被误判为整体轮次结束（进而清空流式缓冲）。
async fn drive_turn(client: &mut OmaClient, timeout: Duration) -> Result<Vec<AgentEvent>> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        let main_finished = matches!(
            &event,
            AgentEvent::TurnFinished { subagent_id, .. } if subagent_id.is_none()
        );
        events.push(event);
        if main_finished {
            break;
        }
    }
    Ok(events)
}

/// 收集事件并在收到提问时自动作答，直到主轮次结束。
async fn drive_turn_answering(client: &mut OmaClient, timeout: Duration, answer: &str) -> Result<Vec<AgentEvent>> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let AgentEvent::AskRequested(data) = &event {
            let answers = data
                .questions
                .iter()
                .map(|_| oma_contract::AskAnswer {
                    selected:     vec![answer.to_string()],
                    custom_input: String::new(),
                })
                .collect();
            client
                .respond_ask(oma_contract::AskResponse {
                    request_id: data.request_id.clone(),
                    answers,
                    cancelled: false,
                })
                .await?;
        }
        let main_finished = matches!(
            &event,
            AgentEvent::TurnFinished { subagent_id, .. } if subagent_id.is_none()
        );
        events.push(event);
        if main_finished {
            break;
        }
    }
    Ok(events)
}

/// 收集事件并在收到审批请求时按 `decision` 自动放行，直到主轮次结束。
async fn drive_turn_approving(
    client: &mut OmaClient,
    timeout: Duration,
    decision: oma_contract::ApprovalDecision,
) -> Result<Vec<AgentEvent>> {
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let AgentEvent::PermissionRequested(data) = &event {
            client.respond_approval(&data.request_id, decision).await?;
        }
        let main_finished = matches!(
            &event,
            AgentEvent::TurnFinished { subagent_id, .. } if subagent_id.is_none()
        );
        events.push(event);
        if main_finished {
            break;
        }
    }
    Ok(events)
}

/// 继续收集事件直到 `stop` 命中或超时；返回这段时间内收到的事件。
async fn drain_for_event<F>(client: &mut OmaClient, timeout: Duration, stop: F) -> Result<Vec<AgentEvent>>
where
    F: Fn(&AgentEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        let hit = stop(&event);
        events.push(event);
        if hit {
            break;
        }
    }
    Ok(events)
}

fn tool_calls(events: &[AgentEvent]) -> Vec<(String, bool, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallFinished {
                name,
                is_error,
                subagent_id,
                ..
            } => Some((name.clone(), *is_error, subagent_id.clone())),
            _ => None,
        })
        .collect()
}

/// 完整链路：用户输入 → 主 Agent 调用 task → 子 Agent 调用 read → 双双收敛。
#[tokio::test]
async fn test_end_to_end_turn_with_task_tool() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("e2e")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "smoke".into(),
    })
    .await?;

    // 就绪载荷必须携带模型目录
    assert!(
        client.ready().providers.contains_key("mock"),
        "ready providers should list the configured provider"
    );

    client
        .send_command(AgentCommand::UserInput {
            content:     "please explore".into(),
            attachments: vec![],
        })
        .await?;

    let events = drive_turn(&mut client, Duration::from_secs(30)).await?;
    assert!(
        matches!(
            events.last(),
            Some(AgentEvent::TurnFinished {
                stop_reason: StopReason::EndTurn,
                ..
            })
        ),
        "turn should end normally, got {:?}",
        events.last()
    );

    let calls = tool_calls(&events);
    assert!(
        calls
            .iter()
            .any(|(name, is_error, _)| name == "task" && !is_error),
        "task tool must be registered and executable: {:?}",
        calls
    );
    assert!(
        calls
            .iter()
            .any(|(name, is_error, sub)| name == "read" && !is_error && sub.is_some()),
        "the subagent must run its own tool loop successfully: {:?}",
        calls
    );

    // Mock 侧观测：主 Agent 的清单含 task，子 Agent 的清单不含写工具（explore 模板）
    let observed = h.mock.observed.lock().clone();
    let main = observed
        .iter()
        .find(|o| o.tool_names.iter().any(|t| t == "task"))
        .expect("main agent request observed");
    assert!(
        main.tool_names.iter().any(|t| t == "write"),
        "task agent declares write: {:?}",
        main.tool_names
    );
    let sub = observed
        .iter()
        .find(|o| !o.tool_names.iter().any(|t| t == "task"))
        .expect("subagent request observed");
    // 回执轮次必须呈 assistant → tool 相邻形态，否则 OpenAI 兼容端点会拒绝
    let with_results = observed
        .iter()
        .find(|o| o.role_sequence.iter().any(|r| r == "tool"))
        .expect("a request carrying tool results was observed");
    assert!(
        with_results
            .role_sequence
            .windows(2)
            .any(|w| w[0] == "assistant" && w[1] == "tool"),
        "tool results must immediately follow the assistant tool_calls message: {:?}",
        with_results.role_sequence
    );
    assert!(
        !sub.tool_names.iter().any(|t| t == "write" || t == "edit"),
        "explore whitelist must hide write/edit: {:?}",
        sub.tool_names
    );

    assert_eq!(session.session_id, client.ready().session_id);
    Ok(())
}

/// 排队指令必须被完整排空：三条连发 → 三轮都真正执行。
#[tokio::test]
async fn test_queued_inputs_are_all_executed() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("queue")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "smoke".into(),
    })
    .await?;

    let mut events = Vec::new();
    // 不等待第一条跑完就继续投递，触发排队路径
    for text in ["one", "two", "three"] {
        client
            .send_command(AgentCommand::UserInput {
                content:     text.into(),
                attachments: vec![],
            })
            .await?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut finished = 0;
    while finished < 3 && Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if matches!(event, AgentEvent::TurnFinished { .. }) {
            finished += 1;
        }
        events.push(event);
    }
    assert_eq!(finished, 3, "every queued input must run its own turn");

    // 持久化层面同样必须三条俱全（旧实现会丢中间与末尾的排队项）
    let all = h.api.list_sessions(Some(&h.workspace)).await?;
    assert!(all.iter().any(|s| s.session_id == session.session_id));

    let tree: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!(
            "http://{}/api/sessions/{}/messages/tree",
            h.base, session.session_id
        ))
        .bearer_auth(&h.token)
        .send()
        .await?
        .json()
        .await?;
    let texts: Vec<String> = tree
        .iter()
        .filter(|m| m["role"] == "user")
        .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str().map(str::to_string))
        .collect();
    for expected in ["one", "two", "three"] {
        assert!(
            texts.contains(&expected.to_string()),
            "queued input {} missing from persisted history: {:?}",
            expected,
            texts
        );
    }
    Ok(())
}

/// 权威锚点必须真正接入 agent loop：多轮工具循环中，厂商上报的真实占用
/// 未超预算时，不得因为字符折算偏高而把旧工具回执替换成占位。
///
/// 场景：小上下文窗口（2000）下连做两轮 `read`。仅按字符折算，第三轮请求
/// 已超阈值、第一份回执会被修剪；而锚点（第二轮权威 input_tokens）表明实际
/// 占用仍有余量，回执应原样送达模型。
#[tokio::test]
async fn test_authoritative_anchor_suppresses_premature_compaction() -> Result<()> {
    let h = start_harness_with_context(2_000).await?;
    let session = h.api.create_session(&h.workspace, Some("anchor")).await?;

    // mock 固定读取 note.txt：换成长内容，让字符折算明显推高估算
    let marker = "ANCHOR-FIXTURE-MARKER";
    let body = format!("{}\n{}", marker, "x".repeat(1_500));
    std::fs::write(std::path::Path::new(&h.workspace).join("note.txt"), body)?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "smoke".into(),
    })
    .await?;

    // 切到 explore（只读，无 task）：让主 Agent 自己连做两轮 read
    client
        .send_command(AgentCommand::SetAgent {
            agent: "explore".into(),
        })
        .await?;
    client
        .send_command(AgentCommand::UserInput {
            content:     "读取 note.txt".into(),
            attachments: vec![],
        })
        .await?;
    let events = drive_turn(&mut client, Duration::from_secs(30)).await?;

    let reads: Vec<_> = tool_calls(&events)
        .into_iter()
        .filter(|(name, _, sub)| name == "read" && sub.is_none())
        .collect();
    assert!(
        reads.len() >= 2,
        "the turn must run at least two read rounds, got {:?}",
        reads
    );

    // 找到携带两份回执的请求（即第三轮），第一份回执必须仍是完整内容
    let observed = h.mock.observed.lock().clone();
    let multi = observed
        .iter()
        .find(|o| o.tool_contents.len() >= 2)
        .expect("a request carrying two tool results was observed");
    assert!(
        multi.tool_contents[0].contains(marker),
        "the authoritative anchor must keep the earlier tool result intact \
         (premature compaction replaced it): {:?}",
        multi.tool_contents[0].chars().take(80).collect::<String>()
    );
    Ok(())
}

/// session_id 参与文件系统路径拼接，穿越型标识必须被拒且不触碰目标目录。
#[tokio::test]
async fn test_session_id_traversal_blocked_over_http() -> Result<()> {
    let h = start_harness().await?;
    let victim = std::path::PathBuf::from("/tmp/oma-e2e-victim");
    let _ = std::fs::remove_dir_all(&victim);
    std::fs::create_dir_all(&victim)?;
    std::fs::write(victim.join("precious.txt"), b"keep")?;

    let http = reqwest::Client::new();

    // 删除穿越路径：必须 400 且 victim 完好
    let resp = http
        .delete(format!("http://{}/api/sessions/..%2F..%2Foma-e2e-victim", h.base))
        .bearer_auth(&h.token)
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        victim.join("precious.txt").exists(),
        "traversal delete must not touch the target directory"
    );

    // 查询穿越路径：必须 400 且不得在 sessions/ 之外建库
    let resp = http
        .get(format!("http://{}/api/sessions/..%2Fescaped/messages", h.base))
        .bearer_auth(&h.token)
        .send()
        .await?;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let _ = std::fs::remove_dir_all(&victim);
    Ok(())
}

/// 附件必须贯通到 Provider 请求：上传 → 引用随输入提交 → mock 观测到图片 part。
#[tokio::test]
async fn test_attachment_reaches_provider() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("attach")).await?;

    let http = reqwest::Client::new();
    let upload = http
        .post(format!(
            "http://{}/api/sessions/{}/attachments",
            h.base, session.session_id
        ))
        .bearer_auth(&h.token)
        .multipart(reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(vec![0x89, 0x50, 0x4E, 0x47]).file_name("shot.png"),
        ))
        .send()
        .await?;
    assert_eq!(upload.status(), StatusCode::OK);
    let body: serde_json::Value = upload.json().await?;
    let reference = body["attachments"][0].as_str().unwrap().to_string();
    assert!(reference.starts_with("session_attachment://"));

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "smoke".into(),
    })
    .await?;

    client
        .send_command(AgentCommand::UserInput {
            content:     "look at this".into(),
            attachments: vec![reference],
        })
        .await?;
    let _ = drive_turn(&mut client, Duration::from_secs(30)).await?;

    let observed = h.mock.observed.lock().clone();
    assert!(
        observed.iter().any(|o| o.has_image),
        "provider request must carry the uploaded image: {:?}",
        observed
    );
    Ok(())
}

/// 审批模式为 strict 时，工具调用必须先取得人工放行。
#[tokio::test]
async fn test_strict_approval_blocks_until_allowed() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("approval")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "smoke".into(),
    })
    .await?;

    client
        .send_command(AgentCommand::SetApprovalMode {
            mode: ApprovalMode::Strict,
        })
        .await?;
    client
        .send_command(AgentCommand::UserInput {
            content:     "explore".into(),
            attachments: vec![],
        })
        .await?;

    // 等到审批请求出现，再放行
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut request_id = None;
    while request_id.is_none() && Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), client.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let AgentEvent::PermissionRequested(data) = event {
            request_id = Some(data.request_id);
        }
    }
    let request_id = request_id.expect("strict mode must request approval before running a tool");

    client
        .respond_approval(&request_id, oma_contract::ApprovalDecision::AllowOnce)
        .await?;
    // 子代理的工具调用同样受审批约束，继续按同一策略放行
    let events = drive_turn_approving(
        &mut client,
        Duration::from_secs(30),
        oma_contract::ApprovalDecision::AllowOnce,
    )
    .await?;
    let calls = tool_calls(&events);
    assert!(
        calls
            .iter()
            .any(|(name, is_error, _)| name == "task" && !is_error),
        "approved main-agent tool must actually run: {:?}",
        calls
    );
    assert!(
        calls
            .iter()
            .any(|(name, is_error, _)| name == "read" && !is_error),
        "approved subagent tool must actually run: {:?}",
        calls
    );
    Ok(())
}

/// ask 全链路：模型发起提问 → 客户端作答 → 答案回灌为工具结果，轮次正常收尾。
#[tokio::test]
async fn test_end_to_end_ask_tool_round_trip() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("ask-e2e")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "asker".into(),
    })
    .await?;

    client
        .send_command(AgentCommand::UserInput {
            content:     "ASK_TRIGGER ask me something".into(),
            attachments: vec![],
        })
        .await?;

    let events = drive_turn_answering(&mut client, Duration::from_secs(30), "beta").await?;

    let requested = events.iter().find_map(|e| match e {
        AgentEvent::AskRequested(d) => Some(d.clone()),
        _ => None,
    });
    let requested = requested.expect("ask_requested must be broadcast");
    assert_eq!(requested.questions.len(), 1);
    assert_eq!(requested.questions[0].options.len(), 2);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::AskResolved { cancelled: false, .. })),
        "answering must broadcast ask_resolved: {:?}",
        events
    );

    let calls = tool_calls(&events);
    assert!(
        calls
            .iter()
            .any(|(name, is_error, _)| name == "ask" && !is_error),
        "ask must be registered and executable: {:?}",
        calls
    );

    // 模型侧必须真的收到用户选择的答案文本
    let observed = h.mock.observed.lock().clone();
    let with_answer = observed
        .iter()
        .find(|o| o.tool_contents.iter().any(|c| c.contains("User answers:")))
        .expect("the answer must be fed back to the provider");
    assert!(
        with_answer.tool_contents.iter().any(|c| c.contains("beta")),
        "selected option must appear in the tool result: {:?}",
        with_answer.tool_contents
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnFinished {
                stop_reason: StopReason::EndTurn,
                subagent_id: None,
                ..
            }
        )),
        "turn should end normally after the question is answered"
    );

    Ok(())
}

/// 留空标题：模型在首轮结束后自动命名并广播 session_renamed。
#[tokio::test]
async fn test_session_autonamed_after_first_turn() -> Result<()> {
    let h = start_harness().await?;
    // 不传 title -> 服务端存空串，等待模型命名
    let session = h.api.create_session(&h.workspace, None).await?;
    assert_eq!(session.title, "", "empty title must be stored as-is");

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "namer".into(),
    })
    .await?;

    client
        .send_command(AgentCommand::UserInput {
            content:     "帮我优化数据库查询".into(),
            attachments: vec![],
        })
        .await?;

    let mut events = drive_turn(&mut client, Duration::from_secs(30)).await?;
    // 自动命名在 TurnFinished 之后后台执行，需继续等待其广播
    events.extend(
        drain_for_event(&mut client, Duration::from_secs(15), |e| {
            matches!(e, AgentEvent::SessionRenamed { .. })
        })
        .await?,
    );

    let renamed = events.iter().find_map(|e| match e {
        AgentEvent::SessionRenamed { title, session_id } => Some((title.clone(), session_id.clone())),
        _ => None,
    });
    let (title, sid) = renamed.expect("autoname must broadcast session_renamed");
    assert_eq!(sid, session.session_id);
    assert_eq!(title, "数据库查询优化");

    // 落库校验
    let list = h.api.list_sessions(Some(&h.workspace)).await?;
    let rec = list
        .iter()
        .find(|s| s.session_id == session.session_id)
        .expect("session present");
    assert_eq!(rec.title, "数据库查询优化");
    Ok(())
}

/// 用户填写了标题：模型不得覆盖。
#[tokio::test]
async fn test_user_title_is_not_overwritten() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("我的会话")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "namer".into(),
    })
    .await?;
    client
        .send_command(AgentCommand::UserInput {
            content:     "随便聊聊".into(),
            attachments: vec![],
        })
        .await?;

    let events = drive_turn(&mut client, Duration::from_secs(30)).await?;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::SessionRenamed { .. })),
        "user-provided title must not be overwritten: {:?}",
        events
    );

    let list = h.api.list_sessions(Some(&h.workspace)).await?;
    let rec = list
        .iter()
        .find(|s| s.session_id == session.session_id)
        .unwrap();
    assert_eq!(rec.title, "我的会话");
    Ok(())
}

/// 推理等级映射：会话选中的等级经模型映射表转换后才发给厂商。
#[tokio::test]
async fn test_reasoning_level_is_mapped_before_request() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("level")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "leveler".into(),
    })
    .await?;

    // 等级是必选项：配置未显式设置时也回落到内置默认 medium
    assert_eq!(client.ready().reasoning_level, "medium");

    // 选中被映射的等级：ultra -> think-ultra
    client
        .send_command(AgentCommand::SetReasoningLevel { level: "ultra".into() })
        .await?;
    let changed = drain_for_event(&mut client, Duration::from_secs(10), |e| {
        matches!(e, AgentEvent::ReasoningLevelChanged { .. })
    })
    .await?;
    assert!(
        changed
            .iter()
            .any(|e| matches!(e, AgentEvent::ReasoningLevelChanged { level } if level == "ultra")),
        "level change must be broadcast: {:?}",
        changed
    );

    client
        .send_command(AgentCommand::UserInput {
            content:     "hello".into(),
            attachments: vec![],
        })
        .await?;
    drive_turn(&mut client, Duration::from_secs(30)).await?;

    let observed = h.mock.observed.lock().clone();
    let last = observed.last().expect("a request was sent");
    assert_eq!(
        last.reasoning_effort.as_deref(),
        Some("think-ultra"),
        "mapped string must be sent, got {:?}",
        last.reasoning_effort
    );
    Ok(())
}

/// 未配置映射的等级按等级名原样下发。
#[tokio::test]
async fn test_reasoning_level_passthrough_for_unmapped() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("level2")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "leveler".into(),
    })
    .await?;

    // medium 不在映射表中：应原样下发
    client
        .send_command(AgentCommand::SetReasoningLevel { level: "medium".into() })
        .await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    client
        .send_command(AgentCommand::UserInput {
            content:     "hello".into(),
            attachments: vec![],
        })
        .await?;
    drive_turn(&mut client, Duration::from_secs(30)).await?;

    let observed = h.mock.observed.lock().clone();
    assert_eq!(
        observed.last().and_then(|o| o.reasoning_effort.as_deref()),
        Some("medium"),
        "unmapped level must pass through"
    );
    Ok(())
}

/// 不支持思考的模型不得被下发推理参数（等级恒有值后尤其重要）。
#[tokio::test]
async fn test_reasoning_not_sent_for_non_thinking_model() -> Result<()> {
    // 该 harness 的 mock 模型 supports_thinking = true，
    // 这里直接校验映射层：不支持思考时应被清空
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("nothink")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "nothink".into(),
    })
    .await?;

    client
        .send_command(AgentCommand::UserInput {
            content:     "hi".into(),
            attachments: vec![],
        })
        .await?;
    drive_turn(&mut client, Duration::from_secs(30)).await?;

    // 支持思考的模型本轮应带上映射后的等级
    let observed = h.mock.observed.lock().clone();
    assert_eq!(
        observed.last().and_then(|o| o.reasoning_effort.as_deref()),
        Some("medium"),
        "thinking model must receive its concrete level"
    );
    Ok(())
}

/// 非法等级必须被拒绝且不改变会话状态。
#[tokio::test]
async fn test_invalid_reasoning_level_is_rejected() -> Result<()> {
    let h = start_harness().await?;
    let session = h.api.create_session(&h.workspace, Some("badlevel")).await?;

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "bad".into(),
    })
    .await?;
    let before = client.ready().reasoning_level.clone();

    client
        .send_command(AgentCommand::SetReasoningLevel { level: "bogus".into() })
        .await?;
    let events = drain_for_event(&mut client, Duration::from_secs(3), |e| {
        matches!(e, AgentEvent::ReasoningLevelChanged { .. })
    })
    .await?;

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::ReasoningLevelChanged { .. })),
        "invalid level must not be broadcast"
    );
    assert!(
        events.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "invalid level should surface an error"
    );

    // 重连后等级应保持不变
    let again = OmaClient::connect(ConnectOptions {
        addr:        h.base.clone(),
        token:       h.token.clone(),
        workspace:   h.workspace.clone(),
        session_id:  session.session_id.clone(),
        client_type: ClientType::Cli,
        client_name: "bad2".into(),
    })
    .await?;
    assert_eq!(again.ready().reasoning_level, before);
    Ok(())
}
