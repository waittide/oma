//! 命令行定义与中文化。
//!
//! clap 不提供 i18n：`Usage:` / `Options:` / `Commands:` 标题、`[default: …]` 标注、
//! 内建的 `-h/--help`、`-V/--version`、`help` 子命令以及解析错误文案都是硬编码
//! 英文，且没有任何改写入口。因此这里在派生出的命令树上统一改写：自建帮助项与
//! `help` 子命令、用 `help_template` 接管版式、按 [`ErrorKind`] 重排解析错误，
//! 使 CLI 的输出全为中文。

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{
    Arg, ArgAction, Command, CommandFactory, FromArgMatches, Parser, Subcommand,
    error::{ContextKind, ErrorKind},
};

use crate::DEFAULT_ADDR;

/// 帮助版式：结构沿用 clap 默认，仅把英文的 `Usage:` 标题换成中文。
///
/// `{usage-heading}` 渲染的是硬编码的 `Usage:`，所以这里只用 `{usage}`。
const HELP_TEMPLATE: &str = "{before-help}{about-with-newline}\n用法: {usage}\n\n{all-args}{after-help}";

#[derive(Parser)]
#[command(
    name = "oma",
    about = "Oma：多客户端协同 AI Agent",
    long_about = "Oma：多客户端协同 AI Agent\n\n不带子命令时等价于 `oma -h`，不会自动启动任何界面。",
    version = "0.1.0"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// 独立启动后台 Daemon 服务
    Daemon {
        /// Daemon 监听地址
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr:   String,
        /// 覆盖配置文件中的访问 token
        #[arg(long)]
        token:  Option<String>,
        /// 配置文件路径（默认 ~/.config/oma/config.toml）
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// 启动 web 客户端
    Web {
        /// web 监听地址（默认读取 client.toml 的 web.host，缺省 127.0.0.1）
        #[arg(long)]
        host: Option<String>,
        /// web 监听端口（默认读取 client.toml 的 web.port，缺省 5173）
        #[arg(long)]
        port: Option<u16>,
        /// 就绪后自动用浏览器打开页面（默认仅打印监听地址）
        #[arg(long)]
        open: bool,
    },
    /// 启动 tui 客户端
    Tui {
        /// Daemon 地址
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr:      String,
        /// 覆盖配置文件中的访问 token
        #[arg(long)]
        token:     Option<String>,
        /// 目标工作区（默认当前目录）
        #[arg(long)]
        workspace: Option<String>,
    },
    /// 查看 Daemon 服务端运行状态
    Status {
        /// Daemon 地址
        #[arg(long, default_value = DEFAULT_ADDR)]
        addr:  String,
        /// 覆盖配置文件中的访问 token
        #[arg(long)]
        token: Option<String>,
    },
    /// 打印此帮助或指定子命令的帮助
    Help {
        /// 要查看帮助的子命令
        #[arg(value_name = "子命令")]
        subcommand: Option<String>,
    },
}

/// 解析命令行参数；`-h/--help` 与 `-V/--version` 由 clap 打印后直接退出。
pub fn parse() -> Cli {
    let mut cmd = command();
    let matches = match cmd.try_get_matches_from_mut(std::env::args_os()) {
        Ok(matches) => matches,
        Err(err) => {
            // 帮助与版本属于正常输出：交回 clap 打印到 stdout 并以 0 退出
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) {
                err.exit();
            }
            eprintln!("{}", render_error(&err));
            // 与 clap 一致：用法错误以 2 退出，便于脚本区分「参数错」与「运行失败」
            std::process::exit(2);
        }
    };
    Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
}

/// 本地化后的命令定义。
pub fn command() -> Command {
    let cmd = localize(Cli::command());
    let path = cmd.get_name().to_string();
    localize_usage(cmd, &path)
}

/// 把用法串里的 `[OPTIONS]` 占位符换成中文。
///
/// 该占位符由 clap 的用法生成器硬编码，没有任何配置项；这里读回它生成的用法串、
/// 替换后覆盖回去，既保留「按参数自动生成」的准确性，又避免手写用法串与定义脱节。
///
/// 用法串依赖调用路径（`oma web` 这类 bin 名），而 `bin_name` 本来要到解析时才由
/// 父命令填好，所以这里按命令树逐层显式设置，否则渲染出的是缺了前缀的 `web [选项]`。
/// 另外必须在 [`localize`] 之后跑：`render_usage` 会触发 clap 的内部构建，此时命令
/// 树已定型，不会漏掉随后才补上的帮助项。
fn localize_usage(mut cmd: Command, path: &str) -> Command {
    cmd = cmd.bin_name(path.to_string());

    let rendered = cmd.render_usage().to_string();
    if let Some(usage) = rendered.strip_prefix("Usage: ") {
        cmd = cmd.override_usage(usage.replace("[OPTIONS]", "[选项]"));
    }

    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_string())
        .collect();
    for name in names {
        let child_path = format!("{path} {name}");
        cmd = cmd.mut_subcommand(&name, |child| localize_usage(child, &child_path));
    }
    cmd
}

/// 打印顶层帮助，或指定子命令的帮助。
pub fn print_help(subcommand: Option<&str>) -> Result<()> {
    let mut cmd = command();
    match subcommand {
        Some(name) => {
            let sub = cmd
                .find_subcommand_mut(name)
                .with_context(|| format!("未知子命令: {name}（可用: daemon / web / tui / status）"))?;
            sub.print_help()?;
        }
        None => cmd.print_help()?,
    }
    println!();
    Ok(())
}

/// 把 clap 的英文固定文案改写为中文。
///
/// `disable_help_flag` / `disable_version_flag` / `disable_help_subcommand` 都是
/// **全局设置**，会连同子命令一起生效，所以每层都要把帮助项与模板补回去。
fn localize(cmd: Command) -> Command {
    let has_version = cmd.get_version().is_some();
    let mut cmd = cmd
        .help_template(HELP_TEMPLATE)
        .subcommand_help_heading("子命令")
        .subcommand_value_name("子命令")
        .disable_help_flag(true)
        .disable_version_flag(true)
        .disable_help_subcommand(true)
        // 没有分组标题的参数会落进 clap 固定的 `Options:` / `Arguments:` 分组，
        // 那两个标题改不了中文，所以先给派生出的参数逐个挂上中文标题
        .mut_args(|arg| {
            let heading = if arg.is_positional() { "参数" } else { "选项" };
            localize_default(arg.help_heading(heading))
        })
        .next_help_heading("选项")
        .arg(
            Arg::new("help")
                .short('h')
                .long("help")
                .action(ArgAction::Help)
                .help("打印帮助")
                .long_help("打印帮助（-h 摘要，--help 全部）"),
        );
    if has_version {
        cmd = cmd.arg(
            Arg::new("version")
                .short('V')
                .long("version")
                .action(ArgAction::Version)
                .help("打印版本"),
        );
    }
    cmd.mut_subcommands(localize)
}

/// 把 `[default: …]` 标注换成中文的默认值说明。
///
/// 默认值现读自参数本身，因此帮助文案不会与默认值各写一份而漂移。
/// 布尔开关的默认值 clap 本来就不展示，这里同样跳过。
fn localize_default(arg: Arg) -> Arg {
    let shows_default = matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
        && !arg.is_hide_default_value_set()
        && !arg.get_default_values().is_empty();
    if !shows_default {
        return arg;
    }

    let values = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let note = format!("（默认 {values}）");
    let help = match arg.get_help() {
        Some(help) => format!("{help}{note}"),
        None => note,
    };
    arg.help(help).hide_default_value(true)
}

/// 按 [`ErrorKind`] 与上下文重新组织解析错误。
///
/// clap 的错误文案同样是硬编码英文，只能自行渲染；用到的上下文（出错参数、
/// 取值、候选子命令、用法）都从错误自带的 [`ContextKind`] 里取。
fn render_error(err: &clap::Error) -> String {
    fn text(err: &clap::Error, kind: ContextKind) -> Option<String> {
        err.get(kind)
            .map(ToString::to_string)
            .filter(|s| !s.is_empty())
    }

    let message = match err.kind() {
        ErrorKind::UnknownArgument => match text(err, ContextKind::InvalidArg) {
            Some(arg) => format!("未知参数 '{arg}'"),
            None => "未知参数".to_string(),
        },
        ErrorKind::InvalidSubcommand => match text(err, ContextKind::InvalidSubcommand) {
            Some(sub) => format!("未知子命令 '{sub}'"),
            None => "未知子命令".to_string(),
        },
        // clap 把「需要取值但没给」表示成空取值，与「取值非法」共用这一个错误类型
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
            match (text(err, ContextKind::InvalidArg), text(err, ContextKind::InvalidValue)) {
                (Some(arg), Some(value)) => format!("参数 '{arg}' 的取值 '{value}' 无效"),
                (Some(arg), None) => format!("参数 '{arg}' 缺少取值"),
                _ => "参数取值无效".to_string(),
            }
        }
        ErrorKind::NoEquals => match text(err, ContextKind::InvalidArg) {
            Some(arg) => format!("参数 '{arg}' 必须写成 --名字=取值 的形式"),
            None => "参数必须写成 --名字=取值 的形式".to_string(),
        },
        ErrorKind::TooManyValues => match (text(err, ContextKind::InvalidArg), text(err, ContextKind::InvalidValue)) {
            (Some(arg), Some(value)) => format!("参数 '{arg}' 不接受取值 '{value}'"),
            _ => "参数的取值过多".to_string(),
        },
        ErrorKind::TooFewValues => match (text(err, ContextKind::InvalidArg), text(err, ContextKind::MinValues)) {
            (Some(arg), Some(min)) => format!("参数 '{arg}' 至少需要 {min} 个取值"),
            _ => "参数的取值过少".to_string(),
        },
        ErrorKind::WrongNumberOfValues => {
            match (
                text(err, ContextKind::InvalidArg),
                text(err, ContextKind::ExpectedNumValues),
            ) {
                (Some(arg), Some(expected)) => format!("参数 '{arg}' 需要 {expected} 个取值"),
                _ => "参数的取值个数不正确".to_string(),
            }
        }
        // 同一个参数给了多次时，PriorArg 与 InvalidArg 相同
        ErrorKind::ArgumentConflict => match (text(err, ContextKind::InvalidArg), text(err, ContextKind::PriorArg)) {
            (Some(arg), Some(prior)) if arg == prior => format!("参数 '{arg}' 不能重复使用"),
            (Some(arg), Some(prior)) => format!("参数 '{arg}' 不能与 '{prior}' 同时使用"),
            (Some(arg), None) => format!("参数 '{arg}' 不能重复使用"),
            _ => "参数冲突".to_string(),
        },
        // 缺失的必需参数以列表形式放在 InvalidArg 里
        ErrorKind::MissingRequiredArgument => match text(err, ContextKind::InvalidArg) {
            Some(args) => format!("缺少必需参数: {args}"),
            None => "缺少必需参数".to_string(),
        },
        ErrorKind::MissingSubcommand => "缺少子命令".to_string(),
        _ => "命令行参数无效".to_string(),
    };

    let mut out = format!("错误: {message}");
    for (kind, label) in [
        (ContextKind::ValidValue, "可选取值"),
        (ContextKind::ValidSubcommand, "可选子命令"),
    ] {
        if let Some(values) = text(err, kind) {
            out.push_str(&format!("\n提示: {label} {values}"));
        }
    }
    for (kind, what) in [
        (ContextKind::SuggestedSubcommand, "子命令"),
        (ContextKind::SuggestedArg, "参数"),
        (ContextKind::SuggestedValue, "取值"),
    ] {
        if let Some(suggestion) = text(err, kind) {
            out.push_str(&format!("\n提示: 是否想输入{what} '{suggestion}'？"));
        }
    }
    if let Some(usage) = text(err, ContextKind::Usage) {
        // clap 的用法串自带 `Usage:` 标题，这里换成中文
        out.push_str(&format!(
            "\n\n用法: {}",
            usage.strip_prefix("Usage: ").unwrap_or(&usage)
        ));
    }
    out.push_str("\n\n更多信息请执行 oma --help。");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CLI 输出必须全中文：clap 的固定英文文案一旦回归（升级换措辞、或漏改某个
    /// 开关）都会在这里被拦住。
    #[test]
    fn test_help_is_chinese() {
        let mut cmd = command();
        let root = cmd.render_help().to_string();
        for english in [
            "Usage:",
            "Options:",
            "Commands:",
            "Arguments:",
            "Print help",
            "Print version",
            "Print this message",
            "[OPTIONS]",
        ] {
            assert!(!root.contains(english), "帮助里残留英文 `{english}`:\n{root}");
        }
        for wanted in [
            "用法:",
            "子命令:",
            "选项:",
            "独立启动后台 Daemon 服务",
            "启动 web 客户端",
            "启动 tui 客户端",
            "查看 Daemon 服务端运行状态",
        ] {
            assert!(root.contains(wanted), "帮助缺少 `{wanted}`:\n{root}");
        }

        // 子命令帮助同理：`[default: …]` 这类标注也要换成中文默认值
        for name in ["daemon", "web", "tui", "status", "help"] {
            let sub = cmd.find_subcommand_mut(name).expect("子命令应当在命令树里");
            let help = sub.render_help().to_string();
            for english in ["Usage:", "Options:", "[default:", "Print help", "[OPTIONS]"] {
                assert!(!help.contains(english), "`{name}` 帮助里残留英文 `{english}`:\n{help}");
            }
        }
    }

    /// 解析错误同样不能回退到 clap 的英文：拼错子命令/参数是最常见的用法错误。
    #[test]
    fn test_parse_errors_are_chinese() {
        let err = command()
            .try_get_matches_from(["oma", "bogus"])
            .unwrap_err();
        let rendered = render_error(&err);
        assert!(rendered.contains("未知子命令 'bogus'"), "{rendered}");
        assert!(
            !rendered.contains("error:") && !rendered.contains("Usage:") && !rendered.contains("For more information"),
            "{rendered}"
        );

        let err = command()
            .try_get_matches_from(["oma", "web", "--bogus"])
            .unwrap_err();
        let rendered = render_error(&err);
        assert!(rendered.contains("未知参数 '--bogus'"), "{rendered}");
    }
}
