import { ref } from 'vue';
import { toast } from 'vue-sonner';
import { tr } from '../composables/i18n';

/**
 * 需要人参与的事件通知。
 *
 * 两路输出：
 * - 应用内：统一走 vue-sonner，聚焦时即时可见；
 * - 系统级：页面处于后台（切走标签页/最小化）时追加浏览器系统通知，
 *   否则用户已经看着界面，重复弹系统通知只会打扰。
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

/** 是否已获得系统通知授权。 */
export function systemNotificationGranted(): boolean {
  return supported() && notificationPermission.value === 'granted';
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

/** 页面是否在前台可见：后台时才补系统通知。 */
function isForeground(): boolean {
  if (typeof document === 'undefined') return false;
  return document.visibilityState === 'visible' && document.hasFocus();
}

function pushSystem(title: string, body: string) {
  if (!systemNotificationGranted()) return;
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
 * 通知一次需要人参与的事件：应用内 sonner + 后台系统通知。
 *
 * `body` 由调用方按事件补充（会话标题 / 问题 / 工具名）。
 */
export function notifyHumanEvent(kind: HumanEvent, body: string) {
  const title = tr(EVENT_TITLE[kind]);
  toast.info(title, { description: body });
  if (!isForeground()) pushSystem(title, body);
}
