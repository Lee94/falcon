import type { MeegleWorkItemDetail } from "@falcon/shared";

/** 飞书未从 CLI 暴露显示编号；这些前缀来自当前空间的模板编号配置。未知模板宁可退回原始 ID。 */
const PREFIX_BY_TEMPLATE = new Map([
  ["一般BUG", "g"],
  ["迭代BUG", "f"],
  ["设计确认", "f"],
  ["需求任务", "m"],
]);

export function meegleDisplayKey(detail: Pick<MeegleWorkItemDetail, "id" | "template">): string {
  const template = detail.template?.replace(/\s+/g, "").toUpperCase();
  const prefix = template && PREFIX_BY_TEMPLATE.get(template);
  return prefix ? `${prefix}-${detail.id}` : detail.id;
}
