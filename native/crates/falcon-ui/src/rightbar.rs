//! 右侧活动栏（旧 React 版的 `components/RightBar.tsx`）与右面板宿主。
//!
//! 活动栏不成岛：图标直接落在窗口底上。点同一格关面板，点另一格切过去。
//! 面板视图在右侧栏开着时切走不卸载（飞书项目的 CLI 往返 2–6s，切回来要立刻有东西），
//! 关掉右侧栏才卸——沿用 React 版的规则。

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{AnyView, Context, Entity, IntoElement, Render, SharedString, Window, div};
use rust_i18n::t;

use crate::panels::{
    changes::ChangesPanel, files::FilesPanel, git::GitPanel, meegle::MeeglePanel,
};
use crate::theme::Ui;
use crate::ui::icon;
use crate::workspace::{RightPanelId, Workspace};
use crate::zoom::zpx;

/// 转发面板已挪进设置的「中转」页（ADR 0016），这里不再有那一格
const ITEMS: [(RightPanelId, IconName, &str, &str); 4] = [
    (RightPanelId::Files, IconName::Folder, "files.panel", "toggleFilesPanel"),
    (RightPanelId::Changes, IconName::FileDiff, "changes.panel", "toggleChangesPanel"),
    (RightPanelId::Git, IconName::GitBranch, "git.panel", "toggleGitPanel"),
    (RightPanelId::Meegle, IconName::ListTodo, "meegle.panel", "toggleMeeglePanel"),
];

/// 面板在持久化与元素 id 里的名字（沿用 React 版 RightPanelId 的字面量）
pub fn panel_id(p: RightPanelId) -> &'static str {
    match p {
        RightPanelId::Files => "files",
        RightPanelId::Changes => "changes",
        RightPanelId::Git => "git",
        RightPanelId::Meegle => "meegle",
    }
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
        let (open, current) = (ws.state.right_open, ws.state.right_panel);
        let mut bar = div()
            .flex_none()
            .w(zpx(40.))
            .flex()
            .flex_col()
            .items_center()
            .gap(zpx(2.))
            .px(zpx(6.))
            .py(zpx(6.));
        for (panel, icon_name, label, chord) in ITEMS {
            let on = open && current == panel;
            let ws = self.ws.clone();
            let tip: SharedString = format!("{} · {}", t!(label), crate::ui::chord(chord)).into();
            let mut btn = div()
                .id(SharedString::from(format!("rb-{}", panel_id(panel))))
                .size(zpx(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(zpx(8.))
                .cursor_pointer()
                .text_color(ui.muted_foreground)
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(move |_, _, cx| ws.update(cx, |w, cx| w.toggle_right_panel(panel, cx)))
                .child(icon(icon_name).size(zpx(14.)));
            btn = if on {
                btn.bg(ui.tint).text_color(ui.tint_foreground)
            } else {
                btn.hover(|s| s.bg(ui.background.opacity(0.6)).text_color(ui.foreground))
            };
            bar = bar.child(btn);
        }
        bar
    }
}

/// 右面板那座岛：按工作区的 right_panel 显示对应面板，建过的面板在右侧栏关掉之前一直留着
pub struct PanelHost {
    ws: Entity<Workspace>,
    files: Option<Entity<FilesPanel>>,
    changes: Option<Entity<ChangesPanel>>,
    git: Option<Entity<GitPanel>>,
    meegle: Option<Entity<MeeglePanel>>,
}

impl PanelHost {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&ws, |this, ws, cx| {
            // 关掉右侧栏才卸
            if !ws.read(cx).state.right_open {
                this.files = None;
                this.changes = None;
                this.git = None;
                this.meegle = None;
            }
            cx.notify();
        })
        .detach();
        Self {
            ws,
            files: None,
            changes: None,
            git: None,
            meegle: None,
        }
    }

    fn active_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyView {
        let ws = self.ws.clone();
        match self.ws.read(cx).state.right_panel {
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
            RightPanelId::Git => self
                .git
                .get_or_insert_with(|| cx.new(|cx| GitPanel::new(ws, window, cx)))
                .clone()
                .into(),
            RightPanelId::Meegle => self
                .meegle
                .get_or_insert_with(|| cx.new(|cx| MeeglePanel::new(ws, window, cx)))
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
