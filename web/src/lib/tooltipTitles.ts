/**
 * 去掉 tooltip 触发器内部的浏览器原生提示。
 *
 * 组件库的 `UiIconButton` 会把 `label` 同时写成 `aria-label` 与 `title`——`title`
 * 就是浏览器自带的提示框；我们再套一层 `UiTooltip` 时，鼠标停留会先看到主题化的
 * 提示，过一会儿原生的又冒出来，同一个按钮出现两个提示。
 *
 * 组件库不在本仓库（`link:` 依赖），所以统一在应用侧摘掉：只处理
 * `.ui-tooltip-anchor` 内部的 `title`，别处（文件树用 `title` 展示完整路径之类）
 * 保持原样；`aria-label` 不动，无障碍名称不受影响。
 */
const TOOLTIP_ANCHOR = '.ui-tooltip-anchor';

function stripIn(el: Element): void {
  if (el.hasAttribute('title') && el.closest(TOOLTIP_ANCHOR)) el.removeAttribute('title');
  for (const child of el.querySelectorAll('[title]')) {
    if (child.closest(TOOLTIP_ANCHOR)) child.removeAttribute('title');
  }
}

/**
 * 启动清理：先扫一遍已有 DOM，再盯着后续变更。
 *
 * - 属性变更：组件库重新渲染按钮时会再写一次 `title`，这里随即摘掉；摘除本身会再
 *   触发一次属性记录，但那时已经没有 `title`，不会自激。
 * - 新增节点：列表、弹层都会整块插入，只扫新增子树即可。
 */
export function suppressNativeTooltipTitles(): void {
  stripIn(document.body);

  new MutationObserver((records) => {
    for (const record of records) {
      if (record.type === 'attributes') {
        const el = record.target as Element;
        if (el.hasAttribute('title') && el.closest(TOOLTIP_ANCHOR)) el.removeAttribute('title');
        continue;
      }
      for (const node of record.addedNodes) {
        if (node instanceof Element) stripIn(node);
      }
    }
  }).observe(document.body, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ['title'],
  });
}
