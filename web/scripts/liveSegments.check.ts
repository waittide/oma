/**
 * liveSegments 展开逻辑的运行验证。
 *
 * 前端没有测试框架（package.json 无 vitest/jest），因此用 Node 原生类型剥离
 * 直接跑，失败即非零退出码：
 *   node --experimental-strip-types scripts/liveSegments.check.ts
 */
import { foldSegments, type LiveSegment } from '../src/lib/liveSegments.ts';

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

function tool(call_id: string, name: string, done = false): LiveSegment {
  return {
    kind: 'tool',
    key: call_id,
    tool: { call_id, tool_name: name, input: { q: name }, output: done ? 'out' : undefined, done },
  };
}

const text = (key: string, body: string): LiveSegment => ({ kind: 'text', key, text: body });
const think = (key: string, body: string): LiveSegment => ({ kind: 'thinking', key, text: body });

// --- 1. thinking / text / tool 按到达顺序展开 ---
eq(
  'ordered segments',
  foldSegments([think('th1', 'think'), text('tx1', 'hi'), tool('c1', 'shell', true)]),
  [
    { type: 'thinking', thinking: 'think' },
    { type: 'text', text: 'hi' },
    { type: 'tool_use', id: 'c1', name: 'shell', input: { q: 'shell' } },
    { type: 'tool_result', tool_use_id: 'c1', content: 'out', is_error: false },
  ],
);

// --- 2. 未完成的工具只出 tool_use，不出 tool_result ---
eq('running tool has no result', foldSegments([tool('c2', 'read')]), [
  { type: 'tool_use', id: 'c2', name: 'read', input: { q: 'read' } },
]);

// --- 3. 空输入产生空输出 ---
eq('empty segments', foldSegments([]), []);

// --- 4. 连续同类段保持顺序 ---
eq(
  'consecutive text kept in order',
  foldSegments([text('a', 'before'), text('b', 'after')]),
  [
    { type: 'text', text: 'before' },
    { type: 'text', text: 'after' },
  ],
);

// --- 5. 思考段耗时（服务端 `thinking_finished` 下发）随块带出，供折叠头显示 ---
eq(
  'thinking duration carried through',
  foldSegments([{ kind: 'thinking', key: 'th2', text: 'think', durationMs: 318 }]),
  [{ type: 'thinking', thinking: 'think', duration_ms: 318 }],
);

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
