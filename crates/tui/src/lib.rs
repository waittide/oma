//! oma-tui: 基于 Ratatui 的终端客户端
//!
//! 由 `oma tui` 子命令驱动：连接 Daemon 后实时渲染流式文本、思考块、
//! 工具调用与权限审批。入口为 [`run`]，复用调用方的 tokio 运行时。

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use oma_client::{ConnectOptions, OmaClient, SessionApi};
use oma_contract::{AgentCommand, AgentEvent, ApprovalDecision, ClientType, StopReason};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
};
use tokio::sync::mpsc;

/// 单行输入区高度（含边框）
const INPUT_HEIGHT: u16 = 3;
/// 内容区最多保留的行数，超出后丢弃最早的行
const MAX_LINES: usize = 4000;
/// 无事件时的重绘间隔（保持状态栏与光标响应）
const TICK: Duration = Duration::from_millis(80);

/// 一条已渲染的内容行
struct Entry {
    prefix: &'static str,
    text:   String,
    style:  Style,
}

/// 待响应的审批请求
struct PendingApproval {
    request_id: String,
    name:       String,
    summary:    String,
}

struct App {
    workspace: String,
    model:     String,
    agent:     String,
    connected: bool,
    busy:      bool,
    queue:     usize,
    status:    String,
    entries:   Vec<Entry>,
    input:     String,
    scroll:    u16,
    stick:     bool,
    approval:  Option<PendingApproval>,
}

impl App {
    fn new(workspace: String, model: String, agent: String) -> Self {
        Self {
            workspace,
            model,
            agent,
            connected: true,
            busy: false,
            queue: 0,
            status: "就绪".into(),
            entries: Vec::new(),
            input: String::new(),
            scroll: 0,
            stick: true,
            approval: None,
        }
    }

    fn push(&mut self, prefix: &'static str, text: impl Into<String>, style: Style) {
        self.entries.push(Entry {
            prefix,
            text: text.into(),
            style,
        });
        if self.entries.len() > MAX_LINES {
            let drop = self.entries.len() - MAX_LINES;
            self.entries.drain(..drop);
        }
    }

    /// 追加流式增量：与最后一行同类型则续写，否则新起一行
    fn append_stream(&mut self, prefix: &'static str, delta: &str, style: Style) {
        if let Some(last) = self.entries.last_mut() {
            if last.prefix == prefix && style == last.style {
                last.text.push_str(delta);
                return;
            }
        }
        self.push(prefix, delta, style);
    }

    /// 按可用宽度把内容行折成渲染行
    fn wrapped_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for entry in &self.entries {
            let prefix = format!("{} ", entry.prefix);
            let pad = prefix.len();
            for (i, chunk) in wrap_text(&entry.text, width.saturating_sub(pad).max(8))
                .into_iter()
                .enumerate()
            {
                let head = if i == 0 { prefix.clone() } else { " ".repeat(pad) };
                lines.push(Line::from(vec![Span::styled(head, entry.style), Span::raw(chunk)]));
            }
        }
        lines
    }
}

/// 按显示宽度折行（ASCII 记 1，其余按 2 计），尽量在空白处断开
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut used = 0usize;
        let mut last_space: Option<usize> = None;
        for ch in raw.chars() {
            let w = if ch.is_ascii() { 1 } else { 2 };
            if used + w > width && !current.is_empty() {
                match last_space.take() {
                    Some(pos) if pos > 0 => {
                        let rest = current.split_off(pos);
                        out.push(std::mem::take(&mut current));
                        current = rest.trim_start().to_string();
                    }
                    _ => out.push(std::mem::take(&mut current)),
                }
                used = display_width(&current) + w;
            } else {
                used += w;
            }
            if ch == ' ' {
                last_space = Some(current.len());
            }
            current.push(ch);
        }
        out.push(current);
    }
    out
}

/// 终端显示宽度（CJK 等宽字符按 2 列计）
fn display_width(text: &str) -> usize {
    text.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

fn truncate(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = if ch.is_ascii() { 1 } else { 2 };
        if used + w > width.saturating_sub(1) {
            break;
        }
        used += w;
        out.push(ch);
    }
    out.push('…');
    out
}

fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// 工具入参压缩为单行摘要
fn summarize_tool_input(input: &serde_json::Value) -> String {
    let raw = input
        .get("command")
        .or_else(|| input.get("path"))
        .or_else(|| input.get("file_path"))
        .or_else(|| input.get("pattern"))
        .or_else(|| input.get("prompt"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| input.to_string());
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > 80 {
        format!("{}…", flat.chars().take(80).collect::<String>())
    } else {
        flat
    }
}

/// 启动 TUI 客户端：复用当前工作区最近的会话，没有则新建。
pub async fn run(addr: &str, token: &str, workspace: &str) -> Result<()> {
    let api = SessionApi::new(addr, token);
    let record = match api.list_sessions(Some(workspace)).await?.into_iter().next() {
        Some(existing) => existing,
        None => api
            .create_session(workspace, None)
            .await
            .with_context(|| format!("Failed to create a session for {}", workspace))?,
    };

    let mut client = OmaClient::connect(ConnectOptions {
        addr:        addr.to_string(),
        token:       token.to_string(),
        workspace:   workspace.to_string(),
        session_id:  record.session_id.clone(),
        client_type: ClientType::Tui,
        client_name: "oma-tui".into(),
    })
    .await?;

    let mut app = {
        let ready = client.ready();
        let mut app = App::new(
            ready.workspace.clone(),
            ready.active_model.clone(),
            ready.active_agent.clone(),
        );
        app.push(
            "·",
            format!("会话 {} 已连接", short_id(&ready.session_id)),
            Style::default().fg(Color::DarkGray),
        );
        if let Some(leaf) = &ready.current_leaf_id {
            app.push(
                "·",
                format!("当前分支叶子 {}", short_id(leaf)),
                Style::default().fg(Color::DarkGray),
            );
        }
        app
    };

    // crossterm 的阻塞读放在独立线程，主循环只处理两路事件
    let (key_tx, mut key_rx) = mpsc::channel::<KeyEvent>(64);
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if let Event::Key(key) = ev {
                if key_tx.blocking_send(key).is_err() {
                    break;
                }
            }
        }
    });

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &mut client, &mut key_rx).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    client: &mut OmaClient,
    key_rx: &mut mpsc::Receiver<KeyEvent>,
) -> Result<()> {
    let mut last_draw = Instant::now();
    loop {
        if last_draw.elapsed() >= TICK {
            terminal.draw(|frame| draw(frame, app))?;
            last_draw = Instant::now();
        }

        tokio::select! {
            event = client.next_event() => match event {
                Some(event) => {
                    apply_event(app, event);
                    terminal.draw(|frame| draw(frame, app))?;
                    last_draw = Instant::now();
                }
                None => {
                    app.connected = false;
                    app.status = "连接已断开".into();
                    terminal.draw(|frame| draw(frame, app))?;
                    return Ok(());
                }
            },
            key = key_rx.recv() => {
                let Some(key) = key else { return Ok(()) };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if handle_key(app, client, key).await? {
                    return Ok(());
                }
                terminal.draw(|frame| draw(frame, app))?;
                last_draw = Instant::now();
            }
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

/// 处理按键；返回 true 表示退出
async fn handle_key(app: &mut App, client: &mut OmaClient, key: KeyEvent) -> Result<bool> {
    // 审批弹窗优先消费按键
    if let Some(pending) = &app.approval {
        let decision = match key.code {
            KeyCode::Char('y') | KeyCode::Enter => Some(ApprovalDecision::AllowOnce),
            KeyCode::Char('a') => Some(ApprovalDecision::AllowSession),
            KeyCode::Char('n') | KeyCode::Esc => Some(ApprovalDecision::Deny),
            _ => None,
        };
        if let Some(decision) = decision {
            let request_id = pending.request_id.clone();
            let name = pending.name.clone();
            app.approval = None;
            client.respond_approval(&request_id, decision).await?;
            app.push(
                "·",
                format!("审批 {} → {}", name, decision_label(decision)),
                Style::default().fg(Color::Yellow),
            );
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Esc => return Ok(true),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(true),
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_sub(10);
            app.stick = false;
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(10);
            app.stick = true;
        }
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => app.input.clear(),
        KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            client.cancel().await?;
            app.push("·", "已请求中止", Style::default().fg(Color::Yellow));
        }
        KeyCode::Enter => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return Ok(false);
            }
            app.input.clear();
            app.push("你", text.clone(), Style::default().fg(Color::Cyan));
            app.stick = true;
            if let Err(e) = client
                .send_command(AgentCommand::UserInput {
                    content:     text,
                    attachments: Vec::new(),
                })
                .await
            {
                app.push("!", format!("发送失败: {}", e), Style::default().fg(Color::Red));
            }
        }
        KeyCode::Char(c) => app.input.push(c),
        _ => {}
    }
    Ok(false)
}

fn decision_label(decision: ApprovalDecision) -> &'static str {
    match decision {
        ApprovalDecision::AllowOnce => "本次允许",
        ApprovalDecision::AllowSession => "本会话允许",
        ApprovalDecision::Deny => "拒绝",
    }
}

fn apply_event(app: &mut App, event: AgentEvent) {
    match event {
        AgentEvent::TurnStarted { turn_id, .. } => {
            app.busy = true;
            app.stick = true;
            app.status = format!("运行中 · {}", short_id(&turn_id));
        }
        AgentEvent::TurnFinished { stop_reason, usage, .. } => {
            app.busy = false;
            let style = if matches!(stop_reason, StopReason::Error) {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            app.push(
                "·",
                format!(
                    "轮次{} · ↑{} ↓{}",
                    stop_reason_label(stop_reason),
                    usage.input_tokens,
                    usage.output_tokens
                ),
                style,
            );
            app.status = "就绪".into();
        }
        AgentEvent::UserMessage {
            client_name,
            content,
            queued,
            ..
        } => {
            if queued {
                app.push(
                    "·",
                    format!("{} 的消息已排队: {}", client_name, truncate(&content, 60)),
                    Style::default().fg(Color::DarkGray),
                );
            }
        }
        AgentEvent::QueueUpdated { pending } => app.queue = pending,
        // 仅侧栏运行标记使用（Web 端）；TUI 已由 TurnStarted/Finished 维护状态
        AgentEvent::SessionRunning { .. } => {}
        AgentEvent::QueueCleared {} => app.queue = 0,
        AgentEvent::ThinkingDelta { delta, subagent_id } => {
            app.append_stream(
                if subagent_id.is_some() { "↳思" } else { "思" },
                &delta,
                Style::default().fg(Color::Magenta),
            );
        }
        AgentEvent::TextDelta { delta, subagent_id } => {
            app.append_stream(
                if subagent_id.is_some() { "↳" } else { "AI" },
                &delta,
                Style::default().fg(Color::Green),
            );
        }
        AgentEvent::ToolCallStarted(data) => app.push(
            if data.subagent_id.is_some() { "↳⚙" } else { "⚙" },
            format!("{} {}", data.name, summarize_tool_input(&data.input)),
            Style::default().fg(Color::Yellow),
        ),
        AgentEvent::ToolCallFinished {
            name, output, is_error, ..
        } => {
            let style = if is_error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let head: String = output.lines().take(6).collect::<Vec<_>>().join("\n");
            let suffix = if output.lines().count() > 6 { "\n…" } else { "" };
            app.push("  ", format!("{}: {}{}", name, head, suffix), style);
        }
        AgentEvent::PermissionRequested(data) => {
            app.approval = Some(PendingApproval {
                request_id: data.request_id,
                name:       data.name,
                summary:    data.summary,
            });
        }
        AgentEvent::PermissionResolved {
            request_id,
            resolved_by,
            ..
        } => {
            if app
                .approval
                .as_ref()
                .is_some_and(|p| p.request_id == request_id)
            {
                app.approval = None;
            }
            app.push(
                "·",
                format!("审批由 {} 处理", resolved_by),
                Style::default().fg(Color::DarkGray),
            );
        }
        AgentEvent::ModelChanged { active_model } => app.model = active_model,
        AgentEvent::AgentChanged { active_agent } => app.agent = active_agent,
        AgentEvent::ActiveTurnCatchUp(snapshot) => {
            if !snapshot.accumulated_thinking.is_empty() {
                app.push("思", snapshot.accumulated_thinking, Style::default().fg(Color::Magenta));
            }
            if !snapshot.accumulated_text.is_empty() {
                app.push("AI", snapshot.accumulated_text, Style::default().fg(Color::Green));
            }
            if let Some(call) = snapshot.active_tool_call {
                app.push(
                    "⚙",
                    format!("{} {}", call.name, summarize_tool_input(&call.input)),
                    Style::default().fg(Color::Yellow),
                );
            }
            app.approval = snapshot.pending_approval.map(|p| PendingApproval {
                request_id: p.request_id,
                name:       p.name,
                summary:    p.summary,
            });
            app.busy = true;
        }
        AgentEvent::SyncRequired {} => app.push(
            "·",
            "事件流出现缺口，状态可能不完整",
            Style::default().fg(Color::Yellow),
        ),
        AgentEvent::Error { message } => app.push("!", message, Style::default().fg(Color::Red)),
        AgentEvent::SessionRenamed { title, .. } => app.push(
            "·",
            format!("会话已重命名为 {}", title),
            Style::default().fg(Color::DarkGray),
        ),
        AgentEvent::MessagesDeleted { deleted_ids, .. } => app.push(
            "·",
            format!("已删除 {} 条消息", deleted_ids.len()),
            Style::default().fg(Color::DarkGray),
        ),
        AgentEvent::ActiveBranchChanged { current_leaf_id } => app.push(
            "·",
            format!("切换到分支 {}", short_id(&current_leaf_id)),
            Style::default().fg(Color::DarkGray),
        ),
        AgentEvent::ApprovalModeChanged { mode } => app.push(
            "·",
            format!("审批模式 → {}", approval_mode_label(mode)),
            Style::default().fg(Color::DarkGray),
        ),
    }
}

fn stop_reason_label(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "完成",
        StopReason::ToolUse => "工具调用",
        StopReason::MaxTokens => "达到输出上限",
        StopReason::Cancelled => "已取消",
        StopReason::Error => "出错",
    }
}

fn approval_mode_label(mode: oma_contract::ApprovalMode) -> &'static str {
    match mode {
        oma_contract::ApprovalMode::Normal => "normal",
        oma_contract::ApprovalMode::Strict => "strict",
        oma_contract::ApprovalMode::Auto => "auto",
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let [header, body, input] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(INPUT_HEIGHT),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);
    draw_transcript(frame, app, body);
    draw_input(frame, app, input);

    if let Some(pending) = &app.approval {
        draw_approval(frame, pending, frame.area());
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let dot = if app.connected { "●" } else { "○" };
    let state = if app.busy { "运行中" } else { app.status.as_str() };
    let queue = if app.queue > 0 {
        format!(" · 队列 {}", app.queue)
    } else {
        String::new()
    };
    let line = Line::from(vec![
        Span::styled(
            format!("{} ", dot),
            Style::default().fg(if app.connected { Color::Green } else { Color::Red }),
        ),
        Span::styled(app.workspace.clone(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            format!("{} · {}{}", state, app.model, queue),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect) {
    let lines = app.wrapped_lines(area.width.saturating_sub(2) as usize);
    let visible = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let max_offset = total.saturating_sub(visible);
    let offset = if app.stick {
        max_offset as u16
    } else {
        (app.scroll as usize).min(max_offset) as u16
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            format!(" oma · {} 行 ", total),
            Style::default().fg(Color::DarkGray),
        ));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect) {
    let hint = if app.connected {
        " Enter 发送 · Ctrl+X 中止 · Ctrl+U 清空 · PgUp/PgDn 滚动 · Esc 退出 "
    } else {
        " 连接已断开 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.connected { Color::Cyan } else { Color::Red }))
        .title(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    frame.render_widget(Paragraph::new(app.input.clone()).block(block), area);
    let cursor_x = area.x + 1 + display_width(&app.input).min(area.width.saturating_sub(2) as usize) as u16;
    frame.set_cursor_position((cursor_x, area.y + 1));
}

fn draw_approval(frame: &mut Frame, pending: &PendingApproval, area: Rect) {
    let width = area.width.saturating_sub(8).min(90);
    let height = 7.min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);

    let body = Text::from(vec![
        Line::from(vec![
            Span::styled("工具 ", Style::default().fg(Color::DarkGray)),
            Span::styled(pending.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        Line::raw(""),
        Line::styled(
            truncate(&pending.summary, width.saturating_sub(4) as usize),
            Style::default().fg(Color::Gray),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("[y/Enter] 本次允许  ", Style::default().fg(Color::Green)),
            Span::styled("[a] 本会话允许  ", Style::default().fg(Color::Cyan)),
            Span::styled("[n/Esc] 拒绝", Style::default().fg(Color::Red)),
        ]),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Yellow))
        .title("权限审批");
    frame.render_widget(Paragraph::new(body).block(block).wrap(Wrap { trim: false }), popup);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_text_respects_display_width() {
        // 纯 ASCII：按宽度硬折
        assert_eq!(wrap_text("abcdefgh", 4), vec!["abcd", "efgh"]);
        // CJK 每字 2 列
        let wrapped = wrap_text("中文内容", 4);
        assert_eq!(wrapped, vec!["中文", "内容"]);
        // 优先在空白处断开，并去掉行尾空格
        assert_eq!(wrap_text("hello world", 8), vec!["hello", "world"]);
        // 保留空行
        assert_eq!(wrap_text("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn test_wrap_text_keeps_content() {
        let text = "一行很长的中文文本需要被折行处理";
        let joined: String = wrap_text(text, 6).join("");
        assert_eq!(joined, text, "折行不得丢字符");
    }

    #[test]
    fn test_display_width_and_truncate() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("中文"), 4);
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("中文内容", 5), "中文…");
    }

    #[test]
    fn test_summarize_tool_input() {
        let input = serde_json::json!({ "command": "cargo   test\n--all" });
        assert_eq!(summarize_tool_input(&input), "cargo test --all");
        let long = serde_json::json!({ "path": "x".repeat(200) });
        assert!(summarize_tool_input(&long).chars().count() <= 81);
    }

    #[test]
    fn test_append_stream_groups_same_kind() {
        let mut app = App::new("/w".into(), "m".into(), "task".into());
        app.append_stream("AI", "hello ", Style::default().fg(Color::Green));
        app.append_stream("AI", "world", Style::default().fg(Color::Green));
        app.append_stream("思", "thinking", Style::default().fg(Color::Magenta));
        assert_eq!(app.entries.len(), 2);
        assert_eq!(app.entries[0].text, "hello world");
    }
}
