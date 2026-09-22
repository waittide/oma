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

/** 字节数的人类可读形式，与顶栏用量行的缩写保持同一量级。 */
function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

/** 用量行的统一展示：`12.3k 输入 · 678 输出 · 9.0k 缓存读`（无价格字段，不含成本）。
 *
 * 文案与助手消息底部的用量行共用一套 i18n key，两处口径一致；`t` 由调用方注入
 * （ChatView / TopToolbar 各自的 chat 命名空间），纯函数本身不依赖 i18n 上下文。
 */
export function usageLine(
  usage: TokenUsage | null,
  t: (key: string, params?: Record<string, string | number>) => string,
): string {
  if (!usage) return '';
  const parts: string[] = [];
  if (usage.input_tokens) parts.push(t('usageIn', { count: formatTokens(usage.input_tokens) }));
  if (usage.output_tokens) parts.push(t('usageOut', { count: formatTokens(usage.output_tokens) }));
  if (usage.cache_read_tokens) {
    parts.push(t('usageCacheRead', { count: formatTokens(usage.cache_read_tokens) }));
  }
  if (usage.cache_write_tokens) {
    parts.push(t('usageCacheWrite', { count: formatTokens(usage.cache_write_tokens) }));
  }
  return parts.join(' · ');
}
