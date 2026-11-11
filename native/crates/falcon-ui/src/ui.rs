//! 界面小件：岛、图标按钮、状态记号、主机色条。颜色一律取 [`crate::theme::Ui`] 的语义 token。

use std::cell::RefCell;
use std::panic::Location;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use falcon_core::shortcuts::{self, Command};
use falcon_proto::SessionState;
use gpui_kit::assets::IconName;
use gpui_kit::base::animation::ease_out_cubic;
use gpui_kit::base::motion::{Spring, spring};
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Animation, AnimationExt, App, BorderStyle, Bounds, BoxShadow, ClickEvent, Context,
    Corners, Div, Edges, Element, ElementId, Entity, FontWeight, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, Pixels, Render, SharedString,
    Stateful, Styled, Transformation, Window, black, div, hsla, percentage, point, px, quad,
};

use crate::theme::{Ui, radius};
use crate::zoom::zpx;

/// 浮动岛外沿的 0 偏移 `app_border` 细环与浅色环境投影（ADR 0011：深色靠 `app_border` 提亮外沿，
/// 浅色叠加柔和投影；走 `box-shadow` 省掉 `border` 的 1px 尺寸账，也不会被 `overflow-hidden` 内容压掉）
pub fn island_shadows(ui: &Ui) -> Vec<BoxShadow> {
    let ring = BoxShadow {
        color: ui.app_border,
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: false,
    };
    if ui.is_dark {
        vec![ring]
    } else {
        vec![
            ring,
            BoxShadow {
                color: black().opacity(0.055),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            },
            BoxShadow {
                color: black().opacity(0.03),
                offset: point(px(0.), px(1.)),
                blur_radius: px(2.),
                spread_radius: px(0.),
                inset: false,
            },
        ]
    }
}

/// 浮动岛（ADR 0011）：面板底一律 `background`，圆角 LG，不画分栏边框，外沿环与投影走 `box-shadow`
pub fn island(cx: &App) -> Div {
    let ui = Ui::global(cx);
    div()
        .bg(ui.background)
        .rounded(radius::LG)
        .shadow(island_shadows(ui))
        .overflow_hidden()
}

/// 岛里再嵌一块（统计卡、代码块、分段控件的槽）：借窗口底色，不用边框。
/// 窗口底开了透明或模糊时这里仍用实色 `app`：它垫在岛的实底上，跟着变透明会在面板里开一个洞
pub fn sunken(cx: &App) -> Div {
    let ui = Ui::global(cx);
    div().bg(ui.app).rounded(radius::MD)
}

pub fn icon(name: IconName) -> Icon {
    Icon::new(name)
}

/// 拖动时跟着指针的小标签（侧栏的项目）。落点只认载荷的类型，标签只是给人看的
pub struct DragLabel {
    label: SharedString,
}

impl DragLabel {
    pub fn new(label: SharedString, cx: &mut App) -> Entity<Self> {
        cx.new(|_| DragLabel { label })
    }
}

impl Render for DragLabel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx);
        div()
            .max_w(zpx(260.))
            .px_2p5()
            .py_1()
            .rounded(radius::MD)
            .bg(ui.popover)
            .text_color(ui.popover_foreground)
            .border_1()
            .border_color(ui.app_border)
            .shadow_md()
            .text_size(zpx(12.))
            .font_weight(FontWeight::MEDIUM)
            .truncate()
            .child(self.label.clone())
    }
}

/// 「当前选中」那块着色面的淡入（150ms，先快后慢）：`color` 从全透明淡到实色，文字色瞬时切换。
/// 只包在选中的那一块上；未选中不包动画，取消选中立即褪去，不做淡出。
/// 一次性动画在「减少动态效果」下直接落在完成帧（`AnimationElement` 的 oneshot 语义），不会丢选中色；
/// 元素状态按 id 记，主题切换 / 列表重画不会重播，只有新挂上的选中块淡入一次
pub fn selected_fade<E>(id: impl Into<ElementId>, el: E, color: Hsla) -> impl IntoElement
where
    E: IntoElement + Styled + 'static,
{
    el.with_animation(id.into(), Animation::new(Duration::from_millis(150)).with_easing(ease_out_cubic), move |el, delta| {
        el.bg(color.opacity(color.a * delta))
    })
}

/// 选中描边的同步淡入（150ms，与 [`selected_fade`] 同节奏）：用于活动窗格的 1px 焦点环或选中卡片外沿
pub fn border_fade<E>(id: impl Into<ElementId>, el: E, color: Hsla) -> impl IntoElement
where
    E: IntoElement + Styled + 'static,
{
    el.with_animation(id.into(), Animation::new(Duration::from_millis(150)).with_easing(ease_out_cubic), move |el, delta| {
        el.border_color(color.opacity(color.a * delta))
    })
}

/// 按钮 hover 底色的淡入淡出（120ms）。用法：在按钮里垫一层交给它的绝对定位背景块
/// （`relative` 的按钮 + `inset_0` 的这块），按钮本体的返回类型与交互链不变；hover 的
/// 文字色仍由按钮自己的 `hover()` 闭包瞬时切换。背景块不挂监听、不挡事件。
///
/// **只给按钮类控件用**（活动栏、图标按钮、画布条格子），别给列表行：指针扫过一串行会拖
/// 尾迹，而且每行都要在过渡期间逐帧重画所在的视图。
///
/// hover 状态渲染时读不到（`hover()` 只是样式闭包），`on_hover` 挂在这种 absolute 内垫块上
/// 又收不到事件（实测不触发），所以这里自己判定：prepaint 记下元素矩形，request_layout 时
/// 与 `window.mouse_position()` 比对。hover 进出时按钮本体的 hover 样式（文字色）本来就会
/// 通知视图重画，状态变化随之被读到；过渡期间由动画自己排帧。初次挂载不包动画——否则按钮
/// 一出现就闪一下 hover 色；「减少动态效果」下 oneshot 直接落完成帧，hover 色不丢
pub fn hover_fade(id: impl Into<ElementId>, bg: Hsla, radius: Pixels) -> HoverFade {
    let id = id.into();
    HoverFade { id: id.clone(), element: Some(div().id((id, "bg")).absolute().inset_0().rounded(radius)), bg }
}

pub struct HoverFade {
    id: ElementId,
    element: Option<Stateful<Div>>,
    bg: Hsla,
}

#[derive(Default)]
struct HoverState {
    /// 上一帧的元素矩形（request_layout 时本帧的还没算出来）
    bounds: Option<gpui_kit::Bounds<Pixels>>,
    hovered: bool,
    /// 第一次 hover 之后才包动画（初次挂载不播）
    touched: bool,
}

impl IntoElement for HoverFade {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for HoverFade {
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
        window.with_element_state(global_id.expect("HoverFade 总有 id"), |state: Option<HoverState>, window| {
            let mut state = state.unwrap_or_default();
            let hovered = state.bounds.is_some_and(|b| b.contains(&window.mouse_position()));
            if hovered != state.hovered {
                state.hovered = hovered;
                state.touched = true;
            }
            let el = self.element.take().expect("request_layout 只调一次");
            let mut el = if state.touched {
                let (bg, h) = (self.bg, state.hovered);
                el.with_animation(
                    (self.id.clone(), if h { "on" } else { "off" }),
                    Animation::new(Duration::from_millis(120)).with_easing(ease_out_cubic),
                    move |el, delta| el.bg(bg.opacity(bg.a * if h { delta } else { 1. - delta })),
                )
                .into_any_element()
            } else {
                el.into_any_element()
            };
            ((el.request_layout(window, cx), el), state)
        })
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: gpui_kit::Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_element_state(id.expect("HoverFade 总有 id"), |state: Option<HoverState>, _| {
            let mut state = state.unwrap_or_default();
            state.bounds = Some(bounds);
            ((), state)
        });
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

/// 小号图标按钮：hover 灰（底色淡入淡出）、按下 / 选中用 tint（"当前选中"用 tint，hover 用灰）
pub fn icon_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
    size: Pixels,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let ui = Ui::global(cx);
    let tooltip: SharedString = tooltip.into();
    let id = id.into();
    div()
        .id(id.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .relative()
        .rounded(zpx(8.))
        .text_color(ui.muted_foreground)
        .cursor_pointer()
        // 底色交给背景块淡入淡出，文字色瞬时，按下给轻微不透明度回馈
        .hover(|s| s.text_color(ui.foreground))
        .active(|s| s.opacity(0.72))
        .child(hover_fade((id, "hover"), ui.muted, zpx(8.)))
        .child(icon(name).size(size * 0.6))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .on_click(on_click)
}

/// 折叠三角（0° ↔ 90° 顺滑旋转 160ms）：初次挂载直接落在目标角度，只有点击切换后才播过渡，
/// 避免列表或对话初次出现时整排箭头一起转一下
pub fn disclosure_chevron(id: impl Into<ElementId>, expanded: bool, size: Pixels) -> DisclosureChevron {
    DisclosureChevron { id: id.into(), expanded, size, color: None }
}

pub struct DisclosureChevron {
    id: ElementId,
    expanded: bool,
    size: Pixels,
    color: Option<Hsla>,
}

impl DisclosureChevron {
    pub fn text_color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

#[derive(Default, Clone, Copy)]
struct ChevronState {
    prev: Option<bool>,
    animated: bool,
}

impl IntoElement for DisclosureChevron {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for DisclosureChevron {
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
        let expanded = self.expanded;
        let size = self.size;
        let color = self.color;
        let id = self.id.clone();
        window.with_element_state(global_id.expect("DisclosureChevron 总有 id"), |state: Option<ChevronState>, window| {
            let mut state = state.unwrap_or_default();
            match state.prev {
                None => state.prev = Some(expanded),
                Some(prev) if prev != expanded => {
                    state.prev = Some(expanded);
                    state.animated = true;
                }
                _ => {}
            }
            let base = icon(IconName::ChevronRight)
                .size(size)
                .when_some(color, |i, c| i.text_color(c));
            let mut el = if state.animated {
                base.with_animation(
                    (id, if expanded { "open" } else { "closed" }),
                    Animation::new(Duration::from_millis(160)).with_easing(ease_out_cubic),
                    move |i, delta| {
                        let turn = if expanded { 0.25 * delta } else { 0.25 * (1. - delta) };
                        i.transform(Transformation::rotate(percentage(turn)))
                    },
                )
                .into_any_element()
            } else {
                base.when(expanded, |i| i.transform(Transformation::rotate(percentage(0.25))))
                    .into_any_element()
            };
            ((el.request_layout(window, cx), el), state)
        })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
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
        _bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        element.paint(window, cx);
    }
}

pub type SegBoundsSlot = Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>;

pub fn seg_bounds_slot(len: usize) -> SegBoundsSlot {
    Rc::new(RefCell::new(vec![None; len]))
}

/// Apple 风格弹簧滑动分段控件（macOS System Settings 的分段槽）：
/// 底层画无边框凹槽（`app`），中层画跟随时带速度连续性的弹簧滑块（浅色白底微投影、深色提亮浮块 + `app_border`），
/// 顶层画透明底的各段文字与图标。初次挂载直接停在选中段，切换时滑块位置与宽度平滑过渡，静止时零帧开销
pub fn spring_segmented(
    id: impl Into<ElementId>,
    active: Option<usize>,
    slot: SegBoundsSlot,
    row: Div,
    cx: &App,
) -> SpringSegmented {
    let ui = Ui::global(cx);
    let thumb_bg = if ui.is_dark { ui.popover } else { ui.background };
    let thumb_shadows = vec![BoxShadow {
        color: black().opacity(if ui.is_dark { 0.28 } else { 0.08 }),
        offset: point(px(0.), px(1.)),
        blur_radius: px(3.),
        spread_radius: px(0.),
        inset: false,
    }];
    SpringSegmented {
        id: id.into(),
        active,
        slot,
        row: Some(row),
        track_bg: ui.app,
        thumb_bg,
        thumb_border: ui.app_border,
        thumb_shadows,
    }
}

pub struct SpringSegmented {
    id: ElementId,
    active: Option<usize>,
    slot: SegBoundsSlot,
    row: Option<Div>,
    track_bg: Hsla,
    thumb_bg: Hsla,
    thumb_border: Hsla,
    thumb_shadows: Vec<BoxShadow>,
}

impl IntoElement for SpringSegmented {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for SpringSegmented {
    type RequestLayoutState = AnyElement;
    type PrepaintState = Option<Bounds<Pixels>>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut el = self.row.take().expect("request_layout 只调一次").into_any_element();
        (el.request_layout(window, cx), el)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        element.prepaint(window, cx);
        let idx = self.active?;
        let seg = self.slot.borrow().get(idx).copied().flatten()?;
        let rel_x = f32::from(seg.origin.x - bounds.origin.x);
        let seg_w = f32::from(seg.size.width);
        let policy = Spring::new(Duration::from_millis(200))
            .with_damping(0.95)
            .with_epsilon(0.5);
        let anim_x = spring((self.id.clone(), "x"), rel_x, policy, window, cx);
        let anim_w = spring((self.id.clone(), "w"), seg_w, policy, window, cx);
        Some(Bounds::new(
            point(bounds.origin.x + px(anim_x), seg.origin.y),
            gpui_kit::size(px(anim_w.max(0.)), seg.size.height),
        ))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        element: &mut Self::RequestLayoutState,
        thumb: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let track_radii = Corners::all(radius::MD);
        window.paint_quad(quad(
            bounds,
            track_radii,
            self.track_bg,
            Edges::default(),
            gpui_kit::transparent_black(),
            BorderStyle::default(),
        ));
        if let Some(tb) = *thumb {
            let thumb_radii = Corners::all(radius::SM);
            window.paint_drop_shadows(tb, thumb_radii, &self.thumb_shadows);
            window.paint_quad(quad(
                tb,
                thumb_radii,
                self.thumb_bg,
                Edges::all(px(1.)),
                self.thumb_border,
                BorderStyle::default(),
            ));
        }
        element.paint(window, cx);
    }
}

/// 快捷键徽标（命令面板与下拉菜单共用）：紧凑键帽造型，等宽对齐
pub fn kbd_badge(text: impl Into<SharedString>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    div()
        .flex_none()
        .h(zpx(18.))
        .px(zpx(5.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(zpx(5.))
        .bg(ui.app)
        .border_1()
        .border_color(ui.border)
        .font_family(crate::fonts::BERKELEY)
        .text_size(zpx(10.5))
        .text_color(ui.muted_foreground)
        .child(text.into())
}

/// 会话状态记号：形状 + 颜色双编码（旧 React 版的 StatusMark）。creating 是纯前端状态
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Active,
    Unverified,
    Dead,
    Creating,
}

impl From<SessionState> for Mark {
    fn from(s: SessionState) -> Self {
        match s {
            SessionState::Active => Mark::Active,
            SessionState::Unverified => Mark::Unverified,
            SessionState::Dead => Mark::Dead,
            _ => Mark::Active,
        }
    }
}

impl Mark {
    pub fn label(self) -> String {
        use rust_i18n::t;
        match self {
            Mark::Active => t!("session.state_active").to_string(),
            Mark::Unverified => t!("session.state_unverified").to_string(),
            Mark::Dead => t!("session.state_dead").to_string(),
            Mark::Creating => t!("session.state_creating").to_string(),
        }
    }
}

pub fn status_mark(mark: Mark, size: Pixels, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let (name, color) = match mark {
        Mark::Active => (IconName::CircleDot, ui.success),
        Mark::Unverified => (IconName::CircleAlert, ui.warning),
        Mark::Dead => (IconName::CircleX, ui.destructive),
        Mark::Creating => (IconName::LoaderCircle, ui.muted_foreground),
    };
    icon(name).size(size).text_color(color).into_any_element()
}

/// 转圈的图标（程序在跑）：一秒一圈，限帧到 `motion::FPS`，不看「减少动态效果」（转圈就是
/// "还在跑"这个状态本身，见 [`crate::motion`]）。`id` 只要在父元素里唯一
pub fn spinner(id: impl Into<ElementId>, name: IconName, size: Pixels) -> AnyElement {
    use crate::motion::{Motion, MotionExt};
    icon(name)
        .size(size)
        .motion(id, Motion::repeat(1000), |i, t| i.transform(Transformation::rotate(percentage(t))))
        .into_any_element()
}

/// 「在等你」（程序卡在 blocked，OSC 7501）的记号：警示色的一圈、圆心一点。侧栏的会话行与收起的
/// 检出行共用——那一列别的记号都是灰白的，只有它上色，扫一眼就找得到
pub fn needs_you_mark(size: Pixels, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded_full()
        .border(size * 0.13)
        .border_color(ui.warning)
        .child(div().size(size * 0.38).rounded_full().bg(ui.warning))
        .into_any_element()
}

/// 程序状态的图标（终端标题栏的状态胶囊）：blocked 按要的东西分（授权 / 回答 / 凭据），其余按状态
pub fn program_icon(
    tone: falcon_core::program_status::BadgeTone,
    kind: Option<falcon_proto::ProgramBlockKind>,
) -> IconName {
    use falcon_core::program_status::BadgeTone;
    use falcon_proto::ProgramBlockKind;
    match (tone, kind) {
        (BadgeTone::Blocked, Some(ProgramBlockKind::Permission)) => IconName::ShieldQuestionMark,
        (BadgeTone::Blocked, Some(ProgramBlockKind::Question)) => IconName::MessageCircleQuestionMark,
        (BadgeTone::Blocked, Some(ProgramBlockKind::Auth)) => IconName::KeyRound,
        (BadgeTone::Blocked, _) => IconName::Hand,
        (BadgeTone::Error, _) => IconName::TriangleAlert,
        (BadgeTone::Working, _) => IconName::LoaderCircle,
        (BadgeTone::Done, _) => IconName::CircleCheck,
    }
}

/// 程序状态的语义色（图标与文字）：blocked 警示、error 危险、working 强调蓝、done 成功绿
pub fn program_color(tone: falcon_core::program_status::BadgeTone, cx: &App) -> Hsla {
    use falcon_core::program_status::BadgeTone;
    let ui = Ui::global(cx);
    match tone {
        BadgeTone::Blocked => ui.warning,
        BadgeTone::Error => ui.destructive,
        BadgeTone::Working => ui.tint_strong,
        BadgeTone::Done => ui.success,
    }
}

/// 主机身份色：色相由连接串散列而来，饱和度 / 亮度随主题（React 版 hostColor.ts 的 hsl(h, s%, l%)）
pub fn host_color(hue: u32, cx: &App) -> Hsla {
    let ui = Ui::global(cx);
    hsla(hue as f32 / 360.0, ui.host_s as f32 / 100.0, ui.host_l as f32 / 100.0, 1.0)
}

/// 3px 宽的主机色条
pub fn host_bar(bar: Option<falcon_core::host_color::HostBar>, height: Pixels, cx: &App) -> AnyElement {
    use falcon_core::host_color::HostBar;
    let color = match bar {
        Some(HostBar::Hue(h)) => host_color(h, cx),
        Some(HostBar::Neutral) => Ui::global(cx).border,
        None => gpui_kit::transparent_black(),
    };
    div()
        .flex_none()
        .w(zpx(3.))
        .h(height)
        .rounded_full()
        .bg(color)
        .into_any_element()
}

/// 键位按 mac 习惯（⌘）还是 Ctrl+Shift——平台的事（浏览器里看浏览器所在的系统），
/// [`crate::actions::init`] 注册键位时设一次，之后不变
static MAC_KEYS: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_mac_keys(mac: bool) {
    MAC_KEYS.store(mac, Ordering::Relaxed);
}

pub(crate) fn mac_keys() -> bool {
    MAC_KEYS.load(Ordering::Relaxed)
}

/// 键位提示（菜单 / tooltip 里显示的主键位），字面沿用 React 版 `chord()` 的写法
/// （falcon-core 的快捷键表）。不认识的动作 id 给空串
pub fn chord(action: &str) -> String {
    Command::from_id(action).map(|c| shortcuts::chord(c, MAC_KEYS.load(Ordering::Relaxed))).unwrap_or_default()
}

/// 变更面板提交框的提交键。不注册成全局绑定——只在提交框里生效，全局绑定会在终端里抢走 ⌘↵
pub fn commit_chord() -> &'static str {
    if MAC_KEYS.load(Ordering::Relaxed) { "⌘↵" } else { "Ctrl+↵" }
}
