/**
 * 会话用量合计的运行验证。
 *
 * 与 liveSegments.check.ts 同理：前端没有测试框架，用 Node 原生类型剥离直接跑，
 * 失败即非零退出码：
 *   node --experimental-strip-types scripts/sessionUsage.check.ts
 */
import type { ChatMessage } from '../src/types.ts';
import { sessionUsage, usageLine } from '../src/lib/sessionUsage.ts';

let failed = 0;
let passed = 0;

function eq(name: string, actual: unknown, expected: unknown) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a === e) passed++;
  else {
    failed++;
    console.error(`FAIL: ${name}\n  expected ${e}\n  actual   ${a}`);
  }
}

function msg(
  id: string,
  role: 'user' | 'assistant',
  content: ChatMessage['content'],
  extra: { model?: string; usage?: ChatMessage['usage'] } = {},
): ChatMessage {
  return { id, parent_id: null, role, content, created_at: 0, ...extra };
}

const text = (t: string): ChatMessage['content'][number] => ({ type: 'text', text: t });
const toolResult = (id: string): ChatMessage['content'][number] => ({
  type: 'tool_result',
  tool_use_id: id,
  content: 'TOOLRESULT-MARKER',
  is_error: false,
});
const toolUse = (id: string, name: string): ChatMessage['content'][number] => ({
  type: 'tool_use',
  id,
  name,
  input: { command: 'ls' },
});

// --- 1. 同一轮的多条助手消息各自带着「本轮累计值」，只能计一次 ---
eq(
  'usage counted once per turn',
  sessionUsage([
    msg('u1', 'user', [text('hi')]),
    msg('a1', 'assistant', [toolUse('c1', 'shell')], {
      usage: { input_tokens: 100, output_tokens: 10 },
    }),
    msg('r1', 'user', [toolResult('c1')]),
    msg('a2', 'assistant', [text('done')], { usage: { input_tokens: 150, output_tokens: 40 } }),
    msg('u2', 'user', [text('again')]),
    msg('a3', 'assistant', [text('ok')], { usage: { input_tokens: 20, output_tokens: 5 } }),
  ]),
  { input_tokens: 170, output_tokens: 45, cache_read_tokens: 0, cache_write_tokens: 0 },
);

// --- 2. 没有任何用量数据时返回 null（顶栏不展示该段） ---
eq('no usage -> null', sessionUsage([msg('u1', 'user', [text('hi')])]), null);

// --- 3. 工具回执（user 角色）不切轮次：它跟在同一条助手消息后面 ---
eq(
  'tool result does not split the turn',
  sessionUsage([
    msg('u1', 'user', [text('hi')]),
    msg('a1', 'assistant', [toolUse('c1', 'shell')], {
      usage: { input_tokens: 10, output_tokens: 1 },
    }),
    msg('r1', 'user', [toolResult('c1')]),
    msg('a2', 'assistant', [text('done')], { usage: { input_tokens: 30, output_tokens: 4 } }),
  ]),
  { input_tokens: 30, output_tokens: 4, cache_read_tokens: 0, cache_write_tokens: 0 },
);

// --- 4. 用量行的展示（0 值整段省略，带缩写，文案走 i18n） ---
// 与 ChatView 里逐条消息的用量行共用同一套 key，这里用英文桩函数对照
const en = (key: string, params?: Record<string, string | number>) =>
  ({
    usageIn: `${params?.count} in`,
    usageOut: `${params?.count} out`,
    usageCacheRead: `${params?.count} cache read`,
    usageCacheWrite: `${params?.count} cache write`,
  })[key] ?? key;

eq(
  'usage line',
  usageLine(
    { input_tokens: 27345, output_tokens: 1578, cache_read_tokens: 21000, cache_write_tokens: 1200 },
    en,
  ),
  '27.3k in · 1.6k out · 21.0k cache read · 1.2k cache write',
);
eq('usage line with zeros', usageLine({ input_tokens: 0, output_tokens: 7 }, en), '7 out');
eq('usage line with null', usageLine(null, en), '');
// 中文词序与英文不同（数字在前、单位在后），确保插值没写死在字符串里
eq(
  'usage line zh',
  usageLine({ input_tokens: 12000, output_tokens: 800 }, (key, params) =>
    key === 'usageIn' ? `${params?.count} 输入` : `${params?.count} 输出`,
  ),
  '12.0k 输入 · 800 输出',
);

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
