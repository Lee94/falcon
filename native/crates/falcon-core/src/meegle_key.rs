//! 工作项的显示编号。对应 web 的 `lib/meegleKey.ts`。

use falcon_proto::MeegleWorkItemDetail;

/// 飞书未从 CLI 暴露显示编号；这些前缀来自当前空间的模板编号配置。未知模板宁可退回原始 ID。
const PREFIX_BY_TEMPLATE: [(&str, &str); 4] = [("一般BUG", "g"), ("迭代BUG", "f"), ("设计确认", "f"), ("需求任务", "m")];

/// 带前缀的编号（`g-7105690993`）；模板认不出就是原始 id
pub fn meegle_display_key_of(id: &str, template: Option<&str>) -> String {
    // replace(/\s+/g, "").toUpperCase()
    let normalized: Option<String> =
        template.map(|t| t.chars().filter(|c| !crate::js::is_js_whitespace(*c)).collect::<String>().to_uppercase());
    let prefix = normalized
        .as_deref()
        .filter(|t| !t.is_empty())
        .and_then(|t| PREFIX_BY_TEMPLATE.iter().find(|(k, _)| *k == t))
        .map(|(_, p)| *p);
    match prefix {
        Some(p) => format!("{p}-{id}"),
        None => id.to_string(),
    }
}

pub fn meegle_display_key(detail: &MeegleWorkItemDetail) -> String {
    meegle_display_key_of(&detail.id, detail.template.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_key_uses_the_configured_template_prefix() {
        assert_eq!(meegle_display_key_of("7105690993", Some("一般 BUG")), "g-7105690993");
        assert_eq!(meegle_display_key_of("7112501423", Some("迭代BUG")), "f-7112501423");
        assert_eq!(meegle_display_key_of("7112723046", Some("设计确认")), "f-7112723046");
        assert_eq!(meegle_display_key_of("7113000000", Some("需求任务")), "m-7113000000");
    }

    #[test]
    fn unknown_templates_safely_retain_the_raw_work_item_id() {
        assert_eq!(meegle_display_key_of("123", Some("新模板")), "123");
        assert_eq!(meegle_display_key_of("456", None), "456");
        // 以下是 Rust 侧补的：大小写不敏感
        assert_eq!(meegle_display_key_of("1", Some("一般bug")), "g-1");
    }
}
