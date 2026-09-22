/**
 * 「变更」页树视图的行构建。
 *
 * 只依赖「变更文件清单」与「哪些目录已折叠」两个输入，保持纯函数、与组件解耦，
 * 便于用 scripts/changesTree.check.ts 直接验证；组件只负责渲染与折叠交互。
 */

import type { GitFileChange } from '../types';

/** 目录行 */
export interface ChangeDirRow {
  kind: 'dir';
  /** 目录相对路径，兼作渲染 key 与折叠键 */
  path: string;
  /** 末段名称：树靠层级表达位置，不必重复完整路径 */
  name: string;
  /** 缩进层级，根下的目录与文件为 0 */
  depth: number;
  /** 该目录下（含子孙目录）的变更文件数 */
  count: number;
  collapsed: boolean;
}

/** 文件行 */
export interface ChangeFileRow {
  kind: 'file';
  /** 文件相对工作区的完整路径（点开 diff 用） */
  path: string;
  /** 末段名称 */
  name: string;
  depth: number;
  file: GitFileChange;
}

export type ChangeRow = ChangeDirRow | ChangeFileRow;

/** 每层缩进的像素数 */
export const CHANGE_INDENT_STEP = 12;

/** 行首缩进：目录的展开箭头槽位由 CSS 单独占宽，这里只算层级 */
export function changeRowIndent(row: ChangeRow): number {
  return 6 + row.depth * CHANGE_INDENT_STEP;
}

/** 树中间节点：只保留分组所需的信息 */
interface DirNode {
  path: string;
  dirs: Map<string, DirNode>;
  files: { name: string; file: GitFileChange }[];
  count: number;
}

/**
 * 把变更文件的路径摊平成一棵目录树，再按先序展开成待渲染的行。
 *
 * - 同名目录合并，同一目录下的目录排在文件之前，各自按名称升序，保证多次刷新之间顺序稳定；
 * - 已折叠的目录只输出自身一行、不展开子树，因此渲染侧不必做递归；
 * - `count` 是该目录下（含子孙）的变更文件数，与是否折叠无关——折叠后仍要能看出里面有改动。
 */
export function buildChangeRows(
  files: GitFileChange[],
  collapsed: ReadonlySet<string> = new Set(),
): ChangeRow[] {
  const root: DirNode = { path: '', dirs: new Map(), files: [], count: 0 };

  for (const file of files) {
    // 前导/尾随斜杠与空段一并丢掉，避免出现没有名字的行
    const parts = file.path.split('/').filter(Boolean);
    if (parts.length === 0) continue;
    const name = parts.pop() as string;
    root.count += 1;
    let node = root;
    for (const part of parts) {
      let child = node.dirs.get(part);
      if (!child) {
        child = {
          path: node.path ? `${node.path}/${part}` : part,
          dirs: new Map(),
          files: [],
          count: 0,
        };
        node.dirs.set(part, child);
      }
      child.count += 1;
      node = child;
    }
    node.files.push({ name, file });
  }

  const out: ChangeRow[] = [];
  const walk = (node: DirNode, depth: number) => {
    const dirs = [...node.dirs.values()].sort((a, b) => a.path.localeCompare(b.path));
    const own = [...node.files].sort((a, b) => a.name.localeCompare(b.name));
    for (const dir of dirs) {
      const isCollapsed = collapsed.has(dir.path);
      out.push({
        kind: 'dir',
        path: dir.path,
        name: dir.path.slice(dir.path.lastIndexOf('/') + 1),
        depth,
        count: dir.count,
        collapsed: isCollapsed,
      });
      if (!isCollapsed) walk(dir, depth + 1);
    }
    for (const entry of own) {
      out.push({
        kind: 'file',
        path: entry.file.path,
        name: entry.name,
        depth,
        file: entry.file,
      });
    }
  };
  walk(root, 0);
  return out;
}
