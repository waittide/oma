//! oma-tui: 基于 Ratatui 的终端客户端
//!
//! 由 `oma tui` 子命令驱动：连接 Daemon 后实时渲染流式文本、思考块、
//! 工具调用与权限审批。入口为 [`run`]，复用调用方的 tokio 运行时。

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use oma_client::{ConnectOptions, OmaClient, SessionApi};
use oma_contract::{
    AgentCommand, AgentEvent, ApprovalDecision, AskAnswer, AskQuestion, AskResponse, ClientType, Palette,
    ResolvedTheme, StopReason, ThemeMode,
};
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

/// 抹掉控制字符：换行/Tab 之外的不可见字符会提前终结 OSC 序列，
/// 让终端把后续内容当成普通输出，通知也随之中断。
fn sanitize_notification(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// 终端系统通知序列：OSC 9（iTerm2 / Windows Terminal 等）与 OSC 777
/// （rxvt-unicode 及其兼容终端）各发一份，不支持的终端会忽略未知序列；
/// 末尾的响铃（BEL）作为最后兜底，确保至少能提醒到人。
fn notification_sequence(title: &str, body: &str) -> String {
    let title = sanitize_notification(title);
    let body = sanitize_notification(body);
    format!("\x1b]9;{title}: {body}\x07\x1b]777;notify;{title};{body}\x07\x07")
}

/// 发出一条终端通知。
///
/// 写在 stderr：stdout 由 ratatui 持有并负责重绘，混入转义序列可能被下一帧
/// 覆盖或打乱光标；stderr 与 stdout 通常指向同一终端，通知序列同样生效。
fn notify_terminal(title: &str, body: &str) {
    use std::io::Write;

    let mut out = std::io::stderr().lock();
    let _ = write!(out, "{}", notification_sequence(title, body));
    let _ = out.flush();
}

/// 一条可切换的已保存连接（来自 client.toml）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiConnection {
    pub name:  String,
    pub url:   String,
    pub token: String,
}

/// `oma tui` 的启动参数。
pub struct TuiOptions {
    /// 初始连接地址与 token（来自 client.toml 的活动连接或命令行覆盖）
    pub addr:        String,
    pub token:       String,
    pub workspace:   String,
    /// 可切换的连接列表；空则不提供切换入口
    pub connections: Vec<TuiConnection>,
    /// 初始活动连接名（用于切换列表高亮）
    pub active:      Option<String>,
}

/// 会话循环的出口：退出，或切换到另一条连接后重建会话。
enum Outcome {
    Quit,
    Switch {
        addr:  String,
        token: String,
        name:  String,
    },
}

/// 由握手下发的调色板导出的语义配色。
///
/// 全部字段都是 `Color`（`Copy`），因此可按值传进绘制函数，无需处理借用。
/// 令牌到角色的映射与 Web 端保持一致：中性色管文字与边框，强调色管交互高亮。
#[derive(Debug, Clone, Copy)]
struct TuiTheme {
    /// 正文文字（text）
    text:     Color,
    /// 次要文字（subtext0）
    subtext:  Color,
    /// 注释、分隔说明与边框（overlay0）
    muted:    Color,
    /// 用户输入的强调色：来自 theme.accent 选定的令牌
    accent:   Color,
    /// 成功 / 助手正文（green）
    success:  Color,
    /// 错误（red）
    error:    Color,
    /// 警告 / 工具调用（yellow）
    warning:  Color,
    /// 思考内容（mauve）
    thinking: Color,
}

impl TuiTheme {
    /// 测试用零值配色：全部 `Reset`，与实际令牌无关
    #[cfg(test)]
    fn test() -> Self {
        Self {
            text:     Color::Reset,
            subtext:  Color::Reset,
            muted:    Color::Reset,
            accent:   Color::Reset,
            success:  Color::Reset,
            error:    Color::Reset,
            warning:  Color::Reset,
            thinking: Color::Reset,
        }
    }

    fn new(theme: &ResolvedTheme) -> Self {
        // system 模式无法可靠探测终端背景色，而终端底色以深色为主，
        // 故只有显式选择 light 时才用浅色调色板。
        let palette = match theme.mode {
            ThemeMode::Light => &theme.light,
            _ => &theme.dark,
        };
        Self {
            text:     token_color(palette, "text"),
            subtext:  token_color(palette, "subtext0"),
            muted:    token_color(palette, "overlay0"),
            accent:   token_color(palette, &theme.accent),
            success:  token_color(palette, "green"),
            error:    token_color(palette, "red"),
            warning:  token_color(palette, "yellow"),
            thinking: token_color(palette, "mauve"),
        }
    }
}

/// 取令牌对应的终端颜色；令牌缺失或非十六进制时回落 `Reset`（继承终端本身配色）。
fn token_color(palette: &Palette, token: &str) -> Color {
    palette
        .token(token)
        .and_then(parse_hex_color)
        .unwrap_or(Color::Reset)
}

fn parse_hex_color(value: &str) -> Option<Color> {
    let hex = value.strip_prefix('#')?;
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        3 => {
            // #abc 每位重复一次即 #aabbcc
            let dup = |c: char| {
                let v = c.to_digit(16)? as u8;
                Some(v * 16 + v)
            };
            let mut it = hex.chars();
            Some(Color::Rgb(dup(it.next()?)?, dup(it.next()?)?, dup(it.next()?)?))
        }
        6 => Some(Color::Rgb(byte(&hex[0..2])?, byte(&hex[2..4])?, byte(&hex[4..6])?)),
        _ => None,
    }
}

/// 一条已渲染的内容行
struct Entry {
    prefix: &'static str,
    text:   String,
    style:  Style,
}

/// 待响应的审批请求
struct PendingApproval {
    request_id: String,
    tool_name:  String,
    input:      serde_json::Value,
}

/// 待作答的提问（ask 工具）。终端里用数字键选择，`o` 进入自定义输入。
struct PendingAsk {
    request_id: String,
    questions:  Vec<AskQuestion>,
    /// 每题已选项（下标集合）
    picked:     Vec<std::collections::BTreeSet<usize>>,
    /// 当前题目序号
    index:      usize,
    /// 正在输入自定义答案的题目序号
    custom_for: Option<usize>,
    custom:     String,
}

impl PendingAsk {
    fn new(request_id: String, questions: Vec<AskQuestion>) -> Self {
        let picked = vec![std::collections::BTreeSet::new(); questions.len()];
        Self {
            request_id,
            questions,
            picked,
            index: 0,
            custom_for: None,
            custom: String::new(),
        }
    }

    /// 组装与请求 questions 对齐的回答
    fn to_answers(&self) -> Vec<AskAnswer> {
        self.questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let selected: Vec<String> = self.picked[i]
                    .iter()
                    .filter_map(|&oi| q.options.get(oi).map(|o| o.label.clone()))
                    .collect();
                // 自定义输入单列回传，与选项标签区分
                let custom_input = if self.custom_for == Some(i) {
                    self.custom.trim().to_string()
                } else {
                    String::new()
                };
                AskAnswer { selected, custom_input }
            })
            .collect()
    }
}

struct App {
    workspace:   String,
    model:       String,
    agent:       String,
    connected:   bool,
    busy:        bool,
    queue:       usize,
    status:      String,
    entries:     Vec<Entry>,
    input:       String,
    scroll:      u16,
    stick:       bool,
    approval:    Option<PendingApproval>,
    ask:         Option<PendingAsk>,
    /// 最近一次请求的上下文占用（tokens, context_len）
    context:     Option<(usize, usize)>,
    /// 可切换的已保存连接
    connections: Vec<TuiConnection>,
    /// 当前活动连接名
    active_conn: Option<String>,
    /// 连接切换弹窗的高亮下标；None = 未打开
    picker:      Option<usize>,
    /// 由握手下发主题导出的语义配色
    theme:       TuiTheme,
}

impl App {
    fn new(workspace: String, model: String, agent: String, theme: TuiTheme) -> Self {
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
            ask: None,
            context: None,
            connections: Vec::new(),
            active_conn: None,
            picker: None,
            theme,
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
///
/// 支持在界面内切换到 client.toml 里保存的其他连接：切换时重建会话与事件流。
/// 返回最终使用的连接名（未使用已保存连接时为 None），供调用方回写 active。
pub async fn run(options: TuiOptions) -> Result<Option<String>> {
    // crossterm 的阻塞读放在独立线程；切换连接只是重建会话，
    // 不重新开线程，避免多个 reader 竞争同一 tty。
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

    let mut addr = options.addr.clone();
    let mut token = options.token.clone();
    let mut active = options.active.clone();

    let mut terminal = ratatui::init();
    let result = loop {
        match run_session(&mut terminal, &addr, &token, &options, active.clone(), &mut key_rx).await {
            Ok(Outcome::Quit) => break Ok(active),
            Ok(Outcome::Switch {
                addr: next_addr,
                token: next_token,
                name,
            }) => {
                addr = next_addr;
                token = next_token;
                active = Some(name);
            }
            Err(e) => break Err(e),
        }
    };
    ratatui::restore();
    result
}

/// 连接一次、跑一个会话，直到退出或要求切换。
async fn run_session(
    terminal: &mut ratatui::DefaultTerminal,
    addr: &str,
    token: &str,
    options: &TuiOptions,
    active: Option<String>,
    key_rx: &mut mpsc::Receiver<KeyEvent>,
) -> Result<Outcome> {
    let workspace = options.workspace.as_str();
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
        let theme = TuiTheme::new(&ready.active_theme);
        let mut app = App::new(
            ready.workspace.clone(),
            ready.active_model.clone(),
            ready.active_agent.clone(),
            theme,
        );
        app.connections = options.connections.clone();
        // 活动连接：优先调用方给出的名字，否则按地址匹配已保存的条目
        app.active_conn = active.or_else(|| {
            app.connections
                .iter()
                .find(|c| c.url == addr)
                .map(|c| c.name.clone())
        });
        app.push(
            "·",
            format!("会话 {} 已连接", short_id(&ready.session_id)),
            Style::default().fg(theme.muted),
        );
        if let Some(leaf) = &ready.current_leaf_id {
            app.push(
                "·",
                format!("当前分支叶子 {}", short_id(leaf)),
                Style::default().fg(theme.muted),
            );
        }
        app
    };

    event_loop(terminal, &mut app, &mut client, key_rx).await
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    client: &mut OmaClient,
    key_rx: &mut mpsc::Receiver<KeyEvent>,
) -> Result<Outcome> {
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
                    return Ok(Outcome::Quit);
                }
            },
            key = key_rx.recv() => {
                let Some(key) = key else { return Ok(Outcome::Quit) };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(outcome) = handle_key(app, client, key).await? {
                    return Ok(outcome);
                }
                terminal.draw(|frame| draw(frame, app))?;
                last_draw = Instant::now();
            }
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

/// ask 弹窗按键：数字选选项、`o` 自定义输入、`n/p` 切题、Enter 提交、Esc 取消。
async fn handle_ask_key(app: &mut App, client: &mut OmaClient, key: KeyEvent) -> Result<()> {
    let Some(ask) = app.ask.as_mut() else {
        return Ok(());
    };
    let total = ask.questions.len();

    // 自定义输入模式：接管全部字符
    if ask.custom_for.is_some() {
        match key.code {
            KeyCode::Esc => {
                ask.custom_for = None;
                ask.custom.clear();
            }
            KeyCode::Enter => {
                ask.custom_for = None;
            }
            KeyCode::Backspace => {
                ask.custom.pop();
            }
            KeyCode::Char(c) => ask.custom.push(c),
            _ => {}
        }
        return Ok(());
    }

    match key.code {
        KeyCode::Esc => {
            let request_id = ask.request_id.clone();
            app.ask = None;
            client
                .respond_ask(AskResponse {
                    request_id,
                    answers: Vec::new(),
                    is_cancelled: true,
                })
                .await?;
            app.push("·", "已取消提问", Style::default().fg(app.theme.warning));
        }
        KeyCode::Enter => {
            let answers = ask.to_answers();
            let request_id = ask.request_id.clone();
            app.ask = None;
            client
                .respond_ask(AskResponse {
                    request_id,
                    answers,
                    is_cancelled: false,
                })
                .await?;
            app.push("·", "已回答提问", Style::default().fg(app.theme.success));
        }
        KeyCode::Char('n') | KeyCode::Right => {
            ask.index = (ask.index + 1).min(total.saturating_sub(1));
        }
        KeyCode::Char('p') | KeyCode::Left => {
            ask.index = ask.index.saturating_sub(1);
        }
        KeyCode::Char('o') => {
            ask.custom_for = Some(ask.index);
        }
        KeyCode::Char(c) => {
            // 数字键切换该题对应选项；多选可叠加，单选替换
            let Ok(n) = c.to_string().parse::<usize>() else {
                return Ok(());
            };
            if n == 0 {
                return Ok(());
            }
            let idx = n - 1;
            let qi = ask.index;
            let Some(q) = ask.questions.get(qi) else {
                return Ok(());
            };
            if idx >= q.options.len() {
                return Ok(());
            }
            let multi = q.is_multi;
            let picked = &mut ask.picked[qi];
            if multi {
                if !picked.remove(&idx) {
                    picked.insert(idx);
                }
            } else {
                picked.clear();
                picked.insert(idx);
            }
        }
        _ => {}
    }
    Ok(())
}

/// 处理按键；返回 Some 表示离开当前会话（退出或切换连接）。
async fn handle_key(app: &mut App, client: &mut OmaClient, key: KeyEvent) -> Result<Option<Outcome>> {
    // ask 弹窗优先级最高：采纳答案前不应误发消息
    if app.ask.is_some() {
        return handle_ask_key(app, client, key).await.map(|_| None);
    }

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
            let tool_name = pending.tool_name.clone();
            app.approval = None;
            client.respond_approval(&request_id, decision).await?;
            app.push(
                "·",
                format!("审批 {} → {}", tool_name, decision_label(decision)),
                Style::default().fg(app.theme.warning),
            );
        }
        return Ok(None);
    }

    // 连接切换弹窗（无 ask/审批时）：先于普通输入消费按键
    if app.picker.is_some() {
        return Ok(handle_picker_key(app, key));
    }

    match key.code {
        KeyCode::Esc => return Ok(Some(Outcome::Quit)),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Ok(Some(Outcome::Quit));
        }
        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => open_picker(app),
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
            app.push("·", "已请求中止", Style::default().fg(app.theme.warning));
        }
        KeyCode::Enter => {
            let text = app.input.trim().to_string();
            if text.is_empty() {
                return Ok(None);
            }
            app.input.clear();
            app.push("你", text.clone(), Style::default().fg(app.theme.accent));
            app.stick = true;
            if let Err(e) = client
                .send_command(AgentCommand::UserInput {
                    content:     text,
                    attachments: Vec::new(),
                })
                .await
            {
                app.push("!", format!("发送失败: {}", e), Style::default().fg(app.theme.error));
            }
        }
        KeyCode::Char(c) => app.input.push(c),
        _ => {}
    }
    Ok(None)
}

/// 打开连接切换弹窗；无已保存连接时仅提示，不进入空列表。
fn open_picker(app: &mut App) {
    if app.connections.is_empty() {
        app.push(
            "·",
            "没有已保存的连接（可在设置页或 client.toml 中添加）",
            Style::default().fg(app.theme.muted),
        );
        return;
    }
    let index = app
        .active_conn
        .as_ref()
        .and_then(|name| app.connections.iter().position(|c| &c.name == name))
        .unwrap_or(0);
    app.picker = Some(index);
}

/// 连接切换弹窗按键：上下选择、Enter 切换、Esc 取消。
fn handle_picker_key(app: &mut App, key: KeyEvent) -> Option<Outcome> {
    let index = app.picker?;
    let last = app.connections.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc => app.picker = None,
        KeyCode::Up | KeyCode::Char('k') => app.picker = Some(index.saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => app.picker = Some((index + 1).min(last)),
        KeyCode::Enter => {
            let Some(conn) = app.connections.get(index) else {
                app.picker = None;
                return None;
            };
            let outcome = Outcome::Switch {
                addr:  conn.url.clone(),
                token: conn.token.clone(),
                name:  conn.name.clone(),
            };
            return Some(outcome);
        }
        _ => {}
    }
    None
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
        AgentEvent::TurnFinished {
            stop_reason,
            usage,
            subagent_id,
            ..
        } => {
            app.busy = false;
            let style = if matches!(stop_reason, StopReason::Error) {
                Style::default().fg(app.theme.error)
            } else {
                Style::default().fg(app.theme.muted)
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
            // 子代理轮次不是人的待办，且出错已单独报错，均不发系统通知
            if subagent_id.is_none() && !matches!(stop_reason, StopReason::Error) {
                notify_terminal("Oma", &format!("任务完成（{}）", stop_reason_label(stop_reason)));
            }
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
                    Style::default().fg(app.theme.muted),
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
                Style::default().fg(app.theme.thinking),
            );
        }
        AgentEvent::TextDelta { delta, subagent_id } => {
            app.append_stream(
                if subagent_id.is_some() { "↳" } else { "AI" },
                &delta,
                Style::default().fg(app.theme.success),
            );
        }
        AgentEvent::ToolCallStarted(data) => app.push(
            if data.subagent_id.is_some() { "↳⚙" } else { "⚙" },
            format!("{} {}", data.tool_name, summarize_tool_input(&data.input)),
            Style::default().fg(app.theme.warning),
        ),
        AgentEvent::ToolCallFinished {
            tool_name,
            output,
            is_error,
            ..
        } => {
            let style = if is_error {
                Style::default().fg(app.theme.error)
            } else {
                Style::default().fg(app.theme.muted)
            };
            let head: String = output.lines().take(6).collect::<Vec<_>>().join("\n");
            let suffix = if output.lines().count() > 6 { "\n…" } else { "" };
            app.push("  ", format!("{}: {}{}", tool_name, head, suffix), style);
        }
        AgentEvent::AskRequested(data) => {
            app.picker = None;
            if let Some(question) = data.questions.first() {
                notify_terminal("Oma", &format!("需要确认：{}", question.question));
            }
            app.ask = Some(PendingAsk::new(data.request_id, data.questions));
        }
        AgentEvent::AskResolved { resolved_by, .. } => {
            if app.ask.is_some() {
                app.ask = None;
                app.push(
                    "·",
                    format!("提问由 {} 处理", resolved_by),
                    Style::default().fg(app.theme.muted),
                );
            }
        }
        AgentEvent::PermissionRequested(data) => {
            app.picker = None;
            notify_terminal("Oma", &format!("待授权：{}", data.tool_name));
            app.approval = Some(PendingApproval {
                request_id: data.request_id,
                tool_name:  data.tool_name,
                input:      data.input,
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
                Style::default().fg(app.theme.muted),
            );
        }
        AgentEvent::ModelChanged { active_model } => app.model = active_model,
        AgentEvent::AgentChanged { active_agent } => app.agent = active_agent,
        AgentEvent::ActiveTurnCatchUp(snapshot) => {
            if !snapshot.accumulated_thinking.is_empty() {
                app.push(
                    "思",
                    snapshot.accumulated_thinking,
                    Style::default().fg(app.theme.thinking),
                );
            }
            if !snapshot.accumulated_text.is_empty() {
                app.push("AI", snapshot.accumulated_text, Style::default().fg(app.theme.success));
            }
            if let Some(call) = snapshot.active_tool_call {
                app.push(
                    "⚙",
                    format!("{} {}", call.tool_name, summarize_tool_input(&call.input)),
                    Style::default().fg(app.theme.warning),
                );
            }
            app.approval = snapshot.pending_approval.map(|p| PendingApproval {
                request_id: p.request_id,
                tool_name:  p.tool_name,
                input:      p.input,
            });
            // 追问接入时也要恢复未作答的提问
            app.ask = snapshot
                .pending_ask
                .map(|a| PendingAsk::new(a.request_id, a.questions));
            app.busy = true;
        }
        AgentEvent::SyncRequired {} => app.push(
            "·",
            "事件流出现缺口，状态可能不完整",
            Style::default().fg(app.theme.warning),
        ),
        AgentEvent::Error { message } => app.push("!", message, Style::default().fg(app.theme.error)),
        AgentEvent::SessionRenamed { title, .. } => app.push(
            "·",
            format!("会话已重命名为 {}", title),
            Style::default().fg(app.theme.muted),
        ),
        AgentEvent::MessagesDeleted { deleted_ids, .. } => app.push(
            "·",
            format!("已删除 {} 条消息", deleted_ids.len()),
            Style::default().fg(app.theme.muted),
        ),
        AgentEvent::ActiveBranchChanged { current_leaf_id } => app.push(
            "·",
            format!("切换到分支 {}", short_id(&current_leaf_id)),
            Style::default().fg(app.theme.muted),
        ),
        AgentEvent::ApprovalModeChanged { mode } => app.push(
            "·",
            format!("审批模式 → {}", approval_mode_label(mode)),
            Style::default().fg(app.theme.muted),
        ),
        AgentEvent::ContextUsage { tokens, context_len } => {
            app.context = Some((tokens, context_len));
        }
        AgentEvent::ReasoningLevelChanged { level } => app.push(
            "·",
            if level.is_empty() {
                "推理等级 → 模型默认".to_string()
            } else {
                format!("推理等级 → {}", level)
            },
            Style::default().fg(app.theme.muted),
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
    let area = frame.area();
    let [header, body, input] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(INPUT_HEIGHT),
    ])
    .areas(area);

    // 配色按值拷入各绘制函数：TuiTheme 全为 Copy 字段，且 Draw 内 app 已不可变借用
    let theme = app.theme;
    draw_header(frame, app, header, theme);
    draw_transcript(frame, app, body, theme);
    draw_input(frame, app, input, theme);

    if let Some(pending) = &app.ask {
        draw_ask(frame, pending, area, theme);
    } else if let Some(pending) = &app.approval {
        draw_approval(frame, pending, area, theme);
    } else if let Some(index) = app.picker {
        draw_picker(frame, app, index, area, theme);
    }
}

/// 连接切换弹窗：列出已保存连接，高亮当前选中项。
fn draw_picker(frame: &mut Frame, app: &App, index: usize, area: Rect, theme: TuiTheme) {
    if app.connections.is_empty() {
        return;
    }
    let height = (app.connections.len() as u16 + 3).min(area.height);
    let width = area.width.saturating_sub(8).min(64);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " 切换连接 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ",
            Style::default().fg(theme.muted),
        ));
    frame.render_widget(block, popup);

    let inner = Rect {
        x:      popup.x + 1,
        y:      popup.y + 1,
        width:  popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    let lines: Vec<Line<'static>> = app
        .connections
        .iter()
        .enumerate()
        .map(|(i, conn)| {
            let selected = i == index;
            let marker = if selected { "> " } else { "  " };
            let name = if app.active_conn.as_deref() == Some(conn.name.as_str()) {
                format!("{} ✓", conn.name)
            } else {
                conn.name.clone()
            };
            let style = if selected {
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.subtext)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.accent)),
                Span::styled(name, style),
                Span::raw("  "),
                Span::styled(conn.url.clone(), Style::default().fg(theme.muted)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// 上下文占用进度条：10 格 + 百分比，参照 oh-my-pi 的 contextGauge。
fn context_gauge(tokens: usize, window: usize) -> String {
    if window == 0 {
        return format!("ctx {}", tokens);
    }
    let ratio = (tokens as f64 / window as f64).clamp(0.0, 1.0);
    let filled = (ratio * 10.0).round() as usize;
    format!(
        "{}{} {}%",
        "━".repeat(filled),
        "─".repeat(10 - filled),
        (ratio * 100.0).round() as usize
    )
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect, theme: TuiTheme) {
    let dot = if app.connected { "●" } else { "○" };
    let state = if app.busy { "运行中" } else { app.status.as_str() };
    let queue = if app.queue > 0 {
        format!(" · 队列 {}", app.queue)
    } else {
        String::new()
    };
    let ctx = app
        .context
        .map(|(tokens, window)| format!("  {}", context_gauge(tokens, window)))
        .unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(
            format!("{} ", dot),
            Style::default().fg(if app.connected { theme.success } else { theme.error }),
        ),
        Span::styled(
            app.workspace.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} · {}{}", state, app.model, queue),
            Style::default().fg(theme.muted),
        ),
        Span::styled(ctx, Style::default().fg(theme.muted)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect, theme: TuiTheme) {
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
        .border_style(Style::default().fg(theme.muted))
        .title(Span::styled(
            format!(" oma · {} 行 ", total),
            Style::default().fg(theme.muted),
        ));
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((offset, 0)),
        area,
    );
}

fn draw_input(frame: &mut Frame, app: &App, area: Rect, theme: TuiTheme) {
    let hint = if app.connected {
        " Enter 发送 · Ctrl+X 中止 · Ctrl+O 切换连接 · Ctrl+U 清空 · Esc 退出 "
    } else {
        " 连接已断开 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if app.connected { theme.accent } else { theme.error }))
        .title(Span::styled(hint, Style::default().fg(theme.muted)));
    frame.render_widget(
        Paragraph::new(Span::styled(app.input.clone(), Style::default().fg(theme.text))).block(block),
        area,
    );
    let cursor_x = area.x + 1 + display_width(&app.input).min(area.width.saturating_sub(2) as usize) as u16;
    frame.set_cursor_position((cursor_x, area.y + 1));
}

fn draw_approval(frame: &mut Frame, pending: &PendingApproval, area: Rect, theme: TuiTheme) {
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
            Span::styled("工具 ", Style::default().fg(theme.muted)),
            Span::styled(
                pending.tool_name.clone(),
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::styled(
            truncate(&summarize_tool_input(&pending.input), width.saturating_sub(4) as usize),
            Style::default().fg(theme.subtext),
        ),
        Line::raw(""),
        Line::from(vec![
            Span::styled("[y/Enter] 本次允许  ", Style::default().fg(theme.success)),
            Span::styled("[a] 本会话允许  ", Style::default().fg(theme.accent)),
            Span::styled("[n/Esc] 拒绝", Style::default().fg(theme.error)),
        ]),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme.warning))
        .title("权限审批");
    frame.render_widget(Paragraph::new(body).block(block).wrap(Wrap { trim: false }), popup);
}

/// 渲染 ask 弹窗：一题一行选项，多选用 [x]、单选用 (x) 标记。
fn draw_ask(frame: &mut Frame, pending: &PendingAsk, area: Rect, theme: TuiTheme) {
    let width = area.width.saturating_sub(8).min(90);
    let inner_width = width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();

    if let Some(q) = pending.questions.get(pending.index) {
        let mode = if q.is_multi { "多选" } else { "单选" };
        lines.push(Line::from(vec![
            Span::styled(
                format!("问题 {}/{} ", pending.index + 1, pending.questions.len()),
                Style::default().fg(theme.muted),
            ),
            Span::styled(format!("[{}]", mode), Style::default().fg(theme.accent)),
        ]));
        lines.push(Line::raw(""));
        for (li, seg) in wrap_text(&q.question, inner_width.saturating_sub(2))
            .iter()
            .enumerate()
        {
            lines.push(Line::styled(
                if li == 0 {
                    format!("  {}", seg)
                } else {
                    format!("    {}", seg)
                },
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(Line::raw(""));
        for (oi, opt) in q.options.iter().enumerate() {
            let checked = pending.picked[pending.index].contains(&oi);
            let mark = if q.is_multi {
                if checked { "[x]" } else { "[ ]" }
            } else if checked {
                "(x)"
            } else {
                "( )"
            };
            let recommended = q.recommended == Some(oi);
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {}{} ", oi + 1, mark),
                    Style::default().fg(if checked { theme.success } else { theme.muted }),
                ),
                Span::styled(
                    opt.label.clone(),
                    Style::default().fg(if recommended { theme.warning } else { theme.text }),
                ),
                Span::styled(
                    if recommended { " (推荐)" } else { "" },
                    Style::default().fg(theme.muted),
                ),
            ]));
            if !opt.description.is_empty() {
                for seg in wrap_text(&opt.description, inner_width.saturating_sub(7)) {
                    lines.push(Line::styled(format!("      {}", seg), Style::default().fg(theme.muted)));
                }
            }
        }
        lines.push(Line::raw(""));
        if pending.custom_for == Some(pending.index) {
            lines.push(Line::from(vec![
                Span::styled("  自定义> ", Style::default().fg(theme.thinking)),
                Span::styled(pending.custom.clone(), Style::default().fg(theme.text)),
            ]));
        } else if !pending.custom.is_empty() && pending.custom_for.is_none() {
            lines.push(Line::styled(
                format!("  自定义: {}", pending.custom),
                Style::default().fg(theme.thinking),
            ));
        }
    }

    lines.push(Line::raw(""));
    let hint = if pending.custom_for.is_some() {
        "输入内容  Enter 确认  Esc 取消输入"
    } else {
        "数字选择  o 自定义  n/p 切题  Enter 提交  Esc 取消"
    };
    lines.push(Line::styled(hint, Style::default().fg(theme.muted)));

    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(theme.thinking))
        .title("提问");
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
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
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.append_stream("AI", "hello ", Style::default().fg(Color::Green));
        app.append_stream("AI", "world", Style::default().fg(Color::Green));
        app.append_stream("思", "thinking", Style::default().fg(Color::Magenta));
        assert_eq!(app.entries.len(), 2);
        assert_eq!(app.entries[0].text, "hello world");
    }

    #[test]
    fn test_parse_hex_color() {
        assert_eq!(parse_hex_color("#89b4fa"), Some(Color::Rgb(137, 180, 250)));
        // #abc 缩写展开为 #aabbcc
        assert_eq!(parse_hex_color("#abc"), Some(Color::Rgb(170, 187, 204)));
        assert_eq!(parse_hex_color("89b4fa"), None);
        assert_eq!(parse_hex_color("#xyz"), None);
        assert_eq!(parse_hex_color("#12345"), None);
    }

    #[test]
    fn test_notification_sequence_strips_control_chars() {
        // 控制字符会被换成空格，避免提前终结 OSC 序列
        let seq = notification_sequence("Oma", "a\x07b\nc");
        assert!(seq.starts_with("\x1b]9;Oma: a b c\x07"));
        assert!(seq.contains("\x1b]777;notify;Oma;a b c\x07"));
        assert!(seq.ends_with('\x07'));
    }

    /// 连接切换弹窗：打开时定位活动连接，上下选择不越界，Enter 返回切换目标。
    #[test]
    fn test_picker_navigation_and_switch() {
        let theme = TuiTheme::new(&ResolvedTheme {
            mode:   ThemeMode::Dark,
            accent: "blue".into(),
            light:  palette_with("latte", "#111111", "#222222"),
            dark:   palette_with("mocha", "#eeeeee", "#dddddd"),
        });
        let mut app = App::new("/w".into(), "m".into(), "a".into(), theme);
        app.connections = vec![
            TuiConnection {
                name:  "a".into(),
                url:   "http://a".into(),
                token: "ta".into(),
            },
            TuiConnection {
                name:  "b".into(),
                url:   "http://b".into(),
                token: "tb".into(),
            },
        ];
        app.active_conn = Some("b".into());

        // 打开时高亮活动连接；向上不越界
        open_picker(&mut app);
        assert_eq!(app.picker, Some(1));
        handle_picker_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.picker, Some(0));
        handle_picker_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.picker, Some(0));

        // Enter 返回切换目标
        match handle_picker_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Some(Outcome::Switch { addr, token, name }) => {
                assert_eq!(name, "a");
                assert_eq!(addr, "http://a");
                assert_eq!(token, "ta");
            }
            _ => panic!("Enter 应返回切换目标"),
        }

        // 没有已保存连接时仅提示，不进入空列表
        app.connections.clear();
        app.picker = None;
        open_picker(&mut app);
        assert_eq!(app.picker, None);
    }

    #[test]
    fn test_tui_theme_maps_tokens_and_accents() {
        // 浅色模式取 light 调色板，且 accent 随 theme.accent 令牌选择
        let theme = ResolvedTheme {
            mode:   ThemeMode::Light,
            accent: "mauve".into(),
            light:  palette_with("latte", "#111111", "#222222"),
            dark:   palette_with("mocha", "#eeeeee", "#dddddd"),
        };
        let tui = TuiTheme::new(&theme);
        assert_eq!(tui.text, Color::Rgb(0x11, 0x11, 0x11));
        assert_eq!(tui.accent, Color::Rgb(0x22, 0x22, 0x22));

        // 深色模式（含 system：终端底色以深色为主）取 dark 调色板
        let dark = ResolvedTheme {
            mode: ThemeMode::Dark,
            ..theme
        };
        assert_eq!(TuiTheme::new(&dark).text, Color::Rgb(0xEE, 0xEE, 0xEE));
    }

    /// 构造只填充 text/mauve 的调色板：其余令牌缺省，应回落 Reset 而非报错
    fn palette_with(id: &str, text: &str, mauve: &str) -> Palette {
        Palette {
            id:        id.into(),
            name:      id.into(),
            mode:      oma_contract::PaletteMode::Dark,
            crust:     String::new(),
            mantle:    String::new(),
            base:      String::new(),
            surface0:  String::new(),
            surface1:  String::new(),
            surface2:  String::new(),
            overlay0:  String::new(),
            overlay1:  String::new(),
            overlay2:  String::new(),
            subtext0:  String::new(),
            subtext1:  String::new(),
            text:      text.into(),
            lavender:  String::new(),
            blue:      String::new(),
            sapphire:  String::new(),
            sky:       String::new(),
            teal:      String::new(),
            green:     String::new(),
            yellow:    String::new(),
            peach:     String::new(),
            maroon:    String::new(),
            red:       String::new(),
            mauve:     mauve.into(),
            pink:      String::new(),
            flamingo:  String::new(),
            rosewater: String::new(),
        }
    }
}
