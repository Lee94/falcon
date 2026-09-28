//! 飞书项目面板与设置「中转」页共用的小件：提示文字、分段控件、分组标题、药丸、小号图标按钮。
//! 形状照 web 的 `Hint` / `common/Field.tsx` 的 `Segmented` / `GroupTitle` / `Pill`，颜色一律取语义 token。

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt, App, ClickEvent, Div, ElementId, FontWeight, IntoElement, SharedString, Stateful, Transformation, Window,
    div, percentage, relative,
};

use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

/// 空态 / 加载中 / 失败的提示段落（web 的 `Hint`：px-3 py-4 text-xs leading-relaxed）
pub(crate) fn hint(text: impl Into<SharedString>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    div()
        .px_3()
        .py_4()
        .text_size(zpx(12.))
        .line_height(relative(1.6))
        .text_color(ui.muted_foreground)
        .child(text.into())
}

/// 提示 + 一行等宽的原始错误（"加载失败" 下面那行服务端原话）
pub(crate) fn hint_detail(text: impl Into<SharedString>, detail: impl Into<SharedString>, cx: &App) -> Div {
    hint(text, cx).child(div().mt_1().text_size(zpx(11.)).child(detail.into()))
}

/// 分组标题（web 的 `GroupTitle`：11px、muted、下边一道细线）
pub(crate) fn group_title(cx: &App) -> Div {
    let ui = Ui::global(cx);
    div()
        .flex()
        .items_baseline()
        .gap_1()
        .px_3()
        .pt_2()
        .pb_1()
        .text_size(zpx(11.))
        .text_color(ui.muted_foreground)
        .border_b_1()
        .border_color(ui.border)
}

/// 分段控件（web 的 `Segmented dense`）：sunken 槽里一排等宽按钮，选中的那格浮起来（面板底色）。
/// 窄栏里与 h-7 的输入框对齐，所以按钮是 24px 高。
pub(crate) fn segmented<T: Copy + PartialEq + 'static>(
    id: &str,
    value: T,
    options: Vec<(T, String)>,
    on_change: impl Fn(T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let ui = Ui::global(cx).clone();
    let on_change = Rc::new(on_change);
    let mut bar = div().flex().gap(zpx(2.)).p(zpx(2.)).bg(ui.app).rounded(radius::MD);
    for (i, (v, label)) in options.into_iter().enumerate() {
        let on = v == value;
        let cb = on_change.clone();
        let mut b = div()
            .id(ElementId::Name(format!("{id}-{i}").into()))
            .flex_1()
            .h(zpx(24.))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(radius::SM)
            .text_size(zpx(12.))
            .whitespace_nowrap()
            .cursor_pointer()
            .child(label)
            .on_click(move |_, window, cx| cb(v, window, cx));
        b = if on {
            b.bg(ui.background).font_weight(FontWeight::MEDIUM).text_color(ui.foreground)
        } else {
            b.text_color(ui.muted_foreground).hover(|s| s.text_color(ui.foreground))
        };
        bar = bar.child(b);
    }
    bar
}

/// 圆角小药丸（web 的 `Pill`）：状态 / 优先级用弱着色底，编号用等宽 muted
pub(crate) fn pill(text: impl Into<SharedString>, muted: bool, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let d = div()
        .flex()
        .flex_none()
        .items_center()
        .h(zpx(20.))
        .px_2()
        .rounded_full()
        .border_1()
        .border_color(ui.border)
        .text_size(zpx(11.))
        .whitespace_nowrap()
        .child(text.into());
    if muted { d.text_color(ui.muted_foreground) } else { d.bg(ui.accent.opacity(0.6)) }
}

/// 行内的小号图标按钮（web 的 `size-5` / `size-6` ghost 按钮）。`occlude`：它常叠在一整行可点的
/// 行上面（外链、重命名），点它不能再把下面那行也点了。
pub(crate) fn tiny_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
    size: f32,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let glyph = icon(name).size(zpx(if size <= 20. { 12. } else { 14. }));
    icon_button(id, glyph, tooltip, size, false, cx, on_click)
}

/// [`tiny_button`] 的一般形式：图标自己给（可以带颜色 / 转圈动画），可禁用
pub(crate) fn icon_button(
    id: impl Into<ElementId>,
    glyph: impl IntoElement,
    tooltip: impl Into<SharedString>,
    size: f32,
    disabled: bool,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let ui = Ui::global(cx).clone();
    let tip: SharedString = tooltip.into();
    let el = div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(zpx(size))
        .rounded(zpx(6.))
        .text_color(ui.muted_foreground)
        .occlude()
        .child(glyph)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    if disabled {
        el.opacity(0.5)
    } else {
        el.cursor_pointer().hover(|s| s.bg(ui.accent).text_color(ui.foreground)).on_click(on_click)
    }
}

/// 转圈的图标（刷新中）：web 的 `animate-spin`
pub(crate) fn spinning(name: IconName, size: f32) -> impl IntoElement {
    icon(name).size(zpx(size)).with_animation(
        "spin",
        Animation::new(Duration::from_millis(1000)).repeat(),
        |i, delta| i.transform(Transformation::rotate(percentage(delta))),
    )
}
