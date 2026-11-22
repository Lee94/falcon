//! 窗口（pane）的 key 约定：`t:<sessionId>` / `b:<sessionId>` /
//! `b2:<sessionId字节长度>:<sessionId>:<browserSession>` / `f:<projectId>:<path>` / `d`。
//!
//! 默认浏览器继续使用旧 `b:` key，已有工作区不用迁移。命名实例用长度标出终端 id 的字节边界，
//! 因为远端会话 id 自身可以含冒号（`node:...`）；browserSession 一直取到 key 末尾。

/// 一个终端上的具体远端浏览器实例。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserTarget {
    pub session_id: String,
    pub browser_session: String,
}

impl BrowserTarget {
    pub fn new(session_id: &str, browser_session: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            browser_session: if browser_session.is_empty() {
                format!("falcon-{session_id}")
            } else {
                browser_session.to_string()
            },
        }
    }

    pub fn default_for(session_id: &str) -> Self {
        Self::new(session_id, "")
    }

    pub fn is_default(&self) -> bool {
        self.browser_session == format!("falcon-{}", self.session_id)
    }

    pub fn key(&self) -> String {
        if self.is_default() {
            browser_key(&self.session_id)
        } else {
            format!("b2:{}:{}:{}", self.session_id.len(), self.session_id, self.browser_session)
        }
    }
}

/// 解析出来的窗口
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneItem {
    Terminal { key: String, id: String },
    Browser { key: String, session_id: String, browser_session: String },
    File { key: String, project_id: String, path: String },
    Diff { key: String },
}

impl PaneItem {
    pub fn key(&self) -> &str {
        match self {
            PaneItem::Terminal { key, .. }
            | PaneItem::Browser { key, .. }
            | PaneItem::File { key, .. }
            | PaneItem::Diff { key } => key,
        }
    }

    pub fn browser_target(&self) -> Option<BrowserTarget> {
        match self {
            PaneItem::Browser { session_id, browser_session, .. } => Some(BrowserTarget::new(session_id, browser_session)),
            _ => None,
        }
    }
}

/// 差异只有一个（就地替换的预览语义），不带参数
pub const DIFF_KEY: &str = "d";

pub fn term_key(id: &str) -> String {
    format!("t:{id}")
}

pub fn browser_key(session_id: &str) -> String {
    format!("b:{session_id}")
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
    if let Some(session_id) = key.strip_prefix("b:") {
        return (!session_id.is_empty()).then(|| PaneItem::Browser {
            key: key.to_string(),
            session_id: session_id.to_string(),
            browser_session: format!("falcon-{session_id}"),
        });
    }
    if let Some(rest) = key.strip_prefix("b2:") {
        let (len, rest) = rest.split_once(':')?;
        let (session_id, rest) = rest.split_at_checked(len.parse().ok()?)?;
        let browser_session = rest.strip_prefix(':').filter(|name| !name.is_empty())?;
        if session_id.is_empty() {
            return None;
        }
        let target = BrowserTarget::new(session_id, browser_session);
        // 默认实例只有旧 key 一种规范形状，避免同一实例出现两个持久化身份。
        if target.is_default() {
            return None;
        }
        return Some(PaneItem::Browser {
            key: key.to_string(),
            session_id: session_id.to_string(),
            browser_session: target.browser_session,
        });
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
    fn round_trips_the_pane_kinds() {
        assert_eq!(parse_pane_key("d"), Some(PaneItem::Diff { key: "d".into() }));
        assert_eq!(
            parse_pane_key(&term_key("s1")),
            Some(PaneItem::Terminal { key: "t:s1".into(), id: "s1".into() })
        );
        assert_eq!(
            parse_pane_key(&browser_key("s1")),
            Some(PaneItem::Browser { key: "b:s1".into(), session_id: "s1".into(), browser_session: "falcon-s1".into() })
        );
        assert_eq!(
            parse_pane_key(&file_key("p1", "src/a:b.ts")),
            Some(PaneItem::File { key: "f:p1:src/a:b.ts".into(), project_id: "p1".into(), path: "src/a:b.ts".into() })
        );
    }

    #[test]
    fn named_browser_keys_are_unambiguous_and_keep_instances_distinct() {
        let first = BrowserTarget::new("node:remote:abc", "vinci-viewer");
        let second = BrowserTarget::new("node:remote:abc", "vinci-png");
        let other_terminal = BrowserTarget::new("node:remote:def", "vinci-viewer");
        for target in [&first, &second, &other_terminal] {
            let key = target.key();
            let parsed = parse_pane_key(&key).unwrap();
            assert_eq!(parsed.browser_target().as_ref(), Some(target));
            assert_eq!(parsed.key(), key);
        }
        assert_ne!(first.key(), second.key());
        assert_ne!(first.key(), other_terminal.key());
        assert_eq!(BrowserTarget::default_for("s1").key(), "b:s1");
    }

    #[test]
    fn rejects_malformed_keys() {
        for key in [
            "t:", "f:", "f:p1", "f::a", "f:p1:", "x:1", "", "dd", "b2:", "b2:x:s1:n",
            "b2:2:s1:", "b2:1:s1:n", "b2:2:s1:falcon-s1", "b2:999:s1:n",
        ] {
            assert_eq!(parse_pane_key(key), None, "{key:?}");
        }
    }
}
