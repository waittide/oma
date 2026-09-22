/**
 * Git 变更状态的语义分类。
 *
 * 只回答「这条变更算什么」，不关心怎么显示——界面把 `added` / `untracked` 一律画成绿色、
 * `deleted` 画成红色，是渲染层的决定。纯函数，见 scripts/gitStatus.check.ts。
 */

import type { GitFileChange } from '../types';

/**
 * 语义状态。
 *
 * `untracked` 与 `added` 都是「新增」，但仍分开：未跟踪是 git 眼里的全新文件，
 * 已跟踪的新增是本次被暂存进来的，文案上可以区分（英文 new / added）。
 */
export type ChangeStatus = 'untracked' | 'added' | 'modified' | 'deleted' | 'renamed';

/**
 * 由 porcelain 的两列状态字符与未跟踪标记得出语义状态。
 *
 * 优先级为「未跟踪 → 新增 → 删除 → 改名 → 修改」：一条变更可能同时带多个字符
 * （如 `RM` 先改名后又被改内容），只取最能说明性质的那一个。
 * 无法识别的状态（合并冲突 `UU`、类型变更 `T` 等）按「修改」处理，
 * 不把原始字符摆到界面上——它们本就该在终端里解决。
 */
export function changeStatus(file: GitFileChange): ChangeStatus {
  if (file.untracked) return 'untracked';
  const codes = `${file.index}${file.worktree}`;
  if (codes.includes('A')) return 'added';
  if (codes.includes('D')) return 'deleted';
  if (codes.includes('R')) return 'renamed';
  return 'modified';
}
