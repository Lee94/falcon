//! 程序状态（OSC 7501，ADR 0020 决定六）在界面上怎么显示、什么时候提醒。
//!
//! 数据两路来：`GET /api/sessions` 的每条 `Session.program_status`（服务端算好的摘要，没人看着的
//! 会话也有），以及开着窗口的会话经 WS 推来的记录全集（客户端自己 [`summarize`]，写回同一个
//! 字段）。界面只认那个摘要，所以这里的函数都吃 [`ProgramStatusSummary`]。
//!
//! 程序给的 `title` / `message` 是不可信文本：显示到终端之外（侧栏、标题栏的悬停提示、toast）
//! 之前一律过 [`sanitize_display_text`]，并且总带上会话的显示名（规范要求标明来源，免得一个
//! 程序冒充别的会话）——带名字是调用方的事，这里给出的只是程序那半句。

use std::collections::{HashMap, HashSet};

use falcon_proto::{ProgramBlockKind, ProgramState, ProgramStatusRecord, ProgramStatusSummary, sanitize_display_text};

pub use falcon_proto::summarize;

/// 程序那半句最多显示多少个字符（toast、悬停提示里）。侧栏一行放不下这么多，靠 truncate 截
pub const DETAIL_MAX_CHARS: usize = 160;

/// 徽标的语气：决定图标与颜色
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadgeTone {
    /// 在等用户（授权 / 回答 / 凭据）：最醒目
    Blocked,
    Error,
    Working,
    Done,
}

/// 一个会话的程序状态徽标
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramBadge {
    pub tone: BadgeTone,
    /// blocked 时要的是什么（图标按它挑）；其余状态为 None
    pub kind: Option<ProgramBlockKind>,
    /// 短标签的文案 key（`programStatus.*`）。blocked 按 kind 分开说——"在等你授权"和"在等你
    /// 回答"要做的事不一样，光看徽标就该知道
    pub label_key: &'static str,
    /// 更短的那个（两个字），只有 blocked 的胶囊里直接写出来；其余状态只画图标
    pub short_key: &'static str,
    /// working / blocked 的进度（0–100）；缺省 = 不确定进度
    pub progress: Option<u8>,
    /// 程序给的那半句（已消毒、已截断）；程序什么都没说时为 None
    pub detail: Option<String>,
}

/// 状态 → (语气, 短标签, 两个字)。idle 与认不出的状态不显示
fn tone_of(state: ProgramState, kind: Option<ProgramBlockKind>) -> Option<(BadgeTone, &'static str, &'static str)> {
    Some(match state {
        ProgramState::Blocked => (BadgeTone::Blocked, blocked_label_key(kind), blocked_short_key(kind)),
        ProgramState::Error => (BadgeTone::Error, "programStatus.error", "programStatus.short_error"),
        ProgramState::Working => (BadgeTone::Working, "programStatus.working", "programStatus.short_working"),
        ProgramState::Done => (BadgeTone::Done, "programStatus.done", "programStatus.short_done"),
        ProgramState::Idle | ProgramState::Unknown => return None,
    })
}

/// 摘要 → 徽标。idle 与认不出的状态不显示（服务端的摘要本来就不带 idle）
pub fn badge(summary: &ProgramStatusSummary) -> Option<ProgramBadge> {
    let (tone, label_key, short_key) = tone_of(summary.state, summary.kind)?;
    Some(ProgramBadge {
        tone,
        kind: summary.kind.filter(|_| tone == BadgeTone::Blocked),
        label_key,
        short_key,
        progress: summary.progress,
        detail: detail_text(summary, DETAIL_MAX_CHARS),
    })
}

/// blocked 胶囊里的两个字：授权 / 回答 / 凭据，认不出的 kind 只说"等你"
pub fn blocked_short_key(kind: Option<ProgramBlockKind>) -> &'static str {
    match kind {
        Some(ProgramBlockKind::Permission) => "programStatus.short_permission",
        Some(ProgramBlockKind::Question) => "programStatus.short_question",
        Some(ProgramBlockKind::Auth) => "programStatus.short_auth",
        Some(ProgramBlockKind::Unknown) | None => "programStatus.short_blocked",
    }
}

/// 所有可能用到的文案 key（界面的测试拿它对账 zh-CN.json，漏一个就是界面上露出 key 原文）
pub const ALL_TEXT_KEYS: &[&str] = &[
    "programStatus.blocked_permission",
    "programStatus.blocked_question",
    "programStatus.blocked_auth",
    "programStatus.blocked",
    "programStatus.short_permission",
    "programStatus.short_question",
    "programStatus.short_auth",
    "programStatus.short_blocked",
    "programStatus.error",
    "programStatus.short_error",
    "programStatus.working",
    "programStatus.short_working",
    "programStatus.done",
    "programStatus.short_done",
];

/// blocked 的短标签：按 kind 分。kind 缺省或认不出时只说"在等你"
pub fn blocked_label_key(kind: Option<ProgramBlockKind>) -> &'static str {
    match kind {
        Some(ProgramBlockKind::Permission) => "programStatus.blocked_permission",
        Some(ProgramBlockKind::Question) => "programStatus.blocked_question",
        Some(ProgramBlockKind::Auth) => "programStatus.blocked_auth",
        Some(ProgramBlockKind::Unknown) | None => "programStatus.blocked",
    }
}

/// 程序那半句：title 与 message 各自消毒后用 ` · ` 连起来（界面上拼接一律用它，不带语言），
/// 整体再截一次。两个都空时为 None
pub fn detail_text(summary: &ProgramStatusSummary, max_chars: usize) -> Option<String> {
    let clean = |s: &Option<String>| {
        s.as_deref().map(|s| sanitize_display_text(s, max_chars)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
    };
    let joined = match (clean(&summary.title), clean(&summary.message)) {
        (Some(t), Some(m)) => format!("{t} · {m}"),
        (Some(x), None) | (None, Some(x)) => x,
        (None, None) => return None,
    };
    // 两段各自截过，拼起来可能还超；拼好的串已经消过毒，再截一次不会引入新字符
    Some(sanitize_display_text(&joined, max_chars))
}

/// 终端标题（OSC 0 / 2）当成「手上在干什么」的一句话，放进标题栏胶囊的悬停提示。
///
/// 为什么借终端标题：Claude Code（2.1.299 实测）的根记录只在 blocked / done 时带 msg，working
/// 时什么都不说（title 从来不填），悬停提示里只剩"运行中 + 计时"；而它一直把会话的话题写在终端
/// 标题里（`✳ 话题`，跑的时候前缀在 ◐ / ◑ 之间每 960ms 换一次）。别的程序没设标题时，shell 的
/// preexec 通常写的是正在跑的命令，也算一句简述。
///
/// 标题同样是程序给的不可信文本：消毒、截断。打头的状态符号（✳ ◐ 这类符号区的字符、盲文点阵、
/// emoji）去掉——它是给标签页看的动画，提示里只剩一帧没意义。去掉后与程序名一样的不要：Claude
/// Code 还没生成话题时标题就是 "Claude Code"，说了等于没说
pub fn activity_text(title: &str, app: Option<&str>) -> Option<String> {
    let clean = sanitize_display_text(title, DETAIL_MAX_CHARS);
    let text = clean.trim_start_matches(|c: char| c.is_whitespace() || is_status_glyph(c)).trim_end();
    if text.is_empty() {
        return None;
    }
    let squash = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    if app.is_some_and(|app| squash(app) == squash(text)) {
        return None;
    }
    Some(text.to_string())
}

/// 标签页标题前面那种状态符号：箭头 / 技术符号 / 几何图形 / 杂项符号 / 装饰符号 / 盲文点阵
/// （U+2190–U+2BFF）与 emoji（U+1F000–U+1FAFF），再加上 `*` 与 `·`
fn is_status_glyph(c: char) -> bool {
    matches!(c, '\u{2190}'..='\u{2BFF}' | '\u{1F000}'..='\u{1FAFF}' | '*' | '·')
}

/// 终端标题栏上的一个子任务（胶囊旁边计数，悬停列出）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubTask {
    pub tone: BadgeTone,
    pub label_key: &'static str,
    /// 程序给的那半句（已消毒、已截断）
    pub detail: Option<String>,
}

/// 子任务：根记录（空 id）以外、不是 idle 的记录，按 id 排（规范里 id 是 `/` 分隔的路径，排一下
/// 同一棵子树就挨在一起）。Claude Code 把每个后台子 agent 报成一条，title 是它的任务描述
pub fn subtasks(records: &[ProgramStatusRecord]) -> Vec<SubTask> {
    let mut rows: Vec<&ProgramStatusRecord> = records.iter().filter(|r| !r.id.is_empty()).collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows.into_iter()
        .filter_map(|r| {
            let (tone, label_key, _) = tone_of(r.state, r.kind)?;
            let as_summary = ProgramStatusSummary {
                state: r.state,
                kind: r.kind,
                progress: r.progress,
                app: None,
                title: r.title.clone(),
                message: r.message.clone(),
                updated_at: r.updated_at,
            };
            Some(SubTask { tone, label_key, detail: detail_text(&as_summary, DETAIL_MAX_CHARS) })
        })
        .collect()
}

/// 终端标题栏上的计时读数（毫秒）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockReading {
    /// 这一轮从开始跑到现在（中间卡在 blocked 等人的时间也算，那也是这一轮）
    Running(u64),
    /// 卡在 blocked 多久了
    Waiting(u64),
    /// 刚跑完的那一轮一共用了多久
    Took(u64),
}

impl ClockReading {
    /// 读数会随时间涨（界面要每秒重画）
    pub fn ticking(self) -> bool {
        !matches!(self, ClockReading::Took(_))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ClockEntry {
    state: Option<ProgramState>,
    /// blocked 时要的是什么：换了一样（等回答 → 等授权）是新的一次等待，重新计时
    kind: Option<ProgramBlockKind>,
    /// 进入当前状态的时刻；None = 第一次见它就已经是这样了，不知道从什么时候开始
    since: Option<u64>,
    /// 这一轮（working / blocked 连着算）开始的时刻
    run_start: Option<u64>,
    /// 上一轮跑完用了多久
    last_run: Option<u64>,
    /// 第一次见它时摘要的 `updated_at`（宿主机墙钟）：`since` 不知道时，侧栏拿它估 blocked / done /
    /// error 进这个状态多久了。程序重报同一状态会刷新它，所以只会估短、不会估长；working 不用它——
    /// Claude Code 跑一轮时一直在重报，它永远是几秒前
    first_reported: Option<i64>,
    /// 本客户端看着它从 done / error 被清掉（用户看过了，或程序自己收了状态）：侧栏那一行还挂一个
    /// 淡的勾，到下一轮开始为止
    cleared_finish: Option<ClearedFinish>,
}

#[derive(Clone, Copy, Debug)]
struct ClearedFinish {
    error: bool,
    /// 被清掉之前那个 done / error 的 `since` / `first_reported`：「看过了」的时长照旧从跑完算
    since: Option<u64>,
    first_reported: Option<i64>,
}

/// 侧栏会话行尾巴上的那一格是哪一档
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus {
    Working,
    /// blocked：在等你
    NeedsYou,
    /// 跑完了、还没看（done / error 还挂着，等 `status_seen` 清）
    Unread { error: bool },
    /// 跑完了、看过了（本客户端看着它被清掉的；启动前就清掉了的认不出，算 Idle）
    Read { error: bool },
    /// 没报状态、最近操作过
    Idle,
    /// 很久没人操作（[`INACTIVE_AFTER_MS`]）。只盖过 Idle / Read：在跑、在等你、跑完没看的再久也照实说
    Inactive,
}

/// 侧栏会话行尾巴上的读数
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowReading {
    pub status: RowStatus,
    /// 进这一档多久了（毫秒）：working 从这一轮开始、在等你从卡住、跑完的从跑完、Idle / Inactive
    /// 从最后一次操作算。不知道时为 None，界面就不写
    pub age: Option<u64>,
}

/// 多久没操作算「很久没碰」（会话的 `last_active_at`：服务端在接上窗口、敲键时记）
pub const INACTIVE_AFTER_MS: i64 = 24 * 60 * 60 * 1000;

/// 终端标题栏上的计时：各会话的程序状态是**本客户端亲眼看到**从什么时候变的。
///
/// 宿主机给的 `updated_at` 是"最后一次报告"的时间，不是"开始"的时间：Claude Code 跑一轮时会
/// 反复重报 working（msg 在变），拿它算"已运行多久"永远是几秒。所以开始时刻只能自己记——
/// 代价是启动 / 刷新时已经在跑的那一轮不知道从什么时候开始，**宁可不显示也不编一个**。
/// 时间用调用方给的单调毫秒数（界面层的 `Instant`），不碰墙钟，测试也好喂。
#[derive(Debug, Default)]
pub struct StateClock {
    entries: HashMap<String, ClockEntry>,
}

fn in_run(state: Option<ProgramState>) -> bool {
    matches!(state, Some(ProgramState::Working | ProgramState::Blocked))
}

impl StateClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一个会话的最新摘要
    pub fn observe(&mut self, id: &str, summary: Option<&ProgramStatusSummary>, now: u64) {
        let state = summary.map(|s| s.state).filter(|s| !matches!(s, ProgramState::Idle | ProgramState::Unknown));
        let kind = summary.and_then(|s| s.kind).filter(|_| state == Some(ProgramState::Blocked));
        let Some(e) = self.entries.get_mut(id) else {
            let first_reported = summary.map(|s| s.updated_at);
            self.entries.insert(id.to_string(), ClockEntry { state, kind, first_reported, ..Default::default() });
            return;
        };
        if e.state == state && e.kind == kind {
            return;
        }
        let prev = e.state;
        // 跑完的那一条被清掉：记下来给侧栏画「看过了」。别的变化（下一轮开始、跑到一半被打断后
        // 收了状态）都把它作废——一个早先的勾挂在被打断的那一轮后面是在撒谎
        e.cleared_finish = match prev {
            Some(p @ (ProgramState::Done | ProgramState::Error)) if state.is_none() => Some(ClearedFinish {
                error: p == ProgramState::Error,
                since: e.since,
                first_reported: e.first_reported,
            }),
            _ => None,
        };
        e.state = state;
        e.kind = kind;
        e.since = Some(now);
        match state {
            Some(ProgramState::Working | ProgramState::Blocked) => {
                if !in_run(prev) {
                    e.run_start = Some(now);
                }
                e.last_run = None;
            }
            Some(ProgramState::Done | ProgramState::Error) => {
                // done ↔ error 之间来回不算新的一轮，用时照旧
                if in_run(prev) {
                    e.last_run = e.run_start.map(|r| now.saturating_sub(r));
                }
                e.run_start = None;
            }
            _ => {
                e.run_start = None;
                e.last_run = None;
            }
        }
    }

    /// 整张会话列表：逐个 [`Self::observe`]，列表里没有的会话忘掉
    pub fn observe_all<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = (&'a str, Option<&'a ProgramStatusSummary>)>,
        now: u64,
    ) {
        let mut seen = HashSet::new();
        for (id, summary) in sessions {
            seen.insert(id.to_string());
            self.observe(id, summary, now);
        }
        self.entries.retain(|id, _| seen.contains(id));
    }

    pub fn reading(&self, id: &str, now: u64) -> Option<ClockReading> {
        let e = self.entries.get(id)?;
        match e.state? {
            ProgramState::Working => e.run_start.map(|r| ClockReading::Running(now.saturating_sub(r))),
            ProgramState::Blocked => e.since.map(|s| ClockReading::Waiting(now.saturating_sub(s))),
            ProgramState::Done | ProgramState::Error => e.last_run.map(ClockReading::Took),
            ProgramState::Idle | ProgramState::Unknown => None,
        }
    }

    /// 侧栏会话行的读数。`summary` 是会话眼下的摘要（与喂给 [`Self::observe`] 的同一份），
    /// `last_active_at` / `wall_now` 是墙钟毫秒，`now` 是与 observe 同一套的单调毫秒
    pub fn row(
        &self,
        id: &str,
        summary: Option<&ProgramStatusSummary>,
        last_active_at: i64,
        now: u64,
        wall_now: i64,
    ) -> RowReading {
        // 墙钟差：宿主机 / 服务端与本机的钟可能差一点，倒着走的按「刚刚」算
        let wall_age = |t: i64| (wall_now - t).max(0) as u64;
        let held = |since: Option<u64>, first: Option<i64>| since.map(|s| now.saturating_sub(s)).or(first.map(wall_age));
        let e = self.entries.get(id);
        // 还没 observe 过（不该发生，防一手）：当第一次见
        let (since, first) = match e {
            Some(e) => (e.since, e.first_reported),
            None => (None, summary.map(|s| s.updated_at)),
        };
        let (status, age) = match summary.map(|s| s.state) {
            Some(ProgramState::Working) => {
                (RowStatus::Working, e.and_then(|e| e.run_start).map(|r| now.saturating_sub(r)))
            }
            Some(ProgramState::Blocked) => (RowStatus::NeedsYou, held(since, first)),
            Some(s @ (ProgramState::Done | ProgramState::Error)) => {
                (RowStatus::Unread { error: s == ProgramState::Error }, held(since, first))
            }
            Some(ProgramState::Idle | ProgramState::Unknown) | None => {
                let idle = wall_age(last_active_at);
                match e.and_then(|e| e.cleared_finish) {
                    _ if idle >= INACTIVE_AFTER_MS as u64 => (RowStatus::Inactive, Some(idle)),
                    Some(f) => (RowStatus::Read { error: f.error }, held(f.since, f.first_reported)),
                    None => (RowStatus::Idle, Some(idle)),
                }
            }
        };
        RowReading { status, age }
    }
}

/// 时长写成 `m:ss`，一小时以上 `h:mm:ss`（与语言无关，界面直接用）
pub fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// 记录全集里有没有用户看过就该清掉的（done / error）。决定窗口获得焦点、用户在里面操作时
/// 值不值得发 `status_seen`——没有可清的就别打扰服务端
pub fn has_finished(records: &[ProgramStatusRecord]) -> bool {
    records.iter().any(|r| matches!(r.state, ProgramState::Done | ProgramState::Error))
}

/// 用于判断"是不是同一条 blocked"：摘要里没有记录 id，用内容代替
type BlockedKey = (Option<ProgramBlockKind>, Option<String>, Option<String>);

#[derive(Clone, Debug)]
struct Alerted {
    key: BlockedKey,
    updated_at: i64,
}

/// blocked 提醒的去重：**同一条记录只提醒一次**。
///
/// 两路数据（5s 轮询的列表、WS 实时推）都往这里报，各会话最近一次提醒过的那条记在
/// `alerted` 里，离开 blocked 也不删——轮询的响应可能比 WS 推送晚到，带着已经过时的 blocked
/// 回来，按 `updated_at`（宿主机时钟，同一会话内单调）认出是旧的、不再提醒。规则：
///
/// - 进入 blocked（之前不在 blocked）：比上次提醒的新就提醒；
/// - 一直在 blocked：内容（kind / title / message）变了才算新的一条（程序重发同一条报告只是
///   刷新时间戳，不该再响一次）；
/// - 第一轮（启动、刷新页面后头一次拿到列表）只记不提醒：已经卡着的会话在侧栏上看得见，
///   一开窗口就冒一串通知只会让人烦。
///
/// 已知的取舍：没开窗口的会话只靠 5s 轮询，两次轮询之间「答完一条、又卡在内容一模一样的下一条」
/// 会被当成同一条（中间那次离开 blocked 没被看见）；开着窗口的会话每次变化都有推送，没有这个问题。
#[derive(Debug, Default)]
pub struct BlockedAlerts {
    primed: bool,
    alerted: HashMap<String, Alerted>,
    /// 当前处于 blocked 的会话
    blocked: HashSet<String>,
}

/// [`BlockedAlerts::observe`] 的结论
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertChange {
    /// 新的一条 blocked：该提醒
    Blocked,
    /// 刚离开 blocked：先前那条提醒可以撤了
    Resolved,
    Unchanged,
}

impl BlockedAlerts {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一个会话的最新摘要
    pub fn observe(&mut self, id: &str, summary: Option<&ProgramStatusSummary>) -> AlertChange {
        let Some(s) = summary.filter(|s| s.state == ProgramState::Blocked) else {
            return if self.blocked.remove(id) { AlertChange::Resolved } else { AlertChange::Unchanged };
        };
        let key: BlockedKey = (s.kind, s.title.clone(), s.message.clone());
        let was_blocked = !self.blocked.insert(id.to_string());
        let fresh = match self.alerted.get_mut(id) {
            // 不比上次提醒的新：过时的轮询结果，或者就是那一条
            Some(prev) if prev.updated_at >= s.updated_at => false,
            // 一直卡着、内容没变：程序重发了同一条
            Some(prev) if was_blocked && prev.key == key => {
                prev.updated_at = s.updated_at;
                false
            }
            _ => true,
        };
        if !fresh {
            return AlertChange::Unchanged;
        }
        self.alerted.insert(id.to_string(), Alerted { key, updated_at: s.updated_at });
        if self.primed { AlertChange::Blocked } else { AlertChange::Unchanged }
    }

    /// 整张会话列表：逐个 [`Self::observe`]，列表里没有的会话忘掉。第一次调用之后才开始提醒。
    /// 返回有变化的 `(会话 id, 变化)`
    pub fn observe_all<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = (&'a str, Option<&'a ProgramStatusSummary>)>,
    ) -> Vec<(String, AlertChange)> {
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for (id, summary) in sessions {
            seen.insert(id.to_string());
            let change = self.observe(id, summary);
            if change != AlertChange::Unchanged {
                changes.push((id.to_string(), change));
            }
        }
        self.alerted.retain(|id, _| seen.contains(id));
        let before = self.blocked.clone();
        self.blocked.retain(|id| seen.contains(id));
        // 会话没了（终止 / 清除）：那条提醒同样该撤
        changes.extend(before.difference(&self.blocked).map(|id| (id.clone(), AlertChange::Resolved)));
        self.primed = true;
        changes
    }
}

/// 「任务完成」提示音的判定：会话的摘要**进入** done / error（之前不是）时响一声。
///
/// 和 [`BlockedAlerts`] 同一套两路数据、同一个第一轮只记不响，规则却不同：**停在 done / error 里
/// 怎么变都不再响**。Claude Code（2.1.296 实测）一轮跑完报 `done`，之后一直停在 done，只把 msg
/// 换成稍后才生成的本轮摘要，直到下一轮开始才变回 working；按内容区分"新的一条"就会一轮响两次。
/// done 与 error 之间来回同理：都是"跑完了、该来看看"，error 不另响。
///
/// 时间戳照样要认：WS 上 working → done 已经响过，用户看了（`status_seen` 清掉 done），晚到的
/// 轮询还带着那条 done，看起来又"进入"了一次。各会话记着最近一次响过的那条的 `updated_at`，
/// 停在 done 里时跟着往前推（msg 换了，时间戳也换了），不比它新的一律不响。
#[derive(Debug, Default)]
pub struct FinishChimes {
    primed: bool,
    chimed_at: HashMap<String, i64>,
    /// 当前处于 done / error 的会话
    finished: HashSet<String>,
}

impl FinishChimes {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一个会话的最新摘要；该响时返回 true
    pub fn observe(&mut self, id: &str, summary: Option<&ProgramStatusSummary>) -> bool {
        let Some(s) = summary.filter(|s| matches!(s.state, ProgramState::Done | ProgramState::Error)) else {
            self.finished.remove(id);
            return false;
        };
        let entering = self.finished.insert(id.to_string());
        let last = self.chimed_at.entry(id.to_string()).or_insert(i64::MIN);
        let fresh = s.updated_at > *last;
        *last = (*last).max(s.updated_at);
        entering && fresh && self.primed
    }

    /// 整张会话列表：逐个 [`Self::observe`]，列表里没有的会话忘掉。第一次调用之后才开始响。
    /// 返回该响的会话 id
    pub fn observe_all<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = (&'a str, Option<&'a ProgramStatusSummary>)>,
    ) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut chimes = Vec::new();
        for (id, summary) in sessions {
            seen.insert(id.to_string());
            if self.observe(id, summary) {
                chimes.push(id.to_string());
            }
        }
        self.chimed_at.retain(|id, _| seen.contains(id));
        self.finished.retain(|id| seen.contains(id));
        self.primed = true;
        chimes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(state: ProgramState, updated_at: i64) -> ProgramStatusSummary {
        ProgramStatusSummary { state, kind: None, progress: None, app: None, title: None, message: None, updated_at }
    }

    fn blocked(kind: ProgramBlockKind, title: &str, updated_at: i64) -> ProgramStatusSummary {
        ProgramStatusSummary {
            kind: Some(kind),
            title: Some(title.into()),
            ..summary(ProgramState::Blocked, updated_at)
        }
    }

    #[test]
    fn badge_per_state_and_kind() {
        assert_eq!(badge(&summary(ProgramState::Idle, 1)), None);
        assert_eq!(badge(&summary(ProgramState::Unknown, 1)), None);
        let b = badge(&blocked(ProgramBlockKind::Permission, "Allow Bash?", 1)).unwrap();
        assert_eq!((b.tone, b.label_key, b.short_key), (BadgeTone::Blocked, "programStatus.blocked_permission", "programStatus.short_permission"));
        assert_eq!(blocked_short_key(Some(ProgramBlockKind::Auth)), "programStatus.short_auth");
        for k in [b.label_key, b.short_key] {
            assert!(ALL_TEXT_KEYS.contains(&k));
        }
        assert_eq!(b.detail.as_deref(), Some("Allow Bash?"));
        assert_eq!(blocked_label_key(Some(ProgramBlockKind::Question)), "programStatus.blocked_question");
        assert_eq!(blocked_label_key(Some(ProgramBlockKind::Auth)), "programStatus.blocked_auth");
        assert_eq!(blocked_label_key(None), "programStatus.blocked");
        let w = badge(&ProgramStatusSummary { progress: Some(40), ..summary(ProgramState::Working, 1) }).unwrap();
        assert_eq!((w.tone, w.progress, w.detail), (BadgeTone::Working, Some(40), None));
        assert_eq!(badge(&summary(ProgramState::Done, 1)).unwrap().tone, BadgeTone::Done);
        assert_eq!(badge(&summary(ProgramState::Error, 1)).unwrap().label_key, "programStatus.error");
    }

    #[test]
    fn detail_is_sanitized_joined_and_truncated() {
        let s = ProgramStatusSummary {
            title: Some("\u{202E}Allow\u{200B} edit".into()),
            message: Some("src/main.rs\n".into()),
            ..summary(ProgramState::Blocked, 1)
        };
        assert_eq!(detail_text(&s, 100).as_deref(), Some("Allow edit · src/main.rs"));
        assert_eq!(detail_text(&s, 5).as_deref(), Some("Allow…"));
        let blank = ProgramStatusSummary { title: Some("\u{200B} ".into()), ..summary(ProgramState::Done, 1) };
        assert_eq!(detail_text(&blank, 100), None, "只剩不可见字符和空白就当没说");
    }

    #[test]
    fn finished_records() {
        let rec = |state| ProgramStatusRecord {
            id: String::new(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: None,
            updated_at: 1,
        };
        assert!(!has_finished(&[rec(ProgramState::Working), rec(ProgramState::Blocked)]));
        assert!(has_finished(&[rec(ProgramState::Working), rec(ProgramState::Done)]));
        assert!(has_finished(&[rec(ProgramState::Error)]));
    }

    #[test]
    fn first_round_only_primes() {
        let mut a = BlockedAlerts::new();
        let b = blocked(ProgramBlockKind::Permission, "x", 100);
        assert!(a.observe_all([("s1", Some(&b))]).is_empty(), "启动时已经卡着的不提醒");
        // 同一条再来不提醒；离开后再进来的新一条提醒
        assert!(a.observe_all([("s1", Some(&b))]).is_empty());
        let w = summary(ProgramState::Working, 110);
        assert_eq!(a.observe_all([("s1", Some(&w))]), vec![("s1".to_string(), AlertChange::Resolved)]);
        let b2 = blocked(ProgramBlockKind::Permission, "x", 120);
        assert_eq!(a.observe_all([("s1", Some(&b2))]), vec![("s1".to_string(), AlertChange::Blocked)]);
    }

    #[test]
    fn same_record_is_alerted_once() {
        let mut a = BlockedAlerts::new();
        a.observe_all(std::iter::empty());
        assert_eq!(a.observe("s1", Some(&blocked(ProgramBlockKind::Question, "q", 100))), AlertChange::Blocked);
        // 程序重发同一条：只刷新时间戳
        assert_eq!(a.observe("s1", Some(&blocked(ProgramBlockKind::Question, "q", 105))), AlertChange::Unchanged);
        // 还卡着，换了一条（内容不同、更新）：提醒
        assert_eq!(a.observe("s1", Some(&blocked(ProgramBlockKind::Permission, "p", 110))), AlertChange::Blocked);
    }

    #[test]
    fn stale_poll_after_ws_does_not_alert_again() {
        let mut a = BlockedAlerts::new();
        a.observe_all(std::iter::empty());
        let b = blocked(ProgramBlockKind::Permission, "x", 100);
        assert_eq!(a.observe("s1", Some(&b)), AlertChange::Blocked);
        // WS：用户答完了
        assert_eq!(a.observe("s1", None), AlertChange::Resolved);
        // 晚到的轮询还带着那条 blocked
        assert_eq!(a.observe_all([("s1", Some(&b))]), vec![]);
        // 再晚一点的轮询对上了
        assert_eq!(a.observe_all([("s1", None)]), vec![("s1".to_string(), AlertChange::Resolved)]);
        // 过时的不同内容同样不算
        assert_eq!(a.observe("s1", Some(&blocked(ProgramBlockKind::Auth, "y", 90))), AlertChange::Unchanged);
    }

    #[test]
    fn sessions_that_disappear_are_resolved_and_forgotten() {
        let mut a = BlockedAlerts::new();
        a.observe_all(std::iter::empty());
        let b = blocked(ProgramBlockKind::Permission, "x", 100);
        assert_eq!(a.observe("s1", Some(&b)), AlertChange::Blocked);
        assert_eq!(a.observe_all([("s2", None)]), vec![("s1".to_string(), AlertChange::Resolved)]);
        // 忘干净了：同 id 的会话（不会真的发生）再来同一条也当新的
        assert_eq!(a.observe("s1", Some(&b)), AlertChange::Blocked);
    }

    fn done(msg: Option<&str>, updated_at: i64) -> ProgramStatusSummary {
        ProgramStatusSummary { message: msg.map(str::to_string), ..summary(ProgramState::Done, updated_at) }
    }

    #[test]
    fn chime_on_entering_done_only() {
        let mut c = FinishChimes::new();
        c.observe_all(std::iter::empty());
        assert!(!c.observe("s1", Some(&summary(ProgramState::Working, 100))));
        assert!(c.observe("s1", Some(&done(None, 110))), "working → done 响");
        // Claude Code 停在 done 里补上本轮摘要：不再响
        assert!(!c.observe("s1", Some(&done(Some("Fixed the bug"), 115))));
        // done → error 也算还在"跑完了"里
        assert!(!c.observe("s1", Some(&summary(ProgramState::Error, 116))));
        // 下一轮
        assert!(!c.observe("s1", Some(&summary(ProgramState::Working, 200))));
        assert!(c.observe("s1", Some(&summary(ProgramState::Error, 210))), "working → error 同样响");
        // blocked 不是跑完
        assert!(!c.observe("s1", Some(&summary(ProgramState::Blocked, 300))));
        assert!(c.observe("s1", Some(&done(None, 310))), "blocked → done 响");
    }

    #[test]
    fn chime_first_round_only_primes() {
        let mut c = FinishChimes::new();
        assert!(!c.observe("s1", Some(&done(None, 100))), "第一轮之前的 WS 推送只记");
        assert!(c.observe_all([("s1", Some(&done(None, 100))), ("s2", Some(&done(None, 100)))]).is_empty(), "启动时已经跑完的不响");
        assert_eq!(c.observe_all([("s1", None), ("s2", Some(&done(None, 100)))]), Vec::<String>::new());
        assert_eq!(c.observe_all([("s1", Some(&done(None, 120)))]), vec!["s1".to_string()]);
    }

    #[test]
    fn chime_ignores_stale_poll_after_seen() {
        let mut c = FinishChimes::new();
        c.observe_all(std::iter::empty());
        assert!(c.observe("s1", Some(&done(None, 100))));
        assert!(!c.observe("s1", Some(&done(Some("summary"), 105))));
        // 用户看过，WS 推来清掉之后的全集
        assert!(!c.observe("s1", None));
        // 晚到的轮询还带着 done（msg 已换、时间戳 105）：不是新的
        assert!(c.observe_all([("s1", Some(&done(Some("summary"), 105)))]).is_empty());
        assert!(c.observe_all([("s1", None)]).is_empty());
        // 真正的下一轮
        assert_eq!(c.observe_all([("s1", Some(&done(None, 130)))]), vec!["s1".to_string()]);
    }

    #[test]
    fn chime_forgets_sessions_that_disappear() {
        let mut c = FinishChimes::new();
        c.observe_all(std::iter::empty());
        assert!(c.observe("s1", Some(&done(None, 100))));
        assert!(c.observe_all([("s2", None)]).is_empty());
        // 忘干净了：同 id 再来当新的（不会真的发生，只是不留垃圾）
        assert!(c.observe("s1", Some(&done(None, 100))));
    }

    fn rec(id: &str, state: ProgramState, title: Option<&str>) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: id.into(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: title.map(str::to_string),
            message: None,
            updated_at: 1,
        }
    }

    #[test]
    fn subtasks_skip_root_and_idle_and_sort_by_id() {
        let records = vec![
            rec("", ProgramState::Working, Some("main")),
            rec("b", ProgramState::Blocked, Some("\u{202E}review")),
            rec("a", ProgramState::Working, Some("explore")),
            rec("c", ProgramState::Idle, Some("idle")),
        ];
        let t = subtasks(&records);
        assert_eq!(t.len(), 2);
        assert_eq!((t[0].tone, t[0].label_key, t[0].detail.as_deref()), (BadgeTone::Working, "programStatus.working", Some("explore")));
        assert_eq!((t[1].tone, t[1].detail.as_deref()), (BadgeTone::Blocked, Some("review")), "消过毒");
        assert!(subtasks(&[rec("", ProgramState::Done, None)]).is_empty());
    }

    #[test]
    fn activity_text_strips_status_glyphs_and_program_name() {
        let app = Some("claude-code");
        // Claude Code 空闲时 ✳、跑的时候 ◐ ◑ 来回换；别的程序有用盲文点阵转圈的
        assert_eq!(activity_text("✳ 去掉标题栏的计时", app).as_deref(), Some("去掉标题栏的计时"));
        assert_eq!(activity_text("◑ Fix tooltip timer", app).as_deref(), Some("Fix tooltip timer"));
        assert_eq!(activity_text("⠐ npm test", None).as_deref(), Some("npm test"));
        assert_eq!(activity_text("🤖 * ship it ", app).as_deref(), Some("ship it"));
        // 还没生成话题：标题就是程序名
        assert_eq!(activity_text("✳ Claude Code", app), None);
        assert_eq!(activity_text("Claude Code", None).as_deref(), Some("Claude Code"));
        // shell 写的命令照样算；空的、只有符号的不要
        assert_eq!(activity_text("cargo build", None).as_deref(), Some("cargo build"));
        assert_eq!(activity_text(" ✳ ", app), None);
        // 消毒：方向覆盖去掉
        assert_eq!(activity_text("a\u{202E}b", None).as_deref(), Some("ab"));
        // 打头的普通标点留着
        assert_eq!(activity_text("(wip) x", None).as_deref(), Some("(wip) x"));
    }

    #[test]
    fn clock_unknown_start_is_not_invented() {
        let mut c = StateClock::new();
        // 第一次见就已经在跑：不知道从什么时候开始
        c.observe("s1", Some(&summary(ProgramState::Working, 1)), 1_000);
        assert_eq!(c.reading("s1", 5_000), None);
        // 之后亲眼看到它跑完：用时也不知道
        c.observe("s1", Some(&summary(ProgramState::Done, 2)), 6_000);
        assert_eq!(c.reading("s1", 7_000), None);
        assert_eq!(c.reading("nope", 7_000), None);
    }

    #[test]
    fn clock_runs_waits_and_reports_took() {
        let mut c = StateClock::new();
        c.observe("s1", None, 0);
        c.observe("s1", Some(&summary(ProgramState::Working, 1)), 1_000);
        assert_eq!(c.reading("s1", 4_000), Some(ClockReading::Running(3_000)));
        // 重报 working（msg 变了）：不重新计时
        c.observe("s1", Some(&ProgramStatusSummary { message: Some("x".into()), ..summary(ProgramState::Working, 2) }), 5_000);
        assert_eq!(c.reading("s1", 6_000), Some(ClockReading::Running(5_000)));
        // 卡住等人：显示等了多久
        c.observe("s1", Some(&summary(ProgramState::Blocked, 3)), 7_000);
        assert_eq!(c.reading("s1", 9_500), Some(ClockReading::Waiting(2_500)));
        // 还卡着，但换了一样要的东西：重新计时（这一轮照旧）
        c.observe("s1", Some(&blocked(ProgramBlockKind::Permission, "p", 4)), 9_000);
        assert_eq!(c.reading("s1", 9_500), Some(ClockReading::Waiting(500)));
        c.observe("s1", Some(&blocked(ProgramBlockKind::Permission, "p2", 5)), 9_200);
        assert_eq!(c.reading("s1", 9_500), Some(ClockReading::Waiting(500)), "同一样东西只是换了说法：不重新计时");
        // 答完接着跑：这一轮从最开始算
        c.observe("s1", Some(&summary(ProgramState::Working, 4)), 10_000);
        assert_eq!(c.reading("s1", 10_000), Some(ClockReading::Running(9_000)));
        c.observe("s1", Some(&summary(ProgramState::Done, 5)), 31_000);
        assert_eq!(c.reading("s1", 99_000), Some(ClockReading::Took(30_000)));
        // done → error 不是新的一轮
        c.observe("s1", Some(&summary(ProgramState::Error, 6)), 32_000);
        assert_eq!(c.reading("s1", 99_000), Some(ClockReading::Took(30_000)));
        // 看过、清掉
        c.observe("s1", None, 40_000);
        assert_eq!(c.reading("s1", 41_000), None);
        assert!(ClockReading::Running(1).ticking() && ClockReading::Waiting(1).ticking() && !ClockReading::Took(1).ticking());
    }

    #[test]
    fn clock_forgets_sessions_that_disappear() {
        let mut c = StateClock::new();
        c.observe_all([("s1", None)], 0);
        c.observe_all([("s1", Some(&summary(ProgramState::Working, 1)))], 1_000);
        assert_eq!(c.reading("s1", 2_000), Some(ClockReading::Running(1_000)));
        c.observe_all([("s2", None)], 3_000);
        assert_eq!(c.reading("s1", 4_000), None);
        // 忘了之后再出现：当第一次见
        c.observe_all([("s1", Some(&summary(ProgramState::Working, 1)))], 5_000);
        assert_eq!(c.reading("s1", 6_000), None);
    }

    /// 墙钟基准：会话在这一刻操作过，测试的墙钟从这里往后数
    const WALL: i64 = 1_000_000_000_000;

    fn row_of(c: &StateClock, s: Option<&ProgramStatusSummary>, now: u64, wall_now: i64) -> RowReading {
        c.row("s1", s, WALL, now, wall_now)
    }

    #[test]
    fn row_ages_come_from_observed_transitions() {
        let mut c = StateClock::new();
        c.observe("s1", None, 0);
        let w = summary(ProgramState::Working, WALL + 1);
        c.observe("s1", Some(&w), 1_000);
        assert_eq!(row_of(&c, Some(&w), 61_000, WALL), RowReading { status: RowStatus::Working, age: Some(60_000) });
        let b = blocked(ProgramBlockKind::Question, "q", WALL + 2);
        c.observe("s1", Some(&b), 70_000);
        assert_eq!(row_of(&c, Some(&b), 75_000, WALL), RowReading { status: RowStatus::NeedsYou, age: Some(5_000) });
        let d = done(None, WALL + 3);
        c.observe("s1", Some(&d), 80_000);
        assert_eq!(row_of(&c, Some(&d), 320_000, WALL), RowReading { status: RowStatus::Unread { error: false }, age: Some(240_000) });
        // 看过、清掉：勾还在，时长照旧从跑完算
        c.observe("s1", None, 400_000);
        assert_eq!(row_of(&c, None, 800_000, WALL), RowReading { status: RowStatus::Read { error: false }, age: Some(720_000) });
        // 下一轮开始：勾没了
        c.observe("s1", Some(&w), 900_000);
        assert_eq!(row_of(&c, Some(&w), 900_000, WALL).status, RowStatus::Working);
        // 跑到一半收了状态（被打断）：不拿上一轮的勾冒充
        c.observe("s1", None, 950_000);
        assert_eq!(row_of(&c, None, 950_000, WALL + 3_000), RowReading { status: RowStatus::Idle, age: Some(3_000) });
    }

    #[test]
    fn row_first_seen_falls_back_to_reported_time_except_working() {
        let mut c = StateClock::new();
        // 启动时已经在跑：不知道从什么时候开始，不写
        let w = summary(ProgramState::Working, WALL);
        c.observe("s1", Some(&w), 5_000);
        assert_eq!(row_of(&c, Some(&w), 9_000, WALL + 60_000).age, None);
        // 启动时已经跑完：按宿主机报的时间估
        let mut c = StateClock::new();
        let e = summary(ProgramState::Error, WALL - 120_000);
        c.observe("s1", Some(&e), 5_000);
        assert_eq!(row_of(&c, Some(&e), 9_000, WALL), RowReading { status: RowStatus::Unread { error: true }, age: Some(120_000) });
        // 清掉之后同样照它算
        c.observe("s1", None, 10_000);
        assert_eq!(row_of(&c, None, 10_000, WALL + 60_000), RowReading { status: RowStatus::Read { error: true }, age: Some(180_000) });
        // 宿主机的钟比本机快：不出负数
        let mut c = StateClock::new();
        let b = blocked(ProgramBlockKind::Auth, "k", WALL + 30_000);
        c.observe("s1", Some(&b), 0);
        assert_eq!(row_of(&c, Some(&b), 0, WALL).age, Some(0));
    }

    #[test]
    fn row_inactive_only_covers_idle_and_read() {
        let day = INACTIVE_AFTER_MS;
        let mut c = StateClock::new();
        c.observe("s1", None, 0);
        assert_eq!(row_of(&c, None, 0, WALL + day - 1).status, RowStatus::Idle);
        assert_eq!(row_of(&c, None, 0, WALL + 2 * day), RowReading { status: RowStatus::Inactive, age: Some(2 * day as u64) });
        let d = done(None, WALL);
        c.observe("s1", Some(&d), 1_000);
        assert_eq!(row_of(&c, Some(&d), 1_000, WALL + 2 * day).status, RowStatus::Unread { error: false }, "没看的再久也照实说");
        c.observe("s1", None, 2_000);
        assert_eq!(row_of(&c, None, 2_000, WALL + 2 * day).status, RowStatus::Inactive);
        let b = blocked(ProgramBlockKind::Permission, "p", WALL);
        c.observe("s1", Some(&b), 3_000);
        assert_eq!(row_of(&c, Some(&b), 3_000, WALL + 2 * day).status, RowStatus::NeedsYou);
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(65_999), "1:05");
        assert_eq!(format_duration(3_599_000), "59:59");
        assert_eq!(format_duration(3_600_000 + 61_000), "1:01:01");
    }

    #[test]
    fn ws_before_first_list_only_primes() {
        let mut a = BlockedAlerts::new();
        assert_eq!(a.observe("s1", Some(&blocked(ProgramBlockKind::Permission, "x", 100))), AlertChange::Unchanged);
        assert!(a.observe_all([("s1", Some(&blocked(ProgramBlockKind::Permission, "x", 100)))]).is_empty());
    }
}
