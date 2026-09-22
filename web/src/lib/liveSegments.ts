import type { Block, TokenUsage } from '../types';

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

export type LiveSegment =
  | { kind: 'thinking'; key: string; text: string }
  | { kind: 'text'; key: string; text: string }
  | { kind: 'tool'; key: string; tool: LiveTool };

/** 流式轮次缓冲：按到达顺序排列的实时段（thinking/text/tool 交错）。 */
export interface LiveTurn {
  segments: LiveSegment[];
  /**
   * 产出本轮时生效的模型（`provider/model`）。
   *
   * 与持久化消息的 `model` 同一来源（轮次开始时会话的当前模型），供流式消息
   * 显示模型标签——不必等回读，也不受回读期间切换模型的影响。
   */
  model: string;
  /**
   * 本轮累计用量（**到目前为止**，与落库到助手消息的口径一致）。
   *
   * 由服务端在每次模型请求结束后下发（`usage_updated`），因此流式期间就能显示
   * 输入/输出；整轮结束时被最终值覆盖。
   */
  usage: TokenUsage | null;
}

export function emptyLive(): LiveTurn {
  return { segments: [], model: '', usage: null };
}

/** 按到达顺序把实时段展开为渲染块。 */
export function foldSegments(segments: LiveSegment[]): Block[] {
  const out: Block[] = [];
  for (const seg of segments) {
    if (seg.kind === 'thinking') {
      out.push({ type: 'thinking', thinking: seg.text });
      continue;
    }
    if (seg.kind === 'text') {
      out.push({ type: 'text', text: seg.text });
      continue;
    }
    out.push({
      type: 'tool_use',
      id: seg.tool.call_id,
      name: seg.tool.tool_name,
      input: seg.tool.input,
    });
    if (seg.tool.done) {
      out.push({
        type: 'tool_result',
        tool_use_id: seg.tool.call_id,
        content: seg.tool.output ?? '',
        is_error: !!seg.tool.is_error,
      });
    }
  }
  return out;
}
