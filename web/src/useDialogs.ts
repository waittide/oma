import { ref, shallowRef } from 'vue';

/**
 * 自绘对话框服务：替代浏览器原生 confirm / prompt / alert。
 * 单一状态机由 App.vue 挂载的 OuiDialogs 渲染。
 */

export interface DialogRequest {
  kind: 'confirm' | 'prompt' | 'alert';
  title: string;
  message: string;
  defaultValue?: string;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
}

type Resolve = (value: boolean | string | null) => void;

export const dialogRequest = shallowRef<DialogRequest | null>(null);
export const dialogVisible = ref(false);

let resolveCurrent: Resolve | null = null;

function open(req: DialogRequest): Promise<boolean | string | null> {
  // 前一请求未完成时直接拒绝，避免挂起 Promise 泄漏
  resolveCurrent?.(null);
  dialogRequest.value = req;
  dialogVisible.value = true;
  return new Promise<boolean | string | null>((resolve) => {
    resolveCurrent = resolve;
  });
}

export function closeDialog(value: boolean | string | null) {
  dialogVisible.value = false;
  const r = resolveCurrent;
  resolveCurrent = null;
  // 保留 request 供退场动画渲染，动画结束后由组件清空
  r?.(value);
}

export function uiConfirm(opts: {
  title: string;
  message: string;
  confirmText?: string;
  danger?: boolean;
}): Promise<boolean> {
  return open({ ...opts, kind: 'confirm' }).then((v) => v === true);
}

export function uiPrompt(opts: {
  title: string;
  message: string;
  defaultValue?: string;
  confirmText?: string;
}): Promise<string | null> {
  return open({ ...opts, kind: 'prompt' }).then((v) => (typeof v === 'string' ? v : null));
}

export function uiAlert(title: string, message: string): Promise<null> {
  return open({ kind: 'alert', title, message }).then(() => null);
}

// ===== Toast 通知 =====
export interface Toast {
  id: number;
  text: string;
  type: 'success' | 'error';
}

export const toasts = ref<Toast[]>([]);
let toastSeq = 0;

export function toast(text: string, type: Toast['type'] = 'success') {
  const id = ++toastSeq;
  toasts.value.push({ id, text, type });
  setTimeout(() => {
    toasts.value = toasts.value.filter((t) => t.id !== id);
  }, 3200);
}
