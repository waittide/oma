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
 * 每条助手消息记的是**它自己那次请求**的用量（含该次请求的整个提示侧），
 * 所以逐条相加即可：一次工具循环产生的多条消息各算一次，正是这些请求的真实总和。
 */
export function sessionUsage(messages: ChatMessage[]): TokenUsage | null {
  const total: TokenUsage = { input_tokens: 0, output_tokens: 0 };
  let seen = false;
  for (const m of messages) {
    if (m.role !== 'assistant' || !m.usage) continue;
    add(total, m.usage);
    seen = true;
  }
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

/**
 * 用量是否真的拿到了数字。
 *
 * 服务端在尚未收到厂商用量时下发全零（追赶快照、尚无请求完成），此时若照常渲染
 * 会得到一行空文案或无意义的 `0`；调用方据此决定显不显示用量行。
 */
export function hasUsage(usage?: TokenUsage | null): boolean {
  if (!usage) return false;
  return (
    usage.input_tokens > 0 ||
    usage.output_tokens > 0 ||
    (usage.cache_read_tokens ?? 0) > 0 ||
    (usage.cache_write_tokens ?? 0) > 0
  );
}

/**
 * 两次请求的用量相加：把一轮里的多次请求累加成整轮合计。
 *
 * 只累计 token 字段；整轮的耗时由界面按墙钟（提问到收尾）给出，不在这里掺和。
 */
export function accumulateUsage(base: TokenUsage | null, addend: TokenUsage | null): TokenUsage | null {
  if (!addend) return base;
  const out: TokenUsage = base ? { ...base } : { input_tokens: 0, output_tokens: 0 };
  add(out, addend);
  return out;
}
