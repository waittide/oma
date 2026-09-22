/**
 * Git 变更状态分类的运行验证。
 *
 * 前端没有测试框架，用 Node 原生类型剥离直接跑，失败即非零退出码：
 *   node --experimental-strip-types scripts/gitStatus.check.ts
 */
import { changeStatus } from '../src/lib/gitStatus.ts';
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

/** porcelain 的行状态：`index` 是暂存区侧、`worktree` 是工作区侧 */
function s(index: string, worktree: string, untracked = false): GitFileChange {
  return { path: 'a.ts', index, worktree, untracked };
}

// 未跟踪：即便两列都是 `?`，也走 untracked 而不是回落到修改
eq('未跟踪', changeStatus(s('?', '?', true)), 'untracked');
// 未跟踪标记优先于任何状态字符
eq('未跟踪优先', changeStatus(s('A', 'M', true)), 'untracked');

// 单字符
eq('暂存新增', changeStatus(s('A', ' ')), 'added');
eq('工作区删除', changeStatus(s(' ', 'D')), 'deleted');
eq('暂存删除', changeStatus(s('D', ' ')), 'deleted');
eq('暂存改名', changeStatus(s('R', ' ')), 'renamed');
eq('暂存修改', changeStatus(s('M', ' ')), 'modified');
eq('工作区修改', changeStatus(s(' ', 'M')), 'modified');

// 多字符时按优先级取一个：新增 > 删除 > 改名 > 修改
eq('AM 取新增', changeStatus(s('A', 'M')), 'added');
eq('RM 取改名', changeStatus(s('R', 'M')), 'renamed');
eq('MD 取删除', changeStatus(s('M', 'D')), 'deleted');
eq('AD 取新增', changeStatus(s('A', 'D')), 'added');

// 无法识别的状态按修改处理，不把 `UU` / `!!` 之类的原始字符带出界面
eq('合并冲突按修改', changeStatus(s('U', 'U')), 'modified');
eq('已忽略按修改', changeStatus(s('!', '!')), 'modified');
eq('类型变更按修改', changeStatus(s('T', ' ')), 'modified');
eq('空状态按修改', changeStatus(s('', '')), 'modified');

if (failed > 0) {
  console.error(`\n${failed} failed, ${passed} passed`);
  process.exit(1);
}
console.log(`${passed} passed, 0 failed`);
