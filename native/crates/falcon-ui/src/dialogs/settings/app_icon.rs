//! 外观 › 应用图标（ADR 0018）：旧 React 版的 AppIconPicker。内置几套 + 一格自定义；选择存在服务端，
//! 这里改了 Dock 立刻换，别的设备下次加载换。
//!
//! 预览用的是嵌进二进制的 macOS 版式 PNG（与 Dock 里看到的一模一样），React 版用的是圆角方块。

use std::sync::Arc;

use falcon_core::app_icon::{APP_ICON_IDS, CUSTOM, label_key};
use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::Button;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FontWeight, Image, IntoElement, Render, SharedString, Window, div,
    img,
};
use rust_i18n::t;

use super::widgets::section;
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::Workspace;
use crate::zoom::zpx;

pub struct AppIconPicker {
    ws: Entity<Workspace>,
}

impl AppIconPicker {
    pub fn new(ws: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
        Self { ws }
    }
}

/// 一格：图标 + 名字；选中用 tint，hover 用灰（React 版的 Tile）
fn tile(
    id: impl Into<SharedString>,
    picture: AnyElement,
    label: String,
    on: bool,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let ui = Ui::global(cx).clone();
    let fg = ui.foreground;
    let hover_bg = ui.accent.opacity(0.5);
    div()
        .id(id.into())
        .w(zpx(96.))
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .px_2()
        .pt_3()
        .pb_2()
        .rounded(radius::SM)
        .text_xs()
        .map(|d| {
            if on {
                d.bg(ui.tint).text_color(ui.tint_foreground).font_weight(FontWeight::MEDIUM)
            } else {
                d.text_color(ui.muted_foreground).hover(move |s| s.bg(hover_bg).text_color(fg))
            }
        })
        .when(enabled, |d| d.cursor_pointer().on_click(on_click))
        .child(picture)
        .child(div().max_w_full().truncate().child(label))
}

/// macOS 版式的图四周本来就留了 10% 的透明边，比 React 版的圆角方块（56）画大一号才一样大
fn picture(image: Arc<Image>) -> AnyElement {
    img(image).size(zpx(64.)).into_any_element()
}

impl Render for AppIconPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let w = self.ws.read(cx);
        let state = w.app_icon.clone();
        let enabled = state.is_some() && !w.app_icon_busy;
        let selected = state.as_ref().map(|s| s.selected.clone()).unwrap_or_default();
        let has_custom = state.as_ref().is_some_and(|s| s.custom.is_some());
        let custom_image = w.custom_icon.as_ref().map(|c| c.image.clone());

        let mut grid = div().flex().flex_wrap().gap_2().pt_1();
        for id in APP_ICON_IDS {
            let Some(image) = crate::app_icon::builtin_image(id) else { continue };
            let ws = self.ws.downgrade();
            grid = grid.child(tile(
                format!("app-icon-{id}"),
                picture(image),
                t!(label_key(id)).to_string(),
                selected == id,
                enabled,
                move |_, _, cx| {
                    ws.update(cx, |w, cx| w.set_app_icon(id, cx)).ok();
                },
                cx,
            ));
        }
        let ws = self.ws.downgrade();
        grid = grid.child(if has_custom {
            // 图还在取：先占位，取回来 ws 通知重画
            let pic = match custom_image {
                Some(image) => picture(image),
                None => div().size(zpx(64.)).into_any_element(),
            };
            tile(
                "app-icon-custom",
                pic,
                t!("appIcon.custom").to_string(),
                selected == CUSTOM,
                enabled,
                move |_, _, cx| {
                    ws.update(cx, |w, cx| w.set_app_icon(CUSTOM, cx)).ok();
                },
                cx,
            )
            .into_any_element()
        } else {
            tile(
                "app-icon-custom",
                crate::ui::sunken(cx)
                    .size(zpx(56.))
                    .m(zpx(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(IconName::ImagePlus).size(zpx(20.)))
                    .into_any_element(),
                t!("appIcon.custom").to_string(),
                false,
                enabled,
                move |_, _, cx| {
                    ws.update(cx, |w, cx| w.upload_app_icon(cx)).ok();
                },
                cx,
            )
            .into_any_element()
        });

        let mut buttons = div().flex().flex_none().gap_2();
        if has_custom {
            let ws = self.ws.downgrade();
            buttons = buttons.child(
                Button::new("app-icon-remove")
                    .outline()
                    .xsmall()
                    .disabled(!enabled)
                    .label(t!("appIcon.remove").to_string())
                    .on_click(move |_, _, cx| {
                        ws.update(cx, |w, cx| w.remove_custom_app_icon(cx)).ok();
                    }),
            );
        }
        let ws = self.ws.downgrade();
        buttons = buttons.child(
            Button::new("app-icon-upload")
                .outline()
                .xsmall()
                .disabled(!enabled)
                .label(t!(if has_custom { "appIcon.replace" } else { "appIcon.upload" }).to_string())
                .on_click(move |_, _, cx| {
                    ws.update(cx, |w, cx| w.upload_app_icon(cx)).ok();
                }),
        );
        let info = falcon_platform::get(cx).info();
        let dock_note = info.mac && !info.browser;
        let footer = div()
            .mt_3()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_xs()
                    .line_height(zpx(18.))
                    .text_color(ui.muted_foreground)
                    .child(t!("appIcon.customHint").to_string())
                    // Dock / 访达那句只对 macOS 原生客户端成立（ADR 0018 决定五），浏览器版与
                    // Windows 不画
                    .when(dock_note, |d| d.child(t!("native.settings.appIconDockNote").to_string())),
            )
            .child(buttons);

        section(
            t!("appIcon.title").to_string(),
            Some(t!("appIcon.hint").to_string()),
            vec![grid.into_any_element(), footer.into_any_element()],
            cx,
        )
    }
}
