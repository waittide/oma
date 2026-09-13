/**
 * liveSegments 归位逻辑的运行验证。
 *
 * 前端没有测试框架（package.json 无 vitest/jest），因此用 Node 原生类型剥离
 * 直接跑，失败即非零退出码：
 *   node --experimental-strip-types src/lib/liveSegments.check.ts
 */
import {
  findTaskHost,
  foldSegments,
  SubagentHostStack,
  type LiveSegment,
} from '../src/lib/liveSegments.ts';

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

function tool(call_id: string, name: string, done = false, host?: string): LiveSegment {
  return {
    kind: 'tool',
    key: call_id,
    host_call_id: host,
    tool: { call_id, tool_name: name, input: { q: name }, output: done ? 'out' : undefined, done },
  };
}

/**
 * 建一个可后续置为完成的工具段。
 *
 * 真实流程里 `tool_call_finished` 只把已有段的 `done` 置位（chat.ts 对同
 * call_id 去重），**不会**追加第二个段。测试必须同样建模，否则会造出真实
 * 场景不存在的重复段。
 */
function mutableTool(call_id: string, name: string, host?: string) {
  const seg = tool(call_id, name, false, host);
  return {
    seg,
    finish: () => {
      const t = (seg as { tool: { done: boolean; output?: string } }).tool;
      t.done = true;
      t.output = 'out';
    },
  };
}

const text = (key: string, body: string, host?: string): LiveSegment => ({
  kind: 'text',
  key,
  text: body,
  host_call_id: host,
});
const think = (key: string, body: string, host?: string): LiveSegment => ({
  kind: 'thinking',
  key,
  text: body,
  host_call_id: host,
});

// --- 1. 无子代理时行为与改造前一致 ---
eq(
  'top level only',
  foldSegments([think('th1', 'think'), text('tx1', 'hi'), tool('c1', 'shell', true)]),
  [
    { type: 'thinking', thinking: 'think' },
    { type: 'text', text: 'hi' },
    { type: 'tool_use', id: 'c1', name: 'shell', input: { q: 'shell' } },
    { type: 'tool_result', tool_use_id: 'c1', content: 'out', is_error: false },
  ],
);

// --- 2. 子代理内容收进宿主 task 卡片，不占顶层 ---
const host = mutableTool('task1', 'task');
const subRead = mutableTool('c2', 'read', 'task1');
subRead.finish();
const nestedSegs = [
  host.seg,
  think('th2', 'sub think', 'task1'),
  text('tx2', 'sub text', 'task1'),
  subRead.seg,
  tool('c3', 'grep', false, 'task1'),
];
const nested = foldSegments(nestedSegs);
eq('nested into host card (running)', nested, [
  { type: 'tool_use', id: 'task1', name: 'task', input: { q: 'task' } },
  {
    type: 'subagent',
    tool_use_id: 'task1',
    blocks: [
      { type: 'thinking', thinking: 'sub think' },
      { type: 'text', text: 'sub text' },
      { type: 'tool_use', id: 'c2', name: 'read', input: { q: 'read' } },
      { type: 'tool_result', tool_use_id: 'c2', content: 'out', is_error: false },
      { type: 'tool_use', id: 'c3', name: 'grep', input: { q: 'grep' } },
    ],
  },
]);

// 宿主完成后，task 卡片自身出结果，子代理过程仍在卡片内
host.finish();
eq(
  'nested into host card (finished)',
  foldSegments(nestedSegs).map((b) => (b.type === 'subagent' ? 'SUBRUN' : b.type)),
  ['tool_use', 'SUBRUN', 'tool_result'],
);

// 顶层不得残留任何子代理段（核心诉求：不与主 agent 同层级）
eq(
  'no subagent leakage to top level',
  foldSegments(nestedSegs).filter((b) => b.type !== 'subagent'),
  [
    { type: 'tool_use', id: 'task1', name: 'task', input: { q: 'task' } },
    { type: 'tool_result', tool_use_id: 'task1', content: 'out', is_error: false },
  ],
);

// --- 3. 主 agent 在子代理前后的内容保持原顺序 ---
// 真实顺序：任务开始 → 子代理跑 → task 出结果 → 主 agent 继续说
const host3 = mutableTool('t3', 'task');
host3.finish();
eq(
  'order preserved around subagent',
  foldSegments([
    text('a', 'before'),
    host3.seg,
    text('b', 'inner', 't3'),
    text('c', 'after'),
  ]).map((b) => (b.type === 'subagent' ? 'SUBRUN' : b.type)),
  ['text', 'tool_use', 'SUBRUN', 'tool_result', 'text'],
);
// 子代理已结束但宿主还没出结果时，task 卡片依然只有 tool_use
eq(
  'subagent done before host result',
  foldSegments([tool('t4', 'task'), text('d', 'inner', 't4')]).map((b) =>
    b.type === 'subagent' ? 'SUBRUN' : b.type,
  ),
  ['tool_use', 'SUBRUN'],
);

// --- 4. 两个子代理各自归位，不串台 ---
eq(
  'two subagents stay separate',
  foldSegments([
    tool('t1', 'task'),
    text('x', 'in-1', 't1'),
    tool('t2', 'task'),
    text('y', 'in-2', 't2'),
  ]).filter((b) => b.type === 'subagent'),
  [
    { type: 'subagent', tool_use_id: 't1', blocks: [{ type: 'text', text: 'in-1' }] },
    { type: 'subagent', tool_use_id: 't2', blocks: [{ type: 'text', text: 'in-2' }] },
  ],
);

// --- 5. 嵌套子代理（task 再调 task）---
const deep = foldSegments([
  tool('outer', 'task'),
  tool('inner', 'task', false, 'outer'),
  text('z', 'deepest', 'inner'),
]);
const outerCard = deep.find((b) => b.type === 'subagent') as {
  blocks: { type: string; blocks?: unknown[] }[];
};
eq('nested: only outer card at top', deep.filter((b) => b.type === 'subagent').length, 1);
eq('nested: inner task inside outer card', outerCard.blocks[0], {
  type: 'tool_use',
  id: 'inner',
  name: 'task',
  input: { q: 'task' },
});
// 最内层内容必须出现在 inner 卡片的子代理块里，不能丢
eq(
  'nested: deepest folded into inner host',
  outerCard.blocks.filter((b) => b.type === 'subagent'),
  [{ type: 'subagent', tool_use_id: 'inner', blocks: [{ type: 'text', text: 'deepest' }] }],
);

// --- 6. 宿主不存在的孤儿段降级到顶层，不丢内容 ---
eq(
  'orphan content degrades to top level (not dropped)',
  foldSegments([text('o', 'orphan', 'ghost')]),
  [{ type: 'text', text: 'orphan' }],
);

// --- 7. 无子代理内容时不产生空的 subagent 块 ---
eq(
  'no empty subagent block emitted',
  foldSegments([tool('t1', 'task')]).some((b) => b.type === 'subagent'),
  false,
);

// --- 8. findTaskHost 取最近的未完成 task ---
eq(
  'findTaskHost picks latest open task',
  findTaskHost([tool('t1', 'task', true), tool('t2', 'task'), tool('t3', 'shell')]),
  't2',
);
eq('findTaskHost ignores finished task', findTaskHost([tool('t1', 'task', true)]), undefined);
eq('findTaskHost ignores non-task', findTaskHost([tool('t1', 'shell')]), undefined);
eq('findTaskHost empty', findTaskHost([]), undefined);

// --- 9. 真实事件序列（由 e2e-subagent-events.ts 实测抓取）---
// 事件到达顺序：
//   main tool_call_started task
//   sub  turn_started / text_delta / tool_call_started read / tool_call_finished read
//   sub  text_delta / turn_finished
//   main tool_call_finished task
// 这里把该序列按 chat.ts 的规则转成 segments，验证归位结果符合预期。
const e2eHost = mutableTool('call_task', 'task');
const e2eSubTool = mutableTool('call_read', 'read', 'call_task');
e2eSubTool.finish();
e2eHost.finish();
const e2eSegs: LiveSegment[] = [
  e2eHost.seg,
  text('e-sub-1', 'subagent says hi', 'call_task'),
  e2eSubTool.seg,
  text('e-sub-2', 'done', 'call_task'),
];
eq(
  'e2e sequence folds under task card',
  foldSegments(e2eSegs),
  [
    { type: 'tool_use', id: 'call_task', name: 'task', input: { q: 'task' } },
    {
      type: 'subagent',
      tool_use_id: 'call_task',
      blocks: [
        { type: 'text', text: 'subagent says hi' },
        { type: 'tool_use', id: 'call_read', name: 'read', input: { q: 'read' } },
        { type: 'tool_result', tool_use_id: 'call_read', content: 'out', is_error: false },
        { type: 'text', text: 'done' },
      ],
    },
    { type: 'tool_result', tool_use_id: 'call_task', content: 'out', is_error: false },
  ],
);

// --- 10. 宿主栈：推弹必须与子代理起止严格配对 ---
// 回归：找不到宿主时若不占位，内层结束时会把外层宿主弹掉，造成嵌套错位。
{
  const openTask = tool('t1', 'task');
  const stack = new SubagentHostStack();

  stack.push([openTask]);
  eq('stack: 找到宿主时记录它', stack.current(), 't1');

  // 一个找不到宿主的子代理（如宿主已结束）嵌套在内：必须占位
  stack.push([]);
  eq('stack: 无宿主时占位为 undefined', stack.current(), undefined);
  eq('stack: 深度正确', stack.depth, 2);

  stack.pop();
  eq('stack: 内层弹出后回到外层宿主', stack.current(), 't1');
  eq('stack: 深度回到 1', stack.depth, 1);

  stack.pop();
  eq('stack: 全部弹出后为空', stack.current(), undefined);

  // 宿主已完成的 task 不再被认作宿主
  stack.push([tool('t2', 'task', true), tool('t3', 'shell')]);
  eq('stack: 不认已完成 task 为宿主', stack.current(), undefined);
  stack.clear();
  eq('stack: clear 后深度归零', stack.depth, 0);
}

console.log(`\n${passed} passed, ${failed} failed`);
process.exit(failed === 0 ? 0 : 1);
