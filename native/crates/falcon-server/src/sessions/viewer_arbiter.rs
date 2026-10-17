//! 多个 Viewer 共用一个 PTY 时的仲裁。移植自 `packages/server/src/sessions/viewerArbiter.ts`。
//! 纯状态，零 I/O。
//!
//! 一个会话在服务端只有一个 PTY / 一条守护进程连接，宿主机那一侧的多客户端协调用不上
//! （守护进程只按"最近操作的那条连接"取尺寸，falcon-server 的所有 Viewer 合起来算一条）。
//!
//! - 尺寸跟「最近真操作过的、看得见的那扇窗口」（ADR 0022，tmux 的 `window-size latest`、zmx 的
//!   leader 同一口径）：谁真的敲键 / 粘贴 / 点击 / 滚轮（[`is_user_input`]）PTY 就换成谁的格子；
//!   客户端报了看不见的（切走的画布、最小化的窗口、后台标签页）不参与，都看不见时才退回到全部
//!   ——不然没人看着时新开的会话一直等不到尺寸。没操作过的人之间按连上来的先后：后连上来的人
//!   只是看，不抢。持有者走了或看不见了，交给剩下的人里最近操作过的那个；所有报过格子的人都
//!   走了维持原样（没人看着的会话没必要为此重画一次）。
//!   以前取的是所有人的最小 cols × 最小 rows：后台挂着的窄窗口把所有人压小，cols 与 rows 分开
//!   取还能拼出谁都不是的尺寸。取最小当初是因为客户端不知道 PTY 的尺寸、只能按自己的窗口排，
//!   PTY 比它大就折乱（宽屏收到按窄屏排的整屏重画，自己格子没变不会再报，一直错下去）——现在
//!   生效尺寸经 WS 的 `size` 推给每扇窗口，不是它定的按这个尺寸建网格、在本地缩放 / 裁剪着画。
//! - 深浅跟「最近操作的那端」：后来连上的人只是看，不该把正在干活那端的配色翻掉。第一个报
//!   深浅的人先持有；之后谁真的敲键 / 粘贴 / 点击 / 滚轮谁接手；持有者走了交给剩下的人里
//!   最近操作过的那个。见 [`is_user_input`]——终端自动发的答复不算操作。深浅不看可见性：
//!   后台窗口没人操作，本来就接不了手。
//!
//! Viewer 的座位按**连上来的先后**排（TS 的 `Map` 保插入顺序）：持有者走了、剩下的人都没
//! 操作过时，交给最早连上来的那个。所以这里用 `Vec` 而不是 `HashMap`。

use std::sync::LazyLock;

use falcon_proto::OscColorHint;
use regex::{Captures, Regex};

use super::term_size::TermSize;

#[derive(Debug, Clone)]
struct Seat {
    size: Option<TermSize>,
    hint: Option<OscColorHint>,
    /// 最近一次真操作的序号；0 = 连上来之后还没操作过
    active_at: u64,
    /// 客户端报的这扇窗口在不在屏幕上；没报过（老客户端）当看得见
    visible: bool,
}

impl Default for Seat {
    fn default() -> Self {
        Self { size: None, hint: None, active_at: 0, visible: true }
    }
}

/// [`ViewerArbiter::leave`] 的结果
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LeaveOutcome {
    /// 换上来的持有者的深浅（没换人为 None）
    pub hint: Option<OscColorHint>,
}

/// 多 Viewer 仲裁。`V` 是 Viewer 的身份（连接 id 之类），只要求能比较相等。
#[derive(Debug, Clone)]
pub struct ViewerArbiter<V> {
    seats: Vec<(V, Seat)>,
    /// 深浅以谁为准；None = 还没人报过（或报过的人都走了）
    owner: Option<V>,
    seq: u64,
}

impl<V> Default for ViewerArbiter<V> {
    fn default() -> Self {
        Self { seats: Vec::new(), owner: None, seq: 0 }
    }
}

impl<V: PartialEq + Clone> ViewerArbiter<V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记下这个 Viewer 的窗口放得下的格子。生效尺寸看 [`Self::size_owner`]
    pub fn resize(&mut self, viewer: &V, size: TermSize) {
        self.seat(viewer).size = Some(size);
    }

    /// 记下这扇窗口看不看得见。生效尺寸看 [`Self::size_owner`]
    pub fn set_visible(&mut self, viewer: &V, visible: bool) {
        self.seat(viewer).visible = visible;
    }

    /// 尺寸跟谁、是多少：报过格子的人里，看得见的优先（都看不见才看全部），取最近真操作过的
    /// 那个，同分（都没操作过）取先连上来的。没人报过格子时 None（调用方保留原尺寸）
    pub fn size_owner(&self) -> Option<(&V, TermSize)> {
        let sized = || self.seats.iter().filter_map(|(v, s)| s.size.map(|size| (v, s, size)));
        let any_visible = sized().any(|(_, s, _)| s.visible);
        let mut best: Option<(&V, u64, TermSize)> = None;
        for (v, seat, size) in sized().filter(|(_, s, _)| s.visible || !any_visible) {
            // 严格大于：同分时留住先连上来的
            if best.is_none_or(|(_, at, _)| seat.active_at > at) {
                best = Some((v, seat.active_at, size));
            }
        }
        best.map(|(v, _, size)| (v, size))
    }

    /// 生效尺寸（[`Self::size_owner`] 的尺寸）
    pub fn size(&self) -> Option<TermSize> {
        self.size_owner().map(|(_, size)| size)
    }

    /// Viewer 报来深浅。它是（或成为）持有者时返回要生效的那份，否则 None——先存着，等它操作时再用
    pub fn hint(&mut self, viewer: &V, hint: OscColorHint) -> Option<OscColorHint> {
        self.seat(viewer).hint = Some(hint.clone());
        if self.owner.is_none() {
            self.owner = Some(viewer.clone());
        }
        if self.owner.as_ref() == Some(viewer) { Some(hint) } else { None }
    }

    /// Viewer 的输入。真操作就记一次（尺寸据此换人，见 [`Self::size_owner`]）；报过深浅、又不是
    /// 现任深浅持有者时换人，返回它的深浅
    pub fn input(&mut self, viewer: &V, data: &str) -> Option<OscColorHint> {
        if !is_user_input(data) {
            return None;
        }
        self.seq += 1;
        let seq = self.seq;
        let seat = self.seat(viewer);
        seat.active_at = seq;
        // 没报过深浅的（老客户端）不接手：换给它也没有颜色可用
        let hint = seat.hint.clone()?;
        if self.owner.as_ref() == Some(viewer) {
            return None;
        }
        self.owner = Some(viewer.clone());
        Some(hint)
    }

    /// Viewer 离开。hint：换上来的深浅持有者的深浅（没换人为 None）。尺寸照常看 [`Self::size_owner`]
    pub fn leave(&mut self, viewer: &V) -> LeaveOutcome {
        self.seats.retain(|(v, _)| v != viewer);
        let mut hint = None;
        if self.owner.as_ref() == Some(viewer) {
            self.owner = None;
            // TS 的 best 从 -1 起（这里是 None）：没操作过（active_at = 0）的人也能接手，
            // 同分时先连上来的优先
            let mut best: Option<u64> = None;
            for (v, seat) in &self.seats {
                if let Some(h) = &seat.hint
                    && best.is_none_or(|b| seat.active_at > b)
                {
                    best = Some(seat.active_at);
                    self.owner = Some(v.clone());
                    hint = Some(h.clone());
                }
            }
        }
        LeaveOutcome { hint }
    }

    fn seat(&mut self, viewer: &V) -> &mut Seat {
        let idx = match self.seats.iter().position(|(v, _)| v == viewer) {
            Some(i) => i,
            None => {
                self.seats.push((viewer.clone(), Seat::default()));
                self.seats.len() - 1
            }
        };
        &mut self.seats[idx].1
    }
}

/// 终端自己会发、而不是人按出来的序列：
/// - CSI 答复：DA1 / DA2（`c`）、CPR（`R`）、DSR 与 997 深浅报告（`n`）、窗口尺寸报告（`t`）、
///   DECRPM（`$y`）、kitty 键盘协议的查询答复（`?…u`，按键事件不带 `?`）、焦点进出（`I` / `O`）；
/// - OSC / DCS 答复（颜色查询、XTVERSION、DECRQSS…）。
///
/// 客户端的终端（falcon-term 里的 alacritty_terminal）会替宿主机程序的查询自动答复——Zellij 时代
/// 实测是 `CSI 14t`，每次 resize 都有（falcon-term 现在不答 14t，DA / DSR / `CSI 18t` 照答）。
/// 被动看着的那端也在"输入"，不滤掉的话谁答得晚谁抢走深浅。
/// 误伤可以接受：老式 Shift+F3（`CSI 1;2R`）与 CPR 同形，只是不算一次操作。
///
/// （JS 的 `\d` 只认 ASCII 数字，Rust regex 的 `\d` 是 Unicode，这里一律写 `[0-9]`）
static TERMINAL_REPLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\x1b\[[?>]?[0-9;]*[cRnt]|\x1b\[\?[0-9;]*\$y|\x1b\[\?[0-9]*u|\x1b\[[IO]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1bP[^\x1b]*\x1b\\",
    )
    .expect("TERMINAL_REPLY")
});

/// SGR 鼠标报告；没按键的移动（按钮位 3 + 移动位 32）只是鼠标划过，不算操作
static SGR_MOUSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[<([0-9]+);[0-9]+;[0-9]+[Mm]").expect("SGR_MOUSE"));

/// 这段输入里有没有人真的做了什么（敲键、粘贴、点击、滚轮），见 [`TERMINAL_REPLY`]
pub fn is_user_input(data: &str) -> bool {
    let rest = TERMINAL_REPLY.replace_all(data, "");
    let rest = SGR_MOUSE.replace_all(&rest, |caps: &Captures| {
        if js_to_int32(js_digits_to_number(&caps[1])) & 35 == 35 { String::new() } else { caps[0].to_string() }
    });
    !rest.is_empty()
}

/// `Number("<纯 ASCII 数字串>")`：位数再多也只是精度丢失 / Infinity，不会失败
fn js_digits_to_number(digits: &str) -> f64 {
    digits.parse::<f64>().unwrap_or(f64::NAN)
}

/// JS 位运算前的 ToInt32：NaN / ±Infinity 为 0，其余截断后按 2^32 取模
fn js_to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    let m = x.trunc().rem_euclid(4_294_967_296.0);
    (m as u32) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::TermAppearance;

    fn dark() -> OscColorHint {
        OscColorHint {
            appearance: Some(TermAppearance::Dark),
            background: Some("#000000".into()),
            foreground: Some("#ffffff".into()),
        }
    }

    fn light() -> OscColorHint {
        OscColorHint {
            appearance: Some(TermAppearance::Light),
            background: Some("#ffffff".into()),
            foreground: Some("#000000".into()),
        }
    }

    fn size(cols: u16, rows: u16) -> TermSize {
        TermSize { cols, rows }
    }

    fn arbiter() -> ViewerArbiter<&'static str> {
        ViewerArbiter::new()
    }

    // 尺寸：最近真操作过的、看得见的那扇窗口（ADR 0022）

    #[test]
    fn size_has_no_size_until_someone_measured() {
        let a = arbiter();
        assert_eq!(a.size(), None);
    }

    #[test]
    fn size_a_newcomer_only_watching_does_not_take_the_size() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.resize(&"phone", size(60, 30));
        // 没人操作过：先连上来的说了算，后来的手机只是看
        assert_eq!(a.size_owner(), Some((&"desk", size(200, 50))));
        // 先到的那扇拖窗照样生效
        a.resize(&"desk", size(220, 60));
        assert_eq!(a.size(), Some(size(220, 60)));
    }

    #[test]
    fn size_follows_whoever_really_types() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.resize(&"phone", size(60, 30));
        a.input(&"phone", "l");
        assert_eq!(a.size_owner(), Some((&"phone", size(60, 30))));
        a.input(&"desk", "\r");
        assert_eq!(a.size_owner(), Some((&"desk", size(200, 50))));
    }

    #[test]
    fn size_automatic_terminal_replies_do_not_take_the_size() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.resize(&"phone", size(60, 30));
        a.input(&"phone", "\x1b[?62;22c\x1b[I\x1b[<35;10;5M");
        assert_eq!(a.size_owner(), Some((&"desk", size(200, 50))));
    }

    #[test]
    fn size_is_never_a_mix_of_two_viewers() {
        let mut a = arbiter();
        a.resize(&"wide", size(200, 20));
        a.resize(&"tall", size(80, 60));
        a.input(&"tall", "x");
        // 以前分别取最小会拼出 80×20，谁都不是这个尺寸
        assert_eq!(a.size(), Some(size(80, 60)));
    }

    #[test]
    fn size_a_hidden_window_does_not_count_even_if_it_typed_last() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.resize(&"phone", size(60, 30));
        a.input(&"desk", "x");
        a.set_visible(&"desk", false);
        assert_eq!(a.size_owner(), Some((&"phone", size(60, 30))));
        // 切回来：它最近操作过，尺寸还给它
        a.set_visible(&"desk", true);
        assert_eq!(a.size_owner(), Some((&"desk", size(200, 50))));
    }

    #[test]
    fn size_a_hidden_newcomer_does_not_take_the_size_on_connect() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.input(&"desk", "x");
        a.set_visible(&"bg", false);
        a.resize(&"bg", size(60, 30));
        assert_eq!(a.size_owner(), Some((&"desk", size(200, 50))));
    }

    #[test]
    fn size_falls_back_to_everyone_when_nobody_is_visible() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.resize(&"phone", size(60, 30));
        a.input(&"phone", "x");
        a.set_visible(&"desk", false);
        a.set_visible(&"phone", false);
        // 没人看着时照样有尺寸（新开的会话要靠它接回），跟最近操作过的那个
        assert_eq!(a.size_owner(), Some((&"phone", size(60, 30))));
    }

    #[test]
    fn size_viewers_that_never_measured_do_not_count() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.input(&"fresh", "x");
        assert_eq!(a.size_owner(), Some((&"desk", size(200, 50))));
    }

    #[test]
    fn size_goes_to_the_most_recently_active_when_the_owner_leaves() {
        let mut a = arbiter();
        a.resize(&"A", size(200, 50));
        a.resize(&"B", size(120, 40));
        a.resize(&"C", size(60, 30));
        a.input(&"C", "x");
        a.input(&"B", "x");
        a.input(&"A", "x");
        a.leave(&"A");
        assert_eq!(a.size_owner(), Some((&"B", size(120, 40))));
    }

    #[test]
    fn size_reports_none_once_the_last_measured_viewer_is_gone() {
        let mut a = arbiter();
        a.resize(&"desk", size(200, 50));
        a.leave(&"desk");
        assert_eq!(a.size(), None);
    }

    // describe("ViewerArbiter colors")

    #[test]
    fn colors_first_viewer_to_report_owns_the_colors() {
        let mut a = arbiter();
        assert_eq!(a.hint(&"A", dark()), Some(dark()));
    }

    #[test]
    fn colors_a_newcomer_only_watching_does_not_flip_the_owners_colors() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        assert_eq!(a.hint(&"B", light()), None);
    }

    #[test]
    fn colors_owner_changing_its_own_theme_applies_right_away() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        assert_eq!(a.hint(&"A", light()), Some(light()));
    }

    #[test]
    fn colors_whoever_types_takes_over_and_only_once() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        a.hint(&"B", light());
        assert_eq!(a.input(&"B", "l"), Some(light()));
        assert_eq!(a.input(&"B", "s"), None);
        assert_eq!(a.input(&"A", "\r"), Some(dark()));
    }

    #[test]
    fn colors_automatic_terminal_replies_do_not_take_over() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        a.hint(&"B", light());
        assert_eq!(a.input(&"B", "\x1b[4;600;800t"), None);
        assert_eq!(a.input(&"B", "\x1b[I"), None);
    }

    #[test]
    fn colors_a_viewer_that_never_reported_colors_cannot_take_over() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        assert_eq!(a.input(&"old-client", "x"), None);
        // A 依然是持有者：它改主题照常生效
        assert_eq!(a.hint(&"A", light()), Some(light()));
    }

    #[test]
    fn colors_when_the_owner_leaves_the_most_recently_active_remaining_viewer_takes_over() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        a.hint(&"B", light());
        a.hint(
            &"C",
            OscColorHint {
                appearance: Some(TermAppearance::Dark),
                background: Some("#111111".into()),
                foreground: None,
            },
        );
        a.input(&"B", "x");
        a.input(&"A", "y");
        assert_eq!(a.leave(&"A").hint, Some(light()));
    }

    #[test]
    fn colors_a_non_owner_leaving_changes_nothing() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        a.hint(&"B", light());
        assert_eq!(a.leave(&"B").hint, None);
        assert_eq!(a.hint(&"A", light()), Some(light()));
    }

    #[test]
    fn colors_after_everyone_with_colors_left_the_next_report_owns_again() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        assert_eq!(a.leave(&"A").hint, None);
        assert_eq!(a.hint(&"B", light()), Some(light()));
    }

    // describe("isUserInput")

    #[test]
    fn is_user_input_keystrokes_paste_and_clicks_are_operations() {
        assert!(is_user_input("a"));
        assert!(is_user_input("\r"));
        assert!(is_user_input("\x1b[A")); // ↑
        assert!(is_user_input("\x1b[97;5u")); // kitty 协议的 Ctrl+a
        assert!(is_user_input("\x1b[200~hello\x1b[201~"));
        assert!(is_user_input("\x1b[<0;10;5M")); // 左键按下
        assert!(is_user_input("\x1b[<64;10;5M")); // 滚轮
        assert!(is_user_input("\x1b[<32;11;5M")); // 按着左键拖
    }

    #[test]
    fn is_user_input_automatic_replies_are_not() {
        assert!(!is_user_input("\x1b[?62;22c")); // DA1
        assert!(!is_user_input("\x1b[>0;276;0c")); // DA2
        assert!(!is_user_input("\x1b[12;40R")); // CPR
        assert!(!is_user_input("\x1b[0n")); // DSR
        assert!(!is_user_input("\x1b[?997;1n")); // 深浅报告
        assert!(!is_user_input("\x1b[4;600;800t\x1b[6;16;8t")); // 窗口 / 格子像素
        assert!(!is_user_input("\x1b[?2004;1$y")); // DECRPM
        assert!(!is_user_input("\x1b[?1u")); // kitty 键盘查询答复
        assert!(!is_user_input("\x1b[I"));
        assert!(!is_user_input("\x1b[O"));
        assert!(!is_user_input("\x1b]11;rgb:0000/0000/0000\x1b\\"));
        assert!(!is_user_input("\x1b]10;rgb:ffff/ffff/ffff\x07"));
        assert!(!is_user_input("\x1bP>|xterm.js(5.5.0)\x1b\\")); // XTVERSION
    }

    #[test]
    fn is_user_input_bare_mouse_motion_is_not_with_or_without_modifiers() {
        assert!(!is_user_input("\x1b[<35;10;5M"));
        assert!(!is_user_input("\x1b[<35;10;5M\x1b[<35;11;5M"));
        assert!(!is_user_input("\x1b[<39;10;5M")); // Shift + 移动
    }

    #[test]
    fn is_user_input_a_reply_glued_to_a_keystroke_still_counts() {
        assert!(is_user_input("\x1b[Ix"));
    }

    // 以下不在 TS 测试里：钉住 Rust 侧与 JS 语义的几处差异点

    #[test]
    fn digit_classes_are_ascii_only() {
        // 全角数字不是 JS 的 \d：不是答复，算一次输入
        assert!(is_user_input("\x1b[１２R"));
    }

    #[test]
    fn huge_mouse_button_numbers_follow_js_to_int32() {
        assert_eq!(js_to_int32(4_294_967_331.0), 35); // 2^32 + 35
        assert_eq!(js_to_int32(f64::INFINITY), 0);
        assert!(!is_user_input("\x1b[<4294967331;1;1M"));
    }

    #[test]
    fn leave_hands_over_to_the_earliest_seat_on_ties() {
        let mut a = arbiter();
        a.hint(&"A", dark());
        a.hint(&"B", light());
        a.hint(&"C", dark());
        // B、C 都没操作过（active_at = 0），先连上来的 B 接手
        assert_eq!(a.leave(&"A").hint, Some(light()));
    }
}
