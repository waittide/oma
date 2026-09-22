import { h, render } from 'vue';
import { LuCheck, LuCopy } from 'vue-icons-plus/lu';
import { toast } from '@waittide/ui';
import { tr } from '../composables/i18n';
import { copyText } from './clipboard';

/**
 * 代码块复制按钮。
 *
 * Markdown 通过 `v-html` 注入，注入的 DOM 里挂不了 Vue 组件（每个流式增量都重新
 * mount 一次代价过高），因此把 vue-icons-plus 的图标在首次使用时渲染成 SVG 字符串
 * 缓存起来，之后按字符串插入 DOM。图标仍是矢量图标，不是文字图标。
 */

/** 复制成功后的图标回退延时（ms） */
const FEEDBACK_MS = 1200;
/** 图标尺寸：与界面里其它小图标同档，默认 24px 塞进按钮里会显得过大 */
const ICON_SIZE = 13;

let copyIcon = '';
let checkIcon = '';

/** 把图标组件渲染成 SVG 字符串并缓存（只做一次）。 */
function iconSvg(component: unknown): string {
  const holder = document.createElement('div');
  render(h(component as never, { size: ICON_SIZE }), holder);
  const svg = holder.innerHTML;
  render(null, holder); // 卸载，避免留下游离组件实例
  return svg;
}

function ensureIcons() {
  if (!copyIcon) copyIcon = iconSvg(LuCopy);
  if (!checkIcon) checkIcon = iconSvg(LuCheck);
}

/**
 * 复制文本。
 *
 * 优先用剪贴板 API；它在非安全上下文（局域网 http 访问）不可用，
 * 此时退回隐藏 textarea + execCommand，否则「复制」在部分部署方式下会静默失效。
 */

/** 点击：复制代码原文并给出图标与通知反馈。 */
async function onCopyClick(btn: HTMLButtonElement) {
  const block = btn.closest('.code-block');
  const code = block?.querySelector('code');
  if (!code) return;

  // 取 textContent 而非 innerHTML：后者会把 &lt; 之类的实体原样复制出去
  const ok = await copyText(code.textContent ?? '');
  if (!ok) {
    toast.error(tr('blocks.copyFailed'));
    return;
  }

  toast.success(tr('blocks.copied'));
  btn.classList.add('copied');
  btn.innerHTML = checkIcon;
  window.setTimeout(() => {
    btn.classList.remove('copied');
    btn.innerHTML = copyIcon;
  }, FEEDBACK_MS);
}

/**
 * 给容器内每个代码块补上「右上角复制按钮」。
 *
 * 幂等：已处理过的块直接跳过，因此可在每次渲染/更新后无脑调用。
 * 返回本次新处理的代码块数量。
 */
export function enhanceCodeBlocks(root: HTMLElement | null | undefined): number {
  if (!root) return 0;
  const blocks = root.querySelectorAll<HTMLPreElement>('pre:not(.code-enhanced)');
  if (blocks.length === 0) return 0;
  ensureIcons();

  blocks.forEach((pre) => {
    pre.classList.add('code-enhanced');
    const wrapper = document.createElement('div');
    wrapper.className = 'code-block';
    pre.replaceWith(wrapper);
    wrapper.appendChild(pre);

    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'code-copy';
    btn.setAttribute('aria-label', tr('blocks.copyCode'));
    btn.title = tr('blocks.copyCode');
    btn.innerHTML = copyIcon;
    btn.addEventListener('click', (e) => {
      e.stopPropagation();
      void onCopyClick(btn);
    });
    wrapper.appendChild(btn);
  });
  return blocks.length;
}
