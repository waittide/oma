/**
 * apply_patch / unified diff 的行解析。
 *
 * 前端的差异化展示只依赖「每行是什么」这一件事，因此解析逻辑保持纯函数、
 * 与组件解耦，便于用 scripts/patchDiff.check.ts 直接验证。
 */

/** 一行的语义分类 */
export type DiffKind = 'meta' | 'file' | 'hunk' | 'add' | 'del' | 'ctx';

export interface DiffLine {
  kind: DiffKind;
  text: string;
}

/** `edit` 工具结果里 diff 段的起始标记行 */
const UNIFIED_DIFF_MARKER = 'Unified Diff:';

/**
 * 单行分类。
 *
 * 先判 `*** `（apply_patch 指令）再判 `+` / `-`，否则
 * `*** Add File: x` 之类会被当成新增行；`---` / `+++` 是文件头而非删除/新增行，
 * 必须排在 `+` / `-` 之前。
 */
export function classifyDiffLine(line: string): DiffKind {
  if (line.startsWith('*** ')) return 'meta';
  if (line.startsWith('@@')) return 'hunk';
  if (line.startsWith('--- ') || line.startsWith('+++ ')) return 'file';
  if (line.startsWith('+')) return 'add';
  if (line.startsWith('-')) return 'del';
  return 'ctx';
}

/** 把补丁原文或 unified diff 切成分类型的行 */
export function parseDiffLines(text: string): DiffLine[] {
  if (!text) return [];
  return text.split('\n').map((line) => ({ kind: classifyDiffLine(line), text: line }));
}

/**
 * 拆分 `edit` 的结果：摘要与 unified diff 两段。
 *
 * 摘要里含 `- Update: path` 这类以 `-` 开头的列表项，整段丢给行解析会被误判成
 * 删除行，因此必须先把 diff 段切出来再分别渲染。找不到标记时
 * `diff` 为 `null`，调用方按普通文本兜底。
 */
export function splitUnifiedDiff(output: string): { summary: string; diff: string | null } {
  const lines = output.split('\n');
  const marker = lines.findIndex((line) => line.trim() === UNIFIED_DIFF_MARKER);
  if (marker < 0) return { summary: output, diff: null };
  return {
    summary: lines.slice(0, marker).join('\n').trim(),
    diff: lines
      .slice(marker + 1)
      .join('\n')
      .replace(/^\n+/, '')
      .replace(/\s+$/, ''),
  };
}
