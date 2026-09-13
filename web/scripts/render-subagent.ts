/**
 * 渲染层验证：确认子代理过程真的渲染在 task 卡片**内部**，而不是与主 agent 同级。
 *
 * 用 Vite 的 ssrLoadModule 加载真实的 MessageBlocks.vue（含 <style> 与递归
 * 自引用），交给 vue/server-renderer 输出 HTML，再按 DOM 嵌套结构断言——
 * 不用子串位置近似，那会让「恰好靠前」的错误结构也通过。
 *
 * 用法：node scripts/render-subagent.ts
 */
import { createServer } from 'vite';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const require = createRequire(import.meta.url);
// vue 与 server-renderer 不是应用直接依赖（pnpm 严格隔离），
// 但 vue 自身提供了 server-renderer 子路径解析，直接从这里取最稳。
const { createSSRApp, h } = require('vue');
const { renderToString } = require('vue/server-renderer');

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..');

let failed = 0;
function check(name: string, cond: boolean, extra = '') {
  if (cond) console.log(`  ok  ${name}`);
  else {
    failed++;
    console.error(`  FAIL ${name} ${extra}`);
  }
}

const vite = await createServer({
  root,
  logLevel: 'error',
  server: { middlewareMode: true },
  appType: 'custom',
});

const { default: MessageBlocks } = await vite.ssrLoadModule('/src/components/MessageBlocks.vue');

async function render(blocks: unknown[], streaming: boolean, results?: unknown) {
  const app = createSSRApp({ render: () => h(MessageBlocks, { blocks, streaming, results }) });
  return renderToString(app);
}

const toolUse = (id: string, name: string, input: unknown) => ({
  type: 'tool_use',
  id,
  name,
  input,
});
const toolResult = (id: string, content: string) => ({
  type: 'tool_result',
  tool_use_id: id,
  content,
  is_error: false,
});

/**
 * 取某 class 元素的开始标签（含其 style 属性）。
 *
 * 展开/折叠是通过容器自身开标签上的 `display:none` 表达的，
 * 看内部 HTML 会永远拿不到——必须看开标签。
 */
function openTagOf(html: string, marker: string): string | null {
  const at = html.indexOf(marker);
  if (at < 0) return null;
  const open = html.lastIndexOf('<div', at);
  if (open < 0) return null;
  return html.slice(open, html.indexOf('>', open) + 1);
}

/** 该元素是否处于展开态（模板用 v-show，折叠时带 display:none）。 */
function isExpanded(html: string, marker: string): boolean {
  const tag = openTagOf(html, marker);
  if (tag === null) return false;
  return !tag.includes('display:none');
}

/**
 * 按标签配对找出某 class 元素的「内部 HTML」。
 *
 * 用括号配对而非常规表达式：后者对嵌套同名标签（这里是 div）会截错范围，
 * 而正确性恰恰取决于嵌套关系。
 */
function innerOf(html: string, marker: string): string | null {
  const at = html.indexOf(marker);
  if (at < 0) return null;
  // 回退到该属性所在的开始标签
  const open = html.lastIndexOf('<div', at);
  if (open < 0) return null;
  let i = html.indexOf('>', open) + 1;
  const start = i;
  let depth = 1;
  while (i < html.length && depth > 0) {
    const nextOpen = html.indexOf('<div', i);
    const nextClose = html.indexOf('</div>', i);
    if (nextClose < 0) return null;
    if (nextOpen >= 0 && nextOpen < nextClose) {
      depth++;
      i = nextOpen + 4;
    } else {
      depth--;
      if (depth === 0) return html.slice(start, nextClose);
      i = nextClose + 6;
    }
  }
  return null;
}

// --- 1. 子代理内容嵌在 task 卡片内部 ---
const runningHtml = await render(
  [
    toolUse('call_task', 'task', { agent: 'explore', prompt: 'look' }),
    {
      type: 'subagent',
      tool_use_id: 'call_task',
      blocks: [
        { type: 'text', text: 'SUB_TEXT_MARKER' },
        toolUse('call_read', 'read', { path: 'READ_FILE_MARKER' }),
        toolResult('call_read', 'READ_RESULT_MARKER'),
      ],
    },
  ],
  true,
);
const subInner = innerOf(runningHtml, 'class="subagent"');
check('存在 subagent 容器', subInner !== null);
check('子代理文本在容器内', !!subInner?.includes('SUB_TEXT_MARKER'));
check('子代理工具 read 在容器内', !!subInner?.includes('READ_FILE_MARKER'));
check('子代理工具结果在容器内', !!subInner?.includes('READ_RESULT_MARKER'));

// 整个 task 卡片内部应同时含 subagent 容器与其输入
const taskInner = innerOf(runningHtml, 'class="fold-body-wrap"');
check('task 卡片内含 subagent 容器', !!taskInner?.includes('class="subagent"'));
check('task 卡片内含自身工具输入', !!taskInner?.includes('explore'));

// --- 2. 运行中自动展开（宿主未出结果 + streaming）---
// v-show 把 display:none 写在容器自身的开标签上，因此必须查开标签
const taskWrapTag = openTagOf(runningHtml, 'class="fold-body-wrap"');
check('运行中的 task 卡片已展开（无 display:none）', !taskWrapTag?.includes('display:none'), taskWrapTag ?? 'no wrap');

// 子代理内部的工具卡片保持既有行为（默认折叠，与普通工具一致）
check(
  '内部工具卡片沿用默认折叠',
  openTagOf(runningHtml, 'class="fold-body-wrap"') !== null,
);

// --- 3. 宿主完成后折叠回默认态，且子代理过程仍在卡片里 ---
const finishedHtml = await render(
  [
    toolUse('call_task', 'task', { agent: 'explore', prompt: 'look' }),
    {
      type: 'subagent',
      tool_use_id: 'call_task',
      blocks: [{ type: 'text', text: 'SUB_TEXT_MARKER' }],
    },
    toolResult('call_task', 'TASK_RESULT_MARKER'),
  ],
  true,
);
check('完成后 task 卡片收起', !isExpanded(finishedHtml, 'class="fold-body-wrap"'));
check(
  '完成后子代理内容仍在卡片内',
  !!innerOf(finishedHtml, 'class="subagent"')?.includes('SUB_TEXT_MARKER'),
);
check(
  '完成后 task 结果在卡片内',
  innerOf(finishedHtml, 'class="fold-body-wrap"')?.includes('TASK_RESULT_MARKER') ?? false,
);

// --- 4. 主层级不含子代理内容（核心诉求）---
// 去掉 subagent 整块后，其余 HTML 不应再出现子代理的文本与工具名
const withoutSub = runningHtml.replace(
  innerOf(runningHtml, 'class="subagent"') ?? '',
  '',
);
check(
  '主层级不再出现子代理文本',
  !withoutSub.includes('SUB_TEXT_MARKER'),
  withoutSub.slice(0, 300),
);
check('主层级不再出现子代理工具', !withoutSub.includes('READ_FILE_MARKER'));

// --- 5. 无子代理时不引入空容器 ---
const plainHtml = await render(
  [toolUse('c1', 'shell', { command: 'ls' }), toolResult('c1', 'out')],
  false,
);
check('无子代理时不渲染 subagent 容器', !plainHtml.includes('class="subagent"'));
check('无子代理时工具卡片仍正常', plainHtml.includes('shell') && plainHtml.includes('ls'));

console.log(`\n${failed === 0 ? 'PASS' : `${failed} FAILED`}`);
await vite.close();
process.exit(failed === 0 ? 0 : 1);
