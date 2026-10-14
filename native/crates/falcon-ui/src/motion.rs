//! 状态指示的动画（程序状态的转圈、扫光、呼吸，换状态时的弹 / 抖 / 闪）：**不看「减少动态效果」**。
//!
//! GPUI 的 `with_animation` 在 `App::reduce_motion` 为真时整个停在静止帧上，而 gpui-base 启动时
//! 把系统的「减少动态效果」（macOS 辅助功能 → 显示）读进这个开关。实测开着它的机器上，运行中的
//! 图标一动不动、底边的扫光根本不出现——看不出程序还活着。可这几处动画表达的就是状态本身：
//! macOS 自己的进度指示（NSProgressIndicator）在减少动态效果下也照转，这个开关要压的是大幅位移
//! （缩放、视差、滑入）。所以这里另写一个动画元素，与 `AnimationElement` 同一套（元素状态记起点、
//! 按时间算进度、限帧重画），只是不看那个开关；界面别处（组件库的过渡）照旧跟系统设置走。
//!
//! 只给几像素大的状态指示用，别拿它做大块的位移。

use std::cell::Cell;
use std::panic::Location;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    AnyElement, App, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Window,
};
use web_time::Instant;

/// 持续动画（转圈、扫光、呼吸）的帧率上限。动画逐帧重画的是发起它的整个视图（画布、侧栏），
/// 60Hz 不值：12px 的图标一秒一圈，30 帧看着已经顺；agent 一跑就是几十分钟，省下的是实打实的 CPU
pub const FPS: f32 = 30.;

/// 一段动画：多长、循环与否、缓动
#[derive(Clone, Copy)]
pub struct Motion {
    duration: Duration,
    repeat: bool,
    easing: fn(f32) -> f32,
}

impl Motion {
    /// 播一次（`t` 从 0 走到 1 后停在 1），线性
    pub fn once(ms: u64) -> Self {
        Self { duration: Duration::from_millis(ms), repeat: false, easing: linear }
    }

    /// 循环（`t` 每圈从 0 走到 1），线性
    pub fn repeat(ms: u64) -> Self {
        Self { duration: Duration::from_millis(ms), repeat: true, easing: linear }
    }

    pub fn easing(mut self, easing: fn(f32) -> f32) -> Self {
        self.easing = easing;
        self
    }
}

fn linear(t: f32) -> f32 {
    t
}

/// 呼吸：0 → 1 → 0 一个来回，两头慢（GPUI 的 `pulsating_between(0, 1)`，换成 fn 好放进 [`Motion`]）。
/// t=0 时在中间（0.5），循环接缝处是连续的
pub fn breathe(t: f32) -> f32 {
    let s = (t * 2. * std::f32::consts::PI).sin();
    ((s * s * s + s) / 2. + 1.) / 2.
}

/// 给元素挂上一段 [`Motion`]：`animator` 拿到缓动后的 `t` 改元素。`id` 只要在父元素里唯一；
/// 换一个 id 就是新元素，一次性的动画从头播
pub trait MotionExt: IntoElement + Sized + 'static {
    fn motion(self, id: impl Into<ElementId>, motion: Motion, animator: impl Fn(Self, f32) -> Self + 'static) -> Moving<Self> {
        Moving { id: id.into(), element: Some(self), motion, animator: Box::new(animator) }
    }
}

impl<E: IntoElement + 'static> MotionExt for E {}

pub struct Moving<E> {
    id: ElementId,
    element: Option<E>,
    motion: Motion,
    animator: Box<dyn Fn(E, f32) -> E>,
}

struct MotionState {
    start: Instant,
    /// 下一帧的定时器已经排上了：同一帧里画了几次（布局重算）也只排一个
    frame_pending: Rc<Cell<bool>>,
}

impl<E: IntoElement + 'static> IntoElement for Moving<E> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<E: IntoElement + 'static> Element for Moving<E> {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        window.with_element_state(global_id.expect("Moving 总有 id"), |state: Option<MotionState>, window| {
            let state =
                state.unwrap_or_else(|| MotionState { start: Instant::now(), frame_pending: Rc::new(Cell::new(false)) });
            let elapsed = state.start.elapsed().as_secs_f32() / self.motion.duration.as_secs_f32().max(f32::EPSILON);
            let (t, done) = if self.motion.repeat { (elapsed.fract(), false) } else { (elapsed.min(1.), elapsed >= 1.) };
            let element = self.element.take().expect("request_layout 只调一次");
            let mut element = (self.animator)(element, (self.motion.easing)(t)).into_any_element();
            // 和 `with_max_fps` 的 AnimationElement 一样：定时器到点再 notify 发起它的视图
            if !done && !state.frame_pending.get() {
                state.frame_pending.set(true);
                let pending = state.frame_pending.clone();
                let view = window.current_view();
                let interval = Duration::from_secs_f32(1. / FPS);
                window
                    .spawn(cx, async move |cx| {
                        cx.background_executor().timer(interval).await;
                        pending.set(false);
                        cx.update(move |_, cx| cx.notify(view)).ok();
                    })
                    .detach();
            }
            ((element.request_layout(window, cx), element), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: gpui_kit::Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: gpui_kit::Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}
