/**
 * 补丁行解析的运行验证。
 *
 * 前端没有测试框架，用 Node 原生类型剥离直接跑，失败即非零退出码：
 *   node --experimental-strip-types scripts/patchDiff.check.ts
 */
import { classifyDiffLine, parseDiffLines, splitUnifiedDiff } from '../src/lib/patchDiff.ts';

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

// apply_patch 指令行不能被 `+` / `-` 规则抢先命中
eq('meta', classifyDiffLine('*** Begin Patch'), 'meta');
eq('add file 不是新增行', classifyDiffLine('*** Add File: a.rs'), 'meta');
eq('update file', classifyDiffLine('*** Update File: a.rs'), 'meta');
eq('hunk', classifyDiffLine('@@ -1,3 +1,3 @@'), 'hunk');
// 文件头（`---` / `+++`）必须先于删除/新增判定
eq('old file header', classifyDiffLine('--- a/src/main.rs'), 'file');
eq('new file header', classifyDiffLine('+++ b/src/main.rs'), 'file');
eq('deleted line', classifyDiffLine('-old'), 'del');
eq('added line', classifyDiffLine('+new'), 'add');
eq('context line', classifyDiffLine(' fn main() {}'), 'ctx');
eq('空行', classifyDiffLine(''), 'ctx');

// 逐行解析保持行数与顺序
const parsed = parseDiffLines('@@ -1 +1 @@\n-old\n+new');
eq('行数', parsed.length, 3);
eq('类型序列', parsed.map((l) => l.kind), ['hunk', 'del', 'add']);
eq('空文本', parseDiffLines(''), []);

// 摘要里的 `- Update: x` 不能被当成 diff，必须整段切开
const output = [
  'Successfully applied 1 file operation(s).',
  '- Update: src/main.rs',
  '',
  'Unified Diff:',
  '--- a/src/main.rs',
  '+++ b/src/main.rs',
  '@@ -1 +1 @@',
  '-old',
  '+new',
].join('\n');
const split = splitUnifiedDiff(output);
eq('摘要保留列表项', split.summary, 'Successfully applied 1 file operation(s).\n- Update: src/main.rs');
eq(
  'diff 段去掉前导空行',
  split.diff,
  '--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new',
);

// 无标记时回落到纯文本
const plain = splitUnifiedDiff('Command exited with code 1');
eq('无标记 summary', plain.summary, 'Command exited with code 1');
eq('无标记 diff', plain.diff, null);

if (failed > 0) {
  console.error(`\n${failed} failed, ${passed} passed`);
  process.exit(1);
}
console.log(`${passed} passed, 0 failed`);
