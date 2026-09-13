/**
 * 端到端验证：真实 daemon + 假 Anthropic SSE 服务，确认子代理事件确实
 * 落在父 task 的 started/finished 之间（前端嵌套归位的前提）。
 *
 * 用法：node scripts/e2e-subagent-events.ts
 * 依赖已编译的 oma 二进制；未编译时自动 cargo build。
 */
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const REPO = new URL('../..', import.meta.url).pathname;
const PORT_DAEMON = 17499;
const PORT_FAKE = 17498;
const TOKEN = 'e2e-token';

let failed = 0;
function check(name: string, cond: boolean, extra = '') {
  if (cond) console.log(`  ok  ${name}`);
  else {
    failed++;
    console.error(`  FAIL ${name} ${extra}`);
  }
}

/** 假 Anthropic 服务：按请求轮次依次返回预设的 SSE 响应。 */
function startFakeProvider() {
  let call = 0;
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      call++;
      const parsed = JSON.parse(body);
      // 主 agent 的工具集含 task，子代理（explore）不含——用这个区分层级，
      // 比看 tool_result 可靠：子代理首次请求是全新上下文，也没有 tool_result
      const toolNames: string[] = (parsed.tools ?? []).map((t: any) => t.name);
      const isMain = toolNames.includes('task');
      const hasToolResult = JSON.stringify(parsed.messages ?? []).includes('tool_result');

      res.writeHead(200, { 'content-type': 'text/event-stream' });
      const send = (ev: string, data: unknown) =>
        res.write(`event: ${ev}\ndata: ${JSON.stringify(data)}\n\n`);

      // 主 agent 首轮：请求调用 task
      // 子代理首轮：先说话，再请求调用 read
      // 其余（含拿到工具结果后的收尾）：纯文本
      if (isMain && !hasToolResult) {
        send('message_start', { type: 'message_start', message: { usage: { input_tokens: 10 } } });
        send('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'call_task', name: 'task', input: {} } });
        send('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: JSON.stringify({ agent: 'explore', prompt: 'look around' }) } });
        send('content_block_stop', { type: 'content_block_stop', index: 0 });
        send('message_delta', { type: 'message_delta', delta: { stop_reason: 'tool_use' } });
        send('message_stop', { type: 'message_stop' });
        res.end();
        return;
      }
      if (!isMain && !hasToolResult) {
        send('message_start', { type: 'message_start', message: { usage: { input_tokens: 10 } } });
        send('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
        send('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'subagent says hi' } });
        send('content_block_stop', { type: 'content_block_stop', index: 0 });
        send('content_block_start', { type: 'content_block_start', index: 1, content_block: { type: 'tool_use', id: 'call_read', name: 'read', input: {} } });
        send('content_block_delta', { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: JSON.stringify({ path: 'Cargo.toml' }) } });
        send('content_block_stop', { type: 'content_block_stop', index: 1 });
        send('message_delta', { type: 'message_delta', delta: { stop_reason: 'tool_use' } });
        send('message_stop', { type: 'message_stop' });
        res.end();
        return;
      }
      send('message_start', { type: 'message_start', message: { usage: { input_tokens: 10 } } });
      send('content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } });
      send('content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'done' } });
      send('content_block_stop', { type: 'content_block_stop', index: 0 });
      send('message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn' } });
      send('message_stop', { type: 'message_stop' });
      res.end();
    });
  });
  return new Promise((resolve) => server.listen(PORT_FAKE, '127.0.0.1', () => resolve(server)));
}

async function main() {
  const fake = await startFakeProvider();
  const home = mkdtempSync(join(tmpdir(), 'oma-e2e-'));
  const configPath = join(home, 'config.toml');
  writeFileSync(configPath, `
default_model = "fake/m1"
default_agent = "task"

[providers.fake]
api_type = "anthropic"
base_url = "http://127.0.0.1:${PORT_FAKE}"
api_key = "x"

[[providers.fake.models]]
id = "m1"
name = "m1"
context_len = 100000
capabilities = ["text_input", "text_output"]
`);

  const bin = join(REPO, 'target/debug/oma');
  const daemon = spawn(bin, ['daemon', '--addr', `127.0.0.1:${PORT_DAEMON}`, '--token', TOKEN, '--config', configPath], {
    env: { ...process.env, HOME: home, XDG_DATA_HOME: join(home, 'data') },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  daemon.stderr.on('data', (d) => process.env.E2E_VERBOSE && process.stderr.write(d));

  // 等服务就绪
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${PORT_DAEMON}/api/server/status`, { headers: { authorization: `Bearer ${TOKEN}` } });
      if (r.ok) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 200));
  }

  // 建会话
  const created = await fetch(`http://127.0.0.1:${PORT_DAEMON}/api/sessions`, {
    method: 'POST',
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json' },
    body: JSON.stringify({ workspace: join(home, 'ws') }),
  });
  const session = await created.json();
  const sessionId = session.session_id ?? session.id;

  // 连 WS（Node 原生 WebSocket）
  const ws = new WebSocket(`ws://127.0.0.1:${PORT_DAEMON}/ws?token=${TOKEN}`);
  const events: { type: string; data?: any }[] = [];

  await new Promise<void>((resolve, reject) => {
    ws.onopen = () => {
      ws.send(JSON.stringify({
        kind: 'connect',
        client_id: 'e2e',
        client_name: 'e2e',
        client_type: 'tui',
        version: '0.1.0',
        session_id: sessionId,
        workspace: join(home, 'ws'),
      }));
      resolve();
    };
    ws.onerror = (e: any) => reject(new Error(`ws error ${e.message ?? e}`));
    ws.onmessage = (m: any) => {
      const msg = JSON.parse(m.data);
      if (msg.kind === 'event') events.push(msg.event);
      else if (process.env.E2E_VERBOSE) console.error('recv', JSON.stringify(msg).slice(0, 200));
    };
  });

  ws.send(
    JSON.stringify({
      kind: 'command',
      command: { type: 'user_input', data: { content: 'go', attachments: [] } },
    }),
  );

  // 等到主轮次结束
  await new Promise<void>((resolve) => {
    const t = setTimeout(resolve, 15000);
    const iv = setInterval(() => {
      if (events.some((e) => e.type === 'turn_finished' && !e.data?.subagent_id)) {
        clearTimeout(t);
        clearInterval(iv);
        resolve();
      }
    }, 100);
  });

  console.log(`\n收到 ${events.length} 个事件\n`);

  const idx = (pred: (e: any) => boolean) => events.findIndex(pred);
  const taskStart = idx((e) => e.type === 'tool_call_started' && e.data?.tool_name === 'task');
  const taskEnd = idx((e) => e.type === 'tool_call_finished' && e.data?.tool_name === 'task');
  const subStart = idx((e) => e.type === 'turn_started' && e.data?.subagent_id);
  const subEnd = idx((e) => e.type === 'turn_finished' && e.data?.subagent_id);
  const subToolStart = idx((e) => e.type === 'tool_call_started' && e.data?.tool_name === 'read');
  const subText = events.find((e) => e.type === 'text_delta' && e.data?.subagent_id);

  check('task 工具已启动', taskStart >= 0);
  check('子代理轮次已开始（带 subagent_id）', subStart >= 0);
  check('子代理内调用了 read', subToolStart >= 0, `subToolStart=${subToolStart}`);
  check('子代理产生了 text_delta', !!subText);
  check('子代理轮次已结束', subEnd >= 0);
  check('task 工具已结束', taskEnd >= 0);
  check(
    '嵌套区间成立：task_start < sub_start < sub_end < task_end',
    taskStart >= 0 && subStart > taskStart && subEnd > subStart && taskEnd > subEnd,
    `taskStart=${taskStart} subStart=${subStart} subEnd=${subEnd} taskEnd=${taskEnd}`,
  );
  check(
    '子代理的工具事件带 subagent_id',
    events[subToolStart]?.data?.subagent_id != null,
  );
  check(
    '主 agent 的工具事件不带 subagent_id',
    events[taskStart]?.data?.subagent_id == null,
  );

  console.log('\n事件序列：');
  for (const e of events) {
    const tag = e.data?.subagent_id ? `sub:${String(e.data.subagent_id).slice(0, 8)} ` : 'main ';
    const name = e.data?.tool_name ?? (e.data?.delta ? JSON.stringify(String(e.data.delta).slice(0, 20)) : '');
    console.log(`  ${tag}${e.type} ${name}`);
  }

  try { ws.close(); } catch {}
  daemon.kill('SIGKILL');
  fake.close();
  console.log(`\n${failed === 0 ? 'PASS' : `${failed} FAILED`}`);
  process.exit(failed === 0 ? 0 : 1);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
