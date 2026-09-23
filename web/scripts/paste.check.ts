/**
 * 剪贴板附件提取的运行验证。
 *
 * 前端没有测试框架，用 Node 原生类型剥离直接跑，失败即非零退出码：
 *   node --experimental-strip-types scripts/paste.check.ts
 */
import { pastedFiles } from '../src/lib/paste.ts';

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

const png = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'image.png', { type: 'image/png' });
const jpg = new File([new Uint8Array([0xff, 0xd8])], 'photo.jpg', { type: 'image/jpeg' });
const txt = new File(['hello'], 'note.txt', { type: 'text/plain' });

/** items 里的一项：位图项的 kind 是 file，纯文本项是 string */
function item(file: File | null, kind = 'file') {
  return { kind, getAsFile: () => file };
}

/** 只比文件名，File 本身 JSON 化后是空对象，比不出内容 */
const names = (files: File[]) => files.map((f) => f.name);

// 截图：位图只出现在 items 里，files 是空的
eq('截图', names(pastedFiles({ items: [item(png)], files: [] })), ['image.png']);

// 一次粘贴多张图：全部取出，顺序不乱
eq('两张图', names(pastedFiles({ items: [item(png), item(jpg)], files: [] })), ['image.png', 'photo.jpg']);

// files 与 items 是同一批文件时不重复上传
eq('items 优先于 files', names(pastedFiles({ items: [item(png)], files: [png] })), ['image.png']);

// 只有 files（文件管理器里复制的文件，不带位图）
eq('只有 files', names(pastedFiles({ items: [], files: [txt] })), ['note.txt']);

// 纯文本粘贴：items 里是 string，没有文件 → 返回空，调用方不拦默认行为
eq('纯文本粘贴', pastedFiles({ items: [item(null, 'string')], files: [] }), []);

// 非文件项 / 取不出文件：忽略，不产生 undefined
eq('取不出文件', pastedFiles({ items: [item(null)], files: [] }), []);

// 字段缺失或整个剪贴板为空
eq('缺字段', pastedFiles({}), []);
eq('空对象', pastedFiles(null), []);

if (failed > 0) {
  console.error(`\n${failed} failed, ${passed} passed`);
  process.exit(1);
}
console.log(`${passed} passed, 0 failed`);
