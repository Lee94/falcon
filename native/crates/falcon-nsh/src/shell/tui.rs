//! TUI 字符网格宿主（`Idiom::Terminal` / `nsh-platform-tui`）键盘优先工作台壳层。
//!
//! 专为 SSH 无图形环境设计：紧凑三栏字符网格排布，支持键盘快速切换项目/会话、打开命令面板，
//! 并与 `FalconTerminal` 的 `Enter` / `Ctrl+\` 全按键直通模式无缝联动。

use falcon_core::pane_key::{PaneItem, parse_pane_key};
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::store::{ActiveDialog, RightPanelKind, use_workspace};
use crate::terminal::{bind_safe_key, falcon_terminal};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::views::{
    agent_pane_view, diff_pane_view, file_pane_view, files_panel, git_panel, plugin_panel,
};
use crate::zoom::zpx;

#[component]
pub fn TuiShell() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();

    // TUI 导航态快捷键（直通开启时让位给内嵌终端会话）
    {
        let st_enter = store.clone();
        bind_safe_key("enter", false, move || {
            if !st_enter.tui_passthrough.get()
                && st_enter.active_dialog.get() == ActiveDialog::None
            {
                st_enter.set_tui_passthrough(true);
            }
        });
    }

    view()
        .size_full()
        .flex_col()
        .bg(move || rgb_to_hsla(theme_ctrl.resolved().ui.app))
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let sessions = store.sessions.get();
            let focused = store.focused_pane_key.get();
            let passthrough = store.tui_passthrough.get();
            let rp_kind = store.right_panel_kind.get();

            let st_pal = store.clone();
            let topbar = view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(8.0))
                .py(zpx(2.0))
                .bg(rgb_to_hsla(ui.card))
                .border_b_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    text(format!(
                        "{} · {}",
                        t!("native.nsh.appTitle"),
                        t!("native.nsh.RendererBadge")
                    ))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    view()
                        .px(zpx(6.0))
                        .py(zpx(1.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.secondary))
                        .on_press(move || st_pal.active_dialog.set(ActiveDialog::Palette))
                        .child(
                            text(t!("palette.title"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.secondary_foreground)),
                        ),
                );

            let left_list = view()
                .w(zpx(180.0))
                .h_full()
                .flex_col()
                .p(zpx(4.0))
                .gap(zpx(2.0))
                .bg(rgb_to_hsla(ui.sidebar))
                .children(sessions.into_iter().map(|s| {
                    let attached = store.is_session_attached(&s.id);
                    let st_f = store.clone();
                    let sid = s.id.clone();
                    view()
                        .px(zpx(4.0))
                        .py(zpx(2.0))
                        .cursor_pointer()
                        .bg(if focused == falcon_core::pane_key::term_key(&s.id) {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.sidebar)
                        })
                        .on_press(move || st_f.focus_session(&sid))
                        .child(
                            text(if attached {
                                s.display_title()
                            } else {
                                format!("{} ({})", s.display_title(), t!("native.nsh.detachedBadge"))
                            })
                            .text_xs()
                            .number_of_lines(1)
                            .text_color(rgb_to_hsla(ui.sidebar_foreground)),
                        )
                }));

            let center = match parse_pane_key(&focused) {
                Some(PaneItem::Terminal { id, .. }) => {
                    if store.agent_views.get().contains_key(&id) && !passthrough {
                        agent_pane_view()
                            .session_id(id)
                            .focused(true)
                            .into_element()
                    } else {
                        falcon_terminal()
                            .session_id(id)
                            .focused(true)
                            .tui_passthrough(passthrough)
                            .into_element()
                    }
                }
                Some(PaneItem::File {
                    project_id, path, ..
                }) => file_pane_view()
                    .project_id(project_id)
                    .path(path)
                    .into_element(),
                Some(PaneItem::Diff { .. }) => diff_pane_view().into_element(),
                None => view().into_element(),
            };

            let right_pane = match rp_kind {
                RightPanelKind::Files => files_panel().into_element(),
                RightPanelKind::Git => git_panel().into_element(),
                RightPanelKind::Plugin(pid) => plugin_panel().plugin_id(pid).into_element(),
            };

            view()
                .size_full()
                .flex_col()
                .child(topbar)
                .child(
                    view()
                        .flex_1()
                        .flex_row()
                        .overflow_hidden()
                        .child(left_list)
                        .child(
                            view()
                                .flex_1()
                                .h_full()
                                .bg(rgb_to_hsla(ui.background))
                                .child(center),
                        )
                        .child(
                            view()
                                .w(zpx(240.0))
                                .h_full()
                                .bg(rgb_to_hsla(ui.background))
                                .child(right_pane),
                        ),
                )
                .into_element()
        }))
}
