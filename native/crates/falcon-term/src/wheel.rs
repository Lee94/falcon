//! 滚轮归谁（[`route_wheel`]）与滚轮事件 → 滚动量（`packages/web/src/lib/rio/wheel.ts` 的移植）。
//!
//! 归属按 xterm 的语义判：宿主机上不再有 Zellij 常驻备用屏、常开鼠标上报，客户端收到的是程序
//! 原样输出的字节（ADR 0020），程序要不要滚轮、要哪种形式，都看它自己设的模式。
//!
//! 两种口径，取决于滚轮归谁：
//!
//! - [`WheelAccumulator::push`]：本地 scrollback 用，像素按 cell_height 折成行。不足一行的
//!   小数余量攒起来，攒够一行才吐——触控板慢滚每个事件只有几像素，直接截断就滚不动；方向翻转时
//!   清掉余量，免得反向滑动先"补"上一段。
//! - [`WheelAccumulator::click`]：程序接管了滚轮（鼠标上报 / 备用屏转方向键）时用，一个事件最多
//!   折成一次点击。程序收到每条上报自己会滚好几行（vim 默认 3 行），按行数发 n 条会让鼠标一格
//!   冲出十几行。xterm.js 的口径（MouseService._consumeWheelEvent）是行数只当门槛、每个事件
//!   最多发一条，多出的整行丢掉，且 |delta| < 50 的像素事件当作触控板再乘 0.3。照抄——旧 React
//!   版的两个引擎（xterm / rio）当初就按这套对齐手感。
//!
//! GPUI 的 `ScrollDelta::Lines` 对应 DOM 的 deltaMode = 1，`ScrollDelta::Pixels` 对应 0。

use alacritty_terminal::term::TermMode;

/// 一次滚轮该交给谁
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelRoute {
    /// 程序开了鼠标上报：按滚轮键（64 / 65）报给程序
    Report,
    /// 备用屏、没开鼠标上报、程序没关 ?1007（alternate scroll）：转成上下方向键。
    /// `app_cursor` = DECCKM 开着，方向键用 SS3 形式
    Arrows { app_cursor: bool },
    /// 本地 scrollback（备用屏里没有历史，滚了也不动）
    Scrollback,
}

/// 滚轮归谁。`mouse_protocol` 是模式跟踪器给的鼠标协议（0 = 没开上报），`shift` 按着时
/// 绕过鼠标上报（与点击、拖选同一个约定）。
///
/// 备用屏转方向键要看 ?1007：alacritty 的 `TermMode` 默认就带 `ALTERNATE_SCROLL`（与 xterm 的
/// alternateScroll 资源打开时一样），程序发 `CSI ? 1007 l` 关掉它时，滚轮就不该再变成一串方向键
/// ——那时程序多半在备用屏里自己处理按键，凭空多出来的 ↑↓ 会被当成用户输入（翻命令历史、移光标）。
/// 以前不判这一条也没出事，是因为备用屏一直是 Zellij 的，程序的模式根本到不了客户端。
pub fn route_wheel(mode: TermMode, mouse_protocol: u16, shift: bool) -> WheelRoute {
    if mouse_protocol != 0 && !shift {
        return WheelRoute::Report;
    }
    if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
        return WheelRoute::Arrows { app_cursor: mode.contains(TermMode::APP_CURSOR) };
    }
    WheelRoute::Scrollback
}

/// [`WheelRoute::Arrows`] 的一格：朝历史方向（`up`）是 ↑。DECCKM 开着用 SS3（`ESC O A`），
/// 否则 CSI（`ESC [ A`），与按键编码同一个口径
pub fn arrow_for_wheel(up: bool, app_cursor: bool) -> &'static str {
    match (up, app_cursor) {
        (true, true) => "\x1bOA",
        (true, false) => "\x1b[A",
        (false, true) => "\x1bOB",
        (false, false) => "\x1b[B",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeltaMode {
    Pixel,
    Line,
    Page,
}

/// xterm.js 的启发式：单个像素事件 |delta| 小于它就当触控板
const TRACKPAD_MAX_DELTA: f64 = 50.0;
const TRACKPAD_DAMPING: f64 = 0.3;

#[derive(Default, Debug, Clone)]
pub struct WheelAccumulator {
    remainder: f64,
}

impl WheelAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// `delta_y` 沿用 DOM 的符号约定：向下滚为正。`page_rows` 是页模式一页折算的行数（传当前 rows）。
    /// 返回本次应滚动的整行数，正数朝历史方向。
    pub fn push(&mut self, mode: DeltaMode, delta_y: f64, cell_height: f64, page_rows: usize) -> i32 {
        self.accumulate(to_lines(mode, delta_y, cell_height, page_rows))
    }

    /// 程序接管滚轮时的口径：攒够一行算一次点击，多出的整行丢掉。参数同 [`Self::push`]。
    /// 返回 1 朝历史方向、-1 朝底部、0 还没攒够。
    pub fn click(&mut self, mode: DeltaMode, delta_y: f64, cell_height: f64, page_rows: usize) -> i32 {
        let mut lines = to_lines(mode, delta_y, cell_height, page_rows);
        if mode == DeltaMode::Pixel && delta_y.abs() < TRACKPAD_MAX_DELTA {
            lines *= TRACKPAD_DAMPING;
        }
        self.accumulate(lines).signum()
    }

    pub fn reset(&mut self) {
        self.remainder = 0.0;
    }

    fn accumulate(&mut self, lines: f64) -> i32 {
        if !lines.is_finite() || lines == 0.0 {
            return 0;
        }
        if self.remainder != 0.0 && (lines > 0.0) != (self.remainder > 0.0) {
            self.remainder = 0.0;
        }
        let total = self.remainder + lines;
        let whole = total.trunc();
        self.remainder = total - whole;
        whole as i32
    }
}

fn to_lines(mode: DeltaMode, delta_y: f64, cell_height: f64, page_rows: usize) -> f64 {
    match mode {
        DeltaMode::Line => -delta_y,
        DeltaMode::Page => -delta_y * page_rows as f64,
        DeltaMode::Pixel => {
            if cell_height > 0.0 {
                -delta_y / cell_height
            } else {
                0.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! 用例逐条移植自 packages/web/src/lib/rio/wheel.test.ts。
    use super::DeltaMode::{Line, Page, Pixel};
    use super::*;

    #[test]
    fn line_mode_passes_through_with_sign_flip() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.push(Line, -3.0, 16.0, 24), 3);
        assert_eq!(acc.push(Line, 2.0, 16.0, 24), -2);
    }

    #[test]
    fn pixel_mode_accumulates_remainder() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.push(Pixel, -6.4, 16.0, 24), 0);
        assert_eq!(acc.push(Pixel, -6.4, 16.0, 24), 0);
        assert_eq!(acc.push(Pixel, -6.4, 16.0, 24), 1);
        assert_eq!(acc.push(Pixel, -12.8, 16.0, 24), 1);
    }

    #[test]
    fn direction_flip_clears_remainder() {
        let mut acc = WheelAccumulator::new();
        acc.push(Pixel, -12.0, 16.0, 24);
        assert_eq!(acc.push(Pixel, 4.0, 16.0, 24), 0);
        assert_eq!(acc.push(Pixel, 12.0, 16.0, 24), -1);
    }

    #[test]
    fn page_mode_uses_rows() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.push(Page, -1.0, 16.0, 40), 40);
    }

    #[test]
    fn zero_or_nonfinite_does_nothing() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.push(Pixel, -100.0, 0.0, 24), 0);
        assert_eq!(acc.push(Pixel, f64::NAN, 16.0, 24), 0);
    }

    #[test]
    fn reset_clears() {
        let mut acc = WheelAccumulator::new();
        acc.push(Pixel, -12.0, 16.0, 24);
        acc.reset();
        assert_eq!(acc.push(Pixel, -4.0, 16.0, 24), 0);
    }

    #[test]
    fn click_one_notch_one_click() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.click(Pixel, -100.0, 16.0, 24), 1);
        assert_eq!(acc.click(Pixel, -4.0, 16.0, 24), 0);
        assert_eq!(acc.click(Pixel, 100.0, 16.0, 24), -1);
    }

    #[test]
    fn click_trackpad_damping() {
        let mut acc = WheelAccumulator::new();
        for i in 0..8 {
            assert_eq!(acc.click(Pixel, -6.0, 16.0, 24), 0, "第 {} 次", i + 1);
        }
        assert_eq!(acc.click(Pixel, -6.0, 16.0, 24), 1);
    }

    #[test]
    fn click_large_delta_not_damped() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.click(Pixel, -50.0, 16.0, 24), 1);
        let mut damped = WheelAccumulator::new();
        assert_eq!(damped.click(Pixel, -49.0, 16.0, 24), 0);
    }

    #[test]
    fn click_line_and_page() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.click(Line, -3.0, 16.0, 24), 1);
        assert_eq!(acc.click(Line, 3.0, 16.0, 24), -1);
        assert_eq!(acc.click(Page, -1.0, 16.0, 40), 1);
    }

    #[test]
    fn route_follows_xterm_semantics() {
        let normal = TermMode::default();
        let alt = TermMode::default() | TermMode::ALT_SCREEN;
        assert!(normal.contains(TermMode::ALTERNATE_SCROLL), "alacritty 默认开着 ?1007");
        // 鼠标上报优先；Shift 绕过
        assert_eq!(route_wheel(alt, 1002, false), WheelRoute::Report);
        assert_eq!(route_wheel(normal, 1000, false), WheelRoute::Report);
        assert_eq!(route_wheel(normal, 1000, true), WheelRoute::Scrollback);
        // 备用屏、没开上报：转方向键，DECCKM 决定形式
        assert_eq!(route_wheel(alt, 0, false), WheelRoute::Arrows { app_cursor: false });
        assert_eq!(route_wheel(alt | TermMode::APP_CURSOR, 0, false), WheelRoute::Arrows { app_cursor: true });
        assert_eq!(route_wheel(alt, 1002, true), WheelRoute::Arrows { app_cursor: false });
        // 主屏：本地 scrollback
        assert_eq!(route_wheel(normal, 0, false), WheelRoute::Scrollback);
    }

    #[test]
    fn alternate_scroll_off_stops_arrows() {
        // 程序发了 CSI ? 1007 l
        let alt = (TermMode::default() | TermMode::ALT_SCREEN) - TermMode::ALTERNATE_SCROLL;
        assert_eq!(route_wheel(alt, 0, false), WheelRoute::Scrollback);
        assert_eq!(route_wheel(alt | TermMode::APP_CURSOR, 0, false), WheelRoute::Scrollback);
        // 关掉 ?1007 不影响鼠标上报
        assert_eq!(route_wheel(alt, 1006, false), WheelRoute::Report);
    }

    #[test]
    fn arrows() {
        assert_eq!(arrow_for_wheel(true, false), "\x1b[A");
        assert_eq!(arrow_for_wheel(false, false), "\x1b[B");
        assert_eq!(arrow_for_wheel(true, true), "\x1bOA");
        assert_eq!(arrow_for_wheel(false, true), "\x1bOB");
    }

    #[test]
    fn click_and_push_share_remainder() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.click(Pixel, -40.0, 16.0, 24), 0);
        assert_eq!(acc.click(Pixel, 40.0, 16.0, 24), 0);
        assert_eq!(acc.click(Pixel, 40.0, 16.0, 24), -1);
        assert_eq!(acc.push(Pixel, 8.0, 16.0, 24), -1);
    }
}
