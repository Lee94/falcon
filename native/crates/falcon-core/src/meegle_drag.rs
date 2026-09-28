//! 拖飞书工作项（到侧栏的检出行上预填附属项目）的载荷。对应 web 的 `lib/meegleDrag.ts`。
//!
//! web 走 dataTransfer 的私有 MIME；原生走 GPUI 的类型化拖拽（设计文档 §4.2），载荷就是
//! [`MeegleWorkItemDragPayload`] 本身。JSON 形状与 MIME 仍然保留：跨进程 / 跨窗口拖放、
//! 以及与 web 对照时用。**普通文本 / 链接的拖放绝不能触发派生**，所以只认这个私有类型，
//! 解析时对标识符再过一遍白名单（它们之后会原样进 CLI 的 argv）。

use falcon_proto::{is_valid_meegle_key, is_valid_meegle_work_item_id};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::utf16_len;

/// Falcon 私有的拖放类型
pub const MEEGLE_WORK_ITEM_MIME: &str = "application/x-falcon-meegle-work-item+json";

/// `{"version":1,"kind":"meegle-work-item","id":…,"spaceKey":…}`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MeegleWorkItemDragPayload {
    /// 恒为 1
    pub version: u32,
    /// 恒为 `"meegle-work-item"`
    pub kind: String,
    pub id: String,
    pub space_key: String,
}

impl MeegleWorkItemDragPayload {
    /// 标识符合法才给载荷（web 的 `canDragMeegleWorkItem` + 构造）
    pub fn new(id: &str, space_key: &str) -> Option<Self> {
        can_drag_meegle_work_item(id, space_key).then(|| MeegleWorkItemDragPayload {
            version: 1,
            kind: "meegle-work-item".into(),
            id: id.into(),
            space_key: space_key.into(),
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

pub fn can_drag_meegle_work_item(id: &str, space_key: &str) -> bool {
    is_valid_meegle_work_item_id(id) && is_valid_meegle_key(space_key)
}

pub fn has_meegle_work_item_type<S: AsRef<str>>(types: &[S]) -> bool {
    types.iter().any(|t| t.as_ref() == MEEGLE_WORK_ITEM_MIME)
}

/// 解析私有 MIME 下的原文。超长（>256 码元）、不是 JSON、版本 / 种类不对、标识符不合法
/// 一律 `None`——坏载荷绝不半信半疑地放行。
pub fn parse_meegle_work_item_payload(raw: &str) -> Option<MeegleWorkItemDragPayload> {
    if raw.is_empty() || utf16_len(raw) > 256 {
        return None;
    }
    let value: Value = serde_json::from_str(raw).ok()?;
    let obj = value.as_object()?;
    if obj.get("version").and_then(Value::as_f64) != Some(1.0) || obj.get("kind").and_then(Value::as_str) != Some("meegle-work-item") {
        return None;
    }
    let id = obj.get("id").and_then(Value::as_str)?;
    let space_key = obj.get("spaceKey").and_then(Value::as_str)?;
    MeegleWorkItemDragPayload::new(id, space_key)
}

/// web 的 `parseMeegleWorkItemDrag(dataTransfer)`：先看类型表里有没有私有 MIME，再取它的数据
pub fn parse_meegle_work_item_drag<S: AsRef<str>>(
    types: &[S],
    get_data: impl Fn(&str) -> String,
) -> Option<MeegleWorkItemDragPayload> {
    if !has_meegle_work_item_type(types) {
        return None;
    }
    parse_meegle_work_item_payload(&get_data(MEEGLE_WORK_ITEM_MIME))
}

/// web 的 `writeMeegleWorkItemDrag`：要写进拖放数据的 (effectAllowed, MIME, JSON)；
/// 标识符不合法时什么都不写
pub fn write_meegle_work_item_drag(id: &str, space_key: &str) -> Option<(&'static str, &'static str, String)> {
    let payload = MeegleWorkItemDragPayload::new(id, space_key)?;
    Some(("copy", MEEGLE_WORK_ITEM_MIME, payload.to_json()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reader(types: &[&str], value: &str) -> Option<MeegleWorkItemDragPayload> {
        let value = value.to_string();
        parse_meegle_work_item_drag(types, |t| if t == MEEGLE_WORK_ITEM_MIME { value.clone() } else { String::new() })
    }

    #[test]
    fn writes_and_parses_the_private_work_item_payload() {
        let (effect, mime, json) = write_meegle_work_item_drag("123456", "space_key").unwrap();
        assert_eq!(effect, "copy");
        assert_eq!(mime, MEEGLE_WORK_ITEM_MIME);
        assert_eq!(json, r#"{"version":1,"kind":"meegle-work-item","id":"123456","spaceKey":"space_key"}"#);
        assert_eq!(
            reader(&[MEEGLE_WORK_ITEM_MIME], &json),
            Some(MeegleWorkItemDragPayload {
                version: 1,
                kind: "meegle-work-item".into(),
                id: "123456".into(),
                space_key: "space_key".into()
            })
        );
    }

    #[test]
    fn ignores_ordinary_drags_without_the_private_mime_type() {
        let raw = json!({ "version": 1, "kind": "meegle-work-item", "id": "123", "spaceKey": "space" }).to_string();
        assert!(!has_meegle_work_item_type(&["text/plain"]));
        assert_eq!(reader(&["text/plain"], &raw), None);
    }

    #[test]
    fn rejects_malformed_wrong_version_and_invalid_identifier_payloads() {
        let parse = |v: Value| reader(&[MEEGLE_WORK_ITEM_MIME], &v.to_string());
        assert_eq!(reader(&[MEEGLE_WORK_ITEM_MIME], "{"), None);
        assert_eq!(parse(json!({ "version": 2, "kind": "meegle-work-item", "id": "123", "spaceKey": "space" })), None);
        assert_eq!(parse(json!({ "version": 1, "kind": "meegle-work-item", "id": "feat/x", "spaceKey": "space" })), None);
        assert_eq!(parse(json!({ "version": 1, "kind": "meegle-work-item", "id": "123", "spaceKey": "../space" })), None);
    }

    #[test]
    fn does_not_write_a_payload_when_the_item_lacks_safe_identifiers() {
        assert_eq!(write_meegle_work_item_drag("", "space"), None);
    }

    #[test]
    fn oversized_payloads_are_refused() {
        // 以下是 Rust 侧补的：长度上限按 UTF-16 码元
        let raw = format!(r#"{{"version":1,"kind":"meegle-work-item","id":"1","spaceKey":"s","pad":"{}"}}"#, "x".repeat(250));
        assert_eq!(parse_meegle_work_item_payload(&raw), None);
        assert!(parse_meegle_work_item_payload(r#"{"version":1.0,"kind":"meegle-work-item","id":"1","spaceKey":"s"}"#).is_some());
    }
}
