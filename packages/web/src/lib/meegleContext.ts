import type { MeegleWorkItemDetail } from "@falcon/shared";

type Translate = (key: string) => string;

/** 单行元数据不允许换行制造新的 Markdown 小节；正文保持原始代码、日志和图片引用。 */
function line(value: string): string {
  return value.replace(/[\r\n]+/g, " ").trim();
}

export function formatMeegleContext(
  detail: MeegleWorkItemDetail,
  _fetchedAt: string,
  t: Translate
): string {
  const label = (key: string) => t(`meegle.${key}`);
  const missing = label("contextUnavailable");
  const description = detail.descriptionMarkdown ?? detail.description;
  const attachments = detail.attachments?.length
    ? detail.attachments.map((file) => `- ${file.name ? `[${line(file.name)}](${file.url})` : file.url}`).join("\n")
    : missing;
  const comments = detail.comments?.length
    ? detail.comments.map((comment) => {
        const body = comment.content.trim() || missing;
        const files = comment.attachments?.map((url) => `- ${url}`).join("\n");
        return [comment.createdAt ? `### ${line(comment.createdAt)}` : "###", body, files].filter(Boolean).join("\n\n");
      }).join("\n\n")
    : missing;
  const sections = [
    `# ${line(detail.name) || missing}`,
    `## ${label("contextDescription")}\n\n${description?.trim() || missing}`,
    `## ${label("contextAttachments")}\n\n${attachments}${
      detail.attachmentsUnavailable ? `\n\n> ${label("contextAttachmentsIncomplete")}` : ""
    }`,
    `## ${label("contextComments")}\n\n${comments}${
      detail.commentsUnavailable ? `\n\n> ${label("contextCommentsIncomplete")}` : ""
    }`,
  ];
  return `${sections.join("\n\n")}\n`;
}
