//! 工作画布的滚动层纯函数：滚轮手势的轴向锁定、内部滚动容器是否还能接这次滚轮、
//! 手势结束后的对齐目标、把活动列滚进视口。对应 web 的 `lib/termCanvas.ts`。
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

/// 手势结束后的对齐目标：离最近一条列左边不超过 proximity 才吸过去，否则停在原地。
///
/// - `lefts`：各列左边相对画布内容原点的偏移
/// - `max_scroll`：可滚的最远处；最后一列吸不到边时按它算
///
/// 返回目标滚动偏移；`None` = 不用动。
pub fn settle_target(scroll_left: f64, lefts: &[f64], proximity: f64, max_scroll: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    let mut best_dist = f64::INFINITY;
    for &left in lefts {
        let target = left.max(0.0).min(max_scroll.max(0.0));
        let dist = (target - scroll_left).abs();
        if dist < best_dist {
            best_dist = dist;
            best = Some(target);
        }
    }
    if best_dist > proximity || best_dist < 1.0 {
        return None;
    }
    best
}

/// 把一列滚进视口需要的滚动偏移：已经整列可见就不动；在左边露不全就对齐左边，
/// 在右边露不全就对齐右边（列比视口还宽时也按左边对齐）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reveal {
    pub scroll_left: f64,
    /// 画布内容区宽度
    pub viewport: f64,
    /// 列相对内容原点的偏移与宽度
    pub left: f64,
    pub width: f64,
}

pub fn reveal_scroll_left(r: Reveal) -> Option<f64> {
    let Reveal { scroll_left, viewport, left, width } = r;
    if width >= viewport {
        return if (scroll_left - left).abs() < 1.0 { None } else { Some(left) };
    }
    if left < scroll_left {
        return Some(left);
    }
    let right = left + width;
    if right > scroll_left + viewport {
        return Some(right - viewport);
    }
    None
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
    fn settle_target_snaps_only_within_proximity() {
        let lefts = [0.0, 400.0, 800.0];
        assert_eq!(settle_target(30.0, &lefts, 80.0, 800.0), Some(0.0));
        assert_eq!(settle_target(370.0, &lefts, 80.0, 800.0), Some(400.0));
        assert_eq!(settle_target(200.0, &lefts, 80.0, 800.0), None);
        // 已经对齐就不动
        assert_eq!(settle_target(400.0, &lefts, 80.0, 800.0), None);
        assert_eq!(settle_target(400.4, &lefts, 80.0, 800.0), None);
        // 最后一列吸不到边时按能滚到的最远处算
        assert_eq!(settle_target(560.0, &lefts, 80.0, 600.0), Some(600.0));
        assert_eq!(settle_target(770.0, &lefts, 80.0, 600.0), None);
        // 没有列时不动
        assert_eq!(settle_target(100.0, &[], 80.0, 800.0), None);
    }

    fn reveal(scroll_left: f64, viewport: f64, left: f64, width: f64) -> Option<f64> {
        reveal_scroll_left(Reveal { scroll_left, viewport, left, width })
    }

    #[test]
    fn reveal_leaves_fully_visible_columns_alone() {
        assert_eq!(reveal(0.0, 1000.0, 0.0, 500.0), None);
        assert_eq!(reveal(0.0, 1000.0, 500.0, 500.0), None);
    }

    #[test]
    fn reveal_aligns_the_clipped_side() {
        assert_eq!(reveal(300.0, 1000.0, 0.0, 500.0), Some(0.0));
        assert_eq!(reveal(0.0, 1000.0, 1000.0, 500.0), Some(500.0));
        assert_eq!(reveal(0.0, 1000.0, 800.0, 500.0), Some(300.0));
    }

    #[test]
    fn reveal_aligns_left_when_the_column_is_wider_than_the_viewport() {
        assert_eq!(reveal(0.0, 500.0, 600.0, 640.0), Some(600.0));
        assert_eq!(reveal(600.0, 500.0, 600.0, 640.0), None);
    }
}
