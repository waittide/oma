/**
 * 剪贴板附件提取。
 *
 * 截图这类位图没有文本形态：浏览器只在 paste 事件的 `items` 里给出 `image/png`
 * 条目，不去读它，粘贴截图就什么都不会发生（终端里同样的问题由 TUI 的 `@路径` 承担）。
 *
 * 这里只做「取出文件」这一步纯逻辑，事件绑定与上传留在组件里：结构与 DOM 的
 * `DataTransfer` 兼容但不是它的引用，便于用假数据跑 check 脚本。
 */
export interface ClipboardPayload {
  items?: ArrayLike<{ kind: string; getAsFile(): File | null }> | null;
  files?: ArrayLike<File> | null;
}

/** 粘贴事件里可上传的文件；空数组表示「没有文件」，此时不该拦默认粘贴行为。 */
export function pastedFiles(data: ClipboardPayload | null | undefined): File[] {
  if (!data) return [];
  const fromItems: File[] = [];
  for (const item of Array.from(data.items ?? [])) {
    if (item.kind !== 'file') continue;
    const file = item.getAsFile();
    if (file) fromItems.push(file);
  }
  // 有的浏览器（或同时复制了文件与位图时）files 与 items 是同一批文件，
  // 优先用 items 即可，两边都取会重复上传成两份附件
  if (fromItems.length > 0) return fromItems;
  return Array.from(data.files ?? []);
}
