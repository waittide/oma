import { ref } from 'vue';

/**
 * 工作区文件数据的刷新信号。
 *
 * 「文件」与「变更」两页取的都是磁盘快照，服务端不推送它们的变化：用户在终端里
 * commit、agent 用工具改了文件，界面都无从得知（历史树走 WebSocket 事件，是实时的）。
 * 这里把「可以重取了」收敛成一条信号，由需要的一方订阅，避免各处自写一套触发逻辑。
 */

/** 自增即代表「工作区数据可能已变化」；消费方 `watch` 它后自行重取 */
export const workspaceRevision = ref(0);

/**
 * 合并窗口（毫秒）。
 *
 * 一轮对话里连续改十个文件，完成事件是逐条到达的，不该换来十次 `git status`。
 * 窗口内的多次声明合并为一次；从**首次**声明起算、不做尾随顺延——否则持续不断的
 * 改动会让刷新一直饿着，面板永远追不上。
 */
export const COALESCE_MS = 400;

let pending: ReturnType<typeof setTimeout> | null = null;

/** 声明工作区数据可能已变化（合并窗口内的重复声明会被丢掉） */
export function markWorkspaceDirty(): void {
  if (pending !== null) return;
  pending = setTimeout(() => {
    pending = null;
    workspaceRevision.value += 1;
  }, COALESCE_MS);
}

/** 会改动工作区磁盘的工具；`read` 只读，不必触发重取 */
const MUTATING_TOOLS = new Set(['write', 'edit', 'shell']);

/** 该工具是否会改动工作区（决定工具结束后要不要重取文件与变更） */
export function mutatesWorkspace(toolName: string): boolean {
  return MUTATING_TOOLS.has(toolName);
}

/**
 * 工具执行结束后声明工作区可能已变化。
 *
 * 只认写类工具，但 `shell` 也计入：它是 agent 提交代码、生成产物的唯一途径，
 * 宁可多取一次也不漏掉。反复调用的代价由合并窗口吸收。
 */
export function markToolFinished(toolName: string): void {
  if (mutatesWorkspace(toolName)) markWorkspaceDirty();
}
