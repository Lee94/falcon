//! 窗口（pane）的 key 约定：`t:<sessionId>` / `f:<projectId>:<path>` / `d`。
//!
//! 对应旧 React 版的 `lib/paneKey.ts`。工作区的排布（[`crate::layout`]）只搬运这一个字符串，
//! 具体是终端、文件还是差异，靠解析 key 得到——列里混排三种视图，再给每种各存一份
//! 顺序必然漂移。
//!
//! path 里可以有冒号（相对路径里极少见但合法），所以 file key 只在 projectId 后切一次。

/// 解析出来的窗口
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneItem {
    Terminal { key: String, id: String },
    File { key: String, project_id: String, path: String },
    Diff { key: String },
}

impl PaneItem {
    pub fn key(&self) -> &str {
        match self {
            PaneItem::Terminal { key, .. } | PaneItem::File { key, .. } | PaneItem::Diff { key } => key,
        }
    }
}

/// 差异只有一个（就地替换的预览语义），不带参数
pub const DIFF_KEY: &str = "d";

pub fn term_key(id: &str) -> String {
    format!("t:{id}")
}

pub fn file_key(project_id: &str, path: &str) -> String {
    format!("f:{project_id}:{path}")
}

pub fn parse_pane_key(key: &str) -> Option<PaneItem> {
    if key == DIFF_KEY {
        return Some(PaneItem::Diff { key: key.to_string() });
    }
    if let Some(id) = key.strip_prefix("t:") {
        return (!id.is_empty()).then(|| PaneItem::Terminal { key: key.to_string(), id: id.to_string() });
    }
    if let Some(rest) = key.strip_prefix("f:") {
        // projectId 不能为空（cut <= 0 在 TS 里一并挡掉）
        let cut = rest.find(':').filter(|&i| i > 0)?;
        let project_id = &rest[..cut];
        let path = &rest[cut + 1..];
        if path.is_empty() {
            return None;
        }
        return Some(PaneItem::File {
            key: key.to_string(),
            project_id: project_id.to_string(),
            path: path.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_the_three_kinds() {
        assert_eq!(parse_pane_key("d"), Some(PaneItem::Diff { key: "d".into() }));
        assert_eq!(
            parse_pane_key(&term_key("s1")),
            Some(PaneItem::Terminal { key: "t:s1".into(), id: "s1".into() })
        );
        assert_eq!(
            parse_pane_key(&file_key("p1", "src/a:b.ts")),
            Some(PaneItem::File { key: "f:p1:src/a:b.ts".into(), project_id: "p1".into(), path: "src/a:b.ts".into() })
        );
    }

    #[test]
    fn rejects_malformed_keys() {
        for key in ["t:", "f:", "f:p1", "f::a", "f:p1:", "x:1", "", "dd"] {
            assert_eq!(parse_pane_key(key), None, "{key:?}");
        }
    }
}
