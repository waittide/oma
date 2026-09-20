/**
 * 会话用量合计与 Markdown 导出的运行验证。
 *
 * 与 liveSegments.check.ts 同理：前端没有测试框架，用 Node 原生类型剥离直接跑，
 * 失败即非零退出码：
 *   node --experimental-strip-types scripts/sessionExport.check.ts
 */
import type { ChatMessage } from '../src/types.ts';
import { sessionToMarkdown, sessionUsage, usageLine } from '../src/lib/sessionExport.ts';

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
    msg('a1', 'assistant', [toolUse('c1', 'bash')], {
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
    msg('a1', 'assistant', [toolUse('c1', 'bash')], {
      usage: { input_tokens: 10, output_tokens: 1 },
    }),
    msg('r1', 'user', [toolResult('c1')]),
    msg('a2', 'assistant', [text('done')], { usage: { input_tokens: 30, output_tokens: 4 } }),
  ]),
  { input_tokens: 30, output_tokens: 4, cache_read_tokens: 0, cache_write_tokens: 0 },
);

// --- 4. 用量行的展示（0 值整段省略，带缩写） ---
eq(
  'usage line',
  usageLine({ input_tokens: 27345, output_tokens: 1578, cache_read_tokens: 21000, cache_write_tokens: 1200 }),
  '27.3k in · 1.6k out · 21.0k cache R · 1.2k cache W',
);
eq('usage line with zeros', usageLine({ input_tokens: 0, output_tokens: 7 }), '7 out');
eq('usage line with null', usageLine(null), '');

// --- 5. 导出：工具回执不写成「用户说的话」，工具调用单独成段 ---
const md = sessionToMarkdown(
  [
    msg('u1', 'user', [text('帮我看下仓库')]),
    msg('a1', 'assistant', [toolUse('c1', 'grep')], {
      model: 'demo/big-model',
      usage: { input_tokens: 100, output_tokens: 20 },
    }),
    msg('r1', 'user', [toolResult('c1')]),
  ],
  { title: '验收会话', workspace: '/tmp/ws' },
);
eq('markdown has title', md.startsWith('# 验收会话'), true);
eq('markdown mentions workspace', md.includes('> Workspace: `/tmp/ws`'), true);
eq('markdown keeps user text once', md.split('帮我看下仓库').length - 1, 1);
eq('markdown has tool section', md.includes('### Tool: grep'), true);
eq('markdown has model line', md.includes('> Model: demo/big-model'), true);
eq('markdown has usage line', md.includes('> 100 in · 20 out'), true);
eq('markdown has no tool result as user text', md.includes('TOOLRESULT-MARKER'), false);

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
