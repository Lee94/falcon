//! 右侧活动栏（旧 React 版的 `components/RightBar.tsx`）与右面板宿主。
//!
//! 活动栏不成岛：图标直接落在窗口底上。点同一格关面板，点另一格切过去。
//! 面板视图在右侧栏开着时切走不卸载（切回来要立刻有东西），关掉右侧栏才卸——沿用 React 版的规则。
//!
//! 内置的两格之后隔一道短线是插件（ADR 0025）：服务端列表里启用着、清单没坏的那些，
//! 一个插件一格，面板都是 [`PluginPanel`]。

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use std::collections::HashMap;

use gpui_kit::{AnyElement, AnyView, App, Context, Entity, IntoElement, Render, SharedString, Window, div};
use rust_i18n::t;

use crate::panels::{changes::ChangesPanel, files::FilesPanel, plugin::{PluginPanel, plugin_icon}};
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{RightPanelId, Workspace};
use crate::zoom::zpx;

/// 转发面板已挪进设置的「中转」页（ADR 0016），这里不再有那一格
const ITEMS: [(RightPanelId, IconName, &str, &str); 2] = [
    (RightPanelId::Files, IconName::Folder, "files.panel", "toggleFilesPanel"),
    (RightPanelId::Changes, IconName::FileDiff, "changes.panel", "toggleChangesPanel"),
];

/// 活动栏上的一格：30px 的方块，选中用 tint（淡入），hover 用灰
fn bar_button(
    id: String,
    glyph: IconName,
    tip: SharedString,
    on: bool,
    ui: &Ui,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    let mut btn = div()
        .id(SharedString::from(id.clone()))
        .size(zpx(30.))
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::theme::radius::SM)
        .cursor_pointer()
        .text_color(ui.muted_foreground)
        .active(|s| s.opacity(0.72))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    // 背景块先画（垫在图标下面），hover 的底色淡入淡出、文字色瞬时
    if !on {
        btn = btn.child(crate::ui::hover_fade(format!("hover-{id}"), ui.background.opacity(0.65), crate::theme::radius::SM));
    }
    let btn = btn.child(icon(glyph).size(zpx(15.))).on_click(on_click);
    if on {
        let tint = ui.tint;
        crate::ui::selected_fade(
            format!("sel-{id}"),
            btn.text_color(ui.tint_foreground)
                .border_1()
                .border_color(ui.tint_strong.opacity(0.28)),
            tint,
        )
        .into_any_element()
    } else {
        let fg = ui.foreground;
        btn.hover(move |s| s.text_color(fg)).into_any_element()
    }
}

fn toggle(ws: &Entity<Workspace>, panel: RightPanelId, cx: &mut App) {
    ws.update(cx, |w, cx| w.toggle_right_panel(panel, cx));
}

pub struct RightBar {
    ws: Entity<Workspace>,
}

impl RightBar {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
        Self { ws }
    }
}

impl Render for RightBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let ws = self.ws.read(cx);
        let (open, current) = (ws.state.right_open, ws.state.right_panel.clone());
        let plugins = ws.panel_plugins();
        let mut bar = div()
            .flex_none()
            .w(zpx(40.))
            .flex()
            .flex_col()
            .items_center()
            .gap(zpx(4.))
            .px(zpx(5.))
            .py(zpx(8.));
        for (panel, icon_name, label, chord) in ITEMS {
            let on = open && current == panel;
            let ws = self.ws.clone();
            let tip: SharedString = format!("{} · {}", t!(label), crate::ui::chord(chord)).into();
            let id = format!("rb-{}", panel.as_wire());
            bar = bar.child(bar_button(id, icon_name, tip, on, &ui, move |_, _, cx| toggle(&ws, panel.clone(), cx)));
        }
        if !plugins.is_empty() {
            bar = bar.child(div().w(zpx(14.)).h(zpx(1.)).my(zpx(4.)).bg(ui.border));
        }
        for p in plugins {
            let panel = RightPanelId::Plugin(p.id.clone());
            let on = open && current == panel;
            let ws = self.ws.clone();
            let tip: SharedString = p.name.clone().into();
            let id = format!("rb-{}", panel.as_wire());
            bar = bar.child(bar_button(id, plugin_icon(&p.icon), tip, on, &ui, move |_, _, cx| toggle(&ws, panel.clone(), cx)));
        }
        bar
    }
}

/// 右面板那座岛：按工作区的 right_panel 显示对应面板，建过的面板在右侧栏关掉之前一直留着
pub struct PanelHost {
    ws: Entity<Workspace>,
    files: Option<Entity<FilesPanel>>,
    changes: Option<Entity<ChangesPanel>>,
    /// 插件面板按插件 id 各一扇
    plugins: HashMap<String, Entity<PluginPanel>>,
}

impl PanelHost {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |this, ws, cx| {
            // 关掉右侧栏才卸
            if !ws.read(cx).state.right_open {
                this.files = None;
                this.changes = None;
                this.plugins.clear();
            }
            cx.notify();
        })
        .detach();
        Self {
            ws,
            files: None,
            changes: None,
            plugins: HashMap::new(),
        }
    }

    fn active_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        let ws = self.ws.clone();
        match self.ws.read(cx).state.right_panel.clone() {
            RightPanelId::Files => self
                .files
                .get_or_insert_with(|| cx.new(|cx| FilesPanel::new(ws, window, cx)))
                .clone()
                .into(),
            RightPanelId::Changes => self
                .changes
                .get_or_insert_with(|| cx.new(|cx| ChangesPanel::new(ws, window, cx)))
                .clone()
                .into(),
            RightPanelId::Plugin(id) => self
                .plugins
                .entry(id.clone())
                .or_insert_with(|| cx.new(|cx| PluginPanel::new(ws, id, window, cx)))
                .clone()
                .into(),
        }
    }
}

impl Render for PanelHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = self.active_view(window, cx);
        crate::ui::island(cx).size_full().flex().flex_col().child(view)
    }
}
