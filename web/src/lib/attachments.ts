import { ref } from 'vue';
import { api } from '../api';

/**
 * `session_attachment://` 引用 → 可展示的 blob URL。
 *
 * 附件下载接口需要 Bearer 鉴权，<img src> 无法附带请求头，
 * 因此这里显式取字节后转 object URL，并按引用缓存避免重复下载。
 */
export const ATTACHMENT_PREFIX = 'session_attachment://';

const cache = new Map<string, string>();
const pending = new Set<string>();
/** 已解析的引用 → blob URL（响应式，供模板直接读取） */
export const resolved = ref<Record<string, string>>({});

/** 图片块的可用地址：data URI / http URL 原样返回，附件引用走异步解析 */
export function imageSrc(sessionId: string, data: string): string | undefined {
  if (data.startsWith('data:') || data.startsWith('http')) return data;
  if (!data.startsWith(ATTACHMENT_PREFIX) || !sessionId) return undefined;
  if (!cache.has(data) && !pending.has(data)) {
    pending.add(data);
    void api
      .fetchAttachment(sessionId, data.slice(ATTACHMENT_PREFIX.length))
      .then((blob) => {
        const url = URL.createObjectURL(blob);
        cache.set(data, url);
        resolved.value = { ...resolved.value, [data]: url };
      })
      .catch(() => {
        // 附件缺失（如已被删除）时保持占位，不阻塞消息渲染
        resolved.value = { ...resolved.value, [data]: '' };
      })
      .finally(() => pending.delete(data));
  }
  return resolved.value[data];
}

/** 会话切换时释放上一会话的 object URL */
export function releaseAll() {
  for (const url of cache.values()) URL.revokeObjectURL(url);
  cache.clear();
  pending.clear();
  resolved.value = {};
}
