//! 工作画布的滚轮层纯函数：滚轮手势的轴向锁定、内部滚动容器是否还能接这次滚轮、
//! 横向手势翻画布。对应 web 的 `lib/termCanvas.ts`。
//!
//! 画布本身不再滚动（一块画布一屏，见 [`crate::layout::canvas_groups`]），横向手势
//! 的用处从"滚画布"变成"翻到左 / 右一块画布"。
//!
//! 列宽与窗口高度这类排布几何在 [`crate::layout`]，事件接线在视图层。

/// 两次滚轮事件间隔超过它就算新手势。macOS 触控板的惯性事件间隔在 16–50ms，
/// 人手两次连续滑动之间通常远大于此；取 120ms 兼顾两边。
pub const WHEEL_GESTURE_GAP_MS: f64 = 120.0;
/// 锁定后改判轴向的门槛：另一根轴单次至少这么多像素、且比主轴大一倍以上
pub const WHEEL_AXIS_FLIP_MIN_PX: f64 = 8.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAxis {
    X,
    Y,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WheelSample {
    pub delta_x: f64,
    pub delta_y: f64,
    /// 毫秒
    pub time_stamp: f64,
}

/// 滚轮手势的轴向锁定。
///
/// 触控板两指滑动几乎不可能只有一根轴，逐事件比较 |dx| / |dy| 会让一段横向滑动里
/// 混进几条被当成纵向的事件（终端跟着翻两行），反过来纵向滚终端时画布也会横漂。
/// 原生平台的做法是手势开始时定轴、整段手势只认这根轴，这里照做：
///
/// - 手势第一条有位移的事件定轴（相等算纵向：终端是主要消费者，宁可少滚画布）；
/// - 同一手势内不改判，除非另一根轴单次位移明显更大（惯性还没停用户就换了方向滑，
///   macOS 会立刻停掉惯性开始新手势，事件之间没有间隔可以判断）；
/// - 静默超过 [`WHEEL_GESTURE_GAP_MS`] 视为新手势重新定轴。
///
/// GPUI 的 ScrollWheelEvent 带 TouchPhase，能直接知道手势起止；但鼠标滚轮没有 phase，
/// 这里的时间间隔判据两种输入都能用，与 web 保持同一手感。
#[derive(Debug, Clone)]
pub struct WheelAxisLock {
    axis: Option<WheelAxis>,
    last: f64,
}

impl Default for WheelAxisLock {
    fn default() -> Self {
        WheelAxisLock { axis: None, last: f64::NEG_INFINITY }
    }
}

impl WheelAxisLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// 本事件归哪根轴；两轴都是 0 的空事件返回 `None` 且不改变状态
    pub fn classify(&mut self, s: WheelSample) -> Option<WheelAxis> {
        let ax = s.delta_x.abs();
        let ay = s.delta_y.abs();
        if ax == 0.0 && ay == 0.0 {
            return None;
        }
        if self.axis.is_some() && s.time_stamp - self.last > WHEEL_GESTURE_GAP_MS {
            self.axis = None;
        }
        self.last = s.time_stamp;
        let dominant = if ax > ay { WheelAxis::X } else { WheelAxis::Y };
        match self.axis {
            None => self.axis = Some(dominant),
            Some(axis) if axis != dominant => {
                let major = ax.max(ay);
                let minor = ax.min(ay);
                if major >= WHEEL_AXIS_FLIP_MIN_PX && major > minor * 2.0 {
                    self.axis = Some(dominant);
                }
            }
            Some(_) => {}
        }
        self.axis
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// WheelEvent 的 deltaMode
pub const DOM_DELTA_PIXEL: u32 = 0;
pub const DOM_DELTA_LINE: u32 = 1;
pub const DOM_DELTA_PAGE: u32 = 2;

/// 把滚轮 delta 折成像素：按行 / 页给的（Firefox 的鼠标滚轮；原生这边是 GPUI 的
/// `ScrollDelta::Lines`）乘上行高 / 页高。`line_size` / `page_size` 在 TS 里缺省 16 / 800。
pub fn wheel_delta_px(delta: f64, delta_mode: u32, line_size: f64, page_size: f64) -> f64 {
    match delta_mode {
        DOM_DELTA_LINE => delta * line_size,
        DOM_DELTA_PAGE => delta * page_size,
        _ => delta,
    }
}

/// 窗口内部滚动容器的溢出度量。画布自己接管横向滚轮，但文件 / 差异的内容区、终端的
/// 回滚区也会滚——这些还能沿手势方向滚时，事件必须留给内部，画布不抢。
///
/// overflow 用 web 的计算值字面量（`auto` / `scroll` / `overlay` 可滚，其余不行），
/// 原生视图层按自己的滚动容器填 `"auto"` 或 `"hidden"` 即可。
#[derive(Debug, Clone, PartialEq)]
pub struct OverflowBox {
    pub overflow_x: String,
    pub overflow_y: String,
    pub scroll_left: f64,
    pub scroll_top: f64,
    pub client_width: f64,
    pub client_height: f64,
    pub scroll_width: f64,
    pub scroll_height: f64,
}

/// 贴边容差：亚像素 / 缩放会让 max - scrollLeft 剩 0.5px，不能当成还能滚
const OVERFLOW_EDGE_PX: f64 = 1.0;

/// `overlay` 是 Chrome 旧值，按可滚处理
pub fn overflow_scrollable(value: &str) -> bool {
    matches!(value, "auto" | "scroll" | "overlay")
}

/// 这个容器还能沿 axis 消化这次滚轮吗。正 delta = 增加 scroll 偏移（右 / 下）。
pub fn overflow_can_consume(bx: &OverflowBox, axis: WheelAxis, delta_px: f64) -> bool {
    if !delta_px.is_finite() || delta_px == 0.0 {
        return false;
    }
    let (overflow, pos, client, scroll) = match axis {
        WheelAxis::X => (&bx.overflow_x, bx.scroll_left, bx.client_width, bx.scroll_width),
        WheelAxis::Y => (&bx.overflow_y, bx.scroll_top, bx.client_height, bx.scroll_height),
    };
    if !overflow_scrollable(overflow) {
        return false;
    }
    let max = scroll - client;
    if max <= OVERFLOW_EDGE_PX {
        return false;
    }
    if delta_px < 0.0 { pos > OVERFLOW_EDGE_PX } else { pos < max - OVERFLOW_EDGE_PX }
}

/// 从内到外，任一容器还能沿该轴滚就归内部
pub fn inner_takes_wheel(boxes: &[OverflowBox], axis: WheelAxis, delta_px: f64) -> bool {
    boxes.iter().any(|b| overflow_can_consume(b, axis, delta_px))
}

/// 一段横向手势累计横移超过这么多才翻画布：比随手的横漂大，又不用滑满一屏
pub const CANVAS_SWIPE_PX: f64 = 80.0;

/// 横向手势翻画布：一段手势（含 macOS 的惯性尾巴）至多翻一块，累计横移过了门槛才翻。
/// 逐事件翻的话，触控板一次滑动几十条事件会一口气翻到底。
///
/// 手势的切分与 [`WheelAxisLock`] 同一个口径：静默超过 [`WHEEL_GESTURE_GAP_MS`] 算新手势。
/// 这段手势里只要有一条被窗口内部（文件长行、差异）吃掉过，整段都不翻——横着滚
/// 一个宽文件滚到头、手还没停，不该顺势把画布也翻走。
#[derive(Debug, Clone)]
pub struct CanvasSwipe {
    sum: f64,
    done: bool,
    last: f64,
}

impl Default for CanvasSwipe {
    fn default() -> Self {
        CanvasSwipe { sum: 0.0, done: false, last: f64::NEG_INFINITY }
    }
}

impl CanvasSwipe {
    pub fn new() -> Self {
        Self::default()
    }

    fn touch(&mut self, time_stamp: f64) {
        if time_stamp - self.last > WHEEL_GESTURE_GAP_MS {
            self.sum = 0.0;
            self.done = false;
        }
        self.last = time_stamp;
    }

    /// 喂一条归给画布的横向事件。返回 1 = 翻到右边一块，-1 = 左边一块，0 = 不翻
    pub fn push(&mut self, delta_px: f64, time_stamp: f64) -> i64 {
        self.touch(time_stamp);
        if self.done || !delta_px.is_finite() {
            return 0;
        }
        self.sum += delta_px;
        if self.sum.abs() < CANVAS_SWIPE_PX {
            return 0;
        }
        self.done = true;
        if self.sum > 0.0 { 1 } else { -1 }
    }

    /// 这段手势被窗口内部接走了：剩下的事件都不翻
    pub fn hold(&mut self, time_stamp: f64) {
        self.touch(time_stamp);
        self.done = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use WheelAxis::{X, Y};

    fn s(delta_x: f64, delta_y: f64, time_stamp: f64) -> WheelSample {
        WheelSample { delta_x, delta_y, time_stamp }
    }

    #[test]
    fn first_event_of_a_gesture_picks_the_axis_and_diagonal_noise_does_not_flip_it() {
        let mut lock = WheelAxisLock::new();
        assert_eq!(lock.classify(s(12.0, 3.0, 0.0)), Some(X));
        // 惯性尾巴里 |dy| 略大于 |dx|，仍归横向
        assert_eq!(lock.classify(s(2.0, 3.0, 16.0)), Some(X));
        assert_eq!(lock.classify(s(0.0, 1.0, 32.0)), Some(X));
    }

    #[test]
    fn a_tie_counts_as_vertical() {
        let mut lock = WheelAxisLock::new();
        assert_eq!(lock.classify(s(4.0, 4.0, 0.0)), Some(Y));
    }

    #[test]
    fn empty_events_neither_pick_an_axis_nor_extend_the_gesture() {
        let mut lock = WheelAxisLock::new();
        assert_eq!(lock.classify(s(0.0, 0.0, 0.0)), None);
        assert_eq!(lock.classify(s(0.0, 9.0, 10.0)), Some(Y));
        assert_eq!(lock.classify(s(0.0, 0.0, 20.0)), None);
        // 空事件没有刷新时间戳：从上一条有位移的事件起算，超过间隔就是新手势
        assert_eq!(lock.classify(s(9.0, 0.0, 10.0 + WHEEL_GESTURE_GAP_MS + 1.0)), Some(X));
    }

    #[test]
    fn silence_past_the_gap_starts_a_new_gesture() {
        let mut lock = WheelAxisLock::new();
        assert_eq!(lock.classify(s(12.0, 0.0, 0.0)), Some(X));
        // 正好在间隔内：还是同一手势，小幅纵向抖动不改判
        assert_eq!(lock.classify(s(0.0, 5.0, WHEEL_GESTURE_GAP_MS)), Some(X));
        assert_eq!(lock.classify(s(1.0, 5.0, WHEEL_GESTURE_GAP_MS * 2.0 + 1.0)), Some(Y));
    }

    #[test]
    fn a_clearly_larger_other_axis_flips_the_lock() {
        let mut lock = WheelAxisLock::new();
        assert_eq!(lock.classify(s(30.0, 0.0, 0.0)), Some(X));
        // 小抖动不算：8px 以下、或没到主轴两倍
        assert_eq!(lock.classify(s(3.0, 7.0, 16.0)), Some(X));
        assert_eq!(lock.classify(s(6.0, 10.0, 32.0)), Some(X));
        assert_eq!(lock.classify(s(2.0, 20.0, 48.0)), Some(Y));
        // 改判后保持
        assert_eq!(lock.classify(s(3.0, 2.0, 64.0)), Some(Y));
    }

    #[test]
    fn reset_picks_the_axis_again() {
        let mut lock = WheelAxisLock::new();
        assert_eq!(lock.classify(s(12.0, 0.0, 0.0)), Some(X));
        lock.reset();
        assert_eq!(lock.classify(s(0.0, 5.0, 1.0)), Some(Y));
    }

    #[test]
    fn wheel_delta_px_scales_lines_and_pages() {
        assert_eq!(wheel_delta_px(7.0, 0, 16.0, 800.0), 7.0);
        assert_eq!(wheel_delta_px(3.0, 1, 16.0, 800.0), 48.0);
        assert_eq!(wheel_delta_px(-1.0, 2, 16.0, 640.0), -640.0);
    }

    fn bx() -> OverflowBox {
        OverflowBox {
            overflow_x: "hidden".into(),
            overflow_y: "hidden".into(),
            scroll_left: 0.0,
            scroll_top: 0.0,
            client_width: 100.0,
            client_height: 100.0,
            scroll_width: 100.0,
            scroll_height: 100.0,
        }
    }

    #[test]
    fn overflow_scrollable_values() {
        for v in ["auto", "scroll", "overlay"] {
            assert!(overflow_scrollable(v), "{v}");
        }
        for v in ["hidden", "visible", "clip"] {
            assert!(!overflow_scrollable(v), "{v}");
        }
    }

    #[test]
    fn hidden_or_non_overflowing_boxes_do_not_consume() {
        assert!(!overflow_can_consume(&OverflowBox { scroll_height: 400.0, ..bx() }, Y, 10.0));
        assert!(!overflow_can_consume(&OverflowBox { overflow_y: "auto".into(), ..bx() }, Y, 10.0));
        assert!(!overflow_can_consume(
            &OverflowBox { overflow_x: "auto".into(), scroll_width: 400.0, ..bx() },
            Y,
            10.0
        ));
    }

    #[test]
    fn zero_delta_does_not_consume() {
        assert!(!overflow_can_consume(&OverflowBox { overflow_y: "auto".into(), scroll_height: 400.0, ..bx() }, Y, 0.0));
    }

    #[test]
    fn vertical_consumes_only_while_it_can_still_move() {
        let y = OverflowBox { overflow_y: "auto".into(), scroll_height: 400.0, ..bx() };
        assert!(overflow_can_consume(&y, Y, 10.0));
        assert!(!overflow_can_consume(&y, Y, -10.0));
        assert!(overflow_can_consume(&OverflowBox { scroll_top: 50.0, ..y.clone() }, Y, -10.0));
        assert!(!overflow_can_consume(&OverflowBox { scroll_top: 300.0, ..y.clone() }, Y, 10.0));
        assert!(overflow_can_consume(&OverflowBox { scroll_top: 300.0, ..y }, Y, -10.0));
    }

    #[test]
    fn horizontal_likewise() {
        let x = OverflowBox { overflow_x: "scroll".into(), scroll_width: 400.0, ..bx() };
        assert!(overflow_can_consume(&x, X, 10.0));
        assert!(!overflow_can_consume(&x, X, -10.0));
        assert!(overflow_can_consume(&OverflowBox { scroll_left: 20.0, ..x.clone() }, X, -10.0));
        assert!(!overflow_can_consume(&OverflowBox { scroll_left: 300.0, ..x }, X, 10.0));
    }

    #[test]
    fn subpixel_edges_count_as_unscrollable() {
        assert!(!overflow_can_consume(&OverflowBox { overflow_y: "auto".into(), scroll_height: 100.4, ..bx() }, Y, 10.0));
        assert!(!overflow_can_consume(
            &OverflowBox { overflow_x: "auto".into(), scroll_width: 400.0, client_width: 100.0, scroll_left: 0.4, ..bx() },
            X,
            -10.0
        ));
    }

    #[test]
    fn inner_takes_wheel_if_any_box_inside_out_can_move() {
        let inner = bx();
        let scroller = OverflowBox { overflow_y: "auto".into(), scroll_height: 400.0, ..bx() };
        assert!(inner_takes_wheel(&[inner.clone(), scroller.clone()], Y, 10.0));
        assert!(!inner_takes_wheel(&[inner], Y, 10.0));
        // 只认被问的那根轴
        assert!(!inner_takes_wheel(std::slice::from_ref(&scroller), X, 10.0));
        assert!(inner_takes_wheel(&[scroller], Y, 10.0));
    }

    #[test]
    fn swipe_flips_once_per_gesture_after_the_threshold() {
        let mut swipe = CanvasSwipe::new();
        let step = CANVAS_SWIPE_PX / 4.0;
        assert_eq!(swipe.push(step, 0.0), 0);
        assert_eq!(swipe.push(step, 16.0), 0);
        assert_eq!(swipe.push(step, 32.0), 0);
        assert_eq!(swipe.push(step, 48.0), 1);
        // 惯性尾巴还在同一段手势里：不再翻
        let mut t = 64.0;
        while t < 600.0 {
            assert_eq!(swipe.push(step * 4.0, t), 0);
            t += 16.0;
        }
    }

    #[test]
    fn swipe_after_a_pause_is_a_new_gesture_and_follows_the_sign() {
        let mut swipe = CanvasSwipe::new();
        assert_eq!(swipe.push(CANVAS_SWIPE_PX, 0.0), 1);
        assert_eq!(swipe.push(-CANVAS_SWIPE_PX, WHEEL_GESTURE_GAP_MS + 1.0), -1);
    }

    #[test]
    fn swipe_jitter_cancels_out() {
        let mut swipe = CanvasSwipe::new();
        for i in 0..20 {
            let d = if i % 2 == 1 { -30.0 } else { 30.0 };
            assert_eq!(swipe.push(d, i as f64 * 16.0), 0);
        }
    }

    #[test]
    fn swipe_held_by_an_inner_scroller_never_flips() {
        let mut swipe = CanvasSwipe::new();
        swipe.hold(0.0);
        assert_eq!(swipe.push(CANVAS_SWIPE_PX * 3.0, 16.0), 0);
        assert_eq!(swipe.push(CANVAS_SWIPE_PX * 3.0, 16.0 + WHEEL_GESTURE_GAP_MS + 1.0), 1);
    }
}
