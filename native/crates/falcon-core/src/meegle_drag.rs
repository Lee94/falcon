//! 拖飞书工作项（到侧栏的检出行上预填附属项目）的载荷。对应旧 React 版的 `lib/meegleDrag.ts`。
//!
//! React 版走 dataTransfer 的私有 MIME（`application/x-falcon-meegle-work-item+json`，JSON 载荷
//! 带 version / kind）；GPUI 客户端走 GPUI 的类型化拖拽（设计文档 §4.2），载荷就是
//! [`MeegleWorkItemDragPayload`] 本身，落点只认这个类型，MIME 与 JSON 那一层随 React 版删掉。
//! **普通文本 / 链接的拖放绝不能触发派生**；构造时对标识符过一遍白名单（它们之后会原样进
//! CLI 的 argv）。

use falcon_proto::{is_valid_meegle_key, is_valid_meegle_work_item_id};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeegleWorkItemDragPayload {
    pub id: String,
    pub space_key: String,
}

impl MeegleWorkItemDragPayload {
    /// 标识符合法才给载荷（React 版的 `canDragMeegleWorkItem` + 构造）
    pub fn new(id: &str, space_key: &str) -> Option<Self> {
        can_drag_meegle_work_item(id, space_key).then(|| MeegleWorkItemDragPayload { id: id.into(), space_key: space_key.into() })
    }
}

pub fn can_drag_meegle_work_item(id: &str, space_key: &str) -> bool {
    is_valid_meegle_work_item_id(id) && is_valid_meegle_key(space_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_safe_identifiers_make_a_payload() {
        assert_eq!(
            MeegleWorkItemDragPayload::new("123456", "space_key"),
            Some(MeegleWorkItemDragPayload { id: "123456".into(), space_key: "space_key".into() })
        );
        assert_eq!(MeegleWorkItemDragPayload::new("", "space"), None);
        assert_eq!(MeegleWorkItemDragPayload::new("feat/x", "space"), None);
        assert_eq!(MeegleWorkItemDragPayload::new("123", "../space"), None);
    }
}
