//! 设置页的排版件（web SettingsModal 里的 SettingSection / SettingRow，common/Field 的
//! Field / Segmented）。颜色一律取语义 token。

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Div, FontWeight, SharedString, Window, div};

use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

/// 一节：标题 + 可选说明 + 内容（web：mb-8，h2 text-base semibold，说明 text-xs）
pub fn section(title: impl Into<SharedString>, description: Option<String>, children: Vec<AnyElement>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let head = div()
        .text_base()
        .font_weight(FontWeight::SEMIBOLD)
        .child(title.into());
    let body = div().flex().flex_col().children(children);
    let mut out = div().flex().flex_col().mb_8().child(head);
    out = match description {
        Some(d) => out
            .child(div().mt_1().mb_3().text_xs().line_height(zpx(18.)).text_color(ui.muted_foreground).child(d))
            .child(body),
        None => out.child(body.mt_3()),
    };
    out
}

/// 一行设置：左边标签 + 提示，右边控件；行与行之间一道细线（最后一行不画）
pub fn row(label: impl Into<SharedString>, hint: Option<String>, control: impl IntoElement, sep: bool, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let mut left = div().min_w_0().flex_1().child(div().text_sm().child(label.into()));
    if let Some(h) = hint {
        left = left.child(div().mt(zpx(2.)).text_xs().line_height(zpx(18.)).text_color(ui.muted_foreground).child(h));
    }
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .py_4()
        .when(sep, |d| d.border_b_1().border_color(ui.border))
        .child(left)
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// 把若干行排成一列，行间自动加分隔线。`more` = 后面还跟着别的内容（脚注、预览、表单），
/// 最后一行也要画线——web 的 `last:border-b-0` 只对整节的最后一个子元素生效
pub fn rows(items: Vec<(String, Option<String>, AnyElement)>, more: bool, cx: &App) -> Vec<AnyElement> {
    let n = items.len();
    items
        .into_iter()
        .enumerate()
        .map(|(i, (label, hint, control))| row(label, hint, control, more || i + 1 < n, cx))
        .collect()
}

/// 表单字段：小号标签 + 控件 + 提示（web 的 Field）
pub fn field(label: impl Into<SharedString>, control: impl IntoElement, cx: &App) -> Div {
    let ui = Ui::global(cx);
    div()
        .flex()
        .flex_col()
        .gap(zpx(6.))
        .child(div().text_xs().text_color(ui.muted_foreground).child(label.into()))
        .child(control)
}

pub struct SegOption<T> {
    pub value: T,
    pub label: String,
    pub icon: Option<IconName>,
}

/// 分段选择（web 的 Segmented / ThemeChoice）：凹槽里浮起一块——槽用 sunken，选中项用岛的
/// 材质，同一套曲率只是小一号。`dense` 是明暗模式那种带图标的小号。
pub fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    value: T,
    options: Vec<SegOption<T>>,
    dense: bool,
    on_change: impl Fn(T, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    let ui = Ui::global(cx).clone();
    let on_change = Rc::new(on_change);
    let mut group = crate::ui::sunken(cx).flex().gap(zpx(2.)).p(zpx(2.));
    for (i, opt) in options.into_iter().enumerate() {
        let on = opt.value == value;
        let f = on_change.clone();
        let v = opt.value;
        let mut b = div()
            .id((id, i))
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_1()
            .whitespace_nowrap()
            .rounded(radius::SM)
            .cursor_pointer()
            .map(|d| if dense { d.h(zpx(24.)).px_2().text_xs() } else { d.h(zpx(28.)).px_3().text_size(zpx(13.)) })
            .on_click(move |_, window, cx| f(v, window, cx));
        b = if on {
            b.bg(ui.background).text_color(ui.foreground).font_weight(FontWeight::MEDIUM)
        } else {
            let fg = ui.foreground;
            b.text_color(ui.muted_foreground).hover(move |s| s.text_color(fg))
        };
        if let Some(name) = opt.icon {
            b = b.child(icon(name).size(zpx(14.)));
        }
        group = group.child(b.child(opt.label));
    }
    group
}
