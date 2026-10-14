//! 会话的自动标题由哪几样拼出来、各自什么时候还算数。纯逻辑：manager 把探测结果与守护进程的
//! 事件喂进来，[`AutoTitle::title`] 给出该推给界面的那个（WS 的 `{type:"title"}`、会话列表的
//! `Session.title`）。界面拿它排在手起的名字之后、agent 名之前（falcon-core 的 `session_title.rs`）。
//!
//! 素材三样，优先级从高到低：
//! 1. **程序设的标题**（OSC 0 / 2，守护进程转来）：Claude Code 写的是会话主题——`/rename` 起的
//!    名字、AI 生成的标题——同时开几个 Claude 会话，靠它才分得开；
//! 2. **程序身份**（OSC 7501 记录里的 `app`）：程序自报的名字。进程名在 macOS 上靠不住：Claude
//!    Code 原生安装版的可执行文件叫 `versions/2.1.299`，`proc_name` 拿到的就是 `2.1.299`；
//! 3. **前台命令**（探测出的进程名），没报 7501 的程序只有这一样。
//!
//! **OSC 标题只在程序报过 7501 时才采信。** shell 自己也写标题（oh-my-zsh 在提示符时写
//! `user@host: ~/dir`、执行命令前写命令行），照单全收就是跟标题栏右边的工作目录重复、每敲一条
//! 命令跳一次；报 7501 的程序是自己声明了"我在前台"的那种。
//!
//! **什么时候不再算数。** 7501 记录与 OSC 标题都可能比程序活得久：程序被 kill -9 时来不及
//! `state=clear`，idle / done 记录按规范留着；标题是终端状态，没人会去清。规则：
//! - 探测看到前台回到了 shell：**那次探测开始之前**到的素材全部作废（回话在路上时到的事件可能
//!   已经是下一个程序的，所以按开始时刻算，见 [`AutoTitle::on_probe`]）；
//! - 程序报了 7501 之后第一次探到的前台命令记作"它"；之后探到的若换了一个（程序退出、没经过
//!   shell 就跑了下一个），它的素材也作废。
//!
//! 只删记录的 Status（`state=clear`、规范要求的 working / blocked 清理、用户看过）不是程序在
//! 说话，不算新素材，也不让素材作废：Claude Code 退出时先 `state=clear` 再收尾，那一两秒里前台
//! 还是它，标题照旧，等探测看到 shell 再收——不在程序名和 `2.1.299` 之间闪一下。

use falcon_proto::{ProgramStatusRecord, sanitize_display_text};

/// 程序标题最多留多少个字符（侧栏、标题栏还会按宽度截）
const TITLE_MAX_CHARS: usize = 80;
/// 程序身份最多留多少个字符
const APP_MAX_CHARS: usize = 40;

/// 一次前台探测的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// 前台在 shell 手里
    Shell,
    /// 前台在跑这个命令（进程名）
    Busy(String),
    /// 问不到（没有 backend、超时、非持久 SSH 会话）：什么都不改
    Unknown,
}

/// 一个会话的自动标题素材
#[derive(Debug, Default)]
pub struct AutoTitle {
    /// 事件序号：每来一样新素材加一
    seq: u64,
    /// 最近一次探到的前台命令；None = 在 shell 里或还没探到过
    fg: Option<String>,
    /// 最近一次带新记录的 7501 状态到达时的序号；0 = 从没有过
    program_seq: u64,
    /// 那次状态里的程序身份
    app: Option<String>,
    /// 程序报了 7501 之后第一次探到的前台命令：之后前台换了人，它的素材就不算了
    program_fg: Option<String>,
    /// 程序设的终端标题（OSC 0 / 2 原文）与到达时的序号
    title: Option<String>,
    title_seq: u64,
    /// 最近一次看到前台回到 shell 的那次探测，开始时的序号：不超过它的素材都作废
    shell_seen: u64,
}

impl AutoTitle {
    pub fn new() -> Self {
        Self::default()
    }

    /// 守护进程推来新的程序状态全集，`old` 是之前那份
    pub fn on_status(&mut self, old: &[ProgramStatusRecord], new: &[ProgramStatusRecord]) {
        // 有新的或更新过的记录才是程序在说话（接回时整份重发、只删了记录，都不算）
        let fresh = new.iter().any(|r| !old.iter().any(|o| o.id == r.id && o.updated_at == r.updated_at));
        if !fresh {
            return;
        }
        self.seq += 1;
        self.program_seq = self.seq;
        self.app = app_of(new);
        self.program_fg = None;
    }

    /// 程序设了终端标题（OSC 0 / 2）。空串 = 没有标题
    pub fn on_program_title(&mut self, title: &str) {
        let next = (!title.is_empty()).then(|| title.to_string());
        // 接回时守护进程会把当前标题再发一遍：同一个标题不是新素材
        if next == self.title {
            return;
        }
        self.title = next;
        self.seq += 1;
        self.title_seq = self.seq;
    }

    /// 开始一次探测时的序号，原样交给 [`AutoTitle::on_probe`]
    pub fn probe_started(&self) -> u64 {
        self.seq
    }

    pub fn on_probe(&mut self, started: u64, probe: Probe) {
        match probe {
            Probe::Shell => {
                self.fg = None;
                self.shell_seen = self.shell_seen.max(started);
            }
            Probe::Busy(command) => {
                // 程序的状态是在这次探测开始之前到的，探到的才是它；回话在路上时才到的，
                // 留给下一次探测认
                if self.program_live() && self.program_fg.is_none() && self.program_seq <= started {
                    self.program_fg = Some(command.clone());
                }
                self.fg = Some(command);
            }
            Probe::Unknown => {}
        }
    }

    /// 报过 7501 的程序此刻还在前台
    fn program_live(&self) -> bool {
        self.program_seq > self.shell_seen && self.program_fg.as_ref().is_none_or(|p| self.fg.as_ref() == Some(p))
    }

    /// 该显示的自动标题；None = 前台在 shell 手里（界面落到 agent 名 / shell 名）
    pub fn title(&self) -> Option<String> {
        if self.program_live() {
            let label = self.app.as_deref().map(app_label).filter(|l| !l.is_empty());
            // 标题若只是程序自己的名字（Claude Code 还没起主题时写的就是 "Claude Code"），
            // 不如用身份：与 agent 兜底同形，不会在两种写法之间跳
            let names = [self.app.as_deref(), label.as_deref(), self.fg.as_deref()];
            let titled = self
                .title
                .as_deref()
                .filter(|_| self.title_seq > self.shell_seen)
                .and_then(clean_program_title)
                .filter(|t| !names.iter().flatten().any(|n| same_name(t, n)));
            // 程序已报 7501、前台还没探到时也有得显示：报告本身就说明它在前台
            if let Some(t) = titled.or(label) {
                return Some(t);
            }
        }
        self.fg.clone()
    }
}

/// 记录里的程序身份：根记录（程序自己）的优先，否则取最近更新的、带 app 的那条
fn app_of(records: &[ProgramStatusRecord]) -> Option<String> {
    records
        .iter()
        .find(|r| r.id.is_empty() && r.app.is_some())
        .or_else(|| records.iter().filter(|r| r.app.is_some()).max_by_key(|r| r.updated_at))
        .and_then(|r| r.app.clone())
}

/// 7501 的 `app` 怎么显示。与 agent 兜底（`SessionAgent` 的 wire 字面量）同一个东西的，改写成
/// 同一个词：agent 会话里 CLI 退回 shell 后侧栏落到 agent 名，跑着时若叫 `claude-code`、一退出
/// 就跳成 `claude`，看着像换了个程序
pub fn app_label(app: &str) -> String {
    let app = sanitize_display_text(app, APP_MAX_CHARS);
    match app.trim() {
        "claude-code" => "claude".to_string(),
        other => other.to_string(),
    }
}

/// 程序给的标题显示到终端之外之前：消毒、截断、去掉打头的状态符号。
///
/// Claude Code 的标题是 `✳ 主题`（空闲）/ `◐ 主题`、`◑ 主题`（干活时每 960ms 换一下）；别的 CLI
/// 用盲文转圈（`⠋`）、圆点之类。状态已经由标题栏的胶囊和侧栏的徽标表达，这里只要主题——
/// 也省得每秒换一次标题。只认"打头一两个非 ASCII 的符号、后面跟空白"这一种形状
fn clean_program_title(raw: &str) -> Option<String> {
    let s = sanitize_display_text(raw, TITLE_MAX_CHARS);
    let s = s.trim();
    let s = match s.split_once(char::is_whitespace) {
        Some((head, rest))
            if head.chars().count() <= 2
                && head.chars().all(|c| !c.is_alphanumeric() && !c.is_ascii())
                && !rest.trim().is_empty() =>
        {
            rest.trim()
        }
        _ => s,
    };
    (!s.is_empty()).then(|| s.to_string())
}

/// 两个名字是不是同一个：只比字母数字、不分大小写（"Claude Code" = "claude-code"）
fn same_name(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
    let a = norm(a);
    !a.is_empty() && a == norm(b)
}

#[cfg(test)]
mod tests {
    use falcon_proto::ProgramState;

    use super::*;

    fn rec(id: &str, app: Option<&str>, at: i64) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: id.into(),
            state: ProgramState::Working,
            kind: None,
            progress: None,
            app: app.map(Into::into),
            title: None,
            message: None,
            updated_at: at,
        }
    }

    /// 喂一次状态（以 t 当前的记录为 old）
    struct Feed {
        t: AutoTitle,
        records: Vec<ProgramStatusRecord>,
    }

    impl Feed {
        fn new() -> Self {
            Feed { t: AutoTitle::new(), records: Vec::new() }
        }
        fn status(&mut self, records: Vec<ProgramStatusRecord>) {
            self.t.on_status(&self.records, &records);
            self.records = records;
        }
        fn probe(&mut self, p: Probe) {
            let started = self.t.probe_started();
            self.t.on_probe(started, p);
        }
        fn busy(&mut self, cmd: &str) {
            self.probe(Probe::Busy(cmd.into()));
        }
        fn title(&self) -> Option<String> {
            self.t.title()
        }
    }

    #[test]
    fn plain_programs_are_named_by_their_foreground_command() {
        let mut f = Feed::new();
        assert_eq!(f.title(), None, "还没探过");
        f.busy("cargo");
        assert_eq!(f.title().as_deref(), Some("cargo"));
        // shell 也写标题，没报 7501 的程序一律不采信
        f.t.on_program_title("fay@mac: ~/code");
        assert_eq!(f.title().as_deref(), Some("cargo"));
        f.probe(Probe::Unknown);
        assert_eq!(f.title().as_deref(), Some("cargo"), "问不到时不改");
        f.probe(Probe::Shell);
        assert_eq!(f.title(), None);
    }

    #[test]
    fn a_program_that_speaks_7501_is_named_by_its_title_then_its_app() {
        let mut f = Feed::new();
        f.probe(Probe::Shell);
        f.status(vec![rec("", Some("claude-code"), 1)]);
        assert_eq!(f.title().as_deref(), Some("claude"), "报了 7501、还没探到前台，也认得出");
        f.busy("2.1.299");
        assert_eq!(f.title().as_deref(), Some("claude"), "不用 macOS 上的进程名");
        f.t.on_program_title("✳ Claude Code");
        assert_eq!(f.title().as_deref(), Some("claude"), "标题只是程序自己的名字");
        f.t.on_program_title("◐ 修复登录 bug");
        assert_eq!(f.title().as_deref(), Some("修复登录 bug"));
        f.t.on_program_title("◑ 修复登录 bug");
        assert_eq!(f.title().as_deref(), Some("修复登录 bug"), "转圈换帧不换标题");
        f.t.on_program_title("");
        assert_eq!(f.title().as_deref(), Some("claude"));
    }

    #[test]
    fn exit_keeps_the_title_until_the_shell_is_seen() {
        let mut f = Feed::new();
        f.status(vec![rec("", Some("claude-code"), 1)]);
        f.busy("2.1.299");
        f.t.on_program_title("✳ 修复登录 bug");
        // 退出：先 state=clear，进程还在收尾
        f.status(vec![]);
        assert_eq!(f.title().as_deref(), Some("修复登录 bug"), "不闪回 2.1.299");
        f.busy("2.1.299");
        assert_eq!(f.title().as_deref(), Some("修复登录 bug"));
        f.probe(Probe::Shell);
        assert_eq!(f.title(), None);
        // 下一个程序没报 7501：上一个的东西一样都不带过去
        f.busy("vim");
        assert_eq!(f.title().as_deref(), Some("vim"));
    }

    #[test]
    fn records_left_by_a_killed_program_stop_counting_once_the_shell_is_seen() {
        let mut f = Feed::new();
        f.status(vec![rec("", Some("claude-code"), 1), rec("agent-1", None, 2)]);
        f.busy("2.1.299");
        f.t.on_program_title("✳ 修复登录 bug");
        // kill -9：来不及 clear；守护进程按规范丢掉 working，只删记录的 Status 不算新素材
        f.probe(Probe::Shell);
        f.status(vec![rec("", Some("claude-code"), 1)]);
        f.busy("htop");
        assert_eq!(f.title().as_deref(), Some("htop"));
        // 接回时整份重发同样的记录、同样的标题：也不算
        f.t.on_program_title("✳ 修复登录 bug");
        let same = f.records.clone();
        f.status(same);
        assert_eq!(f.title().as_deref(), Some("htop"));
    }

    #[test]
    fn a_different_foreground_command_retires_the_program_without_passing_through_the_shell() {
        let mut f = Feed::new();
        f.status(vec![rec("", Some("claude-code"), 1)]);
        f.busy("2.1.299");
        f.status(vec![]);
        // 两次探测之间 Claude 退出、又跑了 htop：探测没赶上 shell
        f.busy("htop");
        assert_eq!(f.title().as_deref(), Some("htop"));
        // 又开了一个 Claude：新报告，重新认
        f.status(vec![rec("", Some("claude-code"), 5)]);
        assert_eq!(f.title().as_deref(), Some("claude"));
        f.busy("2.1.300");
        assert_eq!(f.title().as_deref(), Some("claude"));
    }

    #[test]
    fn events_that_arrive_while_a_probe_is_in_flight_are_not_retired_by_it() {
        let mut t = AutoTitle::new();
        let started = t.probe_started();
        // 探测在路上：程序起来了、报了状态、设了标题
        t.on_status(&[], &[rec("", Some("claude-code"), 1)]);
        t.on_program_title("✳ 修复登录 bug");
        // 回话是程序起来之前采的样
        t.on_probe(started, Probe::Shell);
        assert_eq!(t.title().as_deref(), Some("修复登录 bug"));
        // 同理，回话在路上时才报的状态，不把那次探到的前台认作它
        let started = t.probe_started();
        t.on_status(&[rec("", Some("claude-code"), 1)], &[rec("", Some("claude-code"), 2)]);
        t.on_probe(started, Probe::Busy("make".into()));
        t.on_probe(t.probe_started(), Probe::Busy("2.1.299".into()));
        assert_eq!(t.title().as_deref(), Some("修复登录 bug"));
    }

    #[test]
    fn app_comes_from_the_root_record_first() {
        assert_eq!(app_of(&[rec("a", Some("cargo"), 9), rec("", Some("claude-code"), 1)]).as_deref(), Some("claude-code"));
        assert_eq!(app_of(&[rec("a", Some("x"), 1), rec("b", Some("y"), 2), rec("", None, 3)]).as_deref(), Some("y"));
        assert_eq!(app_of(&[rec("", None, 1)]), None);
        // 报了 7501 却没带 app：前台命令顶上
        let mut f = Feed::new();
        f.status(vec![rec("", None, 1)]);
        assert_eq!(f.title(), None);
        f.busy("make");
        assert_eq!(f.title().as_deref(), Some("make"));
    }

    #[test]
    fn program_titles_are_cleaned() {
        assert_eq!(clean_program_title("✳ 修复登录 bug").as_deref(), Some("修复登录 bug"));
        assert_eq!(clean_program_title("⠋  building").as_deref(), Some("building"));
        assert_eq!(clean_program_title("⚡\u{FE0F} fast").as_deref(), Some("fast"));
        // ASCII 符号、字母数字、中文打头的都是正文
        assert_eq!(clean_program_title("- notes").as_deref(), Some("- notes"));
        assert_eq!(clean_program_title("中 文").as_deref(), Some("中 文"));
        assert_eq!(clean_program_title("✳").as_deref(), Some("✳"), "只有符号时不剥");
        assert_eq!(clean_program_title("a\u{202E}b\n").as_deref(), Some("ab"));
        assert_eq!(clean_program_title("  "), None);
        assert_eq!(clean_program_title(&"长".repeat(100)).unwrap().chars().count(), TITLE_MAX_CHARS + 1);
        assert!(same_name("Claude Code", "claude-code"));
        assert!(!same_name("claude · resume", "claude"));
        assert_eq!(app_label(" cargo\u{200B} "), "cargo");
    }
}
