//! 标题栏的窗口按钮（最小化 / 最大化 / 关闭）与拖动区。
//!
//! 窗口都开成 `appears_transparent` 的自绘标题条：macOS 上红绿灯仍由 AppKit 画、透明标题栏
//! 区域 AppKit 自己能拖；Windows 上 GPUI 则把整条系统标题栏去掉（`hide_title_bar`），三颗按钮
//! 与拖动区都得应用自己画。按钮不接点击事件：给元素标 `window_control_area`，GPUI 在
//! WM_NCHITTEST 里把这块报成 HTMINBUTTON / HTMAXBUTTON / HTCLOSE / HTCAPTION，最小化、
//! 最大化、关闭、拖动、双击标题最大化、Snap 布局浮层全是系统行为，跟原生窗口一致。
//!
//! 尺寸写 `px` 不写 `zpx`：这是窗口外框，不跟界面缩放走（zoom.rs 顶部的清单）。

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Pixels, Window, WindowControlArea, div, px};

use crate::theme::Ui;

/// Windows 标题按钮的标准宽度
const BUTTON_W: f32 = 46.;

/// 把元素标成标题栏拖动区。只在 Windows 上标：macOS 的透明标题栏本来就由 AppKit 处理拖动，
/// 别改它的行为；Linux 不是发布目标（那边 window_control_area 不生效，要另接点击，见
/// gpui-component 的 title_bar.rs）
pub fn drag_area<E: InteractiveElement>(el: E) -> E {
    if !cfg!(windows) {
        el
    } else {
        el.window_control_area(WindowControlArea::Drag)
    }
}

/// 贴在标题条最右边的三颗按钮，高度跟标题条一样；Windows 以外返回 `None`（macOS 的红绿灯是系统画的）
pub fn controls(height: Pixels, window: &Window, cx: &App) -> Option<AnyElement> {
    if !cfg!(windows) {
        return None;
    }
    let ui = Ui::global(cx).clone();
    let button = |id: &'static str, icon: IconName, area: WindowControlArea, close: bool| {
        let (hover_bg, hover_fg) = if close {
            (ui.destructive, ui.destructive_foreground)
        } else {
            (ui.muted, ui.foreground)
        };
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(BUTTON_W))
            .h(height)
            .text_color(ui.muted_foreground)
            .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
            .active(move |s| s.bg(hover_bg.opacity(0.8)).text_color(hover_fg))
            .window_control_area(area)
            .child(crate::ui::icon(icon).size(px(14.)))
    };
    let max_icon = if window.is_maximized() { IconName::WindowRestore } else { IconName::WindowMaximize };
    Some(
        div()
            .flex()
            .flex_none()
            .h(height)
            .child(button("win-min", IconName::WindowMinimize, WindowControlArea::Min, false))
            .child(button("win-max", max_icon, WindowControlArea::Max, false))
            .child(button("win-close", IconName::WindowClose, WindowControlArea::Close, true))
            .into_any_element(),
    )
}
