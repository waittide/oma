/**
 * 工作区刷新信号的运行验证。
 *
 * 前端没有测试框架，用 Node 原生类型剥离直接跑，失败即非零退出码：
 *   node --experimental-strip-types scripts/workspaceSync.check.ts
 */
import {
  COALESCE_MS,
  markToolFinished,
  mutatesWorkspace,
  workspaceRevision,
} from '../src/stores/workspaceSync.ts';

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

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
/** 等过合并窗口，多留一点余量避免边界抖动 */
const settle = () => sleep(COALESCE_MS + 60);

// 写类工具与只读工具的分界
eq('read 不改工作区', mutatesWorkspace('read'), false);
eq('write 改工作区', mutatesWorkspace('write'), true);
eq('edit 改工作区', mutatesWorkspace('edit'), true);
// shell 是 agent 提交代码 / 生成产物的唯一途径，宁可多取一次
eq('shell 改工作区', mutatesWorkspace('shell'), true);
// 未知工具名不触发，避免将来新增只读工具时被误判
eq('未知工具不触发', mutatesWorkspace('mcp__x__y'), false);

// 只读工具不产生刷新
const before = workspaceRevision.value;
markToolFinished('read');
await settle();
eq('read 不触发刷新', workspaceRevision.value, before);

// 写类工具产生一次刷新
markToolFinished('write');
eq('刷新前不增长', workspaceRevision.value, before);
await settle();
eq('write 触发一次刷新', workspaceRevision.value, before + 1);

// 合并窗口：连续多条写类工具的完成事件只换来一次刷新
const burst = workspaceRevision.value;
markToolFinished('write');
markToolFinished('edit');
markToolFinished('shell');
markToolFinished('read');
await settle();
eq('一轮内多次改动合并为一次', workspaceRevision.value, burst + 1);

// 窗口结束后再改动，重新计一次
markToolFinished('edit');
await settle();
eq('窗口后再改动重新触发', workspaceRevision.value, burst + 2);

if (failed > 0) {
  console.error(`\n${failed} failed, ${passed} passed`);
  process.exit(1);
}
console.log(`${passed} passed, 0 failed`);
