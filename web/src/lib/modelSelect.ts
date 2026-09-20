import type { UiSelectGroup } from '@waittide/ui';
import type { ModelInfo } from '../types';

/**
 * 模型清单 → UiSelect 分组选项。
 *
 * provider 作为分组标题，模型为组内选项，值为 `provider/model` 选择器
 * （与后端 find_model 一致），说明文字为上下文窗口大小。
 */
export function toModelSelectGroups(groups: Record<string, ModelInfo[]>): UiSelectGroup[] {
  return Object.entries(groups).map(([provider, models]) => ({
    label: provider,
    options: models.map((model) => ({
      label: model.name || model.id,
      value: `${provider}/${model.id}`,
      description: `${Math.round(model.context_len / 1024)}K`,
    })),
  }));
}

/**
 * 选择器当前应显示的文案。
 *
 * 命中清单时显示模型展示名；未命中（如模型已被删除）时只显示模型名部分，
 * 绝不露出 `provider/` 前缀；空选择器返回 null，由调用方回退到占位文案。
 */
export function modelSelectorLabel(groups: Record<string, ModelInfo[]>, selector: string): string | null {
  if (!selector) return null;
  const i = selector.indexOf('/');
  const provider = i === -1 ? '' : selector.slice(0, i);
  const modelId = i === -1 ? selector : selector.slice(i + 1);
  const model = (groups[provider] ?? []).find((item) => item.id === modelId);
  return model ? model.name || model.id : modelId || provider || selector;
}
