import type { MeegleWorkItemDetail } from "@falcon/shared";

type Translate = (key: string) => string;

/** 单行元数据不允许换行制造新的 Markdown 小节；正文保持原始代码、日志和图片引用。 */
function line(value: string): string {
  return value.replace(/[\r\n]+/g, " ").trim();
}

export function formatMeegleContext(
  detail: MeegleWorkItemDetail,
  fetchedAt: string,
  t: Translate
): string {
  // 详情页允许基础信息降级显示；复制要完整读取，不能把网络失败写成“字段未提供”。
  if (detail.contextFieldsUnavailable) throw new Error(t("meegle.copyContextFailed"));
  const label = (key: string) => t(`meegle.${key}`);
  const missing = label("contextUnavailable");
  const field = (key: string, value?: string) =>
    `- ${label(key)}: ${value?.trim() ? line(value) : missing}`;
  const description = detail.descriptionMarkdown ?? detail.description;
  const sections = [
    `# ${label("contextTitle")}: ${line(detail.name) || missing}`,
    label("contextSourceNote"),
    [
      `## ${label("contextIdentity")}`,
      field("copyKeyLabel", detail.id),
      field("contextLink", detail.url),
      field("d_space", detail.spaceName),
      field("contextSpaceKey", detail.spaceKey),
      field("d_business", detail.business),
      field("d_type", detail.typeName),
      field("contextTypeKey", detail.typeKey),
      field("d_status", detail.status),
      field("d_currentNode", detail.currentNodes.map((node) => node.name).join(" / ")),
      field("d_priority", detail.priority),
      field("contextUpdatedAt", detail.updatedAt),
      field("contextFetchedAt", fetchedAt),
    ].join("\n"),
    `## ${label("contextDescription")}\n\n${description?.trim() || missing}`,
    [
      `## ${label("contextFields")}`,
      "",
      ...(detail.contextFields?.length
        ? detail.contextFields.map((entry) => `### ${line(entry.name)}\n\n${entry.value}`)
        : [missing]),
    ].join("\n"),
    [
      `## ${label("contextGaps")}`,
      label("contextGapsNote"),
      label("contextAttachmentsNote"),
    ].join("\n\n"),
  ];
  return `${sections.join("\n\n")}\n`;
}
