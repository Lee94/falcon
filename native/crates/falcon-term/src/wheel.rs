//! 滚轮事件 → 滚动量。`packages/web/src/lib/rio/wheel.ts` 的移植。两种口径，取决于滚轮归谁：
//!
//! - [`WheelAccumulator::push`]：本地 scrollback 用，像素按 cell_height 折成行。不足一行的
//!   小数余量攒起来，攒够一行才吐——触控板慢滚每个事件只有几像素，直接截断就滚不动；方向翻转时
//!   清掉余量，免得反向滑动先"补"上一段。
//! - [`WheelAccumulator::click`]：程序接管了滚轮（鼠标上报 / alt screen，也就是 zellij 会话的
//!   常态）时用，一个事件最多折成一次点击。zellij 收到每条上报自己再滚 3 行，按行数发 n 条会让
//!   鼠标一格冲出十几行。xterm.js 的口径（MouseService._consumeWheelEvent）是行数只当门槛、每个
//!   事件最多发一条，多出的整行丢掉，且 |delta| < 50 的像素事件当作触控板再乘 0.3。照抄，与 web
//!   两个引擎手感一致。
//!
//! GPUI 的 `ScrollDelta::Lines` 对应 DOM 的 deltaMode = 1，`ScrollDelta::Pixels` 对应 0。

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
    fn click_and_push_share_remainder() {
        let mut acc = WheelAccumulator::new();
        assert_eq!(acc.click(Pixel, -40.0, 16.0, 24), 0);
        assert_eq!(acc.click(Pixel, 40.0, 16.0, 24), 0);
        assert_eq!(acc.click(Pixel, 40.0, 16.0, 24), -1);
        assert_eq!(acc.push(Pixel, 8.0, 16.0, 24), -1);
    }
}
