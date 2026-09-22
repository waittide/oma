//! oma-tui: 基于 Ratatui 的终端客户端
//!
//! 由 `oma tui` 子命令驱动：连接 Daemon 后实时渲染流式文本、思考块、
//! 工具调用。入口为 [`run`]，复用调用方的 tokio 运行时。

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use oma_client::{ConnectOptions, OmaClient, SessionApi, SessionRecord};
use oma_contract::{
    AgentCommand, AgentEvent, AgentSummary, ClientType, ModelInfo, Palette, REASONING_LEVELS, ResolvedTheme,
    StopReason, ThemeMode,
};
use ratatui::{
    Frame,
    crossterm::{
        event::{self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        execute,
    },
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

/// 输入线程转发的终端事件：按键与焦点变化。
///
/// 焦点事件必须与按键走同一条通道：两者都由 [`event::read`] 从同一 tty 读出，
/// 若只转发按键，`read()` 里取到的焦点事件会被丢弃，永远无法知道终端是否失焦。
enum InputEvent {
    Key(KeyEvent),
    /// true = 终端重新获得焦点，false = 失焦
    Focus(bool),
}

/// 一条可切换的已保存连接（来自 client.json）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiConnection {
    pub name:  String,
    pub url:   String,
    pub token: String,
}

/// `oma tui` 的启动参数。
pub struct TuiOptions {
    /// 初始连接地址与 token（来自 client.json 的活动连接或命令行覆盖）
    pub addr:        String,
    pub token:       String,
    pub workspace:   String,
    /// 可切换的连接列表；空则不提供切换入口
    pub connections: Vec<TuiConnection>,
    /// 初始活动连接名（用于切换列表高亮）
    pub active:      Option<String>,
}

/// 会话循环的出口：退出，或换一条连接 / 一个会话后重建。
enum Outcome {
    Quit,
    /// 换连接：地址与 token 一并更换，会话要重新挑
    Switch {
        addr:  String,
        token: String,
        name:  String,
    },
    /// 换会话：连接不动，只换 session_id（标题已在弹窗里提示过，这里不再带）
    SwitchSession {
        session_id: String,
    },
}

/// 弹窗里的一条：显示文本 + 选中后要执行的动作。
///
/// 动作随条目一起带着，渲染与按键就不必知道条目是什么种类；
/// 将来加模型、预设只是多几个构造点。
struct PickerItem {
    /// 主文本：连接名 / 会话标题
    label:   String,
    /// 右侧说明：地址 / 模型与预设
    detail:  String,
    /// 是否就是当前生效的那一项（渲染 ✓）
    current: bool,
    action:  PickerAction,
}

/// 弹窗条目被确认后要做的事
enum PickerAction {
    SwitchConnection {
        addr:  String,
        token: String,
        name:  String,
    },
    SwitchSession {
        session_id: String,
    },
    /// 下发指令切换模型（选择器形如 `provider/model_id`）
    SetModel(String),
    /// 下发指令切换 Agent 预设
    SetAgent(String),
    /// 下发指令切换推理等级（取值必须在 `REASONING_LEVELS` 内）
    SetReasoningLevel(String),
}

/// 弹窗选择器：连接、会话、模型、预设共用同一套渲染与按键。
struct Picker {
    /// 标题栏文案（自带按键提示）
    title: String,
    items: Vec<PickerItem>,
    index: usize,
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

struct App {
    workspace:       String,
    model:           String,
    agent:           String,
    /// 当前会话 id；弹窗里据此标记「就是它」
    session_id:      String,
    /// 当前工作区的会话列表：连接时取一次快照，供切换弹窗使用
    sessions:        Vec<SessionRecord>,
    /// 可选模型清单（provider → 模型），握手下发
    model_catalog:   BTreeMap<String, Vec<ModelInfo>>,
    /// 可选 Agent 预设，握手下发
    agents:          Vec<AgentSummary>,
    /// 当前推理等级；空串表示未设置
    reasoning_level: String,
    connected:       bool,
    busy:            bool,
    queue:           usize,
    status:          String,
    entries:         Vec<Entry>,
    input:           String,
    scroll:          u16,
    stick:           bool,
    /// 最近一次请求的上下文占用（tokens, context_len）
    context:         Option<(usize, usize)>,
    /// 可切换的已保存连接
    connections:     Vec<TuiConnection>,
    /// 当前活动连接名
    active_conn:     Option<String>,
    /// 弹窗选择器；None = 未打开
    picker:          Option<Picker>,
    /// 终端是否处于聚焦状态；仅失焦时才发系统通知
    focused:         bool,
    /// 由握手下发主题导出的语义配色
    theme:           TuiTheme,
}

impl App {
    fn new(workspace: String, model: String, agent: String, theme: TuiTheme) -> Self {
        Self {
            workspace,
            model,
            agent,
            session_id: String::new(),
            sessions: Vec::new(),
            model_catalog: BTreeMap::new(),
            agents: Vec::new(),
            reasoning_level: String::new(),
            connected: true,
            busy: false,
            queue: 0,
            status: "就绪".into(),
            entries: Vec::new(),
            input: String::new(),
            scroll: 0,
            stick: true,
            context: None,
            connections: Vec::new(),
            active_conn: None,
            picker: None,
            // 终端未上报焦点事件时按聚焦处理：宁可不打扰，也不在用户正看着时弹通知
            focused: true,
            theme,
        }
    }

    /// 仅在终端失焦时发系统通知。
    ///
    /// 用户正看着终端时，提问面板已经把事件摆在眼前，再发通知只是噪声。
    /// 由此推导：不支持焦点上报的终端（未开 `focus-events` 的 tmux、老终端等）
    /// 永远不会发通知 —— 这是「只在失焦时才提示」的应有代价，宁可静默也不误扰。
    fn notify(&self, body: String) {
        if self.focused {
            return;
        }
        notify_terminal("Oma", &body);
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
/// 支持在界面内切换到 client.json 里保存的其他连接：切换时重建会话与事件流。
/// 返回最终使用的连接名（未使用已保存连接时为 None），供调用方回写 active。
pub async fn run(options: TuiOptions) -> Result<Option<String>> {
    // crossterm 的阻塞读放在独立线程；切换连接只是重建会话，
    // 不重新开线程，避免多个 reader 竞争同一 tty。
    let (key_tx, mut key_rx) = mpsc::channel::<InputEvent>(64);
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let forwarded = match ev {
                Event::Key(key) => InputEvent::Key(key),
                Event::FocusGained => InputEvent::Focus(true),
                Event::FocusLost => InputEvent::Focus(false),
                _ => continue,
            };
            if key_tx.blocking_send(forwarded).is_err() {
                break;
            }
        }
    });

    let mut addr = options.addr.clone();
    let mut token = options.token.clone();
    let mut active = options.active.clone();
    // 用户显式挑过的会话：换连接后作废，重新按工作区挑
    let mut session: Option<String> = None;

    let mut terminal = ratatui::init();
    // 开启焦点变化上报（CSI ?1004h）：仅用于判断终端是否失焦，退出时恢复。
    // 终端不支持该序列时会直接忽略，不会报错。
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    let result = loop {
        let outcome = run_session(
            &mut terminal,
            &addr,
            &token,
            &options,
            active.clone(),
            session.clone(),
            &mut key_rx,
        )
        .await;
        match outcome {
            Ok(Outcome::Quit) => break Ok(active),
            Ok(Outcome::Switch {
                addr: next_addr,
                token: next_token,
                name,
            }) => {
                addr = next_addr;
                token = next_token;
                active = Some(name);
                session = None;
            }
            Ok(Outcome::SwitchSession { session_id, .. }) => session = Some(session_id),
            Err(e) => break Err(e),
        }
    };
    // 先关上报再恢复终端：否则退出后终端仍会把焦点变化写成转义序列涌向 shell
    let _ = execute!(std::io::stdout(), DisableFocusChange);
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
    wanted_session: Option<String>,
    key_rx: &mut mpsc::Receiver<InputEvent>,
) -> Result<Outcome> {
    let workspace = options.workspace.as_str();
    let api = SessionApi::new(addr, token);
    // 会话列表既用于挑选连接目标，也供切换弹窗使用：连接时取一次快照
    let sessions = api.list_sessions(Some(workspace)).await?;
    let record = match pick_session(&sessions, wanted_session.as_deref()) {
        Some(existing) => existing.clone(),
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
        app.session_id = record.session_id.clone();
        app.sessions = sessions;
        app.model_catalog = ready.model_catalog.clone();
        app.agents = ready.agents.clone();
        app.reasoning_level = ready.reasoning_level.clone();
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

/// 在会话列表里选出要连的那个：显式指定的 id 优先，找不到就退回列表首个。
///
/// 显式 id 可能因为「在别处被删掉」而落空，此时退回首个而不是新建会话——
/// 用户按的是「切到那个会话」，不该换来一个空的。
fn pick_session<'a>(sessions: &'a [SessionRecord], wanted: Option<&str>) -> Option<&'a SessionRecord> {
    wanted
        .and_then(|id| sessions.iter().find(|s| s.session_id == id))
        .or_else(|| sessions.first())
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    client: &mut OmaClient,
    key_rx: &mut mpsc::Receiver<InputEvent>,
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
            input = key_rx.recv() => {
                let Some(input) = input else { return Ok(Outcome::Quit) };
                // 焦点事件只更新状态，不进按键处理
                let Some(key) = classify_input(app, input) else {
                    terminal.draw(|frame| draw(frame, app))?;
                    last_draw = Instant::now();
                    continue;
                };
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

/// 消化输入线程转发的事件：焦点变化更新 [`App::focused`]，按键交回调用方处理。
fn classify_input(app: &mut App, input: InputEvent) -> Option<KeyEvent> {
    match input {
        InputEvent::Focus(focused) => {
            app.focused = focused;
            None
        }
        InputEvent::Key(key) => Some(key),
    }
}

/// 处理按键；返回 Some 表示离开当前会话（退出或切换连接）。
async fn handle_key(app: &mut App, client: &mut OmaClient, key: KeyEvent) -> Result<Option<Outcome>> {
    // 弹窗：先于普通输入消费按键
    if app.picker.is_some() {
        let action = handle_picker_key(app, key);
        return apply_picker_action(app, client, action).await;
    }

    match key.code {
        KeyCode::Esc => return Ok(Some(Outcome::Quit)),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            return Ok(Some(Outcome::Quit));
        }
        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => open_picker(app),
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => open_session_picker(app),
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => open_model_picker(app),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => open_agent_picker(app),
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => open_reasoning_picker(app),
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
        // 未定义的 Ctrl+字母不落进输入框：`Ctrl-D` 之类只该被忽略，
        // 不该在输入区留下一个 `d`
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => app.input.push(c),
        _ => {}
    }
    Ok(None)
}

/// 打开连接切换弹窗；无已保存连接时仅提示，不进入空列表。
fn open_picker(app: &mut App) {
    if app.connections.is_empty() {
        app.push(
            "·",
            "没有已保存的连接（可在设置页或 client.json 中添加）",
            Style::default().fg(app.theme.muted),
        );
        return;
    }
    let items: Vec<PickerItem> = app
        .connections
        .iter()
        .map(|conn| PickerItem {
            label:   conn.name.clone(),
            detail:  conn.url.clone(),
            current: app.active_conn.as_deref() == Some(conn.name.as_str()),
            action:  PickerAction::SwitchConnection {
                addr:  conn.url.clone(),
                token: conn.token.clone(),
                name:  conn.name.clone(),
            },
        })
        .collect();
    // 高亮落在当前连接上：与旧行为一致，打开就能看到「现在在哪」
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换连接 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 打开会话切换弹窗。
///
/// 列表是连接时取的快照：别处新建的会话要等下次连接才会出现。只有一个会话时
/// 不弹窗——切过去只会重连同一个会话、清掉当前记录，没有意义。
fn open_session_picker(app: &mut App) {
    if app.sessions.len() < 2 {
        app.push(
            "·",
            "当前工作区只有一个会话（在 Web 侧新建后重进本界面即可切换）",
            Style::default().fg(app.theme.muted),
        );
        return;
    }
    let items: Vec<PickerItem> = app
        .sessions
        .iter()
        .map(|s| PickerItem {
            label:   s.title.clone(),
            detail:  format!("{} · {}", short_id(&s.session_id), s.active_model),
            current: s.session_id == app.session_id,
            action:  PickerAction::SwitchSession {
                session_id: s.session_id.clone(),
            },
        })
        .collect();
    // 高亮落在当前会话上：打开弹窗第一眼就知道「现在在哪」
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换会话 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 当前生效那一项的下标；没有则从第一项开始。
fn picker_current_index(items: &[PickerItem]) -> usize {
    items.iter().position(|item| item.current).unwrap_or(0)
}

/// 弹窗里要渲染的条目区间：以高亮项为中心，保证它始终落在窗口内。
///
/// 模型清单动辄十几个，超过可见行数时必须跟着滚动——否则高亮项移出可视区，
/// 用户看着没变化、却已经选中了别的东西。
fn picker_window(len: usize, index: usize, visible: usize) -> std::ops::Range<usize> {
    if visible >= len {
        return 0..len;
    }
    let start = index.saturating_sub(visible / 2).min(len - visible);
    start..start + visible
}

/// 打开模型选择器。
///
/// 条目值是 `provider/model_id`（与后端 `find_model` 的键一致）：同名模型挂在
/// 不同 provider 下时，只有带上 provider 才选得准。清单是扁平的，provider 因此
/// 写进右侧说明而不是分组标题——弹窗宽度有限，分组会多占一层缩进。
fn open_model_picker(app: &mut App) {
    let mut items: Vec<PickerItem> = Vec::new();
    for (provider, models) in &app.model_catalog {
        for model in models {
            let selector = format!("{provider}/{}", model.id);
            items.push(PickerItem {
                label:   if model.name.is_empty() {
                    model.id.clone()
                } else {
                    model.name.clone()
                },
                detail:  format!("{selector} · {}K", model.context_len / 1024),
                current: selector == app.model,
                action:  PickerAction::SetModel(selector),
            });
        }
    }
    if items.is_empty() {
        app.push(
            "·",
            "没有可用模型（可在 Web 设置页添加提供商与模型）",
            Style::default().fg(app.theme.muted),
        );
        return;
    }
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换模型 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 打开 Agent 预设选择器。
///
/// 条目值是预设 id（`SetAgent` 收的就是它），说明用预设自己的描述。
fn open_agent_picker(app: &mut App) {
    if app.agents.is_empty() {
        app.push(
            "·",
            "没有可用预设（可在 Web 设置页添加）",
            Style::default().fg(app.theme.muted),
        );
        return;
    }
    let items: Vec<PickerItem> = app
        .agents
        .iter()
        .map(|agent| PickerItem {
            label:   agent.name.clone(),
            detail:  agent.description.clone(),
            current: agent.id == app.agent,
            action:  PickerAction::SetAgent(agent.id.clone()),
        })
        .collect();
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换预设 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 打开推理等级选择器。
///
/// 只列规范等级：服务端把空串判为非法（`is_valid_reasoning_level` 不接受空串），
/// 「未设置」不是能下发的状态，因此不给这个选项。等级名照搬规范值——Web 端也是
/// 直接显示 `minimal` / `xhigh` 这类原值，两个客户端保持一致。
fn open_reasoning_picker(app: &mut App) {
    let items: Vec<PickerItem> = REASONING_LEVELS
        .iter()
        .map(|level| PickerItem {
            label:   (*level).to_string(),
            detail:  String::new(),
            current: *level == app.reasoning_level,
            action:  PickerAction::SetReasoningLevel((*level).to_string()),
        })
        .collect();
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换推理等级 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 弹窗按键：上下选择（j/k 亦可）、Enter 取出动作并关闭、Esc 关闭。
///
/// 返回 `Some(action)` 表示用户确认了某一项；`None` 表示按键已被消费。
fn handle_picker_key(app: &mut App, key: KeyEvent) -> Option<PickerAction> {
    // 只取两个数就放掉借用：下面要 take/清空 app.picker
    let (index, last) = {
        let picker = app.picker.as_ref()?;
        (picker.index, picker.items.len().saturating_sub(1))
    };
    match key.code {
        KeyCode::Esc => app.picker = None,
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(picker) = app.picker.as_mut() {
                picker.index = index.saturating_sub(1);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(picker) = app.picker.as_mut() {
                picker.index = (index + 1).min(last);
            }
        }
        KeyCode::Enter => {
            let picker = app.picker.take()?;
            return picker.items.into_iter().nth(index).map(|item| item.action);
        }
        _ => {}
    }
    None
}

/// 执行弹窗里确认的动作。
///
/// 换连接、换会话要重建会话循环（返回 `Some(Outcome)`）；换模型、换预设只是一条
/// 指令，本连接继续用，界面等 `ModelChanged` / `AgentChanged` 事件回填。
/// 后者失败时把原因写进记录——此时 App 不会被重建，提示是留得住的。
async fn apply_picker_action(
    app: &mut App,
    client: &OmaClient,
    action: Option<PickerAction>,
) -> Result<Option<Outcome>> {
    let Some(action) = action else {
        return Ok(None);
    };
    match action {
        PickerAction::SwitchConnection { addr, token, name } => Ok(Some(Outcome::Switch { addr, token, name })),
        PickerAction::SwitchSession { session_id } => Ok(Some(Outcome::SwitchSession { session_id })),
        PickerAction::SetModel(model) => {
            send_setting(app, client, AgentCommand::SetModel { model }, "切换模型").await;
            Ok(None)
        }
        PickerAction::SetAgent(agent) => {
            send_setting(app, client, AgentCommand::SetAgent { agent }, "切换预设").await;
            Ok(None)
        }
        PickerAction::SetReasoningLevel(level) => {
            send_setting(app, client, AgentCommand::SetReasoningLevel { level }, "切换推理等级").await;
            Ok(None)
        }
    }
}

/// 下发一条设置类指令；失败时把原因写进记录。
///
/// 这类动作不重建会话循环，所以写下的提示留得住（换连接/换会话那两条则留不住，
/// 见 [`PickerAction`]）。成功时什么都不写：界面等 `ModelChanged` 之类的回执。
async fn send_setting(app: &mut App, client: &OmaClient, command: AgentCommand, what: &str) {
    if let Err(e) = client.send_command(command).await {
        app.push("!", format!("{what}失败: {e}"), Style::default().fg(app.theme.error));
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
            // 出错已单独报错，不再发系统通知
            if !matches!(stop_reason, StopReason::Error) {
                app.notify(format!("任务完成（{}）", stop_reason_label(stop_reason)));
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
        AgentEvent::ThinkingDelta { delta } => {
            app.append_stream("思", &delta, Style::default().fg(app.theme.thinking));
        }
        // 思考段耗时只在 Web 端的折叠头上展示；TUI 的思考是顺序流式输出，不另挂耗时
        AgentEvent::ThinkingFinished { .. } => {}
        AgentEvent::TextDelta { delta } => {
            app.append_stream("AI", &delta, Style::default().fg(app.theme.success));
        }
        AgentEvent::ToolCallStarted(data) => app.push(
            "⚙",
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
        AgentEvent::ContextUsage { tokens, context_len } => {
            app.context = Some((tokens, context_len));
        }
        // 本轮累计用量的增量更新：TUI 只在整轮结束时展示一次，不重复刷屏
        AgentEvent::UsageUpdated { .. } => {}
        AgentEvent::ReasoningLevelChanged { level } => {
            app.reasoning_level = level.clone();
            app.push(
                "·",
                if level.is_empty() {
                    "推理等级 → 模型默认".to_string()
                } else {
                    format!("推理等级 → {level}")
                },
                Style::default().fg(app.theme.muted),
            );
        }
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

    if let Some(picker) = &app.picker {
        draw_picker(frame, picker, area, theme);
    }
}

/// 弹窗：列出条目，标记当前生效的那一项。
fn draw_picker(frame: &mut Frame, picker: &Picker, area: Rect, theme: TuiTheme) {
    if picker.items.is_empty() {
        return;
    }
    // 可见行数：最多占屏幕一半，长清单不该顶掉整个界面
    let visible = picker
        .items
        .len()
        .min((area.height / 2).saturating_sub(2).max(1) as usize);
    let height = visible as u16 + 2;
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
        .title(Span::styled(picker.title.clone(), Style::default().fg(theme.muted)));
    frame.render_widget(block, popup);

    let inner = Rect {
        x:      popup.x + 1,
        y:      popup.y + 1,
        width:  popup.width.saturating_sub(2),
        height: popup.height.saturating_sub(2),
    };
    let window = picker_window(picker.items.len(), picker.index, visible);
    let start = window.start;
    let lines: Vec<Line<'static>> = picker.items[window]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            let selected = start + offset == picker.index;
            let marker = if selected { "> " } else { "  " };
            let label = if item.current {
                format!("{} ✓", item.label)
            } else {
                item.label.clone()
            };
            let style = if selected {
                Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.subtext)
            };
            Line::from(vec![
                Span::styled(marker, Style::default().fg(theme.accent)),
                Span::styled(label, style),
                Span::raw("  "),
                Span::styled(item.detail.clone(), Style::default().fg(theme.muted)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// 上下文占用进度条：10 格 + 百分比。
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
        " Enter 发送 · Ctrl+X 中止 · Ctrl+O 连接 N 会话 L 模型 A 预设 R 推理 · Ctrl+U 清空 · Esc 退出 "
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
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
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
        assert_eq!(picker_index(&app), Some(1));
        handle_picker_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(picker_index(&app), Some(0));
        handle_picker_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(picker_index(&app), Some(0));

        // Enter 取出切换目标并关掉弹窗
        match handle_picker_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Some(PickerAction::SwitchConnection { addr, token, name }) => {
                assert_eq!(name, "a");
                assert_eq!(addr, "http://a");
                assert_eq!(token, "ta");
            }
            _ => panic!("Enter 应返回切换目标"),
        }
        assert!(app.picker.is_none(), "确认后弹窗应关闭");

        // 没有已保存连接时仅提示，不进入空列表
        app.connections.clear();
        open_picker(&mut app);
        assert!(app.picker.is_none());
    }

    /// 会话弹窗：高亮当前会话并标 ✓，确认后返回该会话。
    #[test]
    fn test_session_picker_marks_current_and_returns_target() {
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
        app.session_id = "s2".into();
        app.sessions = vec![record("s1", "第一个会话"), record("s2", "当前会话")];

        open_session_picker(&mut app);
        assert_eq!(picker_index(&app), Some(1), "应停在当前会话上");
        let items = &app.picker.as_ref().unwrap().items;
        assert!(items[1].current && !items[0].current, "只有当前会话带 ✓");
        // 说明里带模型名，便于区分同名会话
        assert!(items[1].detail.contains('m'));

        match handle_picker_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Some(PickerAction::SwitchSession { session_id }) => {
                assert_eq!(session_id, "s2");
            }
            _ => panic!("Enter 应返回会话切换目标"),
        }

        // Esc 关掉弹窗且不返回动作
        open_session_picker(&mut app);
        assert!(handle_picker_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_none());
        assert!(app.picker.is_none());

        // 只有一个会话时只提示，不弹窗
        app.sessions.truncate(1);
        open_session_picker(&mut app);
        assert!(app.picker.is_none());
    }

    /// 选会话：显式 id 优先；id 已不存在（别处删掉了）时退回首个而非新建。
    #[test]
    fn test_pick_session_prefers_explicit_id() {
        let list = vec![record("s1", "一"), record("s2", "二")];
        assert_eq!(pick_session(&list, Some("s2")).unwrap().session_id, "s2");
        assert_eq!(pick_session(&list, Some("已删除")).unwrap().session_id, "s1");
        assert_eq!(pick_session(&list, None).unwrap().session_id, "s1");
        assert!(pick_session(&[], None).is_none());
    }

    /// 模型选择器：下发值必须是 `provider/model_id`，否则后端 find_model 找不到；
    /// 没有展示名时回落到 id，说明里带上窗口大小。
    #[test]
    fn test_model_picker_uses_provider_qualified_selector() {
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
        app.model = "p2/m2".into();
        app.model_catalog = BTreeMap::from([
            ("p1".to_string(), vec![model("m1", "一号模型", 200_000)]),
            ("p2".to_string(), vec![model("m2", "", 128_000)]),
        ]);

        open_model_picker(&mut app);
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.items.len(), 2);
        assert_eq!(picker.index, 1, "应停在当前模型上");
        assert!(picker.items[1].current && !picker.items[0].current);

        assert_eq!(picker.items[0].label, "一号模型");
        assert_eq!(picker.items[0].detail, "p1/m1 · 195K");
        assert_eq!(picker.items[1].label, "m2", "没有展示名时回落到 id");
        match &picker.items[0].action {
            PickerAction::SetModel(selector) => assert_eq!(selector, "p1/m1"),
            _ => panic!("模型条目应下发 SetModel"),
        }
    }

    /// 预设选择器：下发值是预设 id（不是展示名），说明用预设自己的描述。
    #[test]
    fn test_agent_picker_uses_preset_id() {
        let mut app = App::new("/w".into(), "m".into(), "极简".into(), TuiTheme::test());
        app.agents = vec![
            AgentSummary {
                id:          "build".into(),
                name:        "构建".into(),
                description: "拥有全部工具".into(),
            },
            AgentSummary {
                id:          "极简".into(),
                name:        "极简".into(),
                description: "少啰嗦".into(),
            },
        ];

        open_agent_picker(&mut app);
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.index, 1, "应停在当前预设上");
        assert!(picker.items[1].current && !picker.items[0].current);
        assert_eq!(picker.items[1].detail, "少啰嗦");
        match &picker.items[1].action {
            PickerAction::SetAgent(id) => assert_eq!(id, "极简"),
            _ => panic!("预设条目应下发 SetAgent"),
        }
    }

    /// 推理等级选择器：只列规范等级（服务端拒绝空串），当前档带 ✓，
    /// 下发值与弹窗里显示的字符串完全一致。
    #[test]
    fn test_reasoning_picker_lists_spec_levels_only() {
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
        app.reasoning_level = "high".into();

        open_reasoning_picker(&mut app);
        let picker = app.picker.as_ref().unwrap();
        assert_eq!(picker.items.len(), REASONING_LEVELS.len());
        assert_eq!(picker.items[0].label, "minimal");
        assert_eq!(picker.items[picker.index].label, "high", "应停在当前档上");
        assert_eq!(picker.items.iter().filter(|i| i.current).count(), 1);
        // 「未设置」不是合法等级，不能出现在清单里
        assert!(picker.items.iter().all(|i| !i.label.is_empty()));
        match &picker.items[picker.index].action {
            PickerAction::SetReasoningLevel(level) => assert_eq!(level, "high"),
            _ => panic!("推理等级条目应下发 SetReasoningLevel"),
        }
    }

    /// 弹窗滚动窗口：高亮项必须始终落在可见区间内，长清单不能被截断。
    #[test]
    fn test_picker_window_keeps_selection_visible() {
        // 清单比可见行数短：全部渲染
        assert_eq!(picker_window(2, 1, 5), 0..2);
        assert_eq!(picker_window(0, 0, 5), 0..0);
        // 顶部与底部：窗口贴边，不越界
        assert_eq!(picker_window(10, 0, 4), 0..4);
        assert_eq!(picker_window(10, 9, 4), 6..10);
        // 中间：高亮项居中
        assert_eq!(picker_window(10, 5, 4), 3..7);
        // 任意下标下窗口都包含它，且长度不超过可见行数
        for len in 1..12usize {
            for index in 0..len {
                let w = picker_window(len, index, 4);
                assert!(
                    w.start <= index && index < w.end,
                    "len={len} index={index} 窗口 {w:?} 漏掉高亮项"
                );
                assert!(w.end - w.start <= 4, "len={len} index={index} 窗口 {w:?} 超出可见行数");
                assert!(w.end <= len);
            }
        }
    }

    /// 焦点上报驱动通知开关：未上报焦点时按聚焦处理（不打扰），
    /// 失焦后才允许通知；按键不受焦点影响，照常交回处理。
    #[test]
    fn test_focus_tracking_gates_notifications() {
        let theme = TuiTheme::new(&ResolvedTheme {
            mode:   ThemeMode::Dark,
            accent: "blue".into(),
            light:  palette_with("latte", "#111111", "#222222"),
            dark:   palette_with("mocha", "#eeeeee", "#dddddd"),
        });
        let mut app = App::new("/w".into(), "m".into(), "a".into(), theme);

        // 初始（终端未上报过焦点）视为聚焦：不发通知
        assert!(app.focused);

        // 失焦后才发通知
        assert!(classify_input(&mut app, InputEvent::Focus(false)).is_none());
        assert!(!app.focused);

        // 重新聚焦后停止通知
        assert!(classify_input(&mut app, InputEvent::Focus(true)).is_none());
        assert!(app.focused);

        // 按键照常交回，不改变焦点状态
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(classify_input(&mut app, InputEvent::Key(key)).is_some());
        assert!(app.focused);
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

    /// 弹窗当前高亮项；未打开时为 None
    fn picker_index(app: &App) -> Option<usize> {
        app.picker.as_ref().map(|picker| picker.index)
    }

    fn record(id: &str, title: &str) -> SessionRecord {
        SessionRecord {
            session_id:   id.into(),
            workspace:    "/w".into(),
            title:        title.into(),
            active_model: "m".into(),
            active_agent: "a".into(),
        }
    }

    fn model(id: &str, name: &str, context_len: usize) -> ModelInfo {
        ModelInfo {
            id: id.into(),
            name: name.into(),
            context_len,
            capabilities: Default::default(),
            max_output: None,
            reasoning_map: Default::default(),
        }
    }
}
