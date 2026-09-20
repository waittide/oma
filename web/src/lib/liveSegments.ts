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

export type LiveSegment =
  | { kind: 'thinking'; key: string; text: string }
  | { kind: 'text'; key: string; text: string }
  | { kind: 'tool'; key: string; tool: LiveTool };

/** 流式轮次缓冲：按到达顺序排列的实时段（thinking/text/tool 交错）。 */
export interface LiveTurn {
  segments: LiveSegment[];
}

export function emptyLive(): LiveTurn {
  return { segments: [] };
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
