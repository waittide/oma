import type { Block } from '../types';

/** 流式轮次里的一个工具调用。 */
export interface LiveTool {
  call_id: string;
  /** 被调用的工具名 */
  tool_name: string;
  input: unknown;
  output?: string;
  is_error?: boolean;
  done: boolean;
}

interface LiveSegmentBase {
  /**
   * 宿主 task 的 call_id；缺省表示属于主 Agent，处于顶层。
   *
   * 子代理事件不带指向父级的引用（`subagent_id` 是 runtime 的 `run_subagent`
   * 现场生成的 uuid），因此由客户端按栈推断归属：主循环与子代理循环都是**串行**
   * 执行工具（runtime 内 `for` + `.await`），子代理事件必然落在父 task 的
   * started / finished 之间。
   */
  host_call_id?: string;
}

export type LiveSegment = LiveSegmentBase &
  (| { kind: 'thinking'; key: string; text: string }
   | { kind: 'text'; key: string; text: string }
   | { kind: 'tool'; key: string; tool: LiveTool });

/** 流式轮次缓冲：按到达顺序排列的实时段（thinking/text/tool 交错）。 */
export interface LiveTurn {
  segments: LiveSegment[];
}

export function emptyLive(): LiveTurn {
  return { segments: [] };
}

/**
 * 把一个工具段展开为 tool_use（完成时追加 tool_result）。
 *
 * 回执必须一起推入：`MessageBlocks` 靠它把卡片从「执行中」翻成终态，
 * 只推 tool_use 会让卡片永远停在执行中。
 */
function pushToolSeg(out: Block[], tool: LiveTool) {
  out.push({ type: 'tool_use', id: tool.call_id, name: tool.tool_name, input: tool.input });
  if (tool.done) {
    out.push({
      type: 'tool_result',
      tool_use_id: tool.call_id,
      content: tool.output ?? '',
      is_error: !!tool.is_error,
    });
  }
}

/** 按宿主 call_id（顶层为 undefined）分组的段，保持组内到达顺序。 */
type SegmentsByHost = Map<string | undefined, LiveSegment[]>;

/**
 * 递归展开某一层：把属于 `host` 的段转成块，遇到 task 工具则把子代理的
 * 段层嵌进去。
 *
 * `seen` 防环：正常情况下宿主关系是一棵树，但数据异常时（如某段自己作为
 * 自己的宿主）不该把渲染拖成死循环，宁可截断也不能卡死界面。
 */
function buildLevel(byHost: SegmentsByHost, host: string | undefined, seen: Set<string>): Block[] {
  const out: Block[] = [];
  for (const seg of byHost.get(host) ?? []) {
    if (seg.kind === 'thinking') {
      out.push({ type: 'thinking', thinking: seg.text });
      continue;
    }
    if (seg.kind === 'text') {
      out.push({ type: 'text', text: seg.text });
      continue;
    }

    const callId = seg.tool.call_id;
    out.push({ type: 'tool_use', id: callId, name: seg.tool.tool_name, input: seg.tool.input });
    // 子代理过程紧跟在宿主 tool_use 之后。用独立分组（而非在 tool_use 与
    // tool_result 之间插队）递归展开，嵌套多少层都不会串台或丢内容
    const inner = byHost.get(callId);
    if (!seen.has(callId) && inner && inner.length > 0) {
      seen.add(callId);
      out.push({ type: 'subagent', tool_use_id: callId, blocks: buildLevel(byHost, callId, seen) });
    }
    if (seg.tool.done) {
      out.push({
        type: 'tool_result',
        tool_use_id: callId,
        content: seg.tool.output ?? '',
        is_error: !!seg.tool.is_error,
      });
    }
  }
  return out;
}

/**
 * 按到达顺序把实时段展开为渲染块，并把子代理内容嵌进其宿主 task 卡片。
 *
 * 子代理段带 `host_call_id`，因此**不会**出现在主层级：它们被收拢成紧随宿主
 * `tool_use` 之后的 `subagent` 块。子代理自身的工具调用也带 `host_call_id`
 * （指向更内层的宿主），所以任意深度的嵌套都能正确归位。
 */
export function foldSegments(segments: LiveSegment[]): Block[] {
  // 存在对应 tool_use 的 call_id 才算有效宿主；否则该段降级到顶层。
  // 字面上不该发生，但「宁可多显示也不能丢内容」——丢内容会让子代理的
  // 工作变成黑盒，而降级平铺只是少了缩进。
  const toolIds = new Set<string>();
  for (const seg of segments) {
    if (seg.kind === 'tool') toolIds.add(seg.tool.call_id);
  }

  const byHost: SegmentsByHost = new Map();
  for (const seg of segments) {
    const host = seg.host_call_id && toolIds.has(seg.host_call_id) ? seg.host_call_id : undefined;
    const list = byHost.get(host);
    if (list) list.push(seg);
    else byHost.set(host, [seg]);
  }
  return buildLevel(byHost, undefined, new Set());
}

/**
 * 找出新子代理的宿主：最近一个尚未完成的 task 工具段。
 *
 * 找不到时返回 undefined：此时该层子代理的内容降级平铺在主层级，
 * 宁可多显示也不丢，好过压入一个错误的宿主把内容藏进不相干的卡片里。
 */
export function findTaskHost(segments: LiveSegment[]): string | undefined {
  for (let i = segments.length - 1; i >= 0; i--) {
    const seg = segments[i]!;
    if (seg.kind === 'tool' && seg.tool.tool_name === 'task' && !seg.tool.done) {
      return seg.tool.call_id;
    }
  }
  return undefined;
}

/**
 * 子代理宿主栈。
 *
 * 子代理事件不带指向父级的引用（`subagent_id` 是 runtime 现场生成的 uuid），
 * 归属只能由客户端推断：主循环与子代理循环都是**串行**执行工具，因此子代理事件
 * 必然落在父 task 的 started / finished 之间——取「当前未完成的 task」即宿主。
 *
 * 用栈而非单值：模板允许 task 再调 task（templates/task.md 的 tools 含 task）。
 * 栈中元素可为 `undefined`，表示该层子代理找不到宿主（内容降级平铺）。**找不到
 * 也必须占位**，因为栈深要与 turn_started / turn_finished 严格配对；否则内层
 * 结束时会把外层的宿主弹掉，造成嵌套错位。
 */
export class SubagentHostStack {
  private readonly stack: (string | undefined)[] = [];

  /** 当前子代理内容应归入的宿主；不在子代理内或宿主未知时为 undefined。 */
  current(): string | undefined {
    return this.stack[this.stack.length - 1];
  }

  /** 子代理轮次开始：记下它的宿主。 */
  push(segments: LiveSegment[]): void {
    this.stack.push(findTaskHost(segments));
  }

  /** 子代理轮次结束：与 push 严格配对地弹出一层。 */
  pop(): void {
    this.stack.pop();
  }

  /** 缓冲重置时必须清空，否则会残留上一轮的宿主。 */
  clear(): void {
    this.stack.length = 0;
  }

  /** 当前深度（测试用）。 */
  get depth(): number {
    return this.stack.length;
  }
}
