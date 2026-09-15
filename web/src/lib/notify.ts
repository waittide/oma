import { ref } from 'vue';
import { toast } from 'vue-sonner';
import { tr } from '../composables/i18n';

/**
 * 需要人参与的事件通知。
 *
 * 仅在页面失焦（切走标签页 / 最小化 / 焦点在别处）时提示：用户正看着界面时，
 * 审批条与提问面板已经把事件摆在眼前，再弹提示只是噪声。
 *
 * 失焦时两路输出：应用内走 vue-sonner（回到页面即可看到），并追加浏览器
 * 系统通知，让用户在其他窗口也能第一时间得知。
 *
 * Notification 权限需要用户手势才能申请，因此由用户动作（发送消息）触发，
 * 而不是页面加载时静默申请——被浏览器忽略后反而更难排查。
 */

export const notificationPermission = ref<NotificationPermission>(
  typeof Notification === 'undefined' ? 'denied' : Notification.permission,
);

function supported(): boolean {
  return typeof Notification !== 'undefined';
}

/**
 * 申请系统通知权限；仅在用户手势中调用。
 * 返回最终权限状态；不支持或失败时返回 denied，不抛错。
 */
export async function ensureNotificationPermission(): Promise<NotificationPermission> {
  if (!supported() || notificationPermission.value !== 'default') {
    return notificationPermission.value;
  }
  try {
    notificationPermission.value = await Notification.requestPermission();
  } catch {
    notificationPermission.value = 'denied';
  }
  return notificationPermission.value;
}

/** 页面是否处于前台可见且拥有焦点：仅失焦时才提示。 */
function isForeground(): boolean {
  if (typeof document === 'undefined') return false;
  return document.visibilityState === 'visible' && document.hasFocus();
}

function pushSystem(title: string, body: string) {
  if (!supported() || notificationPermission.value !== 'granted') return;
  try {
    // 复用同一 tag：连续事件只保留最新一条，避免通知中心被刷屏
    new Notification(title, { body, tag: 'oma-event' });
  } catch {
    // 构造失败（如平台不支持通知展示）时忽略：应用内提示已经给出
  }
}

/** 需要人参与的事件类型。 */
export type HumanEvent = 'turn' | 'ask' | 'approval';

const EVENT_TITLE: Record<HumanEvent, string> = {
  turn: 'notify.turnDone',
  ask: 'notify.askNeeded',
  approval: 'notify.approvalNeeded',
};

/**
 * 通知一次需要人参与的事件：仅在页面失焦时提示。
 *
 * 失焦时同时走应用内 sonner 与浏览器系统通知；未被授权系统通知时至少回到
 * 页面还能看到 sonner。`body` 由调用方按事件补充（会话标题 / 问题 / 工具名）。
 */
export function notifyHumanEvent(kind: HumanEvent, body: string) {
  if (isForeground()) return;
  const title = tr(EVENT_TITLE[kind]);
  toast.info(title, { description: body });
  pushSystem(title, body);
}
