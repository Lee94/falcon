import type { MeegleWorkItemDetail } from "@falcon/shared";
import { meegleDisplayKey } from "./meegleKey.js";

type Translate = (key: string) => string;

/** 单行元数据不允许换行制造新的 Markdown 小节；正文保持原始代码、日志和图片引用。 */
function line(value: string): string {
  return value.replace(/[\r\n]+/g, " ").trim();
}

/**
 * 空小节对接收上下文的 AI 没有信息量，只是白烧 token：没读到内容就连标题一起省掉。
 * 例外是读取失败——那是"飞书里可能有、这次没拿到"，必须留一行交代，否则 AI 会当成没有。
 */
function section(heading: string, body: string, incomplete?: string): string | null {
  if (!body && !incomplete) return null;
  return [`## ${heading}`, [body, incomplete && `> ${incomplete}`].filter(Boolean).join("\n\n")].join(
    "\n\n"
  );
}

export function formatMeegleContext(
  detail: MeegleWorkItemDetail,
  _fetchedAt: string,
  t: Translate
): string {
  const label = (key: string) => t(`meegle.${key}`);
  const description = (detail.descriptionMarkdown ?? detail.description ?? "").trim();
  const attachments = (detail.attachments ?? [])
    .map((file) => `- ${file.name ? `[${line(file.name)}](${file.url})` : file.url}`)
    .join("\n");
  const comments = (detail.comments ?? [])
    .map((comment) => {
      const body = comment.content.trim();
      const files = (comment.attachments ?? []).map((url) => `- ${url}`).join("\n");
      // 正文与附件都空的评论（只有表情、只有人员变更）不值得占一节
      if (!body && !files) return "";
      return [comment.createdAt && `### ${line(comment.createdAt)}`, body, files]
        .filter(Boolean)
        .join("\n\n");
    })
    .filter(Boolean)
    .join("\n\n");
  const sections = [
    // 带前缀的 Key 打头：AI 拿它对得上分支名、提交信息和飞书里的工作项
    `# ${[meegleDisplayKey(detail), line(detail.name)].filter(Boolean).join(" ")}`,
    section(label("contextDescription"), description),
    section(
      label("contextAttachments"),
      attachments,
      detail.attachmentsUnavailable ? label("contextAttachmentsIncomplete") : undefined
    ),
    section(
      label("contextComments"),
      comments,
      detail.commentsUnavailable ? label("contextCommentsIncomplete") : undefined
    ),
  ].filter(Boolean);
  return `${sections.join("\n\n")}\n`;
}
