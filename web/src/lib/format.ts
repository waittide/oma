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
