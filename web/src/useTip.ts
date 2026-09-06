import type { Directive } from 'vue';

/**
 * v-tip 指令：自绘提示气泡，替代浏览器原生 title。
 * 用法：v-tip="'提示文本'" 或 data-tip="文本"（值可响应式更新）
 */
let tipEl: HTMLDivElement | null = null;
let showTimer: ReturnType<typeof setTimeout> | null = null;

const tipTexts = new WeakMap<HTMLElement, string>();

function ensureEl(): HTMLDivElement {
  if (!tipEl) {
    tipEl = document.createElement('div');
    tipEl.className = 'oui-tooltip';
    tipEl.setAttribute('role', 'tooltip');
    document.body.appendChild(tipEl);
  }
  return tipEl;
}

function place(el: HTMLElement, tip: HTMLDivElement) {
  const rect = el.getBoundingClientRect();
  const tipRect = tip.getBoundingClientRect();
  let top = rect.bottom + 6;
  let left = rect.left + rect.width / 2 - tipRect.width / 2;
  // 下方空间不足时翻转到上方
  if (top + tipRect.height > window.innerHeight - 4) {
    top = rect.top - tipRect.height - 6;
  }
  left = Math.max(4, Math.min(left, window.innerWidth - tipRect.width - 4));
  tip.style.top = `${top}px`;
  tip.style.left = `${left}px`;
}

function hide() {
  if (showTimer) clearTimeout(showTimer);
  tipEl?.classList.remove('show');
}

function onEnter(this: HTMLElement) {
  const t = tipTexts.get(this) || this.dataset.tip || '';
  if (!t) return;
  const el = this;
  showTimer = setTimeout(() => {
    const tip = ensureEl();
    tip.textContent = t;
    // 先归零再定位，拿到真实尺寸后落位
    tip.style.top = '0px';
    tip.style.left = '0px';
    place(el, tip);
    tip.classList.add('show');
  }, 350);
}

function unbind(el: HTMLElement) {
  hide();
  el.removeEventListener('mouseenter', onEnter);
  el.removeEventListener('mouseleave', hide);
  el.removeEventListener('mousedown', hide);
}

const ariaTexts = new WeakMap<HTMLElement, string>();

function syncAria(el: HTMLElement) {
  const t = tipTexts.get(el) || el.dataset.tip || '';
  if (!t) return;
  const prev = ariaTexts.get(el);
  // 仅设置/更新由本指令写入的 aria-label，不覆盖调用方已有的
  if (!el.hasAttribute('aria-label') || prev === el.getAttribute('aria-label')) {
    el.setAttribute('aria-label', t);
    ariaTexts.set(el, t);
  }
}

export const vTip: Directive<HTMLElement, string | undefined> = {
  mounted(el, binding) {
    if (binding.value !== undefined) tipTexts.set(el, binding.value);
    el.addEventListener('mouseenter', onEnter);
    el.addEventListener('mouseleave', hide);
    el.addEventListener('mousedown', hide);
    syncAria(el);
  },
  updated(el, binding) {
    if (binding.value !== undefined) tipTexts.set(el, binding.value);
    syncAria(el);
  },
  unmounted: unbind,
};
