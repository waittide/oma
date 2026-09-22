/**
 * 「变更」页树视图行构建的运行验证。
 *
 * 前端没有测试框架，用 Node 原生类型剥离直接跑，失败即非零退出码：
 *   node --experimental-strip-types scripts/changesTree.check.ts
 */
import { buildChangeRows, changeRowIndent, CHANGE_INDENT_STEP } from '../src/lib/changesTree.ts';
import type { GitFileChange } from '../src/types.ts';

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

/** 只关心路径与状态的构造器，porcelain 两列对分组无影响 */
function f(path: string, untracked = false): GitFileChange {
  return { path, index: untracked ? '?' : 'M', worktree: untracked ? '?' : ' ', untracked };
}

/** 行摘要：`目录:…/文件:…`，便于断言顺序与层级 */
function shape(rows: ReturnType<typeof buildChangeRows>): string[] {
  return rows.map((r) =>
    r.kind === 'dir' ? `dir:${r.path}@${r.depth}#${r.count}` : `file:${r.path}@${r.depth}`,
  );
}

// 空输入不产生任何行
eq('空清单', buildChangeRows([]), []);

// 根目录下的文件：depth 为 0，名称即路径
eq(
  '根下扁平文件',
  shape(buildChangeRows([f('b.ts'), f('a.ts')])),
  ['file:a.ts@0', 'file:b.ts@0'],
);

// 目录合并 + 先序展开 + 层级：a/b 下的文件 depth 为 2
const nested = buildChangeRows([
  f('a/b/c.ts'),
  f('a/b/d.ts'),
  f('a/e.ts'),
  f('top.ts'),
]);
eq('嵌套顺序', shape(nested), [
  'dir:a@0#3',
  'dir:a/b@1#2',
  'file:a/b/c.ts@2',
  'file:a/b/d.ts@2',
  'file:a/e.ts@1',
  'file:top.ts@0',
]);

// 文件行只带末段名称，完整路径留在 path 上（点开 diff 需要）
const fileRows = nested.filter((r) => r.kind === 'file');
eq('文件名取末段', fileRows.map((r) => r.name), ['c.ts', 'd.ts', 'e.ts', 'top.ts']);
eq('文件路径完整', fileRows.map((r) => r.path), ['a/b/c.ts', 'a/b/d.ts', 'a/e.ts', 'top.ts']);

// 同一目录下目录排在文件之前（a 的子目录 b 先于 a 自己的文件 m.ts）
// 注意整体仍是先序：a/b 的子树插在 a 与下一个兄弟之间，而不是把所有目录排完再排文件
eq(
  '同层目录先于文件',
  shape(buildChangeRows([f('a/b/x.ts'), f('a/m.ts')])),
  ['dir:a@0#2', 'dir:a/b@1#1', 'file:a/b/x.ts@2', 'file:a/m.ts@1'],
);

// 折叠中间目录：子树整体隐藏，但自身 count 不变（折叠也要能看出里面有改动）
eq(
  '折叠 a/b',
  shape(buildChangeRows([f('a/b/c.ts'), f('a/b/d.ts'), f('a/e.ts'), f('top.ts')], new Set(['a/b']))),
  ['dir:a@0#3', 'dir:a/b@1#2', 'file:a/e.ts@1', 'file:top.ts@0'],
);

// 折叠根级目录：其下所有文件都不出现
eq(
  '折叠 a',
  shape(buildChangeRows([f('a/b/c.ts'), f('a/e.ts'), f('top.ts')], new Set(['a']))),
  ['dir:a@0#2', 'file:top.ts@0'],
);

// 折叠标记落在目录路径上，同名末段但不同父目录的目录互不影响
const sameName = buildChangeRows([f('a/src/x.ts'), f('b/src/y.ts')], new Set(['b/src']));
eq('同名目录各自折叠', shape(sameName), [
  'dir:a@0#1',
  'dir:a/src@1#1',
  'file:a/src/x.ts@2',
  'dir:b@0#1',
  'dir:b/src@1#1',
]);

// 未跟踪等状态原样带到文件行，分组不改写来源数据
const untrackedRows = buildChangeRows([f('a/new.ts', true)]);
const untrackedFile = untrackedRows.find((r) => r.kind === 'file');
eq('未跟踪状态保留', untrackedFile?.kind === 'file' && untrackedFile.file.untracked, true);

// 前导与重复斜杠只影响分组用的层级：名称取规整后的末段，
// 而行上的 `path` 原样保留——它要回传给 diff 接口，不能被改写
const messy = buildChangeRows([f('//a//b.ts')]);
eq(
  '多余斜杠：分组用规整值',
  shape(messy).map((s) => s.split('@')[0]),
  ['dir:a', 'file://a//b.ts'],
);
eq('多余斜杠：名称取末段', messy[1].kind === 'file' && messy[1].name, 'b.ts');
eq('多余斜杠：path 原样保留', messy[1].kind === 'file' && messy[1].path, '//a//b.ts');

// 缩进按层级线性增长
const [dirRow, deepRow] = nested;
eq('缩进 根目录', changeRowIndent(dirRow), 6);
eq('缩进 深两层文件', changeRowIndent(nested[2]), 6 + 2 * CHANGE_INDENT_STEP);

if (failed > 0) {
  console.error(`\n${failed} failed, ${passed} passed`);
  process.exit(1);
}
console.log(`${passed} passed, 0 failed`);
