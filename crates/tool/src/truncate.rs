//! 工具输出截断，与 pi `core/tools/truncate.ts` 行为一致。
//!
//! 两套互相独立的上限，**先到者生效**：
//!   - 行数上限（默认 2000 行）
//!   - 字节上限（默认 50KB）
//!
//! 除 `truncate_tail` 的「末行本身超限」边界外，永不返回半行。

/// 默认行数上限
pub const DEFAULT_MAX_LINES: usize = 2000;
/// 默认字节上限（50KB）
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;
/// grep 单行最长字符数
pub const GREP_MAX_LINE_LENGTH: usize = 500;

/// 触发了哪一条上限
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

/// 截断结果
#[derive(Debug, Clone)]
pub struct Truncation {
    /// 截断后的内容
    pub content:                  String,
    /// 是否发生了截断
    pub truncated:                bool,
    /// 触发上限；未截断时为 `None`
    pub truncated_by:             Option<TruncatedBy>,
    /// 原始内容行数
    pub total_lines:              usize,
    /// 原始内容字节数
    pub total_bytes:              usize,
    /// 输出内容行数
    pub output_lines:             usize,
    /// 输出内容字节数
    pub output_bytes:             usize,
    /// 末行是否被截成半行（仅 `truncate_tail` 的边界情形）
    pub last_line_partial:        bool,
    /// 首行本身超过字节上限（仅 `truncate_head`）
    pub first_line_exceeds_limit: bool,
    /// 本次应用的行数上限
    pub max_lines:                usize,
    /// 本次应用的字节上限
    pub max_bytes:                usize,
}

/// 按行切分并计数；结尾换行不产生额外空行（与 JS `split` + `pop` 等价）。
fn split_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn untouched(content: &str, total_lines: usize, total_bytes: usize, max_lines: usize, max_bytes: usize) -> Truncation {
    Truncation {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes,
        output_lines: total_lines,
        output_bytes: total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// 字节数的人类可读形式（与 pi `formatSize` 一致，如 `50.0KB`）。
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// 用默认上限从**头部**截断（保留开头），适合 read 这类「先看开头」的输出。
pub fn truncate_head(content: &str) -> Truncation {
    truncate_head_with(content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
}

/// 指定上限的头部截断。
pub fn truncate_head_with(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total_bytes = content.len();
    let lines = split_lines(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return untouched(content, total_lines, total_bytes, max_lines, max_bytes);
    }

    // 首行就超过字节上限：整段放弃，让调用方给出可操作的提示
    if lines.first().map(|l| l.len()).unwrap_or(0) > max_bytes {
        return Truncation {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut kept: Vec<&str> = Vec::new();
    let mut used_bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate() {
        if i >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(i > 0);
        if used_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        kept.push(line);
        used_bytes += line_bytes;
    }

    if kept.len() >= max_lines && used_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output = kept.join("\n");
    Truncation {
        output_lines: kept.len(),
        output_bytes: output.len(),
        content: output,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// 用默认上限从**尾部**截断（保留结尾），适合 bash 这类「错误在末尾」的输出。
pub fn truncate_tail(content: &str) -> Truncation {
    truncate_tail_with(content, DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES)
}

/// 指定上限的尾部截断。
pub fn truncate_tail_with(content: &str, max_lines: usize, max_bytes: usize) -> Truncation {
    let total_bytes = content.len();
    let lines = split_lines(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return untouched(content, total_lines, total_bytes, max_lines, max_bytes);
    }

    let mut kept: Vec<String> = Vec::new();
    let mut used_bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if kept.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(!kept.is_empty());
        if used_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // 边界：一行都没放进，且这一行自己就超限 —— 保留它的末尾（半行）
            if kept.is_empty() {
                let partial = truncate_to_bytes_from_end(line, max_bytes);
                used_bytes = partial.len();
                kept.push(partial);
                last_line_partial = true;
            }
            break;
        }
        kept.push((*line).to_string());
        used_bytes += line_bytes;
    }
    kept.reverse();

    if kept.len() >= max_lines && used_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let output = kept.join("\n");
    Truncation {
        output_lines: kept.len(),
        output_bytes: output.len(),
        content: output,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// 从字符串末尾取不超过 `max_bytes` 字节，并对齐到 UTF-8 字符边界。
fn truncate_to_bytes_from_end(s: &str, max_bytes: usize) -> String {
    let bytes = s.as_bytes();
    if bytes.len() <= max_bytes {
        return s.to_string();
    }
    let mut start = bytes.len() - max_bytes;
    while start < bytes.len() && (bytes[start] & 0xC0) == 0x80 {
        start += 1;
    }
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

/// 把单行截到 `max_chars` 个字符，超出时追加 `... [truncated]`。
///
/// 返回 `(文本, 是否被截断)`。
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    let head: String = line.chars().take(max_chars).collect();
    (format!("{head}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_keeps_everything_under_limits() {
        let t = truncate_head("a\nb\nc\n");
        assert!(!t.truncated);
        assert_eq!(t.content, "a\nb\nc\n");
        assert_eq!((t.total_lines, t.output_lines), (3, 3));
    }

    /// 行数上限先到：保留前 N 行且不产生半行。
    #[test]
    fn head_stops_at_line_limit() {
        let t = truncate_head_with("l1\nl2\nl3\nl4\n", 2, DEFAULT_MAX_BYTES);
        assert!(t.truncated);
        assert_eq!(t.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(t.content, "l1\nl2");
        assert_eq!(t.total_lines, 4);
    }

    /// 字节上限先到：不放半行。
    #[test]
    fn head_stops_at_byte_limit() {
        let t = truncate_head_with("aaaa\nbbbb\ncccc\n", 10, 5);
        assert_eq!(t.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(t.content, "aaaa");
    }

    /// 首行即超字节上限：返回空内容并标记，交由调用方给提示。
    #[test]
    fn head_reports_oversized_first_line() {
        let t = truncate_head_with("0123456789\nx\n", 10, 5);
        assert!(t.first_line_exceeds_limit);
        assert!(t.content.is_empty());
    }

    #[test]
    fn tail_keeps_last_lines() {
        let t = truncate_tail_with("l1\nl2\nl3\nl4\n", 2, DEFAULT_MAX_BYTES);
        assert_eq!(t.content, "l3\nl4");
        assert_eq!(t.truncated_by, Some(TruncatedBy::Lines));
    }

    /// 末行本身超限：保留它的末尾（半行），其余全丢。
    #[test]
    fn tail_keeps_partial_when_single_line_too_long() {
        let t = truncate_tail_with("aaaaaaaaaaaaaaaaaaaa", 10, 5);
        assert!(t.last_line_partial);
        assert_eq!(t.content, "aaaaa");
    }

    /// 多字节字符不会被切成半个。
    #[test]
    fn tail_respects_utf8_boundaries() {
        // 9 个汉字共 18 字节；取末尾 5 字节时不能停在字符中间
        let t = truncate_tail_with("中文中文中文", 10, 5);
        assert!(t.last_line_partial);
        assert_eq!(t.content, "文");
    }

    #[test]
    fn line_truncation_appends_marker() {
        let (text, cut) = truncate_line("0123456789", 4);
        assert!(cut);
        assert_eq!(text, "0123... [truncated]");
        let (text, cut) = truncate_line("abc", 4);
        assert!(!cut);
        assert_eq!(text, "abc");
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(DEFAULT_MAX_BYTES), "50.0KB");
        assert_eq!(format_size(2 * 1024 * 1024), "2.0MB");
    }
}
