//! 程序状态（OSC 7501，Program Status Protocol，ADR 0020 决定六）。
//!
//! 解析在宿主机的 VT 进程里（libghostty-vt 按规范校验、解码 base64），一条条报告交给 holder；
//! 记录存在 holder（VT 进程重启也不丢），由本模块的 [`ProgramStatusStore`] 按规范维护。
//! 服务端拿到的是记录全集，存进会话、推给界面；界面只看 [`ProgramStatusSummary`]。
//!
//! 规范（草案 0.4，2026-10-10）里与存储有关的条款，逐条落在这里：
//! - 一条报告**整条替换**同 id 的记录：报告里没带的键就是没有了；
//! - id 是 `/` 分隔的路径，父记录不必存在；`clear` 删掉该 id 及其下所有记录，不带 id 清空全部；
//! - 子记录没带 `app` 时，继承最近的、带 `app` 的祖先的；
//! - 记录属于终端，不属于屏幕：主屏 / 备用屏切换不影响；RIS 清空全部，DECSTR 不清；
//! - 新提示符（OSC 133 `A`）或附着的进程退出时，**必须**丢掉 working / blocked，done / error 留着；
//! - 每个终端最多 256 条，满了淘汰最久没更新的。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// 记录的状态。`clear` 不是记录状态，是报告的动作（[`ProgramStatusReport::clear`]）。
    ///
    /// Rust 侧加了 `Unknown` 兜底：草案还在变，新服务端多一个状态时老客户端照样解得出列表。
    pub enum ProgramState open {
        Idle => "idle",
        Working => "working",
        Done => "done",
        Blocked => "blocked",
        Error => "error",
    }
}

wire_enum! {
    /// blocked 时程序要的是什么：授权、回答还是凭据。
    ///
    /// Rust 侧加了 `Unknown` 兜底，理由同 [`ProgramState`]。
    pub enum ProgramBlockKind open {
        Permission => "permission",
        Question => "question",
        Auth => "auth",
    }
}

/// 一条记录。`title` / `message` 是程序给的不可信文本，显示到终端之外前要过
/// [`sanitize_display_text`]。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatusRecord {
    /// `/` 分隔的路径；空串是根记录
    pub id: String,
    pub state: ProgramState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgramBlockKind>,
    /// 0–100；缺省 = 不确定进度
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    /// 程序标识（分组、图标用），已按规则继承过
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 最后一次更新的毫秒时间戳（宿主机时钟）
    pub updated_at: i64,
}

/// VT 进程转来的一条报告（已校验、已解码）。`state == None` 表示 `clear`。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatusReport {
    #[serde(default)]
    pub id: String,
    /// None = `state=clear`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ProgramState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgramBlockKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ProgramStatusReport {
    pub fn clear(&self) -> bool {
        self.state.is_none()
    }
}

/// 每个终端最多保留的记录数（规范的上限，下限是 64）
pub const MAX_RECORDS: usize = 256;

/// `child` 是不是 `parent` 本身或在它之下。空串是根，所有 id 都在它之下
fn is_under(child: &str, parent: &str) -> bool {
    parent.is_empty() || child == parent || child.strip_prefix(parent).is_some_and(|rest| rest.starts_with('/'))
}

/// 一个终端（会话）的全部记录
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramStatusStore {
    records: Vec<ProgramStatusRecord>,
}

impl ProgramStatusStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn records(&self) -> &[ProgramStatusRecord] {
        &self.records
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// 应用一条报告。返回记录是否变了（变了才值得推给服务端）
    pub fn apply(&mut self, report: ProgramStatusReport, now: i64) -> bool {
        let Some(state) = report.state else {
            let before = self.records.len();
            self.records.retain(|r| !is_under(&r.id, &report.id));
            return self.records.len() != before;
        };
        // app 缺省时继承最近的、带 app 的祖先：从直接父级往根找
        let app = report.app.or_else(|| self.inherited_app(&report.id));
        let record = ProgramStatusRecord {
            // kind 只对 blocked 有意义，progress 只对 working / blocked 有意义（规范原话）
            kind: report.kind.filter(|_| state == ProgramState::Blocked),
            progress: report.progress.filter(|p| *p <= 100 && matches!(state, ProgramState::Working | ProgramState::Blocked)),
            id: report.id,
            state,
            app,
            title: report.title,
            message: report.message,
            updated_at: now,
        };
        match self.records.iter().position(|r| r.id == record.id) {
            Some(i) => {
                // 整条替换；挪到末尾，Vec 的顺序就是「最近更新」的顺序
                self.records.remove(i);
                self.records.push(record);
            }
            None => {
                if self.records.len() >= MAX_RECORDS {
                    // 满了淘汰最久没更新的：末尾是最新的，所以是第一个
                    self.records.remove(0);
                }
                self.records.push(record);
            }
        }
        true
    }

    fn inherited_app(&self, id: &str) -> Option<String> {
        let mut cur = id;
        loop {
            let parent = match cur.rfind('/') {
                Some(i) => &cur[..i],
                None if !cur.is_empty() => "",
                None => return None,
            };
            if let Some(app) = self.records.iter().find(|r| r.id == parent).and_then(|r| r.app.clone()) {
                return Some(app);
            }
            cur = parent;
        }
    }

    /// 新提示符 / 附着的进程退出：丢掉 working 与 blocked（done / error 留着）
    pub fn drop_active(&mut self) -> bool {
        let before = self.records.len();
        self.records.retain(|r| !matches!(r.state, ProgramState::Working | ProgramState::Blocked));
        self.records.len() != before
    }

    /// 用户看过了：丢掉这些 id 的 done / error 记录；`ids` 为空表示全部
    pub fn seen(&mut self, ids: &[String]) -> bool {
        let before = self.records.len();
        self.records.retain(|r| {
            let finished = matches!(r.state, ProgramState::Done | ProgramState::Error);
            !(finished && (ids.is_empty() || ids.contains(&r.id)))
        });
        self.records.len() != before
    }

    /// RIS：清空全部
    pub fn reset(&mut self) -> bool {
        let changed = !self.records.is_empty();
        self.records.clear();
        changed
    }
}

/// 会话列表上显示的那一条：全部记录里最该让人看见的
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatusSummary {
    pub state: ProgramState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProgramBlockKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub updated_at: i64,
}

/// 状态的显眼程度：blocked（在等人）> error > working > done > idle
fn rank(state: ProgramState) -> u8 {
    match state {
        ProgramState::Blocked => 5,
        ProgramState::Error => 4,
        ProgramState::Working => 3,
        ProgramState::Done => 2,
        ProgramState::Idle => 1,
        ProgramState::Unknown => 0,
    }
}

/// 挑出最显眼的一条；同级时根记录（空 id，程序自己的状态）优先，再取最近更新的。全是 idle /
/// 没有记录时为 None——idle 不值得占地方。
///
/// 根记录优先：Claude Code 跑一轮时根记录报 working + 进度 + 正在干什么，后台子 agent 各报一条
/// working、title 是子任务描述；只按时间挑，谁后报谁上，摘要在"Editing src/main.rs 40%"和
/// "Explore the codebase"之间来回跳，进度也跟着丢。子任务只在更显眼时顶上来（卡在等人、出错）
pub fn summarize(records: &[ProgramStatusRecord]) -> Option<ProgramStatusSummary> {
    let best = records
        .iter()
        .filter(|r| rank(r.state) > rank(ProgramState::Idle))
        .max_by_key(|r| (rank(r.state), r.id.is_empty(), r.updated_at))?;
    Some(ProgramStatusSummary {
        state: best.state,
        kind: best.kind,
        progress: best.progress,
        app: best.app.clone(),
        title: best.title.clone(),
        message: best.message.clone(),
        updated_at: best.updated_at,
    })
}

/// 程序给的文本显示到终端之外之前（侧栏、toast）：去掉控制字符、方向覆盖与零宽格式字符
/// （规范：SHOULD disarm text direction overrides and invisible formatting characters），
/// 再按字符数截断。规范已要求终端拒收含控制字符的报告，这里再挡一道。
pub fn sanitize_display_text(s: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max_chars * 4));
    let mut n = 0;
    for c in s.chars() {
        let invisible = c.is_control()
            || matches!(c,
                '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}' | '\u{206A}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}');
        if invisible {
            continue;
        }
        if n == max_chars {
            out.push('…');
            break;
        }
        out.push(c);
        n += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: &str, state: Option<ProgramState>) -> ProgramStatusReport {
        ProgramStatusReport { id: id.into(), state, ..Default::default() }
    }

    #[test]
    fn a_report_replaces_its_record_completely() {
        let mut s = ProgramStatusStore::new();
        let mut r = report("", Some(ProgramState::Working));
        r.title = Some("构建".into());
        r.progress = Some(40);
        s.apply(r, 1);
        s.apply(report("", Some(ProgramState::Working)), 2);
        assert_eq!(s.records().len(), 1);
        assert_eq!(s.records()[0].title, None, "没带的键就是没有了");
        assert_eq!(s.records()[0].progress, None);
        assert_eq!(s.records()[0].updated_at, 2);
    }

    #[test]
    fn kind_only_for_blocked_and_progress_only_for_working_or_blocked() {
        let mut s = ProgramStatusStore::new();
        let mut r = report("a", Some(ProgramState::Done));
        r.kind = Some(ProgramBlockKind::Permission);
        r.progress = Some(50);
        s.apply(r, 1);
        assert_eq!((s.records()[0].kind, s.records()[0].progress), (None, None));
        let mut r = report("a", Some(ProgramState::Blocked));
        r.kind = Some(ProgramBlockKind::Question);
        r.progress = Some(101);
        s.apply(r, 2);
        assert_eq!((s.records()[0].kind, s.records()[0].progress), (Some(ProgramBlockKind::Question), None));
    }

    #[test]
    fn clear_removes_the_subtree_and_no_id_clears_all() {
        let mut s = ProgramStatusStore::new();
        for id in ["build", "build/test", "build/test/unit", "buildx", "lint"] {
            s.apply(report(id, Some(ProgramState::Working)), 1);
        }
        assert!(s.apply(report("build", None), 2));
        let ids: Vec<_> = s.records().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["buildx", "lint"], "build/… 被清，前缀相同但不在其下的 buildx 不动");
        assert!(s.apply(report("", None), 3));
        assert!(s.is_empty());
        assert!(!s.apply(report("", None), 4), "没有可清的就没有变化");
    }

    #[test]
    fn app_is_inherited_from_the_nearest_ancestor_that_has_one() {
        let mut s = ProgramStatusStore::new();
        let mut root = report("", Some(ProgramState::Working));
        root.app = Some("claude-code".into());
        s.apply(root, 1);
        let mut mid = report("a", Some(ProgramState::Working));
        mid.app = Some("cargo".into());
        s.apply(mid, 2);
        s.apply(report("a/b/c", Some(ProgramState::Working)), 3);
        s.apply(report("z", Some(ProgramState::Working)), 4);
        let app = |id: &str| s.records().iter().find(|r| r.id == id).and_then(|r| r.app.clone());
        assert_eq!(app("a/b/c").as_deref(), Some("cargo"), "跳过不存在的 a/b，落到 a");
        assert_eq!(app("z").as_deref(), Some("claude-code"));
    }

    #[test]
    fn evicts_the_least_recently_updated_when_full() {
        let mut s = ProgramStatusStore::new();
        for i in 0..MAX_RECORDS {
            s.apply(report(&format!("r{i}"), Some(ProgramState::Working)), i as i64);
        }
        // 更新 r0，它就不再是最旧的
        s.apply(report("r0", Some(ProgramState::Done)), 1000);
        s.apply(report("new", Some(ProgramState::Working)), 1001);
        assert_eq!(s.records().len(), MAX_RECORDS);
        assert!(s.records().iter().any(|r| r.id == "r0"));
        assert!(!s.records().iter().any(|r| r.id == "r1"), "r1 成了最旧的，被淘汰");
    }

    #[test]
    fn prompt_drops_working_and_blocked_but_keeps_done_and_error() {
        let mut s = ProgramStatusStore::new();
        s.apply(report("w", Some(ProgramState::Working)), 1);
        s.apply(report("b", Some(ProgramState::Blocked)), 1);
        s.apply(report("d", Some(ProgramState::Done)), 1);
        s.apply(report("e", Some(ProgramState::Error)), 1);
        s.apply(report("i", Some(ProgramState::Idle)), 1);
        assert!(s.drop_active());
        let ids: Vec<_> = s.records().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["d", "e", "i"]);
        assert!(s.seen(&["d".into()]));
        assert_eq!(s.records().len(), 2);
        assert!(s.seen(&[]));
        assert_eq!(s.records().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["i"]);
        assert!(s.reset());
        assert!(s.is_empty());
    }

    #[test]
    fn summary_prefers_blocked_then_error_then_working_then_done() {
        let mut s = ProgramStatusStore::new();
        s.apply(report("i", Some(ProgramState::Idle)), 9);
        assert_eq!(summarize(s.records()), None, "只有 idle 不显示");
        s.apply(report("d", Some(ProgramState::Done)), 1);
        s.apply(report("w", Some(ProgramState::Working)), 2);
        assert_eq!(summarize(s.records()).map(|x| x.state), Some(ProgramState::Working));
        s.apply(report("e", Some(ProgramState::Error)), 3);
        assert_eq!(summarize(s.records()).map(|x| x.state), Some(ProgramState::Error));
        let mut b = report("b", Some(ProgramState::Blocked));
        b.kind = Some(ProgramBlockKind::Permission);
        s.apply(b, 0);
        let sum = summarize(s.records()).unwrap();
        assert_eq!((sum.state, sum.kind), (ProgramState::Blocked, Some(ProgramBlockKind::Permission)));
    }

    #[test]
    fn summary_prefers_root_within_the_same_state() {
        let mut s = ProgramStatusStore::new();
        let mut root = report("", Some(ProgramState::Working));
        root.progress = Some(40);
        s.apply(root, 1);
        // 后报的子任务同为 working：根记录照样在上面（进度不丢）
        s.apply(report("agent-1", Some(ProgramState::Working)), 5);
        assert_eq!(summarize(s.records()).map(|x| x.progress), Some(Some(40)));
        // 子任务卡在等人：比 working 显眼，顶上来
        s.apply(report("agent-1", Some(ProgramState::Blocked)), 6);
        assert_eq!(summarize(s.records()).map(|x| x.state), Some(ProgramState::Blocked));
    }

    #[test]
    fn sanitize_strips_direction_overrides_and_truncates() {
        assert_eq!(sanitize_display_text("a\u{202E}b\u{200B}c\nd", 10), "abcd");
        assert_eq!(sanitize_display_text("一二三四五", 3), "一二三…");
        assert_eq!(sanitize_display_text("abc", 3), "abc");
    }

    #[test]
    fn wire_shape() {
        let r = ProgramStatusRecord {
            id: "a".into(),
            state: ProgramState::Blocked,
            kind: Some(ProgramBlockKind::Auth),
            progress: None,
            app: Some("x".into()),
            title: None,
            message: Some("m".into()),
            updated_at: 5,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, r#"{"id":"a","state":"blocked","kind":"auth","app":"x","message":"m","updatedAt":5}"#);
        let back: ProgramStatusRecord = serde_json::from_str(r#"{"id":"","state":"paused","updatedAt":1}"#).unwrap();
        assert_eq!(back.state, ProgramState::Unknown);
    }
}
