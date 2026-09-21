//! `apply_patch` 模式的解析与应用。
//!
//! 参照 oh-my-pi `crates/pi-edit`：模型可见契约是其 `prompts/apply_patch.md`，
//! 解析/报错文案来自 `modes/apply_patch.rs`，hunk 语义来自
//! `diff_string.rs`（`parse_diff_hunks`）与 `modes/patch.rs`（`apply_hunks`）。
//!
//! 本模块保留 envelope / hunk / 拒绝情形的语义，但不做模糊（fuzzy）与
//! 字符级匹配：上下文按**逐行精确**匹配，失败时给出相似度最高的窗口作为提示。
//! 这样「匹配不上」永远不会被猜成一个不想要的修改。

use std::fmt;

// ==========================================
// 标记
// ==========================================

/// envelope 起始行。
const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
/// envelope 结束行。
const END_PATCH_MARKER: &str = "*** End Patch";
/// 新建文件段落头。
const ADD_FILE_MARKER: &str = "*** Add File: ";
/// 删除文件段落头。
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
/// 修改文件段落头。
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
/// 修改段落里的改名指令。
const MOVE_TO_MARKER: &str = "*** Move to: ";
/// hunk 末尾的「匹配到文件结尾」标记。
const EOF_MARKER: &str = "*** End of File";
/// 多文件 envelope 失败时的原子性提示。
pub const ATOMICITY_NOTICE: &str = "No files were modified — sections apply atomically.";

const FIRST_LINE_ERROR: &str = "The first line of the patch must be '*** Begin Patch'";
const LAST_LINE_ERROR: &str = "The last line of the patch must be '*** End Patch'";

/// 统一 diff 元数据行：出现在 hunk 体里也一律忽略（与参考实现的
/// `is_unified_diff_metadata_line` 对齐，不含 `*** ` 系列行）。
const UNIFIED_METADATA_PREFIXES: [&str; 12] = [
    "diff --git ",
    "index ",
    "--- ",
    "+++ ",
    "new file mode ",
    "deleted file mode ",
    "rename from ",
    "rename to ",
    "similarity index ",
    "dissimilarity index ",
    "old mode ",
    "new mode ",
];

/// 单文件 diff 体里不允许出现的多文件标记。
const FILE_HEADER_MARKERS: [&str; 3] = [UPDATE_FILE_MARKER, ADD_FILE_MARKER, DELETE_FILE_MARKER];

// ==========================================
// 错误
// ==========================================

/// 解析或应用 patch 时的失败原因（面向模型的可读文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchError {
    message: String,
}

impl PatchError {
    /// 构造一条错误（可读文案直接面向模型）。
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 带行号的解析错误（与参考实现一致，行号 1 起）。
    fn at_line(line: usize, message: impl fmt::Display) -> Self {
        Self::new(format!("Line {line}: {message}"))
    }
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PatchError {}

// ==========================================
// 解析：envelope → 文件操作
// ==========================================

/// 一次文件操作的类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOp {
    /// `*** Add File:`
    Add,
    /// `*** Delete File:`
    Delete,
    /// `*** Update File:`
    Update,
}

impl FileOp {
    /// 成功摘要里的单字母标记。
    pub const fn summary_label(self) -> char {
        match self {
            Self::Add => 'A',
            Self::Delete => 'D',
            Self::Update => 'M',
        }
    }
}

/// envelope 解析出的一条文件操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchEntry {
    /// 段落头里的路径（相对工作区）。
    pub path: String,
    /// 操作类型。
    pub op: FileOp,
    /// `*** Move to:` 给出的目标路径。
    pub rename: Option<String>,
    /// `Add` 是文件初始内容（已去掉 `+` 前缀）；`Update` 是 hunk 体；`Delete` 为 `None`。
    pub body: Option<String>,
}

/// 去掉行尾 `\r`：CRLF 写就的 patch 也能照常解析。
fn strip_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// 解析一个完整的 `*** Begin Patch` envelope。
pub fn parse_envelope(input: &str) -> Result<Vec<PatchEntry>, PatchError> {
    let mut lines: Vec<&str> = input
        .trim()
        .split('\n')
        .map(strip_cr)
        .collect::<Vec<_>>();
    // 容忍 `<<'EOF' ... EOF` 这类 heredoc 包裹
    if lines.len() >= 2
        && matches!(lines[0], "<<EOF" | "<<'EOF'" | "<<\"EOF\"")
        && lines[lines.len() - 1].trim() == "EOF"
    {
        lines = lines[1..lines.len() - 1].to_vec();
    }
    if lines.first().is_none_or(|line| line.trim() != BEGIN_PATCH_MARKER) {
        return Err(PatchError::new(FIRST_LINE_ERROR));
    }
    if lines.last().is_none_or(|line| line.trim() != END_PATCH_MARKER) {
        return Err(PatchError::new(LAST_LINE_ERROR));
    }
    let end = lines.len() - 1;
    let mut index = 1;
    let mut entries = Vec::new();
    while index < end {
        if lines[index].trim().is_empty() {
            index += 1;
            continue;
        }
        let header = lines[index].trim();
        if let Some(path) = header.strip_prefix(ADD_FILE_MARKER) {
            index += 1;
            let mut content = String::new();
            while index < end {
                let Some(line) = lines[index].strip_prefix('+') else {
                    break;
                };
                content.push_str(line);
                content.push('\n');
                index += 1;
            }
            entries.push(PatchEntry {
                path: path.to_owned(),
                op: FileOp::Add,
                rename: None,
                body: Some(content),
            });
            continue;
        }
        if let Some(path) = header.strip_prefix(DELETE_FILE_MARKER) {
            entries.push(PatchEntry {
                path: path.to_owned(),
                op: FileOp::Delete,
                rename: None,
                body: None,
            });
            index += 1;
            continue;
        }
        if let Some(path) = header.strip_prefix(UPDATE_FILE_MARKER) {
            let path = path.to_owned();
            index += 1;
            let mut rename = None;
            if index < end {
                if let Some(destination) = lines[index].strip_prefix(MOVE_TO_MARKER) {
                    rename = Some(destination.to_owned());
                    index += 1;
                }
            }
            let body_start = index;
            while index < end {
                let line = lines[index];
                if FILE_HEADER_MARKERS
                    .iter()
                    .any(|marker| line.starts_with(marker))
                {
                    break;
                }
                index += 1;
            }
            if body_start == index {
                return Err(PatchError::at_line(
                    body_start + 1,
                    format!("Update file hunk for path '{path}' is empty"),
                ));
            }
            entries.push(PatchEntry {
                path,
                op: FileOp::Update,
                rename,
                body: Some(lines[body_start..index].join("\n")),
            });
            continue;
        }
        return Err(PatchError::at_line(
            index + 1,
            format!(
                "'{header}' is not a valid hunk header. Valid hunk headers: '*** Add File: \
                 {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
            ),
        ));
    }
    Ok(entries)
}

// ==========================================
// 解析：hunk
// ==========================================

/// 一个 `@@` 段落。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hunk {
    /// `@@` 里给出的定位上下文（多行用 `\n` 连接）。
    pub context: Option<String>,
    /// `@@ -n` / `@@ line n` 给出的旧文件行号提示（1 起）。
    pub old_start_line: Option<usize>,
    /// 该段落是否包含未改动的上下文行。
    pub has_context_lines: bool,
    /// 期望在文件里出现的旧行。
    pub old_lines: Vec<String>,
    /// 替换后的新行。
    pub new_lines: Vec<String>,
    /// 是否带 `*** End of File`。
    pub is_end_of_file: bool,
}

/// 一行是否是 diff 内容行（以 ` ` / `+` / `-` 开头且不是 unified 元数据）。
fn is_diff_content_line(line: &str) -> bool {
    match line.as_bytes().first() {
        Some(b' ') => true,
        Some(b'+') => !line.starts_with("+++ "),
        Some(b'-') => !line.starts_with("--- "),
        _ => false,
    }
}

fn is_metadata_line(trimmed: &str) -> bool {
    UNIFIED_METADATA_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// 去掉 diff 体里的包裹行/元数据行与末尾空行。
fn normalize_diff(body: &str) -> String {
    let mut lines = body.split('\n').collect::<Vec<_>>();
    while let Some(last) = lines.last().copied() {
        if last.is_empty() || (last.trim().is_empty() && !is_diff_content_line(last)) {
            lines.pop();
        } else {
            break;
        }
    }
    if lines
        .first()
        .is_some_and(|line| matches!(line.trim(), BEGIN_PATCH_MARKER | END_PATCH_MARKER))
    {
        lines.remove(0);
    }
    if lines
        .last()
        .is_some_and(|line| matches!(line.trim(), BEGIN_PATCH_MARKER | END_PATCH_MARKER))
    {
        lines.pop();
    }
    lines
        .into_iter()
        .filter(|line| is_diff_content_line(line) || !is_metadata_line(line.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析 `@@ -a[,b] +c[,d] @@ [context]` 形式的统一 diff 头。
fn parse_unified_header(header: &str) -> Option<(usize, usize, Option<String>)> {
    let rest = header.strip_prefix("@@")?;
    let (body, tail) = rest.split_once("@@")?;
    let mut parts = body.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    if parts.next().is_some() {
        return None;
    }
    let parse = |spec: &str| spec.split(',').next().unwrap_or_default().parse::<usize>().ok();
    let tail = tail.trim();
    Some((
        parse(old)?,
        parse(new)?,
        (!tail.is_empty()).then(|| tail.to_owned()),
    ))
}

/// 解析 `@@ lines 12` / `@@ line 12-14` 形式（末尾允许 `@@`）的行号提示。
fn parse_line_hint(value: &str) -> Option<usize> {
    let value = value.strip_suffix("@@").unwrap_or(value).trim();
    let mut parts = value.split_whitespace();
    let keyword = parts.next()?;
    if !keyword.eq_ignore_ascii_case("line") && !keyword.eq_ignore_ascii_case("lines") {
        return None;
    }
    let numbers = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let start = numbers.split('-').next().unwrap_or_default();
    start.trim().parse::<usize>().ok()
}

/// `@@ top of file` 之类的「文件开头」锚点。
fn is_top_of_file(value: &str) -> bool {
    ["top of file", "start of file", "beginning of file"]
        .iter()
        .any(|anchor| value.eq_ignore_ascii_case(anchor))
}

struct ParsedHunk {
    hunk: Hunk,
    consumed: usize,
}

/// 解析单个 `@@` 段落（`lines[0]` 是段落头或直接是 hunk 首行）。
fn parse_one_hunk(lines: &[&str], line_number: usize) -> Result<ParsedHunk, PatchError> {
    if lines.is_empty() {
        return Err(PatchError::at_line(line_number, "Diff does not contain any lines"));
    }
    let mut contexts = Vec::<String>::new();
    let mut old_start_line = None;

    let header = lines[0];
    let trimmed_header = header.trim_end();
    let is_header = header.starts_with("@@");
    let unified_header = if is_header {
        parse_unified_header(trimmed_header)
    } else {
        None
    };
    let is_empty_context_marker = trimmed_header
        .strip_prefix("@@")
        .and_then(|rest| rest.strip_suffix("@@"))
        .is_some_and(|middle| middle.trim().is_empty());

    let start_index;
    if is_header && (trimmed_header == "@@" || is_empty_context_marker) {
        start_index = 1;
    } else if let Some((old, new, context)) = unified_header {
        if old < 1 || new < 1 {
            return Err(PatchError::at_line(
                line_number,
                "Line numbers in @@ header must be >= 1",
            ));
        }
        if let Some(context) = context {
            contexts.push(context);
        }
        old_start_line = Some(old);
        start_index = 1;
    } else if is_header && trimmed_header.starts_with("@@ ") {
        let context_value = &trimmed_header[3..];
        let trimmed_context = context_value.trim();
        let normalized = trimmed_context
            .strip_prefix("@@")
            .map_or(trimmed_context, str::trim_start);
        if let Some(hint) = parse_line_hint(normalized) {
            if hint < 1 {
                return Err(PatchError::at_line(line_number, "Line hint must be >= 1"));
            }
            old_start_line = Some(hint);
        } else if is_top_of_file(normalized) {
            old_start_line = Some(1);
        } else if !trimmed_context.is_empty() {
            contexts.push(context_value.to_owned());
        }
        start_index = 1;
    } else if is_header {
        let context_value = trimmed_header[2..].trim();
        if !context_value.is_empty() {
            contexts.push(context_value.to_owned());
        }
        start_index = 1;
    } else {
        // 允许段落体直接以 hunk 行开头（无 `@@` 头）
        start_index = 0;
    }

    // 连续的额外 `@@` 行都算定位上下文（例如 `@@ class X` + `@@ def m():`）
    let mut start_index = start_index;
    while start_index < lines.len() {
        let next = lines[start_index];
        if !next.starts_with("@@") {
            break;
        }
        let trimmed = next.trim_end();
        if let Some(nested) = trimmed.strip_prefix("@@ ") {
            if !nested.trim().is_empty() {
                contexts.push(nested.to_owned());
            }
            start_index += 1;
        } else if trimmed == "@@" {
            start_index += 1;
        } else {
            break;
        }
    }
    if start_index >= lines.len() {
        return Err(PatchError::at_line(
            line_number + 1,
            "Hunk does not contain any lines",
        ));
    }

    let mut hunk = Hunk {
        context: (!contexts.is_empty()).then(|| contexts.join("\n")),
        old_start_line,
        ..Hunk::default()
    };
    let mut parsed_lines = 0_usize;
    for (offset, line) in lines.iter().enumerate().skip(start_index) {
        let next_line = lines.get(offset + 1).copied();
        if line.is_empty()
            && parsed_lines > 0
            && next_line.is_some_and(|next| next.trim_start().starts_with("@@"))
        {
            break;
        }
        if !is_diff_content_line(line) && line.trim_end() == EOF_MARKER && line.starts_with(EOF_MARKER)
        {
            if parsed_lines == 0 {
                return Err(PatchError::at_line(
                    line_number + 1,
                    "Hunk does not contain any lines",
                ));
            }
            hunk.is_end_of_file = true;
            parsed_lines += 1;
            break;
        }
        if matches!(line.trim(), "..." | "…") {
            hunk.has_context_lines = true;
            parsed_lines += 1;
            continue;
        }
        match line.as_bytes().first().copied() {
            None => {
                hunk.has_context_lines = true;
                hunk.old_lines.push(String::new());
                hunk.new_lines.push(String::new());
            }
            Some(b' ') => {
                hunk.has_context_lines = true;
                hunk.old_lines.push(line[1..].to_owned());
                hunk.new_lines.push(line[1..].to_owned());
            }
            Some(b'+') => hunk.new_lines.push(line[1..].to_owned()),
            Some(b'-') => hunk.old_lines.push(line[1..].to_owned()),
            _ if !line.starts_with("@@") => {
                hunk.has_context_lines = true;
                hunk.old_lines.push((*line).to_owned());
                hunk.new_lines.push((*line).to_owned());
            }
            _ if parsed_lines == 0 => {
                return Err(PatchError::at_line(
                    line_number + 1,
                    format!(
                        "Unexpected line in hunk: '{line}'. Lines must start with ' ' (context), '+' \
                         (add), or '-' (remove)"
                    ),
                ));
            }
            _ => break,
        }
        parsed_lines += 1;
    }
    if parsed_lines == 0 {
        return Err(PatchError::at_line(
            line_number + start_index,
            "Hunk does not contain any lines",
        ));
    }
    Ok(ParsedHunk {
        hunk,
        consumed: parsed_lines + start_index,
    })
}

/// 一个文件段落体里出现几个文件标记（>1 判定为误用单文件体）。
fn count_file_markers(body: &str) -> usize {
    let mut markers = 0;
    for line in body.split('\n') {
        if is_diff_content_line(line) {
            continue;
        }
        let trimmed = line.trim();
        if FILE_HEADER_MARKERS
            .iter()
            .any(|marker| trimmed.starts_with(marker))
            || trimmed.starts_with("diff --git ")
        {
            markers += 1;
        }
    }
    markers
}

/// 解析 `*** Update File:` 段落体里的所有 hunk。
pub fn parse_hunks(body: &str) -> Result<Vec<Hunk>, PatchError> {
    let marker_count = count_file_markers(body);
    if marker_count > 1 {
        return Err(PatchError::new(format!(
            "Diff contains {marker_count} file markers. Single-file patches cannot contain \
             multi-file markers."
        )));
    }
    let normalized = normalize_diff(body);
    let lines = normalized.split('\n').collect::<Vec<_>>();
    let mut hunks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            index += 1;
            continue;
        }
        if !is_diff_content_line(line) && is_metadata_line(trimmed) {
            index += 1;
            continue;
        }
        if trimmed.starts_with("@@")
            && lines[index + 1..].iter().all(|next| next.trim().is_empty())
        {
            break;
        }
        let parsed = parse_one_hunk(&lines[index..], index + 1)?;
        hunks.push(parsed.hunk);
        index += parsed.consumed;
    }
    Ok(hunks)
}

// ==========================================
// 应用
// ==========================================

/// 段落按旧的 `[start, start+len)` 换成新行。
struct Replacement {
    start: usize,
    old_len: usize,
    new_lines: Vec<String>,
}

/// 逐行精确匹配：返回 `[from, max_start]` 内所有匹配起点。
fn match_indices(lines: &[&str], pattern: &[String], from: usize, eof: bool) -> Vec<usize> {
    if pattern.len() > lines.len() {
        return Vec::new();
    }
    let max_start = lines.len() - pattern.len();
    // `*** End of File`：只认贴着文件末尾的那一处
    let (from, to) = if eof { (max_start, max_start) } else { (from, max_start) };
    if from > to {
        return Vec::new();
    }
    (from..=to)
        .filter(|index| {
            lines[*index..*index + pattern.len()]
                .iter()
                .zip(pattern)
                .all(|(actual, expected)| actual == expected)
        })
        .collect()
}

/// 失败提示：全文件里与期望旧行最像的窗口（行号 0 起 + 相似度）。
fn closest_window(lines: &[&str], pattern: &[String]) -> Option<(usize, f64)> {
    if pattern.is_empty() || pattern.len() > lines.len() {
        return None;
    }
    let max_start = lines.len() - pattern.len();
    let mut best: Option<(usize, f64)> = None;
    for index in 0..=max_start {
        let matched = lines[index..index + pattern.len()]
            .iter()
            .zip(pattern)
            .filter(|(actual, expected)| actual.trim() == expected.trim())
            .count();
        let score = matched as f64 / pattern.len() as f64;
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((index, score));
        }
    }
    best.filter(|(_, score)| *score > 0.0)
}

/// 未找到期望旧行时的报错文案（列出期望内容并给出最接近的位置）。
fn not_found_error(lines: &[&str], path: &str, hunk: &Hunk) -> PatchError {
    let expected = hunk.old_lines.join("\n");
    match closest_window(lines, &hunk.old_lines) {
        Some((index, score)) => PatchError::new(format!(
            "Failed to find expected lines in {path}:\n{expected}\n\nClosest match ({:.0}% similar) \
             near line {}.",
            score * 100.0,
            index + 1
        )),
        None => PatchError::new(format!(
            "Failed to find expected lines in {path}:\n{expected}"
        )),
    }
}

/// 定位 `@@` 上下文：返回上下文最后一行的行号（0 起）。
fn find_context(lines: &[&str], hunk: &Hunk, from: usize, path: &str) -> Result<usize, PatchError> {
    let Some(context) = hunk.context.as_deref() else {
        return Ok(from);
    };
    let parts = context
        .split('\n')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let mut cursor = from;
    for (index, part) in parts.iter().enumerate() {
        // 上下文行允许「前后空白不敏感」地匹配：模型常抄不准缩进
        let candidates = (cursor..lines.len())
            .filter(|line| lines[*line].trim() == *part)
            .collect::<Vec<_>>();
        let last = index + 1 == parts.len();
        match candidates.as_slice() {
            [] => {
                return Err(PatchError::new(format!(
                    "Failed to find context '{}' in {path}",
                    context.replace('\n', " > ")
                )));
            }
            [found] => {
                if last {
                    return Ok(*found);
                }
                cursor = found + 1;
            }
            _ if last => {
                let listed = candidates
                    .iter()
                    .take(5)
                    .map(|line| (line + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(PatchError::new(format!(
                    "Found {} matches for context '{part}' in {path} (lines {listed}).\n\nAdd more \
                     surrounding context or additional @@ anchors to make it unique.",
                    candidates.len()
                )));
            }
            _ => {
                // 中间层级重复出现时取第一处，继续往下定位
                cursor = candidates[0] + 1;
            }
        }
    }
    Ok(cursor)
}

fn compute_replacements(
    lines: &[&str],
    path: &str,
    hunks: &[Hunk],
) -> Result<Vec<Replacement>, PatchError> {
    let mut replacements = Vec::new();
    let mut cursor = 0_usize;
    for hunk in hunks {
        let line_hint = hunk.old_start_line;
        if let Some(hint) = line_hint {
            if hint < 1 {
                return Err(PatchError::new(format!(
                    "Line hint {hint} is out of range for {path} (line numbers start at 1)"
                )));
            }
            // 段内没有任何上下文、也没给 `@@` 时，行号提示直接把游标拉过去
            if hunk.context.is_none() && !hunk.has_context_lines {
                cursor = (hint - 1).min(lines.len());
            }
        }

        let mut anchor = cursor;
        if hunk.context.is_some() {
            anchor = find_context(lines, hunk, cursor, path)?;
            let first_old = hunk.old_lines.first().map(|line| line.trim());
            let final_context = hunk
                .context
                .as_deref()
                .and_then(|context| context.split('\n').next_back())
                .map(str::trim);
            let hierarchical = hunk
                .context
                .as_deref()
                .is_some_and(|context| context.contains('\n') || context.split_whitespace().count() > 2);
            anchor = if first_old == final_context || hierarchical {
                anchor
            } else {
                anchor + 1
            };
        }

        // 纯新增段落：插在锚点之后（无锚点则插在文件末尾）
        if hunk.old_lines.is_empty() {
            let insertion = if hunk.context.is_some() {
                anchor
            } else if let Some(hint) = line_hint {
                if hint < 1 {
                    return Err(PatchError::new(format!(
                        "Line hint {hint} is out of range for insertion in {path} (line numbers \
                         start at 1)"
                    )));
                }
                if hint > lines.len() + 1 {
                    return Err(PatchError::new(format!(
                        "Line hint {hint} is out of range for insertion in {path} (file has {} \
                         lines)",
                        lines.len()
                    )));
                }
                hint - 1
            } else if lines.last() == Some(&"") {
                lines.len() - 1
            } else {
                lines.len()
            };
            replacements.push(Replacement {
                start: insertion,
                old_len: 0,
                new_lines: hunk.new_lines.clone(),
            });
            continue;
        }

        let mut pattern = hunk.old_lines.clone();
        let mut new_lines = hunk.new_lines.clone();
        let mut matches = match_indices(lines, &pattern, anchor, hunk.is_end_of_file);
        if matches.is_empty() && pattern.last().is_some_and(|line| line.is_empty()) {
            // 末行是空上下文行却没匹配上：去掉再试一次
            pattern.pop();
            if new_lines.last().is_some_and(|line| line.is_empty()) {
                new_lines.pop();
            }
            matches = match_indices(lines, &pattern, anchor, hunk.is_end_of_file);
        }
        if matches.is_empty() && anchor != 0 {
            matches = match_indices(lines, &pattern, 0, hunk.is_end_of_file);
        }
        let found = match matches.as_slice() {
            [] => return Err(not_found_error(lines, path, hunk)),
            [found] => *found,
            many => {
                // 行号提示能唯一定位时按提示走（与参考实现一致）
                let hinted = line_hint
                    .map(|hint| hint - 1)
                    .map(|hint| {
                        many.iter()
                            .copied()
                            .filter(|index| index.abs_diff(hint) <= 200)
                            .collect::<Vec<_>>()
                    })
                    .filter(|filtered| filtered.len() == 1)
                    .map(|filtered| filtered[0]);
                let Some(hinted) = hinted else {
                    let listed = many
                        .iter()
                        .take(5)
                        .map(|line| (line + 1).to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(PatchError::new(format!(
                        "Found {} matches for the text in {path} (lines {listed}).\n\nAdd more \
                         surrounding context or additional @@ anchors to make it unique.",
                        many.len()
                    )));
                };
                hinted
            }
        };
        if pattern == new_lines {
            cursor = found + pattern.len();
            continue;
        }
        replacements.push(Replacement {
            start: found,
            old_len: pattern.len(),
            new_lines,
        });
        cursor = found + pattern.len();
    }

    replacements.sort_by_key(|replacement| replacement.start);
    for pair in replacements.windows(2) {
        let [left, right] = pair else { unreachable!() };
        if right.start < left.start + left.old_len {
            let range = |replacement: &Replacement| {
                if replacement.old_len == 0 {
                    format!("{} (insertion)", replacement.start + 1)
                } else {
                    format!(
                        "{}-{}",
                        replacement.start + 1,
                        replacement.start + replacement.old_len
                    )
                }
            };
            return Err(PatchError::new(format!(
                "Overlapping hunks detected in {path} at lines {} and {}. Split hunks or add more \
                 context to avoid overlap.",
                range(left),
                range(right)
            )));
        }
    }
    Ok(replacements)
}

/// 把 hunk 应用到文件内容上，返回新内容。
pub fn apply_hunks(content: &str, path: &str, hunks: &[Hunk]) -> Result<String, PatchError> {
    if hunks.is_empty() {
        return Err(PatchError::new("Diff contains no hunks"));
    }
    let had_newline = content.ends_with('\n');
    let mut lines = content.split('\n').collect::<Vec<_>>();
    let stripped = had_newline && lines.last() == Some(&"");
    if stripped {
        lines.pop();
    }
    let replacements = compute_replacements(&lines, path, hunks)?;
    let mut result = lines.iter().map(|line| (*line).to_owned()).collect::<Vec<_>>();
    for replacement in replacements.iter().rev() {
        result.splice(
            replacement.start..replacement.start + replacement.old_len,
            replacement.new_lines.clone(),
        );
    }
    if stripped {
        result.push(String::new());
    }
    let mut next = result.join("\n");
    if had_newline && !next.ends_with('\n') {
        next.push('\n');
    } else if !had_newline {
        next.truncate(next.trim_end_matches('\n').len());
    }
    Ok(next)
}

/// 成功摘要（与参考实现的 A/M/D 顺序一致）。
pub fn format_summary(operations: &[(FileOp, &str)]) -> String {
    let mut lines = vec![format!(
        "Successfully applied {} file operation(s).",
        operations.len()
    )];
    lines.extend(
        operations
            .iter()
            .map(|(op, path)| format!("{} {path}", op.summary_label())),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Result<Vec<PatchEntry>, PatchError> {
        parse_envelope(input)
    }

    #[test]
    fn parses_heredoc_wrapper() {
        let parsed = parse("<<'EOF'\n*** Begin Patch\n*** Add File: a.txt\n+hello\n*** End Patch\nEOF")
            .unwrap();
        assert_eq!(
            parsed,
            vec![PatchEntry {
                path:   "a.txt".into(),
                op:     FileOp::Add,
                rename: None,
                body:   Some("hello\n".into()),
            }]
        );
    }

    #[test]
    fn requires_envelope_markers() {
        assert_eq!(
            parse("*** Add File: a").unwrap_err().to_string(),
            FIRST_LINE_ERROR
        );
        assert_eq!(
            parse("*** Begin Patch\n*** Add File: a\n+x")
                .unwrap_err()
                .to_string(),
            LAST_LINE_ERROR
        );
    }

    #[test]
    fn rejects_empty_update_section_and_bad_header() {
        assert_eq!(
            parse("*** Begin Patch\n*** Update File: foo.txt\n*** End Patch")
                .unwrap_err()
                .to_string(),
            "Line 3: Update file hunk for path 'foo.txt' is empty"
        );
        assert!(
            parse("*** Begin Patch\n*** Frobnicate File: foo\n*** End Patch")
                .unwrap_err()
                .to_string()
                .starts_with("Line 2: '*** Frobnicate File: foo' is not a valid hunk header.")
        );
    }

    #[test]
    fn parses_move_and_multiple_sections() {
        let parsed = parse(
            "*** Begin Patch\n*** Update File: old/name.txt\n*** Move to: renamed/name.txt\n@@\n-a\n+b\n\
             *** Delete File: gone.txt\n*** Add File: new.txt\n+x\n*** End Patch",
        )
        .unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].rename.as_deref(), Some("renamed/name.txt"));
        assert_eq!(parsed[0].op, FileOp::Update);
        assert_eq!(parsed[1].op, FileOp::Delete);
        assert_eq!(parsed[2].op, FileOp::Add);
        assert_eq!(parsed[2].body.as_deref(), Some("x\n"));
    }

    #[test]
    fn empty_envelope_yields_no_entries() {
        assert!(parse("*** Begin Patch\n*** End Patch").unwrap().is_empty());
    }

    #[test]
    fn hunk_headers_carry_context_lines_and_hints() {
        let hunks = parse_hunks("@@ class BaseClass\n@@ \t def method():\n ctx\n-old\n+new").unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].context.as_deref(), Some("class BaseClass\n\t def method():"));
        assert_eq!(hunks[0].old_lines, vec!["ctx".to_string(), "old".to_string()]);
        assert_eq!(hunks[0].new_lines, vec!["ctx".to_string(), "new".to_string()]);
        assert!(hunks[0].has_context_lines);

        let unified = parse_hunks("@@ -12,3 +14,4 @@ fn main\n-old\n+new").unwrap();
        assert_eq!(unified[0].old_start_line, Some(12));
        assert_eq!(unified[0].context.as_deref(), Some("fn main"));

        let hinted = parse_hunks("@@ lines 5-7\n-old\n+new").unwrap();
        assert_eq!(hinted[0].old_start_line, Some(5));
        assert_eq!(hinted[0].context, None);
    }

    #[test]
    fn hunk_end_of_file_marker() {
        let hunks = parse_hunks("@@\n first\n-second\n+second updated\n*** End of File").unwrap();
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].is_end_of_file);
        assert_eq!(
            hunks[0].old_lines,
            vec!["first".to_string(), "second".to_string()]
        );
    }

    #[test]
    fn end_of_file_marker_anchors_to_tail() {
        let content = "a\nfoo\nb\nfoo\n";
        let hunks = parse_hunks("@@\n-foo\n+bar\n*** End of File").unwrap();
        assert_eq!(apply_hunks(content, "f.txt", &hunks).unwrap(), "a\nfoo\nb\nbar\n");
    }

    #[test]
    fn ambiguous_hunks_are_rejected() {
        let hunks = parse_hunks("@@\n-foo\n+bar").unwrap();
        let error = apply_hunks("foo\nfoo\n", "f.txt", &hunks).unwrap_err();
        assert!(
            error.to_string().contains("Found 2 matches for the text in f.txt"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn context_mismatch_reports_expected_lines() {
        let hunks = parse_hunks("@@\n-missing\n+changed").unwrap();
        let error = apply_hunks("line1\nline2\n", "modify.txt", &hunks).unwrap_err();
        assert!(
            error.to_string().contains("Failed to find expected lines in modify.txt:\nmissing"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn missing_context_anchor_is_rejected() {
        let hunks = parse_hunks("@@ def absent():\n context\n-x\n+y").unwrap();
        let error = apply_hunks("context\nx\n", "f.rs", &hunks).unwrap_err();
        assert!(
            error.to_string().contains("Failed to find context 'def absent():' in f.rs"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn pure_addition_appends_and_pure_deletion_removes() {
        let added = parse_hunks("@@\n+added line 1\n+added line 2").unwrap();
        assert_eq!(
            apply_hunks("line1\nline2\n", "f.txt", &added).unwrap(),
            "line1\nline2\nadded line 1\nadded line 2\n"
        );

        let removed = parse_hunks("@@\n line1\n-line2\n line3").unwrap();
        assert_eq!(
            apply_hunks("line1\nline2\nline3\n", "f.txt", &removed).unwrap(),
            "line1\nline3\n"
        );
    }

    #[test]
    fn unicode_lines_round_trip() {
        let hunks = parse_hunks("@@\n line1\n-naïve café\n+naïve café ✅").unwrap();
        assert_eq!(
            apply_hunks("line1\nnaïve café\nline3\n", "f.txt", &hunks).unwrap(),
            "line1\nnaïve café ✅\nline3\n"
        );
    }

    #[test]
    fn multiple_chunks_in_one_section() {
        let hunks = parse_hunks("@@\n-line2\n+changed2\n@@\n-line4\n+changed4").unwrap();
        assert_eq!(hunks.len(), 2);
        assert_eq!(
            apply_hunks("line1\nline2\nline3\nline4\n", "multi.txt", &hunks).unwrap(),
            "line1\nchanged2\nline3\nchanged4\n"
        );
    }

    #[test]
    fn overlapping_hunks_are_rejected() {
        let hunks = parse_hunks("@@\n-a\n-b\n+x\n@@\n-b\n-c\n+y").unwrap();
        let error = apply_hunks("a\nb\nc\n", "f.txt", &hunks).unwrap_err();
        assert!(
            error.to_string().contains("Overlapping hunks detected in f.txt"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn context_anchor_selects_the_repeated_hunk() {
        // 两处完全相同的 `value = 1`：`@@ class B:` 锚点决定改哪一处
        let content = "class A:\n    value = 1\nclass B:\n    value = 1\n";
        let hunks = parse_hunks("@@ class B:\n-    value = 1\n+    value = 2").unwrap();
        assert_eq!(
            apply_hunks(content, "f.py", &hunks).unwrap(),
            "class A:\n    value = 1\nclass B:\n    value = 2\n"
        );

        // 上下文里同样的行重复出现时，锚点不再唯一 → 拒绝
        let duplicated = "class B:\n    value = 1\nclass B:\n    value = 1\n";
        let error = apply_hunks(duplicated, "f.py", &hunks).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Found 2 matches for context 'class B:' in f.py"),
            "{}",
            error.to_string()
        );
    }

    #[test]
    fn hunks_without_content_are_rejected() {
        // 只有 `@@` 而没有内容行：解析出 0 个 hunk，应用时拒绝（与参考实现一致）
        assert!(parse_hunks("@@").unwrap().is_empty());
        let error = apply_hunks("stable\n", "foo.txt", &[]).unwrap_err();
        assert_eq!(error.to_string(), "Diff contains no hunks");
    }
}
