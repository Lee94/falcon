//! 设置「插件」页（ADR 0025）：装了哪些插件、开关、清单坏了的原因，以及命令插件该放在哪。
//!
//! 插件装在 falcon 后端那台机器上（不是这台客户端），所以目录是服务端给的路径；这里只给"复制路径"，
//! 不给"在访达里打开"——客户端与后端多半不是同一台。改了插件目录之后点「重新扫描」，右侧栏跟着变。

use falcon_proto::{PluginInfo, PluginKind};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, ClipboardItem, Entity, FontWeight, IntoElement, div};
use rust_i18n::t;

use super::widgets;
use crate::panels::meegle::widgets::pill;
use crate::panels::plugin::plugin_icon;
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

fn plugin_row(ws: &Entity<Workspace>, p: &PluginInfo, last: bool, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let kind = match p.kind {
        PluginKind::Builtin => t!("plugin.kindBuiltin"),
        _ => t!("plugin.kindCommand"),
    }
    .to_string();
    let (ws, id, on) = (ws.clone(), p.id.clone(), p.enabled);
    let toggle = Switch::new(gpui_kit::ElementId::Name(format!("plugin-on-{}", p.id).into()))
        .checked(on && p.error.is_none())
        .disabled(p.error.is_some())
        .on_click(move |_, _, cx| ws.update(cx, |w, cx| w.set_plugin_enabled(&id, !on, cx)));
    let mut left = div()
        .min_w_0()
        .flex_1()
        .flex()
        .flex_col()
        .gap(zpx(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon(plugin_icon(&p.icon)).size(zpx(14.)).text_color(ui.muted_foreground))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(p.name.clone()))
                .child(pill(kind, true, cx)),
        );
    if let Some(d) = &p.description {
        left = left.child(div().text_xs().line_height(zpx(18.)).text_color(ui.muted_foreground).child(d.clone()));
    }
    if let Some(dir) = &p.dir {
        left = left.child(div().text_xs().text_color(ui.muted_foreground).font_family(crate::fonts::BERKELEY).child(dir.clone()));
    }
    if let Some(e) = &p.error {
        left = left.child(div().text_xs().line_height(zpx(18.)).text_color(ui.destructive).child(e.clone()));
    }
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_6()
        .py_3()
        .when(!last, |d| d.border_b_1().border_color(ui.border))
        .child(left)
        .child(div().flex_none().child(toggle))
        .into_any_element()
}

pub fn render(ws: &Entity<Workspace>, cx: &App) -> impl IntoElement {
    let ui = Ui::global(cx).clone();
    let list = ws.read(cx).plugins.clone();
    let dir = list.as_ref().map(|l| l.dir.clone());
    let (copy_ws, rescan_ws, copy_dir) = (ws.clone(), ws.clone(), dir.clone());
    let actions = div()
        .flex()
        .gap_2()
        .child(
            Button::new("plugins-copy-dir")
                .outline()
                .small()
                .icon(IconName::Copy)
                .label(t!("plugin.copyDir").to_string())
                .disabled(copy_dir.is_none())
                .on_click(move |_, _, cx| {
                    if let Some(d) = &copy_dir {
                        cx.write_to_clipboard(ClipboardItem::new_string(d.clone()));
                        copy_ws.update(cx, |w, cx| w.toast(ToastKind::Success, t!("plugin.copied").to_string(), None, cx));
                    }
                }),
        )
        .child(
            Button::new("plugins-rescan")
                .outline()
                .small()
                .icon(IconName::RefreshCw)
                .label(t!("plugin.rescan").to_string())
                .on_click(move |_, _, cx| rescan_ws.update(cx, |w, cx| w.refresh_plugins(cx))),
        );
    let dir_row = widgets::row(
        t!("plugin.dir").to_string(),
        Some(dir.unwrap_or_else(|| "—".into())),
        actions,
        true,
        cx,
    );
    let mut rows = vec![dir_row];
    match &list {
        None => rows.push(div().py_4().text_xs().text_color(ui.muted_foreground).child(t!("plugin.loading").to_string()).into_any_element()),
        Some(l) => {
            let n = l.plugins.len();
            rows.extend(l.plugins.iter().enumerate().map(|(i, p)| plugin_row(ws, p, i + 1 == n, cx)));
            if !l.plugins.iter().any(|p| p.kind != PluginKind::Builtin) {
                rows.push(
                    div()
                        .pt_3()
                        .text_xs()
                        .line_height(zpx(18.))
                        .text_color(ui.muted_foreground)
                        .child(t!("plugin.noCustom").to_string())
                        .into_any_element(),
                );
            }
        }
    }
    widgets::section(t!("plugin.settingsTitle").to_string(), Some(t!("plugin.settingsHint").to_string()), rows, cx)
}
