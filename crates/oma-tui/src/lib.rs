//! oma-tui: 基于 Ratatui 的终端客户端
//!
//! 由 `oma tui` 子命令驱动：连接 Daemon 后实时渲染流式文本、思考块、
//! 工具调用。入口为 [`run`]，复用调用方的 tokio 运行时。

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use oma_client::{ConnectOptions, OmaClient, SessionApi, SessionRecord};
use oma_contract::{
    AgentCommand, AgentEvent, AgentSummary, ChatMessage, ClientType, ModelInfo, Palette, REASONING_LEVELS,
    ResolvedTheme, Role, StopReason, ThemeMode, ToolCallStartedData,
};
use ratatui::{
    Frame,
    crossterm::{
        event::{
            self, DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange, Event, KeyCode,
            KeyEvent, KeyEventKind, KeyModifiers,
        },
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
/// 活动动画：8 点盲文旋转帧，节奏与重绘间隔一致
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// 空闲/完成态：实心的 8 点盲文
const SPINNER_DONE: &str = "⣿";
const SPINNER_ADVANCE_MS: u128 = 80;
/// 状态带里工作区路径的最大显示宽度
const STATUS_PATH_MAX: usize = 40;
/// 断线后两次重连之间的间隔
const RECONNECT_DELAY: Duration = Duration::from_millis(1500);
/// 断线后持续重试的上限：超过它就把最后一次错误交出去，不让界面无声地卡住
const RECONNECT_WINDOW: Duration = Duration::from_secs(60);

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
    /// 括号粘贴的整段文本：一次到达，不再是一串按键
    Paste(String),
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
    Quit {
        /// 设置里改过的连接清单；未改动为 None
        connections: Option<Vec<TuiConnection>>,
    },
    /// 换连接：地址与 token 一并更换，会话要重新挑
    Switch {
        addr:        String,
        token:       String,
        name:        String,
        /// 设置里改过的连接清单；未改动为 None
        connections: Option<Vec<TuiConnection>>,
    },
    /// 换会话：连接不动，只换 session_id（标题已在弹窗里提示过，这里不再带）
    SwitchSession { session_id: String },
    /// 重连指定会话：分支切换后需要重新读一遍历史。
    /// 必须带上 id —— 会话是「列表首个」自动挑的，不带 id 重连可能挑到别的会话。
    Reload { session_id: String },
    /// 连接断了：退避后重连同一个会话，而不是把用户踢出界面
    Reconnect { session_id: String },
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
    /// 下发改换当前分支（`SwitchBranch`）
    SwitchBranch {
        leaf_message_id: String,
    },
    /// 编辑重发：把历史消息的文本放回输入框，并在发送时从它之前分叉
    ForkFrom {
        /// 回填输入框的文本
        text:      String,
        /// 分叉点（那条用户消息的父节点）；None = 无处可分叉（会话首条）
        parent_id: Option<String>,
        /// 分叉点的展示文案，写进发送时的分隔行
        label:     String,
    },
    /// 删除该用户消息及其整棵子树
    DeleteMessage {
        message_id: String,
    },
    /// 为当前工作区新建一个会话，并切过去
    NewSession,
}

/// 弹窗选择器：连接、会话、模型、预设、推理等级、分支共用同一套渲染与按键。
struct Picker {
    /// 标题栏文案（自带按键提示）
    title: String,
    items: Vec<PickerItem>,
    index: usize,
}

/// 全屏历史树选择器的一条：已铺好的缩进前缀 + 单行摘要。
struct TreeItem {
    id:         String,
    /// 树形连接线（含祖先缩进），如 `├─ ` / `│  └─ `
    prefix:     String,
    label:      String,
    /// 从根到它是否落在当前激活分支上
    on_current: bool,
    /// 是否是分支末端
    is_leaf:    bool,
}

/// 全屏历史树选择器：整棵树平铺成可上下移动的列表，Enter 切到该节点。
struct TreeSelector {
    items: Vec<TreeItem>,
    index: usize,
}

/// 退出时回传的结果：最终活动连接与（若有改动）连接清单。
pub struct TuiExit {
    pub active:      Option<String>,
    /// 设置里增删改过连接时才为 Some，交由调用方写回 client.json
    pub connections: Option<Vec<TuiConnection>>,
}

/// 设置界面里的一行。
enum SettingsRow {
    /// 指向 `App::connections` 的下标
    Connection(usize),
    Model,
    Agent,
    Reasoning,
    Theme,
}

/// 连接编辑表单：名称/地址/凭证三段，Tab 切换字段、Enter 保存、Esc 取消。
#[derive(Clone)]
struct ConnForm {
    name:    String,
    url:     String,
    token:   String,
    field:   usize,
    /// Some(i) 表示编辑既有连接；None 表示新增
    editing: Option<usize>,
}

/// 设置界面状态。
struct SettingsState {
    index: usize,
    form:  Option<ConnForm>,
}

/// 设置界面按键的结果。
enum SettingsAction {
    None,
    Close,
    Switch {
        addr:  String,
        token: String,
        name:  String,
    },
}

/// 文本输入弹窗：标题 + 一行可编辑文本。
///
/// 列表弹窗（[`Picker`]）只能选不能写，而重命名会话需要输入；做成独立的小弹窗
/// 而不是往列表里塞一个假条目，将来别的输入场景（如新建会话时填标题）可以直接用。
struct TextPrompt {
    /// 标题栏文案（自带按键提示）
    title: String,
    /// 当前输入内容
    input: String,
    kind:  PromptKind,
}

/// 文本输入弹窗确认后要做的事
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptKind {
    /// 重命名当前会话
    RenameSession,
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
    /// 用户消息背景：比中性底色深一档，用来替代角色标签
    user_bg:  Color,
    /// 背景基色（base）：选中态「强调色实底 + base 文字」用
    base:     Color,
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
            user_bg:  Color::Reset,
            base:     Color::Reset,
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
            user_bg:  token_color(palette, "mantle"),
            base:     token_color(palette, "base"),
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

/// 记录块类型：决定前缀、是否可折叠，以及背景/配色的归属。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Notice,
}

/// 工具调用状态：`ToolCallStarted` 建块，`ToolCallFinished` 原地补齐输出与耗时。
#[derive(Debug, Clone)]
struct ToolState {
    call_id:     String,
    name:        String,
    /// 入参压成的单行摘要
    input:       String,
    output:      String,
    is_error:    bool,
    /// 是否已收到结束事件：进行中的调用即使全局折叠也保持展开
    done:        bool,
    duration_ms: Option<u64>,
}

/// 一条记录块
struct Entry {
    kind:        EntryKind,
    text:        String,
    /// 仅 [`EntryKind::Tool`] 使用
    tool:        Option<ToolState>,
    style:       Style,
    /// 思考段耗时（毫秒）；不可测时为 None
    duration_ms: Option<u64>,
    /// 是否仍在进行：进行中的思考/工具调用不参与折叠
    open:        bool,
}

impl Entry {
    fn text(kind: EntryKind, text: impl Into<String>, style: Style) -> Self {
        Self {
            kind,
            text: text.into(),
            tool: None,
            style,
            duration_ms: None,
            open: false,
        }
    }
}

struct App {
    workspace:               String,
    model:                   String,
    agent:                   String,
    /// 当前会话 id；弹窗里据此标记「就是它」
    session_id:              String,
    /// 当前工作区的会话列表：连接时取一次快照，供切换弹窗使用
    sessions:                Vec<SessionRecord>,
    /// 可选模型清单（provider → 模型），握手下发
    model_catalog:           BTreeMap<String, Vec<ModelInfo>>,
    /// 可选 Agent 预设，握手下发
    agents:                  Vec<AgentSummary>,
    /// 当前推理等级；空串表示未设置
    reasoning_level:         String,
    /// 当前分支的叶子消息；分支弹窗据此标记「就是这条」
    current_leaf:            Option<String>,
    connected:               bool,
    busy:                    bool,
    /// 本轮开始的墙钟时刻：整轮耗时由此计（`TurnFinished` 时取差）
    turn_started_at:         Option<Instant>,
    queue:                   usize,
    entries:                 Vec<Entry>,
    input:                   String,
    /// 编辑重发的分叉点：`Some(parent_id)` 表示下一次发送要从该节点长出分支
    fork_from:               Option<String>,
    /// 是否折叠思考块（进行中的思考段不受影响）
    thinking_collapsed:      bool,
    /// 是否折叠工具调用块（进行中的调用不受影响）
    tool_collapsed:          bool,
    /// 分叉点的展示文案（来源用户消息的摘要），发送时写进分隔行
    fork_label:              String,
    scroll:                  u16,
    stick:                   bool,
    /// 最近一次请求的上下文占用（tokens, context_len）
    context:                 Option<(usize, usize)>,
    /// 可切换的已保存连接
    connections:             Vec<TuiConnection>,
    /// 当前活动连接名
    active_conn:             Option<String>,
    /// 弹窗选择器；None = 未打开
    picker:                  Option<Picker>,
    /// 全屏历史树选择器；None = 未打开
    tree_view:               Option<TreeSelector>,
    /// 设置界面；None = 未打开
    settings:                Option<SettingsState>,
    /// 连接清单是否被设置界面改过（决定退出时是否回传写盘）
    connections_dirty:       bool,
    /// 已上传、待随下一条消息发送的剪贴板图片附件（session_attachment:// 引用）
    pending_images:          Vec<String>,
    /// 真正下发的系统提示词（`GET /api/system-prompt`）；未取到时为 None
    system_prompt:           Option<String>,
    /// 系统提示词折叠块是否收起
    system_prompt_collapsed: bool,
    /// 模型/预设变化后需要重取系统提示词
    system_prompt_dirty:     bool,
    /// 文本输入弹窗；None = 未打开
    prompt:                  Option<TextPrompt>,
    /// 终端是否处于聚焦状态；仅失焦时才发系统通知
    focused:                 bool,
    /// 由握手下发主题导出的语义配色
    theme:                   TuiTheme,
    /// 主题模式名（light/dark/system），设置页只读展示
    theme_mode:              String,
    /// 当前强调色令牌名，设置页只读展示
    theme_accent:            String,
    /// 进程启动时刻：活动动画的相位由此推进
    started_at:              Instant,
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
            current_leaf: None,
            connected: true,
            busy: false,
            turn_started_at: None,
            queue: 0,
            entries: Vec::new(),
            input: String::new(),
            fork_from: None,
            thinking_collapsed: false,
            tool_collapsed: false,
            fork_label: String::new(),
            scroll: 0,
            stick: true,
            context: None,
            connections: Vec::new(),
            active_conn: None,
            picker: None,
            tree_view: None,
            settings: None,
            connections_dirty: false,
            pending_images: Vec::new(),
            system_prompt: None,
            system_prompt_collapsed: true,
            system_prompt_dirty: false,
            prompt: None,
            // 终端未上报焦点事件时按聚焦处理：宁可不打扰，也不在用户正看着时弹通知
            focused: true,
            theme,
            theme_mode: String::new(),
            theme_accent: String::new(),
            started_at: Instant::now(),
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

    fn push_entry(&mut self, entry: Entry) {
        self.entries.push(entry);
        if self.entries.len() > MAX_LINES {
            let drop = self.entries.len() - MAX_LINES;
            self.entries.drain(..drop);
        }
    }

    /// 设置里改过连接时回传当前清单（用于写回 client.json）。
    fn dirty_connections(&self) -> Option<Vec<TuiConnection>> {
        self.connections_dirty.then(|| self.connections.clone())
    }

    /// 一条提示/状态记录（不进正文流）。
    fn push_notice(&mut self, text: impl Into<String>, style: Style) {
        self.push_entry(Entry::text(EntryKind::Notice, text, style));
    }

    /// 用户消息：单独成块（后续据类型加背景、去标签）。
    fn push_user(&mut self, text: impl Into<String>, style: Style) {
        self.push_entry(Entry::text(EntryKind::User, text, style));
    }

    /// 追加助手正文增量。
    fn append_assistant(&mut self, delta: &str, style: Style) {
        self.append_stream(EntryKind::Assistant, delta, style);
    }

    /// 追加思考增量：最后一段思考未收尾时续写，否则新起一段。
    fn append_thinking(&mut self, delta: &str, style: Style) {
        self.append_stream(EntryKind::Thinking, delta, style);
    }

    /// 追加流式增量：与最后一块同类且仍在进行时续写，否则新起一块；
    /// 新块开始即把未收尾的思考段标记为结束（服务端没给 `ThinkingFinished` 时也不悬着）。
    fn append_stream(&mut self, kind: EntryKind, delta: &str, style: Style) {
        if let Some(last) = self.entries.last_mut() {
            if last.kind == kind && last.open {
                last.text.push_str(delta);
                return;
            }
        }
        self.finish_thinking(None);
        let mut entry = Entry::text(kind, delta, style);
        entry.open = true;
        self.push_entry(entry);
    }

    /// 收尾最近一段未结束的思考（`duration_ms` 为 None 表示不可测）。
    fn finish_thinking(&mut self, duration_ms: Option<u64>) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.kind == EntryKind::Thinking && entry.open)
        {
            entry.open = false;
            entry.duration_ms = duration_ms;
        }
    }

    /// 工具调用开始：建一条进行中的工具块。
    fn tool_started(&mut self, data: ToolCallStartedData) {
        self.finish_thinking(None);
        let mut entry = Entry::text(EntryKind::Tool, String::new(), Style::default());
        entry.open = true;
        entry.tool = Some(ToolState {
            call_id:     data.call_id,
            name:        data.tool_name,
            input:       summarize_tool_input(&data.input),
            output:      String::new(),
            is_error:    false,
            done:        false,
            duration_ms: None,
        });
        self.push_entry(entry);
    }

    /// 工具调用结束：按 `call_id` 找到开始块原地补齐；找不到（中途接入）就补一条完整的。
    fn tool_finished(&mut self, call_id: &str, tool_name: &str, output: String, is_error: bool, duration_ms: u64) {
        if let Some(entry) = self.entries.iter_mut().rev().find(|entry| {
            entry
                .tool
                .as_ref()
                .is_some_and(|tool| tool.call_id == call_id && !tool.done)
        }) {
            if let Some(tool) = entry.tool.as_mut() {
                tool.output = output;
                tool.is_error = is_error;
                tool.done = true;
                tool.duration_ms = Some(duration_ms);
            }
            entry.open = false;
            return;
        }
        let mut entry = Entry::text(EntryKind::Tool, String::new(), Style::default());
        entry.tool = Some(ToolState {
            call_id: call_id.to_string(),
            name: tool_name.to_string(),
            input: String::new(),
            output,
            is_error,
            done: true,
            duration_ms: Some(duration_ms),
        });
        self.push_entry(entry);
    }

    /// 按可用宽度把内容块折成渲染行。
    fn wrapped_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for entry in &self.entries {
            match entry.kind {
                EntryKind::Tool => self.push_tool_lines(&mut lines, entry, width),
                // 用户消息用更深底色与模型内容区分，不再加角色标签
                EntryKind::User => self.push_user_lines(&mut lines, entry, width),
                // 折叠只作用于已结束的思考段：进行中的永远展开
                EntryKind::Thinking if self.thinking_collapsed && !entry.open => {
                    lines.push(Line::from(Span::styled(
                        collapsed_label("思", entry.duration_ms),
                        Style::default()
                            .fg(self.theme.muted)
                            .add_modifier(Modifier::DIM),
                    )));
                }
                // 展开的思考段把耗时放在首行前缀里（折叠态见上）
                EntryKind::Thinking => {
                    let prefix = match entry.duration_ms {
                        Some(ms) => format!("思 · {}", format_duration(ms)),
                        None => "思".to_string(),
                    };
                    push_wrapped(&mut lines, &prefix, &entry.text, self.dim_style(entry), width);
                }
                _ => push_wrapped(
                    &mut lines,
                    entry_prefix(entry.kind),
                    &entry.text,
                    self.dim_style(entry),
                    width,
                ),
            }
        }
        lines
    }

    /// 内容文本统一降一档亮度：模型回答、思考、工具调用与提示信息都用 DIM；
    /// 用户消息与报错保持醒目（后者是故障信号，淡化会让人错过）。
    fn dim_style(&self, entry: &Entry) -> Style {
        match entry.kind {
            EntryKind::User => entry.style,
            EntryKind::Notice if entry.style.fg == Some(self.theme.error) => entry.style,
            _ => entry.style.add_modifier(Modifier::DIM),
        }
    }

    /// 用户块：整行铺满背景色（右侧补空格），首行不再带「你」标签。
    fn push_user_lines(&self, lines: &mut Vec<Line<'static>>, entry: &Entry, width: usize) {
        let style = entry.style.bg(self.theme.user_bg);
        for chunk in wrap_text(&entry.text, width.max(8)) {
            lines.push(Line::from(Span::styled(pad_to_width(&chunk, width), style)));
        }
    }

    /// 工具块：折叠（已结束）时只留一行头，展开时头 + 输出前几行。
    fn push_tool_lines(&self, lines: &mut Vec<Line<'static>>, entry: &Entry, width: usize) {
        let Some(tool) = entry.tool.as_ref() else {
            return;
        };
        let collapsed = self.tool_collapsed && tool.done && !entry.open;
        let marker = if collapsed { "▸" } else { "⚙" };
        let duration = tool
            .duration_ms
            .map(|ms| format!(" · {}", format_duration(ms)))
            .unwrap_or_default();
        let head_color = if tool.is_error {
            self.theme.error
        } else {
            self.theme.warning
        };
        let head = format!("{marker} {} {}{}", tool.name, tool.input, duration);
        // 报错保持醒目，其余工具内容降一档亮度
        let head_style = Style::default().fg(head_color);
        let head_style = if tool.is_error {
            head_style
        } else {
            head_style.add_modifier(Modifier::DIM)
        };
        lines.push(Line::from(Span::styled(head.trim_end().to_string(), head_style)));

        if collapsed || tool.output.is_empty() {
            return;
        }
        let body_color = if tool.is_error {
            self.theme.error
        } else {
            self.theme.muted
        };
        let head_lines: Vec<&str> = tool.output.lines().take(6).collect();
        let suffix = if tool.output.lines().count() > 6 { "\n…" } else { "" };
        let body = format!("{}{}", head_lines.join("\n"), suffix);
        let body_style = Style::default().fg(body_color);
        let body_style = if tool.is_error {
            body_style
        } else {
            body_style.add_modifier(Modifier::DIM)
        };
        push_wrapped(lines, "  ", &body, body_style, width);
    }

    /// 系统提示词折叠块：收起时只占一行，展开时按宽度折行（降一档亮度）。
    fn system_prompt_lines(&self, width: usize) -> Vec<Line<'static>> {
        let Some(prompt) = &self.system_prompt else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        if self.system_prompt_collapsed {
            lines.push(Line::from(Span::styled(
                format!("▶ 系统提示词 · {} 行（Ctrl+P 展开）", prompt.lines().count()),
                Style::default().fg(self.theme.muted),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "▼ 系统提示词（Ctrl+P 收起）",
                Style::default().fg(self.theme.muted),
            )));
            let style = Style::default()
                .fg(self.theme.subtext)
                .add_modifier(Modifier::DIM);
            for chunk in wrap_text(prompt, width.max(8)) {
                lines.push(Line::from(Span::styled(chunk, style)));
            }
        }
        lines
    }
}

/// 记录块前缀（标签）。
fn entry_prefix(kind: EntryKind) -> &'static str {
    match kind {
        // 用户/模型的块不再用角色标签区分（用户块另有底色，见 push_user_lines）
        EntryKind::User | EntryKind::Assistant => "",
        EntryKind::Thinking => "思",
        EntryKind::Tool => "⚙",
        EntryKind::Notice => "·",
    }
}

/// 折行并写入渲染行：首行带前缀，续行按前缀宽度缩进。
fn push_wrapped(lines: &mut Vec<Line<'static>>, prefix: &str, text: &str, style: Style, width: usize) {
    let pad = if prefix.is_empty() {
        0
    } else {
        display_width(prefix) + 1
    };
    for (i, chunk) in wrap_text(text, width.saturating_sub(pad).max(8))
        .into_iter()
        .enumerate()
    {
        let head = if i == 0 && !prefix.is_empty() {
            format!("{prefix} ")
        } else {
            " ".repeat(pad)
        };
        // 正文与前缀同一样式：此前正文用 Span::raw，颜色只落在前缀上（淡色块看不出效果）
        lines.push(Line::from(Span::styled(format!("{head}{chunk}"), style)));
    }
}

/// 右侧补空格到给定显示宽度（用于把背景色铺满整行）。
fn pad_to_width(text: &str, width: usize) -> String {
    let used = display_width(text);
    if used >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - used))
    }
}

/// 折叠块的标签：`▸ 思 1.2s`（无耗时时只有图标）。
fn collapsed_label(icon: &str, duration_ms: Option<u64>) -> String {
    match duration_ms {
        Some(ms) => format!("▸ {icon} {}", format_duration(ms)),
        None => format!("▸ {icon}"),
    }
}

/// 耗时文案：不足 1 秒按毫秒（不四舍五入成 0s），1 秒以上保留一位小数，超过 1 分钟按分秒。
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let total = ms / 1000;
        format!("{}m{:02}s", total / 60, total % 60)
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

/// 从输入里摘出 `@相对路径` 附件标记，返回（正文, 附件相对路径）。
///
/// TUI 没有文件选择器，用文本约定代替：以 `@` 开头的词是附件引用，其余是正文。
/// 输入区本就是单行，空白折叠成单个空格不影响可读性。
fn extract_attachments(input: &str) -> (String, Vec<String>) {
    let mut body = Vec::new();
    let mut paths = Vec::new();
    for token in input.split_whitespace() {
        match token.strip_prefix('@') {
            Some(path) if !path.is_empty() => paths.push(path.to_string()),
            _ => body.push(token),
        }
    }
    (body.join(" "), paths)
}

/// 把 `@路径` 上传成会话附件，返回 `session_attachment://` 引用。
///
/// 只接受工作区内的相对路径（与 `edit` 工具同一口径）：绝对路径与 `..` 逃逸一律
/// 拒绝，免得把用户的任意文件都读出来传上服务端。失败时把原因写进记录并返回 Err，
/// 调用方据此保留输入——静默丢掉附件比报错更糟。
async fn resolve_attachments(app: &mut App, api: &SessionApi, paths: &[String]) -> Result<Vec<String>> {
    let mut refs = Vec::new();
    for raw in paths {
        let rel = raw.strip_prefix("./").unwrap_or(raw);
        if !is_workspace_relative(rel) {
            app.push_notice(
                format!("附件必须是工作区内的相对路径: {raw}"),
                Style::default().fg(app.theme.error),
            );
            bail!("attachment outside workspace");
        }
        let path = std::path::Path::new(rel);
        let full = std::path::Path::new(&app.workspace).join(path);
        let bytes = match tokio::fs::read(&full).await {
            Ok(bytes) => bytes,
            Err(e) => {
                app.push_notice(
                    format!("读取附件 {raw} 失败: {e}"),
                    Style::default().fg(app.theme.error),
                );
                bail!("cannot read attachment");
            }
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        match api.upload_attachment(&app.session_id, name, bytes).await {
            Ok(uploaded) => refs.extend(uploaded),
            Err(e) => {
                app.push_notice(
                    format!("上传附件 {raw} 失败: {e}"),
                    Style::default().fg(app.theme.error),
                );
                bail!("cannot upload attachment");
            }
        }
    }
    Ok(refs)
}

/// 附件路径是否可接受：非空、非绝对、不含 `..` 逃逸。
fn is_workspace_relative(rel: &str) -> bool {
    !rel.is_empty() && !std::path::Path::new(rel).is_absolute() && !rel.split(['/', '\\']).any(|seg| seg == "..")
}

/// 从系统剪贴板读取图片字节。
///
/// 终端本身拿不到剪贴板图片（除非终端私有协议），这里借助系统工具：
/// Wayland 用 `wl-paste`、X11 用 `xclip`、macOS 用 `pngpaste`。都不可用或
/// 剪贴板里没有图片时返回 None（由调用方给出提示）。
fn read_clipboard_image() -> Option<Vec<u8>> {
    let candidates: [(&str, &[&str]); 3] = [
        ("wl-paste", &["--type", "image/png"]),
        ("xclip", &["-selection", "clipboard", "-t", "image/png", "-o"]),
        ("pngpaste", &["-"]),
    ];
    for (program, args) in candidates {
        match std::process::Command::new(program).args(args).output() {
            Ok(output) if output.status.success() && !output.stdout.is_empty() => return Some(output.stdout),
            _ => continue,
        }
    }
    None
}

/// 读取剪贴板图片并作为附件上传，成功则记入待发送清单。
///
/// 失败时只提示不改输入：`@路径` 附件仍在；图片上传的成本已付，不该静默丢弃。
async fn attach_clipboard_image(app: &mut App, api: &SessionApi) {
    let bytes = match tokio::task::spawn_blocking(read_clipboard_image).await {
        Ok(Some(bytes)) => bytes,
        _ => {
            app.push_notice(
                "剪贴板里没有图片（需要 wl-paste / xclip / pngpaste 之一）",
                Style::default().fg(app.theme.warning),
            );
            return;
        }
    };
    let name = format!("clipboard-{}.png", app.pending_images.len() + 1);
    match api.upload_attachment(&app.session_id, &name, bytes).await {
        Ok(refs) => {
            let count = refs.len();
            app.pending_images.extend(refs);
            app.push_notice(
                format!("已添加图片附件 {count} 张（随下一条消息发送）"),
                Style::default().fg(app.theme.muted),
            );
        }
        Err(e) => app.push_notice(format!("上传剪贴板图片失败: {e}"), Style::default().fg(app.theme.error)),
    }
}

/// 取一次真正下发的系统提示词（随工作区/模型/预设变化重取）。
async fn refresh_system_prompt(app: &mut App, api: &SessionApi) {
    app.system_prompt_dirty = false;
    match api
        .system_prompt(&app.workspace, &app.model, &app.agent)
        .await
    {
        Ok(prompt) => app.system_prompt = Some(prompt),
        Err(e) => app.push_notice(format!("读取系统提示词失败: {e}"), Style::default().fg(app.theme.error)),
    }
}

/// 启动 TUI 客户端：复用当前工作区最近的会话，没有则新建。
///
/// 支持在界面内切换到 client.json 里保存的其他连接：切换时重建会话与事件流。
/// 退出时回传最终活动连接与改动过的连接清单，供调用方回写 client.json。
pub async fn run(options: TuiOptions) -> Result<TuiExit> {
    // crossterm 的阻塞读放在独立线程；切换连接只是重建会话，
    // 不重新开线程，避免多个 reader 竞争同一 tty。
    let (key_tx, mut key_rx) = mpsc::channel::<InputEvent>(64);
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            let forwarded = match ev {
                Event::Key(key) => InputEvent::Key(key),
                Event::FocusGained => InputEvent::Focus(true),
                Event::FocusLost => InputEvent::Focus(false),
                Event::Paste(text) => InputEvent::Paste(text),
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
    // 工作连接清单：设置界面里的增删改都在它上面生效，切换连接后继续沿用
    let mut connections = options.connections.clone();
    let mut edited_connections: Option<Vec<TuiConnection>> = None;
    // 用户显式挑过的会话：换连接后作废，重新按工作区挑
    let mut session: Option<String> = None;
    // 断线后进入重试窗口；只有「连上过又断开」才会设置它
    let mut retry_deadline: Option<Instant> = None;

    let mut terminal = ratatui::init();
    // 开启焦点变化上报（CSI ?1004h）：仅用于判断终端是否失焦，退出时恢复。
    // 终端不支持该序列时会直接忽略，不会报错。
    //
    // 括号粘贴（CSI ?2004h）让终端把一次粘贴包成整段文本发来，而不是逐字符敲键：
    // 否则粘多行文本里的换行会被当成回车，一粘就把消息发出去了。
    let _ = execute!(std::io::stdout(), EnableFocusChange, EnableBracketedPaste);
    let result = loop {
        let setup = SessionSetup {
            addr:        &addr,
            token:       &token,
            workspace:   &options.workspace,
            connections: &connections,
            active:      active.clone(),
        };
        let outcome = run_session(&mut terminal, &setup, session.clone(), &mut key_rx).await;
        match outcome {
            Ok(Outcome::Quit { connections: edited }) => {
                if edited.is_some() {
                    edited_connections = edited;
                }
                break Ok(TuiExit {
                    active,
                    connections: edited_connections,
                });
            }
            Ok(Outcome::Switch {
                addr: next_addr,
                token: next_token,
                name,
                connections: edited,
            }) => {
                if let Some(updated) = edited {
                    connections = updated.clone();
                    edited_connections = Some(updated);
                }
                addr = next_addr;
                token = next_token;
                active = Some(name);
                session = None;
            }
            // 换会话与重连（分支切换）都是「用这个 id 重跑一次会话循环」
            Ok(Outcome::SwitchSession { session_id } | Outcome::Reload { session_id }) => session = Some(session_id),
            Ok(Outcome::Reconnect { session_id }) => {
                session = Some(session_id);
                retry_deadline = Some(Instant::now() + RECONNECT_WINDOW);
                if !wait_before_retry(&mut key_rx).await {
                    break Ok(TuiExit {
                        active,
                        connections: edited_connections,
                    });
                }
            }
            Err(e) => {
                // 连上过之后才重试：地址写错时不该让用户白等一分钟才看到原因
                match retry_deadline {
                    Some(deadline) if Instant::now() < deadline => {
                        if !wait_before_retry(&mut key_rx).await {
                            break Ok(TuiExit {
                                active,
                                connections: edited_connections,
                            });
                        }
                    }
                    _ => break Err(e),
                }
            }
        }
    };
    // 先关上报再恢复终端：否则退出后终端仍会把焦点变化写成转义序列涌向 shell
    let _ = execute!(std::io::stdout(), DisableBracketedPaste, DisableFocusChange);
    ratatui::restore();
    result
}

/// 一次会话循环所需的连接参数（地址/凭证/工作区/可切换连接/活动连接）。
struct SessionSetup<'a> {
    addr:        &'a str,
    token:       &'a str,
    workspace:   &'a str,
    connections: &'a [TuiConnection],
    active:      Option<String>,
}

/// 连接一次、跑一个会话，直到退出或要求切换。
async fn run_session(
    terminal: &mut ratatui::DefaultTerminal,
    setup: &SessionSetup<'_>,
    wanted_session: Option<String>,
    key_rx: &mut mpsc::Receiver<InputEvent>,
) -> Result<Outcome> {
    let workspace = setup.workspace;
    let api = SessionApi::new(setup.addr, setup.token);
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
        addr:        setup.addr.to_string(),
        token:       setup.token.to_string(),
        workspace:   workspace.to_string(),
        session_id:  record.session_id.clone(),
        client_type: ClientType::Tui,
        client_name: "oma-tui".into(),
    })
    .await?;

    let mut app = {
        let ready = client.ready();
        let theme = TuiTheme::new(&ready.active_theme);
        let theme_mode = ready.active_theme.mode.as_str().to_string();
        let theme_accent = ready.active_theme.accent.clone();
        let mut app = App::new(
            ready.workspace.clone(),
            ready.active_model.clone(),
            ready.active_agent.clone(),
            theme,
        );
        app.theme_mode = theme_mode;
        app.theme_accent = theme_accent;
        app.connections = setup.connections.to_vec();
        // 活动连接：优先调用方给出的名字，否则按地址匹配已保存的条目
        app.active_conn = setup.active.clone().or_else(|| {
            app.connections
                .iter()
                .find(|c| c.url == setup.addr)
                .map(|c| c.name.clone())
        });
        app.session_id = record.session_id.clone();
        app.sessions = sessions;
        app.model_catalog = ready.model_catalog.clone();
        app.agents = ready.agents.clone();
        app.reasoning_level = ready.reasoning_level.clone();
        refresh_system_prompt(&mut app, &api).await;
        app.push_notice(
            format!("会话 {} 已连接", short_id(&ready.session_id)),
            Style::default().fg(theme.muted),
        );
        if let Some(leaf) = &ready.current_leaf_id {
            app.push_notice(
                format!("当前分支叶子 {}", short_id(leaf)),
                Style::default().fg(theme.muted),
            );
        }
        app
    };

    event_loop(terminal, &mut app, &mut client, &api, key_rx).await
}

/// 等待下一次重连尝试；期间照常响应退出键。
///
/// 返回 `false` 表示用户在等待期间要求退出。这里必须自己收按键：会话循环此刻已经
/// 退出了，没有别人读输入，若只是 sleep，用户按 Ctrl-C 也没人理会。
async fn wait_before_retry(key_rx: &mut mpsc::Receiver<InputEvent>) -> bool {
    let deadline = Instant::now() + RECONNECT_DELAY;
    while Instant::now() < deadline {
        tokio::select! {
            input = key_rx.recv() => match input {
                Some(InputEvent::Key(key)) if key.kind == KeyEventKind::Press && is_quit_key(key) => {
                    return false;
                }
                Some(_) => {}
                None => return false,
            },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }
    true
}

/// 退出键：Esc 或 Ctrl-C。
fn is_quit_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Esc)
        || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
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
    api: &SessionApi,
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
                    if app.system_prompt_dirty {
                        refresh_system_prompt(app, api).await;
                    }
                    terminal.draw(|frame| draw(frame, app))?;
                    last_draw = Instant::now();
                }
                None => {
                    app.connected = false;
                    terminal.draw(|frame| draw(frame, app))?;
                    return Ok(Outcome::Reconnect {
                        session_id: app.session_id.clone(),
                    });
                }
            },
            input = key_rx.recv() => {
                let Some(input) = input else {
                    return Ok(Outcome::Quit {
                        connections: app.dirty_connections(),
                    });
                };
                // 焦点事件只更新状态，不进按键处理
                let Some(key) = classify_input(app, input) else {
                    terminal.draw(|frame| draw(frame, app))?;
                    last_draw = Instant::now();
                    continue;
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if let Some(outcome) = handle_key(app, client, api, key).await? {
                    return Ok(outcome);
                }
                terminal.draw(|frame| draw(frame, app))?;
                last_draw = Instant::now();
            }
            _ = tokio::time::sleep(TICK) => {}
        }
    }
}

/// 消化输入线程转发的事件：焦点变化更新 [`App::focused`]，粘贴直接进输入，按键交回调用方。
fn classify_input(app: &mut App, input: InputEvent) -> Option<KeyEvent> {
    match input {
        InputEvent::Focus(focused) => {
            app.focused = focused;
            None
        }
        InputEvent::Key(key) => Some(key),
        InputEvent::Paste(text) => {
            insert_paste(app, &text);
            None
        }
    }
}

/// 把粘贴的整段文本放进当前获得输入的那个框。
///
/// 列表弹窗没有可输入的地方，粘贴对它是噪声，直接丢掉；文本弹窗（重命名）优先于主输入区。
fn insert_paste(app: &mut App, text: &str) {
    if app.picker.is_some() {
        return;
    }
    let text = clean_paste(text);
    match app.prompt.as_mut() {
        Some(prompt) => prompt.input.push_str(&text),
        None => app.input.push_str(&text),
    }
}

/// 粘贴文本的清理：输入区是单行，换行与制表符换成空格，其余控制字符丢掉。
///
/// 开启括号粘贴后整段文本一次到达，逐字符时代「换行=回车」的语义消失了；
/// 若不清理，多行文本里夹带的控制字符会原样留在输入里，看不见又删不掉。
fn clean_paste(text: &str) -> String {
    let unified = text.replace("\r\n", "\n");
    unified
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// 处理按键；返回 Some 表示离开当前会话（退出或切换连接）。
async fn handle_key(app: &mut App, client: &mut OmaClient, api: &SessionApi, key: KeyEvent) -> Result<Option<Outcome>> {
    // Ctrl-C 是全局退出键：无论弹窗/设置是否打开都要生效，
    // 否则在设置页里按 Ctrl-C 只会被当成普通字符消费掉，退不出去。
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(Some(Outcome::Quit {
            connections: app.dirty_connections(),
        }));
    }
    // 弹窗：先于普通输入消费按键
    if app.picker.is_some() {
        let action = handle_picker_key(app, key);
        return apply_picker_action(app, client, api, action).await;
    }
    if app.prompt.is_some() {
        let request = handle_prompt_key(app, key);
        return apply_prompt(app, api, request).await;
    }
    if app.tree_view.is_some() {
        match handle_tree_key(app, key) {
            TreeAction::Close => app.tree_view = None,
            TreeAction::Select(leaf_message_id) => {
                app.tree_view = None;
                let session_id = app.session_id.clone();
                send_setting(app, client, AgentCommand::SwitchBranch { leaf_message_id }, "切换分支").await;
                return Ok(Some(Outcome::Reload { session_id }));
            }
            TreeAction::None => {}
        }
        return Ok(None);
    }
    if app.settings.is_some() {
        match handle_settings_key(app, key) {
            SettingsAction::Close => app.settings = None,
            SettingsAction::Switch { addr, token, name } => {
                return Ok(Some(Outcome::Switch {
                    addr,
                    token,
                    name,
                    connections: app.dirty_connections(),
                }));
            }
            SettingsAction::None => {}
        }
        return Ok(None);
    }
    // 编辑重发中的 Esc 先取消分叉，再按一次才退出：直接退出会让用户丢掉正在编辑的内容
    if key.code == KeyCode::Esc && app.fork_from.is_some() {
        app.fork_from = None;
        app.fork_label.clear();
        app.push_notice("已取消编辑重发", Style::default().fg(app.theme.muted));
        return Ok(None);
    }
    if is_quit_key(key) {
        return Ok(Some(Outcome::Quit {
            connections: app.dirty_connections(),
        }));
    }

    match key.code {
        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => open_picker(app),
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => open_session_picker(app, api).await,
        KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => open_model_picker(app),
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => open_agent_picker(app),
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => open_reasoning_picker(app),
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => open_branch_picker(app, api).await,
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => open_fork_picker(app, api).await,
        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => open_rename_prompt(app),
        // 折叠开关：分别控制思考段与工具调用；进行中的块不受影响（见 wrapped_lines）
        KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.thinking_collapsed = !app.thinking_collapsed;
        }
        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.tool_collapsed = !app.tool_collapsed;
        }
        // 系统提示词折叠块开合
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.system_prompt_collapsed = !app.system_prompt_collapsed;
        }
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
            app.push_notice("已请求中止", Style::default().fg(app.theme.warning));
        }
        // 粘贴剪贴板图片：作为附件随下一条消息发送
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            attach_clipboard_image(app, api).await;
        }
        KeyCode::Enter => {
            let raw = app.input.trim().to_string();
            // 只有待发送的图片附件时也允许发送（正文可以为空）
            if raw.is_empty() && app.pending_images.is_empty() {
                return Ok(None);
            }
            // 斜杠命令：本地分发，不发给模型
            if let Some((cmd, args)) = parse_slash(&raw) {
                app.input.clear();
                return handle_slash(app, api, cmd, args).await;
            }
            let (text, paths) = extract_attachments(&raw);
            // 附件先落库：上传失败就整条不发送，并把输入原样留给用户改，静默丢掉附件更糟
            let attachments = match resolve_attachments(app, api, &paths).await {
                Ok(attachments) => attachments,
                Err(_) => return Ok(None),
            };
            // 剪贴板图片此前已上传：取出来与 @路径 附件合并后一起发送
            let pending_images: Vec<String> = std::mem::take(&mut app.pending_images);
            let mut attachments = attachments;
            attachments.extend(pending_images.iter().cloned());
            if text.is_empty() && attachments.is_empty() {
                return Ok(None);
            }
            let fork = app.fork_from.take();
            if fork.is_some() && !attachments.is_empty() {
                // ForkAndRun 没有附件位（协议如此），与 Web 一致直接拒绝，别把附件丢掉
                app.fork_from = fork;
                // 图片还没发出去，放回待发送清单，用户取消分叉后仍能用
                app.pending_images = pending_images;
                app.push_notice(
                    "编辑重发不支持附件：先取消分叉或去掉 @路径",
                    Style::default().fg(app.theme.error),
                );
                return Ok(None);
            }
            app.input.clear();
            let label = std::mem::take(&mut app.fork_label);
            // 分叉处插一条分隔行：TUI 的记录是只追加的日志，旧分支的尾巴还留在上方，
            // 不标出边界就看不出新回答是从哪里长出来的
            if fork.is_some() {
                app.push_notice(
                    format!("── 从「{label}」处重新开始 ──"),
                    Style::default().fg(app.theme.muted),
                );
            }
            // 记录里仍显示用户原样敲的那行（含 @路径），发出去的内容才是摘掉标记的正文
            app.push_user(raw, Style::default().fg(app.theme.accent));
            if !attachments.is_empty() {
                let mut parts = paths.clone();
                if !pending_images.is_empty() {
                    parts.push(format!("剪贴板图片 {} 张", pending_images.len()));
                }
                app.push_notice(
                    format!("附件 {} 个：{}", attachments.len(), parts.join("、")),
                    Style::default().fg(app.theme.muted),
                );
            }
            app.stick = true;
            let command = match fork {
                // 编辑重发：从分叉点长出新分支，而不是追加到当前叶子后面
                Some(parent_message_id) => AgentCommand::ForkAndRun {
                    parent_message_id,
                    new_content: Some(text),
                },
                None => AgentCommand::UserInput {
                    content: text,
                    attachments,
                },
            };
            if let Err(e) = client.send_command(command).await {
                app.push_notice(format!("发送失败: {}", e), Style::default().fg(app.theme.error));
            }
        }
        // 未定义的 Ctrl+字母不落进输入框：`Ctrl-D` 之类只该被忽略，
        // 不该在输入区留下一个 `d`
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => app.input.push(c),
        _ => {}
    }
    Ok(None)
}

/// 解析斜杠命令：`/model` → `("model", "")`，`/rename 新名字` → `("rename", "新名字")`。
///
/// 非斜杠输入返回 None（当作普通消息发送）。
fn parse_slash(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim().strip_prefix('/')?.trim();
    if rest.is_empty() {
        return None;
    }
    Some(match rest.split_once(char::is_whitespace) {
        Some((cmd, args)) => (cmd, args.trim()),
        None => (rest, ""),
    })
}

/// 斜杠命令分发表；返回 Some 表示要离开当前会话循环（退出 / 换会话）。
async fn handle_slash(app: &mut App, api: &SessionApi, cmd: &str, args: &str) -> Result<Option<Outcome>> {
    match cmd {
        "help" => {
            app.push_notice(SLASH_HELP, Style::default().fg(app.theme.muted));
            Ok(None)
        }
        "model" => {
            open_model_picker(app);
            Ok(None)
        }
        "agent" => {
            open_agent_picker(app);
            Ok(None)
        }
        "reasoning" => {
            open_reasoning_picker(app);
            Ok(None)
        }
        "session" => {
            open_session_picker(app, api).await;
            Ok(None)
        }
        "tree" => {
            open_tree(app, api).await;
            Ok(None)
        }
        "delete" => {
            open_delete_picker(app, api).await;
            Ok(None)
        }
        "settings" => {
            open_settings(app);
            Ok(None)
        }
        "new" => match api.create_session(&app.workspace, None).await {
            Ok(record) => Ok(Some(Outcome::SwitchSession {
                session_id: record.session_id,
            })),
            Err(e) => {
                app.push_notice(format!("新建会话失败: {e}"), Style::default().fg(app.theme.error));
                Ok(None)
            }
        },
        "rename" => {
            open_rename_prompt(app);
            // 带参数时直接改名，不进弹窗
            if !args.is_empty() {
                app.prompt = None;
                match api.rename_session(&app.session_id, args).await {
                    Ok(()) => {
                        if let Some(record) = app
                            .sessions
                            .iter_mut()
                            .find(|s| s.session_id == app.session_id)
                        {
                            record.title = args.to_string();
                        }
                        app.push_notice(format!("会话已重命名为 {args}"), Style::default().fg(app.theme.muted));
                    }
                    Err(e) => app.push_notice(format!("重命名失败: {e}"), Style::default().fg(app.theme.error)),
                }
            }
            Ok(None)
        }
        "clear" => {
            app.entries.clear();
            app.scroll = 0;
            app.stick = true;
            Ok(None)
        }
        "quit" => Ok(Some(Outcome::Quit {
            connections: app.dirty_connections(),
        })),
        _ => {
            app.push_notice(
                format!("未知命令 /{cmd}（/help 查看可用命令）"),
                Style::default().fg(app.theme.error),
            );
            Ok(None)
        }
    }
}

/// `/help` 的命令清单。
const SLASH_HELP: &str = "/help 帮助 · /model 模型 · /agent 预设 · /reasoning 推理等级\n\
     /session 切换会话 · /new 新建会话 · /rename [标题] 重命名 · /tree 历史树\n\
     /delete 删除消息（含子树）· /settings 设置 · /clear 清空记录显示 · /quit 退出";

/// 打开设置界面；高亮落在当前活动连接那一行。
fn open_settings(app: &mut App) {
    let index = app
        .connections
        .iter()
        .position(|conn| app.active_conn.as_deref() == Some(conn.name.as_str()))
        .unwrap_or(0);
    app.settings = Some(SettingsState { index, form: None });
}

/// 设置界面的行：先列连接，再是模型/预设/推理等级/主题。
fn settings_rows(app: &App) -> Vec<SettingsRow> {
    let mut rows: Vec<SettingsRow> = (0..app.connections.len())
        .map(SettingsRow::Connection)
        .collect();
    rows.push(SettingsRow::Model);
    rows.push(SettingsRow::Agent);
    rows.push(SettingsRow::Reasoning);
    rows.push(SettingsRow::Theme);
    rows
}

/// 设置界面按键：移动、打开子项、增删改连接；Esc 关闭。
fn handle_settings_key(app: &mut App, key: KeyEvent) -> SettingsAction {
    // 表单打开时先给它消费按键
    if app
        .settings
        .as_ref()
        .and_then(|state| state.form.as_ref())
        .is_some()
    {
        return handle_conn_form(app, key);
    }
    if app.settings.is_none() {
        return SettingsAction::None;
    }
    let rows = settings_rows(app);
    let last = rows.len().saturating_sub(1);
    let index = app
        .settings
        .as_ref()
        .map(|state| state.index)
        .unwrap_or(0)
        .min(last);
    match key.code {
        KeyCode::Esc => SettingsAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(state) = app.settings.as_mut() {
                state.index = index.saturating_sub(1);
            }
            SettingsAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(state) = app.settings.as_mut() {
                state.index = (index + 1).min(last);
            }
            SettingsAction::None
        }
        KeyCode::Char('a') => {
            open_conn_form(app, None);
            SettingsAction::None
        }
        KeyCode::Char('e') => {
            if let SettingsRow::Connection(i) = &rows[index] {
                open_conn_form(app, Some(*i));
            }
            SettingsAction::None
        }
        KeyCode::Char('d') => {
            if let SettingsRow::Connection(i) = &rows[index] {
                let removed = app.connections.remove(*i);
                if app.active_conn.as_deref() == Some(removed.name.as_str()) {
                    app.active_conn = None;
                }
                app.connections_dirty = true;
                let new_last = settings_rows(app).len().saturating_sub(1);
                if let Some(state) = app.settings.as_mut() {
                    state.index = state.index.min(new_last);
                }
            }
            SettingsAction::None
        }
        KeyCode::Enter => match &rows[index] {
            SettingsRow::Connection(i) => {
                let conn = &app.connections[*i];
                SettingsAction::Switch {
                    addr:  conn.url.clone(),
                    token: conn.token.clone(),
                    name:  conn.name.clone(),
                }
            }
            SettingsRow::Model => {
                open_model_picker(app);
                SettingsAction::None
            }
            SettingsRow::Agent => {
                open_agent_picker(app);
                SettingsAction::None
            }
            SettingsRow::Reasoning => {
                open_reasoning_picker(app);
                SettingsAction::None
            }
            SettingsRow::Theme => {
                app.push_notice(
                    "主题由服务端配置决定（可在 Web 设置里修改）",
                    Style::default().fg(app.theme.muted),
                );
                SettingsAction::None
            }
        },
        _ => SettingsAction::None,
    }
}

/// 打开连接编辑表单；`editing` 为 None 表示新增。
fn open_conn_form(app: &mut App, editing: Option<usize>) {
    let form = match editing.and_then(|i| app.connections.get(i)) {
        Some(conn) => ConnForm {
            name: conn.name.clone(),
            url: conn.url.clone(),
            token: conn.token.clone(),
            field: 0,
            editing,
        },
        None => ConnForm {
            name:    String::new(),
            url:     String::new(),
            token:   String::new(),
            field:   0,
            editing: None,
        },
    };
    if let Some(state) = app.settings.as_mut() {
        state.form = Some(form);
    }
}

/// 连接表单按键：Tab 切换字段、Enter 校验保存、Esc 取消。
fn handle_conn_form(app: &mut App, key: KeyEvent) -> SettingsAction {
    let Some(mut form) = app.settings.as_ref().and_then(|state| state.form.clone()) else {
        return SettingsAction::None;
    };
    match key.code {
        KeyCode::Esc => {
            if let Some(state) = app.settings.as_mut() {
                state.form = None;
            }
            return SettingsAction::None;
        }
        KeyCode::Tab => form.field = (form.field + 1) % 3,
        KeyCode::BackTab => form.field = (form.field + 2) % 3,
        KeyCode::Backspace => {
            let target = match form.field {
                0 => &mut form.name,
                1 => &mut form.url,
                _ => &mut form.token,
            };
            target.pop();
        }
        KeyCode::Enter => {
            if commit_conn_form(app, &form) {
                if let Some(state) = app.settings.as_mut() {
                    state.form = None;
                }
                return SettingsAction::None;
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => match form.field {
            0 => form.name.push(c),
            1 => form.url.push(c),
            _ => form.token.push(c),
        },
        _ => {}
    }
    if let Some(state) = app.settings.as_mut() {
        state.form = Some(form);
    }
    SettingsAction::None
}

/// 校验并写入连接表单：名称/地址非空、名称不与他人重复。
fn commit_conn_form(app: &mut App, form: &ConnForm) -> bool {
    let name = form.name.trim();
    let url = form.url.trim();
    if name.is_empty() || url.is_empty() {
        app.push_notice("连接名称与地址都不能为空", Style::default().fg(app.theme.error));
        return false;
    }
    if app
        .connections
        .iter()
        .enumerate()
        .any(|(i, conn)| Some(i) != form.editing && conn.name == name)
    {
        app.push_notice(
            format!("已存在名为「{name}」的连接"),
            Style::default().fg(app.theme.error),
        );
        return false;
    }

    let conn = TuiConnection {
        name:  name.to_string(),
        url:   url.to_string(),
        token: form.token.trim().to_string(),
    };
    match form.editing {
        Some(i) if i < app.connections.len() => {
            let old_name = app.connections[i].name.clone();
            app.connections[i] = conn;
            if app.active_conn.as_deref() == Some(old_name.as_str()) {
                app.active_conn = Some(name.to_string());
            }
        }
        _ => app.connections.push(conn),
    }
    app.connections_dirty = true;
    true
}

/// 打开连接切换弹窗；无已保存连接时仅提示，不进入空列表。
fn open_picker(app: &mut App) {
    if app.connections.is_empty() {
        app.push_notice(
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
/// 列表现拉：别处（Web、另一个终端）新建的会话要能马上看到，连接时的快照只作为
/// 拉取失败时的退路。首项固定是「新建会话」——否则工作区里只有一个会话时，
/// 弹窗里除了「重连现在这个」无事可做。
async fn open_session_picker(app: &mut App, api: &SessionApi) {
    match api.list_sessions(Some(&app.workspace)).await {
        Ok(list) => app.sessions = list,
        Err(e) => app.push_notice(
            format!("刷新会话列表失败，沿用连接时的快照: {e}"),
            Style::default().fg(app.theme.warning),
        ),
    }
    let items = session_items(&app.sessions, &app.workspace, &app.session_id);
    // 高亮落在当前会话上：打开弹窗第一眼就知道「现在在哪」
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换会话 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 会话弹窗的条目：首项固定是「新建会话」，其余按列表顺序。
fn session_items(sessions: &[SessionRecord], workspace: &str, current: &str) -> Vec<PickerItem> {
    let mut items: Vec<PickerItem> = sessions
        .iter()
        .map(|s| PickerItem {
            // 刚建的会话还没有标题，留空会让弹窗里出现一条莫名其妙的空行
            label:   if s.title.trim().is_empty() {
                "未命名".to_string()
            } else {
                s.title.clone()
            },
            detail:  format!("{} · {}", short_id(&s.session_id), s.active_model),
            current: s.session_id == current,
            action:  PickerAction::SwitchSession {
                session_id: s.session_id.clone(),
            },
        })
        .collect();
    items.insert(
        0,
        PickerItem {
            label:   "＋ 新建会话".into(),
            detail:  format!("为 {workspace} 开一个空会话"),
            current: false,
            action:  PickerAction::NewSession,
        },
    );
    items
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
        app.push_notice(
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
        app.push_notice(
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

/// 打开分支切换弹窗。
///
/// 消息树按需现拉：握手只下发**当前分支**的线性历史，兄弟分支不在其中；连接时拉
/// 一次也会因为「聊过几轮就不准」而失去意义。拉不到就只提示，不打开空弹窗。
async fn open_branch_picker(app: &mut App, api: &SessionApi) {
    let tree = match api.message_tree(&app.session_id).await {
        Ok(tree) => tree,
        Err(e) => {
            app.push_notice(format!("读取分支失败: {e}"), Style::default().fg(app.theme.error));
            return;
        }
    };
    let tips = branch_tips(&tree);
    if tips.len() < 2 {
        app.push_notice(
            "当前会话只有一条分支（从历史消息分叉后才会出现第二条）",
            Style::default().fg(app.theme.muted),
        );
        return;
    }
    // 顺手把过时的叶子标记同步到真正的叶子：本地追加过轮次后握手给的 id 已不再是最末端
    let leaf = branch_leaf(&tree, app.current_leaf.as_deref());
    app.current_leaf = leaf.map(|message| message.id.clone());
    let current = app.current_leaf.clone();
    let items: Vec<PickerItem> = tips
        .iter()
        .map(|tip| PickerItem {
            label:   leaf_snippet(tip),
            detail:  format!("{} · {} 条", short_id(&tip.id), branch_len(&tree, tip)),
            current: current.as_deref() == Some(tip.id.as_str()),
            action:  PickerAction::SwitchBranch {
                leaf_message_id: tip.id.clone(),
            },
        })
        .collect();
    let index = picker_current_index(&items);
    app.picker = Some(Picker {
        title: " 切换分支 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index,
    });
}

/// 打开「删除消息」弹窗：列出当前分支上的用户消息（新的在前），选中即删其子树。
async fn open_delete_picker(app: &mut App, api: &SessionApi) {
    let tree = match api.message_tree(&app.session_id).await {
        Ok(tree) => tree,
        Err(e) => {
            app.push_notice(format!("读取历史失败: {e}"), Style::default().fg(app.theme.error));
            return;
        }
    };
    let leaf = branch_leaf(&tree, app.current_leaf.as_deref());
    app.current_leaf = leaf.map(|message| message.id.clone());
    let items: Vec<PickerItem> = user_messages_on_branch(&tree, leaf)
        .into_iter()
        .map(|message| PickerItem {
            label:   leaf_snippet(message),
            detail:  format!("{} · 删除该消息及其后续", short_id(&message.id)),
            current: false,
            action:  PickerAction::DeleteMessage {
                message_id: message.id.clone(),
            },
        })
        .collect();
    if items.is_empty() {
        app.push_notice("当前分支还没有可删除的用户消息", Style::default().fg(app.theme.muted));
        return;
    }
    app.picker = Some(Picker {
        title: " 删除消息（含子树）· ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index: 0,
    });
}

/// 打开「编辑重发」弹窗：列出当前分支上的用户消息，新的在前。
///
/// 与 Web 端一致：选中一条即把它的文本放回输入框，并把分叉点记成它的父节点，
/// 下次发送就从那里长出新分支。会话首条没有父节点，无处可分叉，降级为普通发送。
async fn open_fork_picker(app: &mut App, api: &SessionApi) {
    let tree = match api.message_tree(&app.session_id).await {
        Ok(tree) => tree,
        Err(e) => {
            app.push_notice(format!("读取历史失败: {e}"), Style::default().fg(app.theme.error));
            return;
        }
    };
    let leaf = branch_leaf(&tree, app.current_leaf.as_deref());
    // 握手给的叶子在本地追加过轮次之后已经过时；趁着刚拉到整棵树，把标记同步到真正的叶子
    app.current_leaf = leaf.map(|message| message.id.clone());
    let items = fork_items(&tree, leaf);
    if items.is_empty() {
        app.push_notice("当前分支还没有可重发的用户消息", Style::default().fg(app.theme.muted));
        return;
    }
    app.picker = Some(Picker {
        title: " 编辑重发 · ↑/↓ 选择 · Enter 确认 · Esc 取消 ".into(),
        items,
        index: 0,
    });
}

/// 编辑重发弹窗的条目：每条用户消息一项，新的在前。
fn fork_items(tree: &[ChatMessage], leaf: Option<&ChatMessage>) -> Vec<PickerItem> {
    user_messages_on_branch(tree, leaf)
        .into_iter()
        .map(|message| PickerItem {
            label:   leaf_snippet(message),
            detail:  match &message.parent_id {
                Some(parent) => format!("{} · 从 {} 处重新开始", short_id(&message.id), short_id(parent)),
                None => format!("{} · 首条消息，无处可分叉", short_id(&message.id)),
            },
            current: false,
            action:  PickerAction::ForkFrom {
                text:      message_text(message),
                parent_id: message.parent_id.clone(),
                label:     leaf_snippet(message),
            },
        })
        .collect()
}

/// 当前分支的代表叶子。
///
/// 服务端下发的叶子（握手、切换分支后）若仍是叶子就直接用；本地又追加过轮次之后
/// 它已长出子节点，此时最新的那条叶子就是当前分支的末端——分支切换都会重连取新叶子，
/// 不存在「别的分支比它还新」的情形。
fn branch_leaf<'a>(tree: &'a [ChatMessage], preferred: Option<&str>) -> Option<&'a ChatMessage> {
    let parents: HashSet<&str> = tree.iter().filter_map(|m| m.parent_id.as_deref()).collect();
    let is_leaf = |m: &ChatMessage| !parents.contains(m.id.as_str());
    if let Some(leaf) = preferred
        .and_then(|id| tree.iter().find(|m| m.id == id))
        .filter(|m| is_leaf(m))
    {
        return Some(leaf);
    }
    tree.iter()
        .filter(|m| is_leaf(m))
        .max_by_key(|m| m.created_at)
}

/// 当前分支上的用户消息：从叶子沿 `parent_id` 回溯到根，新的在前。
fn user_messages_on_branch<'a>(tree: &'a [ChatMessage], leaf: Option<&'a ChatMessage>) -> Vec<&'a ChatMessage> {
    let by_id: HashMap<&str, &ChatMessage> = tree.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut out = Vec::new();
    let mut cursor = leaf;
    while let Some(message) = cursor {
        if message.role == Role::User {
            out.push(message);
        }
        cursor = message
            .parent_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
    }
    out
}

/// 一条消息的完整正文：拼接全部 Text 块，供编辑重发时回填输入框。
fn message_text(message: &ChatMessage) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            oma_contract::Block::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 可切换的分支代表：每个叶子向上退到最近的**用户可见**消息，按时间倒序。
///
/// 叶子若停在工具回执上（一轮被中断、或回执后没有新正文），用户想回到的仍是那条
/// 可见消息；直接列出回执既看不懂、也不是个像样的位置。两个叶子退到同一条消息时
/// 只留一个。
fn branch_tips(tree: &[ChatMessage]) -> Vec<&ChatMessage> {
    let by_id: HashMap<&str, &ChatMessage> = tree.iter().map(|m| (m.id.as_str(), m)).collect();
    let parents: HashSet<&str> = tree.iter().filter_map(|m| m.parent_id.as_deref()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut tips: Vec<&ChatMessage> = Vec::new();
    for leaf in tree.iter().filter(|m| !parents.contains(m.id.as_str())) {
        let mut cursor = Some(leaf);
        while let Some(message) = cursor {
            if is_user_visible(message) {
                if seen.insert(message.id.as_str()) {
                    tips.push(message);
                }
                break;
            }
            cursor = message
                .parent_id
                .as_deref()
                .and_then(|id| by_id.get(id).copied());
        }
    }
    tips.sort_by_key(|m| std::cmp::Reverse(m.created_at));
    tips
}

/// 用户看得见的消息：有内容，且不是纯工具回执——回执会并进工具调用卡片，
/// 在界面上不单独成条。
fn is_user_visible(message: &ChatMessage) -> bool {
    !message.content.is_empty()
        && !message
            .content
            .iter()
            .any(|block| matches!(block, oma_contract::Block::ToolResult { .. }))
}

/// 从某条消息回溯到根的消息条数：让用户看出这条分支有多长。
fn branch_len(tree: &[ChatMessage], tip: &ChatMessage) -> usize {
    let by_id: HashMap<&str, &ChatMessage> = tree.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut count = 0;
    let mut cursor = Some(tip);
    while let Some(message) = cursor {
        count += 1;
        cursor = message
            .parent_id
            .as_deref()
            .and_then(|id| by_id.get(id).copied());
    }
    count
}

/// 打开全屏历史树（`/tree`）：现拉整棵树，平铺成可上下选择、可切换分支的树形列表。
async fn open_tree(app: &mut App, api: &SessionApi) {
    let tree = match api.message_tree(&app.session_id).await {
        Ok(tree) => tree,
        Err(e) => {
            app.push_notice(format!("读取历史失败: {e}"), Style::default().fg(app.theme.error));
            return;
        }
    };
    if tree.is_empty() {
        app.push_notice("当前会话还没有消息", Style::default().fg(app.theme.muted));
        return;
    }
    // 本地追加过轮次后握手给到的叶子已过时，趁刚拉到整棵树同步一次
    let leaf = branch_leaf(&tree, app.current_leaf.as_deref());
    app.current_leaf = leaf.map(|message| message.id.clone());
    let items = build_tree_items(&tree, app.current_leaf.as_deref());
    let index = items
        .iter()
        .position(|item| Some(item.id.as_str()) == app.current_leaf.as_deref())
        .unwrap_or(0);
    app.tree_view = Some(TreeSelector { items, index });
}

/// 把消息树平铺成带连接线前缀的列表（按创建时间的先序 DFS，根在前）。
fn build_tree_items(tree: &[ChatMessage], current_leaf: Option<&str>) -> Vec<TreeItem> {
    let ids: HashSet<&str> = tree.iter().map(|message| message.id.as_str()).collect();
    let mut children: HashMap<Option<&str>, Vec<&ChatMessage>> = HashMap::new();
    for message in tree {
        let parent = message
            .parent_id
            .as_deref()
            .filter(|parent| ids.contains(parent));
        children.entry(parent).or_default().push(message);
    }
    for kids in children.values_mut() {
        kids.sort_by_key(|message| message.created_at);
    }

    // 当前激活分支：从叶子沿 parent_id 回溯到根
    let by_id: HashMap<&str, &ChatMessage> = tree
        .iter()
        .map(|message| (message.id.as_str(), message))
        .collect();
    let mut on_current: HashSet<String> = HashSet::new();
    let mut cursor = current_leaf.and_then(|id| by_id.get(id).copied());
    while let Some(message) = cursor {
        on_current.insert(message.id.clone());
        cursor = message
            .parent_id
            .as_deref()
            .and_then(|parent| by_id.get(parent).copied());
    }

    let mut items = Vec::new();
    let roots = children.get(&None).cloned().unwrap_or_default();
    let root_count = roots.len();
    for (i, root) in roots.iter().enumerate() {
        walk_tree(root, "", i + 1 == root_count, &children, &on_current, &mut items);
    }
    items
}

/// 先序 DFS 一步：铺好连接线前缀后递归子节点。
fn walk_tree(
    message: &ChatMessage,
    prefix: &str,
    is_last: bool,
    children: &HashMap<Option<&str>, Vec<&ChatMessage>>,
    on_current: &HashSet<String>,
    out: &mut Vec<TreeItem>,
) {
    // 顶层不画连接线（根本就多个/单个都不需要 ├─）；只对子层标 ├─ / └─
    let connector = if prefix.is_empty() {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };
    let kids = children
        .get(&Some(message.id.as_str()))
        .cloned()
        .unwrap_or_default();
    out.push(TreeItem {
        id:         message.id.clone(),
        prefix:     format!("{prefix}{connector}"),
        label:      tree_label(message),
        on_current: on_current.contains(&message.id),
        is_leaf:    kids.is_empty(),
    });

    let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });
    let kid_count = kids.len();
    for (i, kid) in kids.iter().enumerate() {
        walk_tree(kid, &child_prefix, i + 1 == kid_count, children, on_current, out);
    }
}

/// 树里一行的摘要：角色标记 + 单行内容（工具调用回落到工具名）。
fn tree_label(message: &ChatMessage) -> String {
    let marker = match message.role {
        Role::User => "›",
        Role::Assistant => "◆",
        Role::System => "·",
    };
    format!("{marker} {}", leaf_snippet(message))
}

/// 历史树按键的结果。
enum TreeAction {
    None,
    Close,
    Select(String),
}

/// 历史树按键：上下移动、Enter 选中、Esc 关闭。
fn handle_tree_key(app: &mut App, key: KeyEvent) -> TreeAction {
    let Some(tree) = app.tree_view.as_ref() else {
        return TreeAction::None;
    };
    let index = tree.index;
    let last = tree.items.len().saturating_sub(1);
    match key.code {
        KeyCode::Esc => TreeAction::Close,
        KeyCode::Up | KeyCode::Char('k') => {
            if let Some(tree) = app.tree_view.as_mut() {
                tree.index = index.saturating_sub(1);
            }
            TreeAction::None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let Some(tree) = app.tree_view.as_mut() {
                tree.index = (index + 1).min(last);
            }
            TreeAction::None
        }
        KeyCode::Enter => match app
            .tree_view
            .as_ref()
            .and_then(|tree| tree.items.get(index))
        {
            Some(item) => TreeAction::Select(item.id.clone()),
            None => TreeAction::None,
        },
        _ => TreeAction::None,
    }
}

/// 分支代表的单行摘要：优先正文，其次工具调用，最后回落到角色名。
fn leaf_snippet(message: &ChatMessage) -> String {
    let text = message.content.iter().find_map(|block| match block {
        oma_contract::Block::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
        _ => None,
    });
    let raw = match text {
        // 多行与连续空白压成一行，弹窗里每项只占一行
        Some(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
        None => message
            .content
            .iter()
            .find_map(|block| match block {
                oma_contract::Block::ToolUse { name, .. } => Some(format!("⚙ {name}")),
                _ => None,
            })
            .unwrap_or_else(|| message.role.as_str().to_string()),
    };
    truncate(&raw, 24)
}

/// 打开重命名弹窗：预填当前标题，Enter 确认、Esc 取消。
///
/// 标题从会话快照里取；快照里没有（列表拉取失败过）就留空，不影响改名本身。
fn open_rename_prompt(app: &mut App) {
    let current = app
        .sessions
        .iter()
        .find(|s| s.session_id == app.session_id)
        .map(|s| s.title.clone())
        .unwrap_or_default();
    app.prompt = Some(TextPrompt {
        title: " 重命名会话 · Enter 确认 · Esc 取消 ".into(),
        input: current,
        kind:  PromptKind::RenameSession,
    });
}

/// 文本弹窗按键：Backspace 编辑、Enter 取出请求并关闭、Esc 取消。
///
/// 返回 `Some((kind, input))` 表示用户确认；`None` 表示按键已被消费或弹窗未开。
fn handle_prompt_key(app: &mut App, key: KeyEvent) -> Option<(PromptKind, String)> {
    // 弹窗没开时按键不该走到这里
    app.prompt.as_ref()?;
    match key.code {
        KeyCode::Esc => app.prompt = None,
        KeyCode::Backspace => {
            if let Some(prompt) = app.prompt.as_mut() {
                prompt.input.pop();
            }
        }
        KeyCode::Enter => {
            let prompt = app.prompt.take()?;
            return Some((prompt.kind, prompt.input));
        }
        // 与输入区一致：未定义的 Ctrl+字母不落进文本
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(prompt) = app.prompt.as_mut() {
                prompt.input.push(c);
            }
        }
        _ => {}
    }
    None
}

/// 执行文本弹窗确认后的动作。
async fn apply_prompt(
    app: &mut App,
    api: &SessionApi,
    request: Option<(PromptKind, String)>,
) -> Result<Option<Outcome>> {
    let Some((kind, input)) = request else {
        return Ok(None);
    };
    match kind {
        PromptKind::RenameSession => {
            let title = input.trim().to_string();
            if title.is_empty() {
                app.push_notice("会话标题不能为空", Style::default().fg(app.theme.error));
                return Ok(None);
            }
            match api.rename_session(&app.session_id, &title).await {
                // 本地快照跟着改，列表弹窗在服务端事件到之前也是对的
                Ok(()) => {
                    if let Some(record) = app
                        .sessions
                        .iter_mut()
                        .find(|s| s.session_id == app.session_id)
                    {
                        record.title = title;
                    }
                }
                Err(e) => app.push_notice(format!("重命名失败: {e}"), Style::default().fg(app.theme.error)),
            }
            Ok(None)
        }
    }
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
/// 分两类：换连接、换会话、换分支都要重建会话循环（返回 `Some(Outcome)`），
/// 其中换分支是「先发指令、再重连」——服务端改了当前叶子，界面上的记录必须重读；
/// 换模型、换预设、换推理等级只是一条指令，本连接继续用，界面等
/// `ModelChanged` / `AgentChanged` / `ReasoningLevelChanged` 事件回填。
/// 发指令失败时把原因写进记录——此时 App 不会被重建，提示是留得住的。
async fn apply_picker_action(
    app: &mut App,
    client: &OmaClient,
    api: &SessionApi,
    action: Option<PickerAction>,
) -> Result<Option<Outcome>> {
    let Some(action) = action else {
        return Ok(None);
    };
    match action {
        PickerAction::SwitchConnection { addr, token, name } => Ok(Some(Outcome::Switch {
            addr,
            token,
            name,
            connections: app.dirty_connections(),
        })),
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
        PickerAction::SwitchBranch { leaf_message_id } => {
            // 指令只改服务端的当前叶子，界面上的记录仍是旧分支的：必须重连一次，
            // 让服务端把新分支的线性历史重新下发
            let session_id = app.session_id.clone();
            send_setting(app, client, AgentCommand::SwitchBranch { leaf_message_id }, "切换分支").await;
            Ok(Some(Outcome::Reload { session_id }))
        }
        PickerAction::ForkFrom { text, parent_id, label } => {
            // 只把文本放回输入框并记下分叉点：真正的分叉发生在下次发送（见 handle_key 的 Enter）
            app.input = text;
            app.fork_from = parent_id;
            app.fork_label = label;
            let hint = match app.fork_from {
                Some(_) => format!("编辑重发：Enter 从「{}」处重新开始 · Esc 取消", app.fork_label),
                None => "这是会话首条消息，无处可分叉，将作为普通消息发送".into(),
            };
            app.push_notice(hint, Style::default().fg(app.theme.muted));
            Ok(None)
        }
        PickerAction::DeleteMessage { message_id } => {
            // 服务端会连带删掉整棵子树并回退当前叶子；删完重连重读历史
            match api.delete_message(&app.session_id, &message_id).await {
                Ok((deleted, _leaf)) => {
                    app.push_notice(
                        format!("已删除 {} 条消息（含子树）", deleted.len()),
                        Style::default().fg(app.theme.muted),
                    );
                    Ok(Some(Outcome::Reload {
                        session_id: app.session_id.clone(),
                    }))
                }
                Err(e) => {
                    app.push_notice(format!("删除消息失败: {e}"), Style::default().fg(app.theme.error));
                    Ok(None)
                }
            }
        }
        PickerAction::NewSession => {
            let workspace = app.workspace.clone();
            match api.create_session(&workspace, None).await {
                // 新会话没有任何消息，切过去即是空白界面
                Ok(record) => Ok(Some(Outcome::SwitchSession {
                    session_id: record.session_id,
                })),
                Err(e) => {
                    app.push_notice(format!("新建会话失败: {e}"), Style::default().fg(app.theme.error));
                    Ok(None)
                }
            }
        }
    }
}

/// 下发一条设置类指令；失败时把原因写进记录。
///
/// 这类动作不重建会话循环，所以写下的提示留得住（换连接/换会话那两条则留不住，
/// 见 [`PickerAction`]）。成功时什么都不写：界面等 `ModelChanged` 之类的回执。
async fn send_setting(app: &mut App, client: &OmaClient, command: AgentCommand, what: &str) {
    if let Err(e) = client.send_command(command).await {
        app.push_notice(format!("{what}失败: {e}"), Style::default().fg(app.theme.error));
    }
}

fn apply_event(app: &mut App, event: AgentEvent) {
    match event {
        AgentEvent::TurnStarted { .. } => {
            app.busy = true;
            app.stick = true;
            app.turn_started_at = Some(Instant::now());
        }
        AgentEvent::TurnFinished { stop_reason, usage, .. } => {
            app.busy = false;
            let style = if matches!(stop_reason, StopReason::Error) {
                Style::default().fg(app.theme.error)
            } else {
                Style::default().fg(app.theme.muted)
            };
            // 整轮耗时由服务端回执时刻减去 TurnStarted 的墙钟（中途接入时无起点则省略）
            let elapsed = app
                .turn_started_at
                .take()
                .map(|start| format!("耗时 {} · ", format_duration(start.elapsed().as_millis() as u64)))
                .unwrap_or_default();
            app.push_notice(
                format!(
                    "轮次{} · {}↑{} ↓{} · 缓存读 {} · 缓存写 {}",
                    stop_reason_label(stop_reason),
                    elapsed,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_tokens,
                    usage.cache_write_tokens,
                ),
                style,
            );
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
                app.push_notice(
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
            app.append_thinking(&delta, Style::default().fg(app.theme.thinking));
        }
        // 一段思考结束：收尾这一段并记下它的墙钟耗时
        AgentEvent::ThinkingFinished { duration_ms } => app.finish_thinking(Some(duration_ms)),
        AgentEvent::TextDelta { delta } => {
            app.append_assistant(&delta, Style::default().fg(app.theme.success));
        }
        AgentEvent::ToolCallStarted(data) => app.tool_started(data),
        AgentEvent::ToolCallFinished {
            call_id,
            tool_name,
            output,
            is_error,
            duration_ms,
        } => app.tool_finished(&call_id, &tool_name, output, is_error, duration_ms),
        AgentEvent::ModelChanged { active_model } => {
            app.model = active_model;
            app.system_prompt_dirty = true;
        }
        AgentEvent::AgentChanged { active_agent } => {
            app.agent = active_agent;
            app.system_prompt_dirty = true;
        }
        AgentEvent::ActiveTurnCatchUp(snapshot) => {
            if !snapshot.accumulated_thinking.is_empty() {
                let mut entry = Entry::text(
                    EntryKind::Thinking,
                    snapshot.accumulated_thinking.clone(),
                    Style::default().fg(app.theme.thinking),
                );
                entry.duration_ms = snapshot.thinking_duration_ms;
                // 没有工具在跑且还没有正文，说明思考仍在继续：保持展开
                entry.open = snapshot.active_tool_call.is_none() && snapshot.accumulated_text.is_empty();
                app.push_entry(entry);
            }
            if !snapshot.accumulated_text.is_empty() {
                let mut entry = Entry::text(
                    EntryKind::Assistant,
                    snapshot.accumulated_text,
                    Style::default().fg(app.theme.success),
                );
                entry.open = true;
                app.push_entry(entry);
            }
            if let Some(call) = snapshot.active_tool_call {
                // 中途接入的调用仍在进行：建成 open 的进行中工具块
                let mut entry = Entry::text(EntryKind::Tool, String::new(), Style::default());
                entry.open = true;
                entry.tool = Some(ToolState {
                    call_id:     call.call_id,
                    name:        call.tool_name,
                    input:       summarize_tool_input(&call.input),
                    output:      String::new(),
                    is_error:    false,
                    done:        false,
                    duration_ms: None,
                });
                app.push_entry(entry);
            }
            app.busy = true;
        }
        AgentEvent::SyncRequired {} => {
            app.push_notice("事件流出现缺口，状态可能不完整", Style::default().fg(app.theme.warning))
        }
        AgentEvent::Error { message } => app.push_notice(message, Style::default().fg(app.theme.error)),
        AgentEvent::SessionRenamed { title, .. } => app.push_notice(
            format!("会话已重命名为 {}", title),
            Style::default().fg(app.theme.muted),
        ),
        AgentEvent::MessagesDeleted { deleted_ids, .. } => app.push_notice(
            format!("已删除 {} 条消息", deleted_ids.len()),
            Style::default().fg(app.theme.muted),
        ),
        AgentEvent::ActiveBranchChanged { current_leaf_id } => {
            app.current_leaf = Some(current_leaf_id.clone());
            app.push_notice(
                format!("切换到分支 {}", short_id(&current_leaf_id)),
                Style::default().fg(app.theme.muted),
            );
        }
        AgentEvent::ContextUsage { tokens, context_len } => {
            app.context = Some((tokens, context_len));
        }
        // 本轮累计用量的增量更新：TUI 只在整轮结束时展示一次，不重复刷屏
        AgentEvent::UsageUpdated { .. } => {}
        // 工作区登记集合变化：TUI 的会话列表按工作区现拉，不维护侧栏分组
        AgentEvent::WorkspacesChanged { .. } => {}
        AgentEvent::ReasoningLevelChanged { level } => {
            app.reasoning_level = level.clone();
            app.push_notice(
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
    let [body, band, input] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(INPUT_HEIGHT),
    ])
    .areas(area);

    // 配色按值拷入各绘制函数：TuiTheme 全为 Copy 字段，且 Draw 内 app 已不可变借用
    let theme = app.theme;
    draw_transcript(frame, app, body, theme);
    draw_status_band(frame, app, band, theme);
    draw_input(frame, app, input, theme);

    if let Some(picker) = &app.picker {
        draw_picker(frame, picker, area, theme);
    }
    if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt, area, theme);
    }
    if let Some(tree) = &app.tree_view {
        draw_tree(frame, tree, area, theme);
    }
    if let Some(settings) = &app.settings {
        draw_settings(frame, app, settings, area, theme);
    }
}

/// 设置界面：连接列表 + 模型/预设/推理等级/主题；连接可增删改与切换。
fn draw_settings(frame: &mut Frame, app: &App, settings: &SettingsState, area: Rect, theme: TuiTheme) {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " 设置 · ↑/↓ 选择 · Enter 打开/切换 · A 新增连接 E 编辑 D 删除 · Esc 关闭 ",
            Style::default().fg(theme.muted),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = settings_rows(app);
    let visible = inner.height as usize;
    let window = picker_window(rows.len(), settings.index, visible.max(1));
    let start = window.start;
    let lines: Vec<Line<'static>> = rows[window.clone()]
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            let selected = start + offset == settings.index;
            let text = settings_row_text(app, row);
            let style = if selected {
                Style::default().fg(theme.base).bg(theme.accent)
            } else if settings_row_is_active(app, row) {
                Style::default().fg(theme.accent)
            } else {
                Style::default().fg(theme.subtext)
            };
            let text = if selected {
                pad_to_width(&text, inner.width as usize)
            } else {
                text
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);

    if let Some(form) = &settings.form {
        draw_conn_form(frame, form, area, theme);
    }
}

/// 设置一行的文本：连接显示「● 名称  地址」，其余显示「标签  当前值」。
fn settings_row_text(app: &App, row: &SettingsRow) -> String {
    match row {
        SettingsRow::Connection(i) => match app.connections.get(*i) {
            Some(conn) => {
                let marker = if app.active_conn.as_deref() == Some(conn.name.as_str()) {
                    "●"
                } else {
                    "○"
                };
                format!("{marker} {}   {}", conn.name, conn.url)
            }
            None => String::new(),
        },
        SettingsRow::Model => format!("模型        {}", app.model),
        SettingsRow::Agent => format!("预设        {}", app.agent),
        SettingsRow::Reasoning => {
            let level = if app.reasoning_level.is_empty() {
                "模型默认"
            } else {
                app.reasoning_level.as_str()
            };
            format!("推理等级    {level}")
        }
        SettingsRow::Theme => format!("主题        {} · {}", app.theme_mode, app.theme_accent),
    }
}

/// 该行是否代表当前生效的连接（用于强调色）。
fn settings_row_is_active(app: &App, row: &SettingsRow) -> bool {
    match row {
        SettingsRow::Connection(i) => app
            .connections
            .get(*i)
            .is_some_and(|conn| app.active_conn.as_deref() == Some(conn.name.as_str())),
        _ => false,
    }
}

/// 连接编辑表单弹窗：三段字段，当前字段高亮。
fn draw_conn_form(frame: &mut Frame, form: &ConnForm, area: Rect, theme: TuiTheme) {
    let width = area.width.saturating_sub(8).min(64);
    let height = 6;
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let title = if form.editing.is_some() {
        " 编辑连接 "
    } else {
        " 新增连接 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            format!("{title}· Tab 切换 · Enter 保存 · Esc 取消 "),
            Style::default().fg(theme.muted),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let fields = [("名称", &form.name), ("地址", &form.url), ("凭证", &form.token)];
    let lines: Vec<Line<'static>> = fields
        .iter()
        .enumerate()
        .map(|(i, (label, value))| {
            let mut style = Style::default().fg(theme.subtext);
            if i == form.field {
                style = style.add_modifier(Modifier::BOLD).fg(theme.accent);
            }
            Line::from(Span::styled(format!("{label}  {value}"), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// 全屏历史树：整棵树带连接线平铺，选中态用主题强调色实底 + base 文字。
fn draw_tree(frame: &mut Frame, tree: &TreeSelector, area: Rect, theme: TuiTheme) {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Span::styled(
            " 历史树 · ↑/↓ 选择 · Enter 切换分支 · Esc 关闭 ",
            Style::default().fg(theme.muted),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if tree.items.is_empty() {
        return;
    }

    let visible = inner.height as usize;
    let window = picker_window(tree.items.len(), tree.index, visible.max(1));
    let start = window.start;
    let lines: Vec<Line<'static>> = tree.items[window]
        .iter()
        .enumerate()
        .map(|(offset, item)| {
            let selected = start + offset == tree.index;
            // 末端标记并进正文：选中态要把整行铺满底色，标记若落在补白之后会被裁掉
            let mut text = format!("{}{}", item.prefix, item.label);
            if item.is_leaf {
                text.push_str("  · 末端");
            }
            // 选中：强调色实底 + base 文字（与 Web 选中态同一口径）；当前分支：强调色文字
            let (text, style) = if selected {
                (
                    pad_to_width(&text, inner.width as usize),
                    Style::default().fg(theme.base).bg(theme.accent),
                )
            } else if item.on_current {
                (text, Style::default().fg(theme.accent))
            } else {
                (text, Style::default().fg(theme.subtext))
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// 文本输入弹窗：标题 + 一行输入（光标落在文本末尾）。
fn draw_prompt(frame: &mut Frame, prompt: &TextPrompt, area: Rect, theme: TuiTheme) {
    let width = area.width.saturating_sub(8).min(64);
    let height = 3;
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
        .title(Span::styled(prompt.title.clone(), Style::default().fg(theme.muted)));
    let inner = block.inner(popup);
    frame.render_widget(
        Paragraph::new(Span::styled(prompt.input.clone(), Style::default().fg(theme.text))).block(block),
        popup,
    );
    let cursor_x = inner.x + display_width(&prompt.input).min(inner.width.saturating_sub(1) as usize) as u16;
    frame.set_cursor_position((cursor_x, inner.y));
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

/// 输入框上方的一行状态带：活动动画 + 模型名 + 工作路径（限宽左截断）+ 上下文占比。
fn draw_status_band(frame: &mut Frame, app: &App, area: Rect, _theme: TuiTheme) {
    frame.render_widget(Paragraph::new(status_band_line(app, area.width)), area);
}

/// 组装状态带内容；抽成纯函数以便断言顺序与截断行为。
fn status_band_line(app: &App, width: u16) -> Line<'static> {
    let theme = app.theme;
    if !app.connected {
        return Line::from(Span::styled(
            "○ 连接已断开，正在重连…",
            Style::default().fg(theme.error),
        ));
    }

    let (glyph, glyph_style) = if app.busy {
        (
            spinner_frame(app.started_at.elapsed()),
            Style::default().fg(theme.accent),
        )
    } else {
        (SPINNER_DONE, Style::default().fg(theme.success))
    };
    let ctx = app
        .context
        .map(|(tokens, window)| context_gauge(tokens, window))
        .unwrap_or_default();

    // 路径按剩余宽度左截断：模型名/动画/上下文占比先占位，其余给路径
    let reserved = display_width(glyph) + 1 + display_width(&app.model) + 3 + 3 + display_width(&ctx);
    let path_max = (width as usize)
        .saturating_sub(reserved)
        .clamp(8, STATUS_PATH_MAX);
    let path = truncate_path(&app.workspace, path_max);

    let mut spans = vec![
        Span::styled(glyph.to_string(), glyph_style),
        Span::styled(
            format!(" {} · {} · {}", app.model, path, ctx),
            Style::default().fg(theme.muted),
        ),
    ];
    if app.queue > 0 {
        spans.push(Span::styled(
            format!(" · 队列 {}", app.queue),
            Style::default().fg(theme.warning),
        ));
    }
    Line::from(spans)
}

/// 活动动画当前帧：按 80ms 步进在 8 点盲文帧之间轮转。
fn spinner_frame(elapsed: Duration) -> &'static str {
    SPINNER_FRAMES[(elapsed.as_millis() / SPINNER_ADVANCE_MS) as usize % SPINNER_FRAMES.len()]
}

/// 左截断路径：保留尾部（真正的项目名），超出时前置一个省略号。
fn truncate_path(path: &str, max: usize) -> String {
    if display_width(path) <= max {
        return path.to_string();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 2; // 省略号本身占两个显示列
    for ch in path.chars().rev() {
        let w = if ch.is_ascii() { 1 } else { 2 };
        if used + w > max {
            break;
        }
        used += w;
        tail.push(ch);
    }
    let mut out = String::from("…");
    out.extend(tail.iter().rev());
    out
}

fn draw_transcript(frame: &mut Frame, app: &App, area: Rect, theme: TuiTheme) {
    let width = area.width.saturating_sub(2) as usize;
    // 系统提示词固定在记录之上（它是「这一轮模型收到什么」的上下文）
    let mut lines = app.system_prompt_lines(width);
    lines.extend(app.wrapped_lines(width));
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
    let hint = if !app.connected {
        " 连接已断开 "
    } else if app.fork_from.is_some() {
        " 编辑重发：Enter 从历史处重新发送 · Esc 取消分叉 "
    } else {
        " Enter 发送（@路径 附件）· Ctrl+V 图片 · Ctrl+P 提示词 · Ctrl+X 中止 · Ctrl+G/Y 折叠思考/工具 · \
         Ctrl+O 连接 N 会话 B 分支 · Ctrl+L 模型 A 预设 R 推理 E 重发 T 改名 · Ctrl+U 清空 · Esc 退出 "
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
    // 测试里 `Block` 一律指消息块：ratatui 的同名控件在这些用例里用不到
    use oma_contract::{Block as MsgBlock, Role, TokenUsage};

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

    /// 退出键的判定：Esc 与 Ctrl-C；普通字符 c 不算。
    #[test]
    fn test_is_quit_key() {
        assert!(is_quit_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(is_quit_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)));
        assert!(!is_quit_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE)));
        assert!(!is_quit_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(!is_quit_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn test_append_stream_groups_same_kind() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.append_assistant("hello ", Style::default().fg(Color::Green));
        app.append_assistant("world", Style::default().fg(Color::Green));
        app.append_thinking("thinking", Style::default().fg(Color::Magenta));
        assert_eq!(app.entries.len(), 2);
        assert_eq!(app.entries[0].kind, EntryKind::Assistant);
        assert_eq!(app.entries[0].text, "hello world");
        assert_eq!(app.entries[1].kind, EntryKind::Thinking);
    }

    /// 折叠只作用于已结束的块：进行中的思考/工具即使全局折叠也保持展开。
    #[test]
    fn test_fold_hides_only_finished_blocks() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.thinking_collapsed = true;
        app.tool_collapsed = true;

        // 进行中的思考：折叠也不隐藏正文
        app.append_thinking("正在想", Style::default());
        assert!(plain_lines(&app).contains("正在想"), "{}", plain_lines(&app));

        // 收到结束事件后即可折叠成一行「▸ 思 …」
        app.finish_thinking(None);
        let folded = plain_lines(&app);
        assert!(folded.contains("▸ 思"), "{folded}");
        assert!(!folded.contains("正在想"), "{folded}");

        // 进行中的工具调用输出照常展示
        app.tool_started(tool_call("c1", "shell"));
        app.tool_finished("c1", "shell", "输出内容".into(), false, 30);
        let folded = plain_lines(&app);
        assert!(folded.contains("shell"), "{folded}");
        assert!(!folded.contains("输出内容"), "已结束的调用在折叠时应隐藏输出：{folded}");
        // 展开态：输出回来
        app.tool_collapsed = false;
        assert!(plain_lines(&app).contains("输出内容"));
    }

    /// 工具结束事件按 call_id 找回开始块；没有开始事件（中途接入）时补一条完整的。
    #[test]
    fn test_tool_finished_matches_call_id_or_appends() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.tool_started(tool_call("c1", "read"));
        app.tool_finished("c2", "shell", "别的输出".into(), false, 10);
        // c2 没有开始事件：另补一块，c1 仍在进行
        assert_eq!(app.entries.len(), 2);
        assert!(app.entries[0].tool.as_ref().is_some_and(|t| !t.done));

        app.tool_finished("c1", "read", "读到的内容".into(), false, 12);
        assert_eq!(app.entries.len(), 2, "c1 应原地补齐而不是新增");
        let tool = app.entries[0].tool.as_ref().unwrap();
        assert!(tool.done);
        assert_eq!(tool.duration_ms, Some(12));
        assert_eq!(tool.output, "读到的内容");
    }

    /// 记录块渲染成纯文本（前缀 + 正文），供折叠断言使用。
    fn plain_lines(app: &App) -> String {
        app.wrapped_lines(80)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn tool_call(call_id: &str, name: &str) -> ToolCallStartedData {
        ToolCallStartedData {
            call_id:   call_id.into(),
            tool_name: name.into(),
            input:     serde_json::json!({ "command": "echo hi" }),
        }
    }

    /// 用户块：整行铺满更深的背景色，且不再有「你 / AI」角色标签。
    #[test]
    fn test_user_block_has_background_and_no_role_label() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.push_user("hello", Style::default());
        app.append_assistant("world", Style::default());

        let lines = app.wrapped_lines(20);
        let first = &lines[0];
        assert_eq!(first.spans[0].style.bg, Some(Color::Reset), "用户块应带背景色");
        assert_eq!(first.spans[0].content.as_ref(), format!("hello{}", " ".repeat(15)));

        let text = plain_lines(&app);
        assert!(!text.contains("你 "), "用户块不该再有角色标签：{text}");
        assert!(!text.contains("AI "), "助手块不该再有角色标签：{text}");
        assert!(text.contains("world"), "{text}");
    }

    /// 耗时文案的单位与边界：不足 1 秒按毫秒，1 秒以上一位小数，超过 1 分钟按分秒。
    #[test]
    fn test_format_duration_units() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(320), "320ms");
        assert_eq!(format_duration(1500), "1.5s");
        assert_eq!(format_duration(65_000), "1m05s");
    }

    /// 内容文本降一档亮度：模型回答/思考/提示用 DIM，用户与报错保持醒目。
    #[test]
    fn test_content_dimmed_but_user_and_error_stay_bright() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.push_user("hi", Style::default());
        app.append_assistant("answer", Style::default());
        app.append_thinking("think", Style::default());
        app.finish_thinking(None);
        app.push_notice("info", Style::default());
        app.push_notice("boom", Style::default().fg(app.theme.error));

        let dimmed = |needle: &str| style_of(&app, needle).add_modifier.contains(Modifier::DIM);
        assert!(dimmed("answer"), "模型回答应降亮度");
        assert!(dimmed("think"), "思考应降亮度");
        assert!(dimmed("info"), "提示信息应降亮度");
        assert!(!dimmed("hi"), "用户消息保持醒目");
        assert!(!dimmed("boom"), "报错保持醒目");
    }

    /// 找到包含指定文本的渲染片段的样式。
    fn style_of(app: &App, needle: &str) -> Style {
        for line in app.wrapped_lines(80) {
            for span in &line.spans {
                if span.content.contains(needle) {
                    return span.style;
                }
            }
        }
        panic!("渲染结果里找不到 {needle:?}");
    }

    /// 系统提示词折叠块：默认收起为一行，展开后可见正文。
    #[test]
    fn test_system_prompt_block_collapse() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        assert!(app.system_prompt_lines(80).is_empty(), "未取到时不占行");

        app.system_prompt = Some("第一行\n第二行".into());
        let folded = line_text(&app.system_prompt_lines(80));
        assert!(folded.contains("▶ 系统提示词"), "{folded}");
        assert!(folded.contains("2 行"), "{folded}");
        assert!(!folded.contains("第一行"), "{folded}");

        app.system_prompt_collapsed = false;
        let open = line_text(&app.system_prompt_lines(80));
        assert!(open.contains("第一行") && open.contains("第二行"), "{open}");
    }

    /// 把渲染行拼成纯文本。
    fn line_text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 状态带顺序：动画 + 模型名 + 工作路径 + 上下文占比；空闲显示实心盲文。
    #[test]
    fn test_status_band_order_and_spinner_state() {
        let mut app = App::new(
            "/home/me/projects/oma".into(),
            "p/m".into(),
            "build".into(),
            TuiTheme::test(),
        );

        let text = band_text(&app);
        assert!(text.starts_with(SPINNER_DONE), "空闲应显示实心盲文：{text}");
        let model = text.find("p/m").unwrap();
        let path = text.find("oma").unwrap();
        assert!(model < path, "模型名应在路径之前：{text}");

        // 工作中换成旋转帧之一
        app.busy = true;
        let busy = band_text(&app);
        assert!(SPINNER_FRAMES.iter().any(|f| busy.starts_with(f)), "{busy}");

        // 上下文占比排在路径之后
        app.context = Some((500, 1000));
        let with_ctx = band_text(&app);
        assert!(with_ctx.contains("50%"), "{with_ctx}");
        assert!(
            with_ctx.find("oma").unwrap() < with_ctx.find("50%").unwrap(),
            "{with_ctx}"
        );
    }

    /// 路径超出限宽时左截断，保留尾部（项目名）。
    #[test]
    fn test_path_truncation_keeps_tail() {
        assert_eq!(truncate_path("/a/b/oma", 40), "/a/b/oma");
        let short = truncate_path("/home/me/projects/oma", 8);
        assert!(short.starts_with('…'), "{short}");
        assert!(short.ends_with("oma"), "{short}");
        assert!(display_width(&short) <= 8, "{short}");
    }

    /// 状态带渲染成纯文本。
    fn band_text(app: &App) -> String {
        status_band_line(app, 80)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// 斜杠命令解析：命令与参数分离，非斜杠输入不当作命令。
    #[test]
    fn test_parse_slash_commands() {
        assert_eq!(parse_slash("/model"), Some(("model", "")));
        assert_eq!(parse_slash("/rename 新名"), Some(("rename", "新名")));
        assert_eq!(parse_slash("  /help "), Some(("help", "")));
        // 以 / 开头一律走命令通道：未知命令由分发表报错（不会静默发出去）
        assert_eq!(parse_slash("/tmp/a.txt"), Some(("tmp/a.txt", "")));
        assert_eq!(parse_slash("/"), None);
        assert_eq!(parse_slash("hello"), None);
    }

    /// 未知命令给出提示且不离开会话循环。
    #[tokio::test]
    async fn test_unknown_slash_command_reports_error() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        // help/未知命令不会发网络请求，地址随意
        let api = SessionApi::new("127.0.0.1:1", "t");
        assert!(
            handle_slash(&mut app, &api, "nope", "")
                .await
                .unwrap()
                .is_none()
        );
        assert!(app.entries.last().unwrap().text.contains("未知命令"));

        handle_slash(&mut app, &api, "help", "").await.unwrap();
        assert!(app.entries.last().unwrap().text.contains("/model"));
    }

    fn conn(name: &str, url: &str, token: &str) -> TuiConnection {
        TuiConnection {
            name:  name.into(),
            url:   url.into(),
            token: token.into(),
        }
    }

    /// 设置界面行：连接若干 + 模型/预设/推理等级/主题；Enter 连接行请求切换。
    #[test]
    fn test_settings_rows_and_switch() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.connections = vec![conn("a", "http://a", "ta"), conn("b", "http://b", "tb")];
        app.active_conn = Some("b".into());

        // 高亮落在活动连接上
        open_settings(&mut app);
        assert_eq!(app.settings.as_ref().unwrap().index, 1);
        assert_eq!(settings_rows(&app).len(), 2 + 4, "两条连接 + 四项设置");

        // Enter 在连接行返回切换目标
        match handle_settings_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            SettingsAction::Switch { addr, name, .. } => {
                assert_eq!(addr, "http://b");
                assert_eq!(name, "b");
            }
            _ => panic!("连接行 Enter 应请求切换"),
        }
        assert!(matches!(
            handle_settings_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            SettingsAction::Close
        ));
    }

    /// 连接表单：新增/编辑写回、重名与空值拒绝、编辑改名时活动连接跟随。
    #[test]
    fn test_connection_form_validation_and_edits() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.connections = vec![conn("a", "http://a", "ta")];
        open_settings(&mut app);

        // 新增
        let add = ConnForm {
            name:    "c".into(),
            url:     "http://c".into(),
            token:   "tc".into(),
            field:   0,
            editing: None,
        };
        assert!(commit_conn_form(&mut app, &add));
        assert_eq!(app.connections.len(), 2);
        assert!(app.connections_dirty, "改动过就要回传写盘");

        // 重名拒绝（新增）
        let dup = ConnForm {
            name:    "a".into(),
            url:     "http://x".into(),
            token:   String::new(),
            field:   0,
            editing: None,
        };
        assert!(!commit_conn_form(&mut app, &dup));
        assert_eq!(app.connections.len(), 2);

        // 空名称拒绝
        let empty = ConnForm {
            name:    "   ".into(),
            url:     "http://x".into(),
            token:   String::new(),
            field:   0,
            editing: None,
        };
        assert!(!commit_conn_form(&mut app, &empty));

        // 编辑第 0 条并改名：活动连接跟随
        app.active_conn = Some("a".into());
        let edit = ConnForm {
            name:    "a2".into(),
            url:     "http://a2".into(),
            token:   "t".into(),
            field:   0,
            editing: Some(0),
        };
        assert!(commit_conn_form(&mut app, &edit));
        assert_eq!(app.connections[0].name, "a2");
        assert_eq!(app.active_conn.as_deref(), Some("a2"));
    }

    /// 连接表单按键：Tab 切换字段、输入落到当前字段、Esc 取消。
    #[test]
    fn test_connection_form_keys() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        open_settings(&mut app);
        open_conn_form(&mut app, None);

        handle_conn_form(&mut app, KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        assert_eq!(app.settings.as_ref().unwrap().form.as_ref().unwrap().name, "h");
        handle_conn_form(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.settings.as_ref().unwrap().form.as_ref().unwrap().field, 1);
        handle_conn_form(&mut app, KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        let form = app.settings.as_ref().unwrap().form.as_ref().unwrap();
        assert_eq!(form.url, "u");
        assert_eq!(form.name, "h", "输入应落在切换后的字段");

        handle_conn_form(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.settings.as_ref().unwrap().form.is_none(), "Esc 关闭表单");
    }

    /// 历史树平铺：先序 DFS、带连接线，当前分支与叶子标记正确。
    #[test]
    fn test_tree_items_order_and_marks() {
        let tree = vec![
            user_msg("u1", None, "问题", 1),
            msg("a1", Some("u1"), vec![body("回答一")], 2),
            msg("t1", Some("a1"), vec![receipt()], 3),
            msg("a2", Some("u1"), vec![body("另一条分支")], 4),
        ];
        let items = build_tree_items(&tree, Some("a1"));
        let ids: Vec<&str> = items.iter().map(|item| item.id.as_str()).collect();
        assert_eq!(ids, vec!["u1", "a1", "t1", "a2"], "按创建时间的先序 DFS");

        assert!(items[0].on_current && items[1].on_current, "u1 → a1 是当前链");
        assert!(!items[3].on_current, "兄弟分支不在当前链上");
        assert!(items[2].is_leaf && items[3].is_leaf, "t1 与 a2 是叶子");
        assert!(!items[0].is_leaf, "u1 有子节点");
        assert!(
            items[1].prefix.contains('├') || items[1].prefix.contains('└'),
            "子层应带连接线：{:?}",
            items[1].prefix
        );
    }

    /// 历史树按键：上下移动、Enter 选中、Esc 关闭。
    #[test]
    fn test_tree_key_navigation_and_select() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        let tree = vec![
            user_msg("u1", None, "问题", 1),
            msg("a1", Some("u1"), vec![body("答")], 2),
        ];
        app.tree_view = Some(TreeSelector {
            items: build_tree_items(&tree, Some("a1")),
            index: 0,
        });

        assert!(matches!(
            handle_tree_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            TreeAction::None
        ));
        assert_eq!(app.tree_view.as_ref().unwrap().index, 1);
        assert!(matches!(
            handle_tree_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            TreeAction::None
        ));
        assert_eq!(app.tree_view.as_ref().unwrap().index, 0);

        // Enter 取出该节点 id（用于 SwitchBranch）
        app.tree_view.as_mut().unwrap().index = 1;
        match handle_tree_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            TreeAction::Select(id) => assert_eq!(id, "a1"),
            _ => panic!("Enter 应取出选中的节点"),
        }
        assert!(matches!(
            handle_tree_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            TreeAction::Close
        ));
    }

    /// 思考段耗时：展开态挂在首行前缀，折叠态挂在折叠标签。
    #[test]
    fn test_thinking_duration_shown_expanded_and_collapsed() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        app.append_thinking("想了很久", Style::default());
        app.finish_thinking(Some(1500));
        assert!(plain_lines(&app).contains("思 · 1.5s"), "{}", plain_lines(&app));

        app.thinking_collapsed = true;
        let folded = plain_lines(&app);
        assert!(folded.contains("▸ 思 1.5s"), "{folded}");
        assert!(!folded.contains("想了很久"), "{folded}");
    }

    /// 整轮回执：显示墙钟耗时与输入/输出、缓存读写用量。
    #[test]
    fn test_turn_finished_reports_duration_and_usage() {
        let mut app = App::new("/w".into(), "m".into(), "task".into(), TuiTheme::test());
        // 起点回拨 2.5s，等价于一轮耗时 2.5s（避免测试真的等待）
        app.turn_started_at = Some(Instant::now() - Duration::from_millis(2500));
        apply_event(
            &mut app,
            AgentEvent::TurnFinished {
                turn_id:     "t1".into(),
                stop_reason: StopReason::EndTurn,
                usage:       TokenUsage {
                    input_tokens:       120,
                    output_tokens:      34,
                    cache_read_tokens:  100,
                    cache_write_tokens: 20,
                },
            },
        );
        let last = app.entries.last().unwrap();
        assert_eq!(last.kind, EntryKind::Notice);
        assert!(last.text.contains("耗时 2.5s"), "{}", last.text);
        assert!(last.text.contains("↑120 ↓34"), "{}", last.text);
        assert!(last.text.contains("缓存读 100"), "{}", last.text);
        assert!(last.text.contains("缓存写 20"), "{}", last.text);
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

    /// 会话弹窗条目：首项固定是「新建会话」，当前会话标 ✓ 且说明里带模型名。
    #[test]
    fn test_session_items_marks_current_and_offers_new() {
        let sessions = vec![record("s1", "第一个会话"), record("s2", "当前会话")];
        let items = session_items(&sessions, "/w", "s2");

        assert_eq!(items.len(), 3);
        match &items[0].action {
            PickerAction::NewSession => {}
            _ => panic!("首项应是「新建会话」"),
        }
        assert!(!items[0].current, "新建会话不是当前项");
        assert!(items[2].current && !items[1].current, "只有当前会话带 ✓");
        assert!(items[2].detail.contains('m'), "说明里带模型名，便于区分同名会话");
        assert_eq!(picker_current_index(&items), 2, "高亮应落在当前会话上");

        // 没有任何会话时也留着「新建会话」，弹窗不会空着
        let only_new = session_items(&[], "/w", "");
        assert_eq!(only_new.len(), 1);
        assert_eq!(picker_current_index(&only_new), 0);

        // 刚建的会话没有标题：用占位文案，避免弹窗里出现空行
        let mut untitled = record("s3", "");
        untitled.active_model = String::new();
        let items = session_items(&[untitled], "/w", "");
        assert_eq!(items[1].label, "未命名");
    }

    /// 会话弹窗的按键：Enter 取出切换目标并关窗，Esc 只关窗。
    #[test]
    fn test_session_picker_keys() {
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
        app.session_id = "s2".into();
        app.sessions = vec![record("s1", "第一个会话"), record("s2", "当前会话")];
        app.picker = Some(Picker {
            title: " 切换会话 ".into(),
            items: session_items(&app.sessions, "/w", &app.session_id),
            index: 2,
        });

        match handle_picker_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Some(PickerAction::SwitchSession { session_id }) => assert_eq!(session_id, "s2"),
            _ => panic!("Enter 应返回会话切换目标"),
        }
        assert!(app.picker.is_none(), "确认后弹窗应关闭");

        // Esc 关窗且不返回动作
        app.picker = Some(Picker {
            title: " 切换会话 ".into(),
            items: session_items(&app.sessions, "/w", &app.session_id),
            index: 0,
        });
        assert!(handle_picker_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_none());
        assert!(app.picker.is_none());
    }

    /// 文本弹窗：预填当前标题，Backspace 编辑、Enter 取出请求、Esc 取消。
    #[test]
    fn test_prompt_edits_and_confirms() {
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
        app.session_id = "s2".into();
        app.sessions = vec![record("s1", "第一个会话"), record("s2", "当前会话")];

        open_rename_prompt(&mut app);
        let prompt = app.prompt.as_ref().unwrap();
        assert_eq!(prompt.input, "当前会话", "应预填当前标题");
        assert_eq!(prompt.kind, PromptKind::RenameSession);

        // 退格删掉末字，再补一个字
        handle_prompt_key(&mut app, KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        handle_prompt_key(&mut app, KeyEvent::new(KeyCode::Char('新'), KeyModifiers::NONE));
        assert_eq!(app.prompt.as_ref().unwrap().input, "当前会新");

        // 未定义的 Ctrl+字母不落进文本（与输入区一致）
        handle_prompt_key(&mut app, KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(app.prompt.as_ref().unwrap().input, "当前会新");

        // Enter 取出请求并关窗
        match handle_prompt_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            Some((PromptKind::RenameSession, input)) => assert_eq!(input, "当前会新"),
            _ => panic!("Enter 应取出重命名请求"),
        }
        assert!(app.prompt.is_none(), "确认后弹窗应关闭");

        // Esc 关窗且不取出请求
        open_rename_prompt(&mut app);
        assert!(handle_prompt_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)).is_none());
        assert!(app.prompt.is_none());

        // 快照里没有当前会话（列表拉取失败过）：留空让用户自己写，不挡住改名
        app.sessions.clear();
        app.session_id = "s9".into();
        open_rename_prompt(&mut app);
        assert!(app.prompt.as_ref().unwrap().input.is_empty());
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

    /// 分支代表：叶子停在工具回执上时退到最近的可见祖先，按时间倒序，去重。
    #[test]
    fn test_branch_tips_falls_back_to_visible_ancestor() {
        let tree = vec![
            msg("u1", None, vec![body("第一个问题")], 1),
            msg("a1", Some("u1"), vec![call("read")], 2),
            msg("t1", Some("a1"), vec![receipt()], 3),
            msg("a2", Some("u1"), vec![body("另一条分支的回答")], 4),
        ];
        let tips = branch_tips(&tree);
        assert_eq!(
            tips.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["a2", "a1"],
            "工具回执不该作为切换目标，应退到它可见的祖先 a1；最近的排前面"
        );
        assert!(tips.iter().all(|m| is_user_visible(m)));
        assert_eq!(branch_len(&tree, tips[1]), 2, "u1 → a1 两条");
        assert_eq!(branch_len(&tree, &tree[2]), 3, "回执自身回溯到根是 3 条");
    }

    /// 分支代表不重复：两个叶子退到同一条可见消息时只留一个。
    #[test]
    fn test_branch_tips_dedups_same_ancestor() {
        let tree = vec![
            msg("u1", None, vec![body("问题")], 1),
            msg("a1", Some("u1"), vec![body("回答")], 2),
            msg("t1", Some("a1"), vec![receipt()], 3),
            msg("t2", Some("a1"), vec![receipt()], 4),
        ];
        let tips = branch_tips(&tree);
        assert_eq!(tips.len(), 1);
        assert_eq!(tips[0].id, "a1");
    }

    /// 分支摘要：优先正文、多行压平、超长截断，没有正文时回落到工具名。
    #[test]
    fn test_leaf_snippet() {
        let multiline = msg("m", None, vec![body("第一行\n\n  第二行")], 1);
        assert_eq!(leaf_snippet(&multiline), "第一行 第二行");

        let long = msg("m", None, vec![body(&"字".repeat(40))], 2);
        let snippet = leaf_snippet(&long);
        assert!(snippet.ends_with('…'), "超长应截断：{snippet}");
        assert!(display_width(&snippet) <= 24);

        // 只有工具调用时用工具名当摘要
        let tool = msg("m", None, vec![call("shell")], 3);
        assert_eq!(leaf_snippet(&tool), "⚙ shell");
    }

    /// 当前分支的代表叶子：服务端给的叶子若仍是叶子就用它（可能停在旧分支上），
    /// 已经过时（长出子节点）时退回最新的叶子。
    #[test]
    fn test_branch_leaf_uses_live_hint_else_newest() {
        let tree = vec![
            user_msg("u1", None, "问题", 1),
            msg("a1", Some("u1"), vec![body("回答")], 2),
            msg("a2", Some("u1"), vec![body("另一条分支")], 3),
        ];
        // 服务端给的叶子仍是叶子：即使它不是最新的也照用（用户正停在旧分支上）
        assert_eq!(branch_leaf(&tree, Some("a1")).unwrap().id, "a1");
        // 过时的 hint（已长出子节点的 u1）与没有 hint 都退回最新叶子
        assert_eq!(branch_leaf(&tree, Some("u1")).unwrap().id, "a2");
        assert_eq!(branch_leaf(&tree, None).unwrap().id, "a2");
        assert!(branch_leaf(&[], None).is_none());
    }

    /// 当前分支的用户消息：只回溯当前叶子这一条链，新的在前。
    #[test]
    fn test_user_messages_on_branch_newest_first() {
        let tree = vec![
            user_msg("u1", None, "第一个问题", 1),
            msg("a1", Some("u1"), vec![body("回答")], 2),
            user_msg("u2", Some("a1"), "第二个问题", 3),
            msg("a2", Some("u2"), vec![body("回答二")], 4),
            // 另一条分支：不该出现在 u2 → a2 这条链上
            user_msg("u3", Some("u1"), "分叉问题", 5),
            msg("a3", Some("u3"), vec![body("分叉回答")], 6),
        ];
        let leaf = branch_leaf(&tree, Some("a2")).unwrap();
        let ids: Vec<&str> = user_messages_on_branch(&tree, Some(leaf))
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, vec!["u2", "u1"], "新的在前，且不含别的分支");

        assert!(user_messages_on_branch(&tree, None).is_empty());
    }

    /// 编辑重发条目：首条消息没有父节点（无处可分叉），其余带父节点；
    /// 回填的是完整正文而非截断后的摘要。
    #[test]
    fn test_fork_items_carry_text_and_parent() {
        let long = "很长的正文".repeat(20);
        let tree = vec![
            user_msg("u1", None, "第一个问题", 1),
            msg("a1", Some("u1"), vec![body("回答")], 2),
            user_msg("u2", Some("a1"), &long, 3),
            msg("a2", Some("u2"), vec![body("回答二")], 4),
        ];
        let leaf = branch_leaf(&tree, Some("a2")).unwrap();
        let items = fork_items(&tree, Some(leaf));
        assert_eq!(items.len(), 2);

        match &items[0].action {
            PickerAction::ForkFrom { text, parent_id, label } => {
                assert_eq!(text, &long, "回填完整正文");
                assert_eq!(parent_id.as_deref(), Some("a1"));
                assert!(label.ends_with('…'), "标签仍是截断摘要：{label}");
            }
            _ => panic!("条目应下发 ForkFrom"),
        }
        match &items[1].action {
            PickerAction::ForkFrom { parent_id, .. } => assert!(parent_id.is_none()),
            _ => panic!("条目应下发 ForkFrom"),
        }
    }

    /// 消息正文提取：拼接全部 Text 块，跳过思考与工具块。
    #[test]
    fn test_message_text_collects_text_blocks() {
        let message = msg("m", None, vec![call("read"), body("正文")], 1);
        assert_eq!(message_text(&message), "正文");
        assert_eq!(message_text(&msg("m2", None, vec![receipt()], 2)), "");
    }

    /// 附件标记：`@路径` 摘成附件引用并从正文里去掉；普通文本里的 `@` 只是文本。
    #[test]
    fn test_extract_attachments() {
        let (text, paths) = extract_attachments("@img/a.png 看看这张图");
        assert_eq!(text, "看看这张图");
        assert_eq!(paths, vec!["img/a.png"]);

        // 多个附件 + 纯附件（没有正文）都允许
        let (text, paths) = extract_attachments("@a.png @b.jpg");
        assert!(text.is_empty());
        assert_eq!(paths, vec!["a.png", "b.jpg"]);

        // 单独的 `@` 与词中的 `@` 不是附件标记
        let (text, paths) = extract_attachments("价格 @ 5 元 mail@host");
        assert_eq!(text, "价格 @ 5 元 mail@host");
        assert!(paths.is_empty());
    }

    /// 附件路径限制在工作区内：绝对路径与 `..` 逃逸一律拒绝。
    #[test]
    fn test_attachment_paths_stay_in_workspace() {
        assert!(is_workspace_relative("img/a.png"));
        assert!(is_workspace_relative("a.png"));
        assert!(!is_workspace_relative(""));
        assert!(!is_workspace_relative("/etc/passwd"));
        assert!(!is_workspace_relative("../secrets.txt"));
        assert!(!is_workspace_relative("img/../../secrets.txt"));
        assert!(!is_workspace_relative("..\\win.txt"));
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

    /// 括号粘贴：整段文本一次进输入框（多行不再被当成多次回车）；文本弹窗
    /// 打开时落进弹窗，列表弹窗直接忽略。
    #[test]
    fn test_paste_goes_into_active_input() {
        let mut app = App::new("/w".into(), "m".into(), "a".into(), TuiTheme::test());
        assert!(classify_input(&mut app, InputEvent::Paste("第一行\n第二行".into())).is_none());
        assert_eq!(app.input, "第一行 第二行", "换行换成空格，不会触发发送");

        // 文本弹窗（重命名）打开时优先落进弹窗，主输入区不受影响
        app.session_id = "s1".into();
        open_rename_prompt(&mut app);
        classify_input(&mut app, InputEvent::Paste("新标题".into()));
        assert_eq!(app.prompt.as_ref().unwrap().input, "新标题");
        assert_eq!(app.input, "第一行 第二行");

        // 列表弹窗没有可输入的地方：粘贴丢掉
        app.prompt = None;
        app.picker = Some(Picker {
            title: " 切换会话 ".into(),
            items: Vec::new(),
            index: 0,
        });
        classify_input(&mut app, InputEvent::Paste("忽略".into()));
        assert_eq!(app.input, "第一行 第二行");
        assert!(app.prompt.is_none());
    }

    /// 粘贴清理：CRLF 归一、控制字符换成空格，正文原样保留。
    #[test]
    fn test_clean_paste_normalizes_control_chars() {
        assert_eq!(clean_paste("a\nb"), "a b");
        assert_eq!(clean_paste("a\r\nb"), "a b", "CRLF 只留一个空格");
        assert_eq!(clean_paste("a\tb"), "a b");
        assert_eq!(clean_paste("a\x07b"), "a b");
        assert_eq!(clean_paste("中文 @pic.png"), "中文 @pic.png");
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

    /// 造一条消息：只填会话树用得到的字段，模型与用量跟这些用例无关。
    fn msg(id: &str, parent: Option<&str>, content: Vec<MsgBlock>, created_at: i64) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            parent_id: parent.map(str::to_string),
            role: Role::Assistant,
            content,
            created_at,
            model: None,
            usage: None,
        }
    }

    /// 造一条用户消息：编辑重发与分支回溯都只看角色与父节点。
    fn user_msg(id: &str, parent: Option<&str>, text: &str, created_at: i64) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            ..msg(id, parent, vec![body(text)], created_at)
        }
    }

    fn body(text: &str) -> MsgBlock {
        MsgBlock::Text { text: text.into() }
    }

    fn call(name: &str) -> MsgBlock {
        MsgBlock::ToolUse {
            id:    "c1".into(),
            name:  name.into(),
            input: serde_json::json!({}),
        }
    }

    fn receipt() -> MsgBlock {
        MsgBlock::ToolResult {
            tool_use_id: "c1".into(),
            content:     "ok".into(),
            is_error:    false,
            duration_ms: None,
        }
    }
}
