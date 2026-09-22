/**
 * 耗时文案的运行验证。
 *
 * 与 liveSegments.check.ts 同理：前端没有测试框架，用 Node 原生类型剥离直接跑，
 * 失败即非零退出码：
 *   node --experimental-strip-types scripts/format.check.ts
 */
import { formatDuration } from '../src/lib/format.ts';

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

// 10 秒以内保留一位小数，便于看清「刚发生」的那几秒
eq('0 ms', formatDuration(0), '0.0 s');
eq('3200 ms', formatDuration(3200), '3.2 s');
eq('9999 ms', formatDuration(9999), '10.0 s');
// 10 秒到 1 分钟按整秒取整；59999ms 进位后是整分钟，不能显示成「60 s」
eq('10000 ms', formatDuration(10000), '10 s');
eq('59999 ms 进位到分钟', formatDuration(59999), '1 m 00 s');
eq('60000 ms', formatDuration(60000), '1 m 00 s');
eq('125000 ms', formatDuration(125000), '2 m 05 s');
// 超过 1 小时换成时:分；3599999ms 同理不能显示成「59 m 60 s」
eq('3599999 ms 进位到时', formatDuration(3599999), '1 h 00 m');
eq('3600000 ms', formatDuration(3600000), '1 h 00 m');
eq('3735000 ms', formatDuration(3735000), '1 h 02 m');
// 负数（时钟回拨、服务端时间戳异常）按 0 处理，不显示「-1.0 s」
eq('negative', formatDuration(-500), '0.0 s');

if (failed > 0) {
  console.error(`\n${failed} failed, ${passed} passed`);
  process.exit(1);
}
console.log(`${passed} passed, 0 failed`);
