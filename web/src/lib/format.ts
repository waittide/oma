import MarkdownIt from 'markdown-it';

const md = new MarkdownIt({
  html: false,
  linkify: true,
  breaks: true,
});

/** 渲染 Markdown 为受信 HTML（html:false 屏蔽原始标签）。 */
export function renderMarkdown(src: string): string {
  return md.render(src);
}

/** 简易 JSON 美化，失败时返回原文。 */
export function prettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

/**
 * 耗时文案：`320 ms` / `3.2 s` / `2 m 05 s` / `1 h 02 m`。
 *
 * 单位用固定的 ms/s/m/h（与用户习惯一致，不做本地化）；最小单位是毫秒——
 * 不足 1 秒直接给毫秒（`0.9 s` 会抹掉亚秒级信息，`0.0 s` 更是等于没给），
 * 1 秒到 10 秒保留一位小数，更长的按整秒/整分取整，避免「125 s」这种要心算的读数。
 */
export function formatDuration(ms: number): string {
  const clamped = Math.max(0, ms);
  if (clamped < 1000) return `${Math.round(clamped)} ms`;
  const seconds = clamped / 1000;
  if (seconds < 10) return `${seconds.toFixed(1)} s`;
  // 先归一到整秒再拆时分：否则 59.9s 会显示成「60 s」、59分59.6秒会显示成「59 m 60 s」
  const total = Math.round(seconds);
  if (total < 60) return `${total} s`;
  const minutes = Math.floor(total / 60);
  if (minutes < 60) return `${minutes} m ${String(total % 60).padStart(2, '0')} s`;
  return `${Math.floor(minutes / 60)} h ${String(minutes % 60).padStart(2, '0')} m`;
}
