//! 飞书项目面板内的下钻栈，及其持久化。对应旧 React 版的 `lib/meegleDrill.ts`。
//!
//! 视图与全景视图共用一页，只是取数接口不同。面板重开时恢复上次停的位置；存储被改坏
//! 或换了版本都只是回到根页，不弹错。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 下钻栈的一层。字段顺序照 React 版读回来之后的样子（`kind` 打头、公共字段在前）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum Drill {
    View {
        space_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        type_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        view_id: String,
        label: String,
        /// 全景视图（跨空间）
        multi: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        type_name: Option<String>,
    },
    Item {
        space_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        type_key: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        id: String,
        /// 打开详情前的占位，详情回来会覆盖
        title: String,
    },
}

/// 每层都要重新取一次数，记太深既没人回得去也白撑存储
pub const MEEGLE_DRILL_MAX_DEPTH: usize = 8;

/// 非空字符串才算有
fn str_field(v: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string)
}

/// 栈里存的全是 CLI 要的标识与显示名，没有正文；坏一层就当没有，绝不半信半疑地放行
fn parse_layer(value: &Value) -> Option<Drill> {
    let v = value.as_object()?;
    let space_key = str_field(v, "spaceKey")?;
    let space_name = str_field(v, "spaceName");
    let type_key = str_field(v, "typeKey");
    let url = str_field(v, "url");
    match v.get("kind").and_then(Value::as_str) {
        // title 只是打开详情前的占位，详情回来会覆盖，空着也能恢复
        Some("item") => Some(Drill::Item {
            space_key,
            space_name,
            type_key,
            url,
            id: str_field(v, "id")?,
            title: str_field(v, "title").unwrap_or_default(),
        }),
        Some("view") => {
            let view_id = str_field(v, "viewId")?;
            Some(Drill::View {
                space_key,
                space_name,
                type_key,
                url,
                label: str_field(v, "label").unwrap_or_else(|| view_id.clone()),
                view_id,
                multi: v.get("multi") == Some(&Value::Bool(true)),
                type_name: str_field(v, "typeName"),
            })
        }
        _ => None,
    }
}

/// 从存储读回来的原文 → 栈。中间断一层，上面几层的返回路径就不对了，只恢复到断点为止
pub fn parse_drill(raw: Option<&str>) -> Vec<Drill> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else { return Vec::new() };
    let Ok(Value::Array(layers)) = serde_json::from_str::<Value>(raw) else { return Vec::new() };
    layers.iter().take(MEEGLE_DRILL_MAX_DEPTH).map_while(parse_layer).collect()
}

/// 写回存储：只留最上面 [`MEEGLE_DRILL_MAX_DEPTH`] 层
pub fn serialize_drill(stack: &[Drill]) -> String {
    serde_json::to_string(&stack[..stack.len().min(MEEGLE_DRILL_MAX_DEPTH)]).unwrap_or_else(|_| "[]".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn view(label: &str) -> Drill {
        Drill::View {
            space_key: "space".into(),
            space_name: None,
            type_key: None,
            url: None,
            view_id: "v1".into(),
            label: label.into(),
            multi: false,
            type_name: None,
        }
    }

    fn item(id: &str, title: &str, type_key: Option<&str>) -> Drill {
        Drill::Item {
            space_key: "space".into(),
            space_name: None,
            type_key: type_key.map(Into::into),
            url: None,
            id: id.into(),
            title: title.into(),
        }
    }

    #[test]
    fn a_drilled_in_position_survives_a_round_trip_through_storage() {
        let mut v = view("我的视图");
        if let Drill::View { type_name, .. } = &mut v {
            *type_name = Some("需求".into());
        }
        let stack = vec![v, item("7105690993", "Broken chart", Some("issue"))];
        assert_eq!(parse_drill(Some(&serialize_drill(&stack))), stack);
    }

    #[test]
    fn nothing_stored_means_the_panel_opens_at_its_tab_root() {
        assert!(parse_drill(None).is_empty());
        assert!(parse_drill(Some("")).is_empty());
        assert!(parse_drill(Some("{]")).is_empty());
        assert!(parse_drill(Some(r#"{"kind":"item"}"#)).is_empty());
    }

    #[test]
    fn layers_missing_cli_identifiers_are_dropped_along_with_everything_above() {
        let raw = json!([
            { "kind": "view", "spaceKey": "space", "viewId": "v1", "label": "我的视图", "multi": false },
            { "kind": "item", "spaceKey": "space", "title": "no id" },
            { "kind": "item", "spaceKey": "space", "id": "7105690993", "title": "unreachable" },
        ])
        .to_string();
        assert_eq!(parse_drill(Some(&raw)), [view("我的视图")]);
        assert!(parse_drill(Some(&json!([{ "kind": "view", "viewId": "v1" }]).to_string())).is_empty());
        assert!(parse_drill(Some(&json!([{ "kind": "other", "spaceKey": "space", "id": "1" }]).to_string())).is_empty());
    }

    #[test]
    fn a_view_that_lost_its_name_still_opens_labelled_by_its_id() {
        let got = parse_drill(Some(&json!([{ "kind": "view", "spaceKey": "space", "viewId": "v1" }]).to_string()));
        assert_eq!(got, [view("v1")]);
    }

    #[test]
    fn an_item_keeps_opening_without_a_placeholder_title() {
        let got = parse_drill(Some(&json!([{ "kind": "item", "spaceKey": "space", "id": "710" }]).to_string()));
        assert_eq!(got, [item("710", "", None)]);
    }

    #[test]
    fn a_runaway_stack_is_capped() {
        let deep: Vec<Drill> = (0..12).map(|i| item(&i.to_string(), &format!("#{i}"), None)).collect();
        assert_eq!(parse_drill(Some(&serde_json::to_string(&deep).unwrap())).len(), 8);
        let back: Vec<Value> = serde_json::from_str(&serialize_drill(&deep)).unwrap();
        assert_eq!(back.len(), 8);
    }

    #[test]
    fn wire_shape_matches_web() {
        // 以下是 Rust 侧补的：可选字段缺省就不写
        assert_eq!(
            serialize_drill(&[item("1", "t", None)]),
            r#"[{"kind":"item","spaceKey":"space","id":"1","title":"t"}]"#
        );
    }
}
