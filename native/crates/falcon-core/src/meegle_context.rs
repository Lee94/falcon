//! 「复制给 AI」的工作项上下文（Markdown）。对应 web 的 `lib/meegleContext.ts`。
//!
//! 只带能帮 AI 干活的东西：带前缀的编号 + 标题、正文、附件、评论。路由元数据（空间、
//! 类型、节点）与人员标识一律不进——前者对 AI 没用，后者不该外流。

use falcon_proto::MeegleWorkItemDetail;

use crate::js::js_trim;
use crate::meegle_key::meegle_display_key;

/// 单行元数据不允许换行制造新的 Markdown 小节；正文保持原始代码、日志和图片引用。
fn line(value: &str) -> String {
    // replace(/[\r\n]+/g, " ").trim()
    let mut out = String::with_capacity(value.len());
    let mut in_break = false;
    for c in value.chars() {
        if c == '\r' || c == '\n' {
            if !in_break {
                out.push(' ');
            }
            in_break = true;
        } else {
            out.push(c);
            in_break = false;
        }
    }
    js_trim(&out).to_string()
}

/// 空小节对接收上下文的 AI 没有信息量，只是白烧 token：没读到内容就连标题一起省掉。
/// 例外是读取失败——那是"飞书里可能有、这次没拿到"，必须留一行交代，否则 AI 会当成没有。
fn section(heading: &str, body: &str, incomplete: Option<&str>) -> Option<String> {
    let incomplete = incomplete.filter(|s| !s.is_empty());
    if body.is_empty() && incomplete.is_none() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !body.is_empty() {
        parts.push(body.to_string());
    }
    if let Some(i) = incomplete {
        parts.push(format!("> {i}"));
    }
    Some(format!("## {heading}\n\n{}", parts.join("\n\n")))
}

/// 生成上下文。`t` 是翻译函数（key 形如 `meegle.contextDescription`）；
/// `_fetched_at` 与 web 签名对齐，目前不进正文。
pub fn format_meegle_context(detail: &MeegleWorkItemDetail, _fetched_at: &str, t: impl Fn(&str) -> String) -> String {
    let label = |key: &str| t(&format!("meegle.{key}"));
    // `descriptionMarkdown ?? description ?? ""`：空串的 Markdown 也算有，不往下落
    let description = js_trim(detail.description_markdown.as_deref().or(detail.description.as_deref()).unwrap_or(""));
    let attachments = detail
        .attachments
        .iter()
        .flatten()
        .map(|file| match file.name.as_deref().filter(|n| !n.is_empty()) {
            Some(name) => format!("- [{}]({})", line(name), file.url),
            None => format!("- {}", file.url),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let comments = detail
        .comments
        .iter()
        .flatten()
        .filter_map(|comment| {
            let body = js_trim(&comment.content);
            let files = comment.attachments.iter().flatten().map(|u| format!("- {u}")).collect::<Vec<_>>().join("\n");
            // 正文与附件都空的评论（只有表情、只有人员变更）不值得占一节
            if body.is_empty() && files.is_empty() {
                return None;
            }
            let heading = comment.created_at.as_deref().filter(|s| !s.is_empty()).map(|c| format!("### {}", line(c)));
            let parts: Vec<String> =
                [heading, Some(body.to_string()), Some(files)].into_iter().flatten().filter(|s| !s.is_empty()).collect();
            Some(parts.join("\n\n"))
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // 带前缀的 Key 打头：AI 拿它对得上分支名、提交信息和飞书里的工作项
    let title = [meegle_display_key(detail), line(&detail.name)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    let attachments_incomplete = detail.attachments_unavailable.unwrap_or(false).then(|| label("contextAttachmentsIncomplete"));
    let comments_incomplete = detail.comments_unavailable.unwrap_or(false).then(|| label("contextCommentsIncomplete"));
    let sections: Vec<String> = [
        Some(format!("# {title}")),
        section(&label("contextDescription"), description, None),
        section(&label("contextAttachments"), &attachments, attachments_incomplete.as_deref()),
        section(&label("contextComments"), &comments, comments_incomplete.as_deref()),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!("{}\n", sections.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::{MeegleAttachment, MeegleComment, MeegleContextField};

    fn detail() -> MeegleWorkItemDetail {
        serde_json::from_str(
            r#"{"id":"7105690993","name":"Broken chart","spaceKey":"space","typeKey":"issue","template":"一般BUG",
                "currentNodes":[{"name":"Fixing","owners":["Private owner"]}],"operators":["Private operator"],
                "roles":[{"name":"QA","members":["Private tester"]}]}"#,
        )
        .unwrap()
    }

    fn t(key: &str) -> String {
        key.to_string()
    }

    #[test]
    fn leads_with_the_prefixed_work_item_key_and_drops_routing_metadata() {
        let mut d = detail();
        d.description = Some("Steps".into());
        let out = format_meegle_context(&d, "2026-01-02T03:04:05Z", t);
        assert!(out.starts_with("# g-7105690993 Broken chart\n"), "{out}");
        assert!(out.contains("meegle.contextDescription"));
        for leak in ["space", "issue", "2026-01-02", "Fixing", "Private"] {
            assert!(!out.contains(leak), "{leak} 漏进了上下文：{out}");
        }
    }

    #[test]
    fn sections_with_nothing_in_them_are_omitted() {
        assert_eq!(format_meegle_context(&detail(), "now", t), "# g-7105690993 Broken chart\n");
        let mut d = detail();
        d.description = Some("Original text".into());
        let out = format_meegle_context(&d, "now", t);
        assert!(out.contains("Original text"));
        assert!(!out.contains("contextAttachments") && !out.contains("contextComments"));
    }

    #[test]
    fn preserves_description_markdown_attachments_and_comments() {
        let markdown = "Steps\n\n```js\nthrow new Error('oops');\n```\n\n![screen](https://example.com/a.png)";
        let mut d = detail();
        d.item.name = "Title\n## Not a section".into();
        d.item.business = Some("Charts".into());
        d.description = Some("Steps [图片]".into());
        d.description_markdown = Some(markdown.into());
        d.context_fields = Some(vec![MeegleContextField { name: "Environment".into(), value: "Browser v1\nOS v2".into() }]);
        d.attachments = Some(vec![MeegleAttachment { name: Some("trace.txt".into()), url: "https://example.com/trace.txt".into() }]);
        d.comments = Some(vec![MeegleComment {
            content: "Please check this case".into(),
            created_at: Some("2026-01-03 10:00:00".into()),
            attachments: Some(vec!["https://example.com/comment.png".into()]),
        }]);
        let out = format_meegle_context(&d, "now", t);
        assert!(out.contains(markdown));
        assert!(out.contains("[trace.txt](https://example.com/trace.txt)"));
        assert!(out.contains("Please check this case"));
        assert!(out.contains("https://example.com/comment.png"));
        for leak in ["Environment", "Browser v1", "Charts", "\n## Not a section", "[图片]"] {
            assert!(!out.contains(leak), "{leak}：{out}");
        }
    }

    #[test]
    fn comments_carrying_neither_text_nor_files_do_not_open_a_section() {
        let mut d = detail();
        d.comments = Some(vec![MeegleComment { content: "  ".into(), created_at: Some("2026-01-03 10:00:00".into()), attachments: None }]);
        assert_eq!(format_meegle_context(&d, "now", t), "# g-7105690993 Broken chart\n");
    }

    #[test]
    fn incomplete_reads_are_disclosed_even_when_the_section_came_back_empty() {
        let mut d = detail();
        d.description = Some("Original text".into());
        d.comments_unavailable = Some(true);
        let out = format_meegle_context(&d, "now", t);
        assert!(out.contains("Original text"));
        assert!(out.contains("meegle.contextComments"));
        assert!(out.contains("contextCommentsIncomplete"));
    }

    #[test]
    fn unknown_templates_keep_the_raw_id_in_the_heading() {
        let mut d = detail();
        d.template = None;
        assert!(format_meegle_context(&d, "now", t).starts_with("# 7105690993 Broken chart\n"));
    }
}
