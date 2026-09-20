import type { ChatMessage, TokenUsage } from '../types';

/** 累加用量的可空合并：字段全部可选，缺失按 0 计。 */
function add(into: TokenUsage, add: TokenUsage | null | undefined) {
  if (!add) return;
  into.input_tokens += add.input_tokens ?? 0;
  into.output_tokens += add.output_tokens ?? 0;
  into.cache_read_tokens = (into.cache_read_tokens ?? 0) + (add.cache_read_tokens ?? 0);
  into.cache_write_tokens = (into.cache_write_tokens ?? 0) + (add.cache_write_tokens ?? 0);
}

/**
 * 整会话的 token 合计。
 *
 * 一条用户输入可能产生多条助手消息（工具循环），而每条助手消息记录的都是
 * **本轮到目前为止的累计值**——逐条相加会把同一轮的输入重复计入。因此按
 * 「用户消息切分出的轮次」分组，每轮只取该轮最后一条助手消息的用量。
 */
export function sessionUsage(messages: ChatMessage[]): TokenUsage | null {
  const total: TokenUsage = { input_tokens: 0, output_tokens: 0 };
  let turn: TokenUsage | null = null;
  let seen = false;

  const flush = () => {
    if (turn) {
      add(total, turn);
      seen = true;
    }
    turn = null;
  };

  for (const m of messages) {
    // 工具回执也是 user 角色（见契约），它跟在同一条助手消息之后；
    // 把它当作轮次边界会把同一轮拆成两段，用量于是被少计。
    // 只有真正带内容的用户消息才是新一轮的开始。
    if (m.role === 'user') {
      if (m.content.some((b) => b.type !== 'tool_result')) flush();
      continue;
    }
    if (m.role === 'assistant' && m.usage) turn = m.usage;
  }
  flush();

  return seen ? total : null;
}

/** 字节数的人类可读形式，与状态栏的 token 缩写保持同一量级。 */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** 用量行的统一展示：`12.3k in · 678 out · 9.0k cache R`（无价格字段，不含成本）。 */
export function usageLine(usage: TokenUsage | null): string {
  if (!usage) return '';
  const parts: string[] = [];
  if (usage.input_tokens) parts.push(`${formatTokens(usage.input_tokens)} in`);
  if (usage.output_tokens) parts.push(`${formatTokens(usage.output_tokens)} out`);
  if (usage.cache_read_tokens) parts.push(`${formatTokens(usage.cache_read_tokens)} cache R`);
  if (usage.cache_write_tokens) parts.push(`${formatTokens(usage.cache_write_tokens)} cache W`);
  return parts.join(' · ');
}

function textOf(message: ChatMessage): string {
  return message.content
    .filter((b): b is { type: 'text'; text: string } => b.type === 'text')
    .map((b) => b.text)
    .join('\n')
    .trim();
}

function toolCallsOf(message: ChatMessage): { name: string; input: unknown }[] {
  return message.content
    .filter((b): b is { type: 'tool_use'; id: string; name: string; input: unknown } => b.type === 'tool_use')
    .map((b) => ({ name: b.name, input: b.input }));
}

/**
 * 把会话导出成 Markdown。
 *
 * 纯前端完成（消息已在内存里），不新增后端接口：导出是「看」的延伸，
 * 不该为它在服务端再开一条写路径。
 */
export function sessionToMarkdown(messages: ChatMessage[], meta: { title: string; workspace: string }): string {
  const out: string[] = [`# ${meta.title || 'Session'}`, ''];
  if (meta.workspace) out.push(`> Workspace: \`${meta.workspace}\``, '');

  for (const m of messages) {
    if (m.role === 'user') {
      const text = textOf(m);
      // 工具回执也是 user 角色（见契约），它们不是「用户说的话」
      if (!text) continue;
      out.push('## User', '', text, '');
      continue;
    }

    const text = textOf(m);
    const thinking = m.content
      .filter((b): b is { type: 'thinking'; thinking: string } => b.type === 'thinking')
      .map((b) => b.thinking)
      .join('\n')
      .trim();

    if (text || thinking || m.usage) {
      out.push('## Assistant', '');
      if (m.model) out.push(`> Model: ${m.model}`, '');
      if (thinking) {
        out.push('<details><summary>Thinking</summary>', '', thinking, '', '</details>', '');
      }
      if (text) out.push(text, '');
      const usage = usageLine(m.usage ?? null);
      if (usage) out.push(`> ${usage}`, '');
    }

    for (const call of toolCallsOf(m)) {
      out.push(`### Tool: ${call.name}`, '', '```json', JSON.stringify(call.input, null, 2), '```', '');
    }
  }

  return out.join('\n').replace(/\n{3,}/g, '\n\n');
}

/** 触发浏览器下载（文件名里带上会话标题，便于多份导出区分）。 */
export function downloadText(filename: string, text: string) {
  const blob = new Blob([text], { type: 'text/markdown;charset=utf-8' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // 立刻回收会让部分浏览器来不及开始下载，放到下一轮事件循环
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
