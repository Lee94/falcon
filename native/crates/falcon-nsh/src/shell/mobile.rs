//! Android 移动端（`Idiom::Phone`）自适应壳层与软键盘上方终端扩展按键条。
//!
//! 复用同一个 `WorkspaceStore`、`FalconTerminal` 与双轨插件面板，针对单手触控重排为四页签架构，
//! 并在软键盘弹出时通过 `keyboard_height()` 自动顶起 `Esc / Tab / Ctrl / Alt / 方向键` 辅助栏。

use falcon_core::pane_key::{PaneItem, parse_pane_key};
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{
    ActiveDialog, MobileTab, RightPanelKind, SettingsTab, WorkspaceStore, use_workspace,
};
use crate::terminal::falcon_terminal;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::views::{
    agent_pane_view, diff_pane_view, file_pane_view, files_panel, git_panel, plugin_panel,
};
use crate::zoom::{pt_px, zpx};

#[component]
pub fn MobileShell() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();

    view()
        .size_full()
        .flex_col()
        .bg(move || rgb_to_hsla(theme_ctrl.resolved().ui.app))
        .pt(move || safe_area().top)
        .pb(move || {
            let kb = keyboard_height();
            if kb.0 > 0.0 {
                kb
            } else {
                safe_area().bottom
            }
        })
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let tab = store.mobile_tab.get();

            let header = {
                let st_prof = store.clone();
                let st_pal = store.clone();
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(zpx(12.0))
                    .py(zpx(8.0))
                    .bg(rgb_to_hsla(ui.background))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.0))
                            .cursor_pointer()
                            .on_press(move || st_prof.active_dialog.set(ActiveDialog::Profiles))
                            .child(
                                text(t!("native.nsh.appTitle"))
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            ),
                    )
                    .child(
                        view()
                            .px(zpx(8.0))
                            .py(zpx(4.0))
                            .rounded(zpx(6.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.secondary))
                            .on_press(move || st_pal.active_dialog.set(ActiveDialog::Palette))
                            .child(
                                text(t!("palette.title"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.secondary_foreground)),
                            ),
                    )
            };

            let body = match tab {
                MobileTab::Projects => render_mobile_projects(&store, &resolved).into_element(),
                MobileTab::Workspace => render_mobile_workspace(&store, &resolved).into_element(),
                MobileTab::Panels => render_mobile_panels(&store, &resolved).into_element(),
                MobileTab::Settings => {
                    let st_open = store.clone();
                    view()
                        .size_full()
                        .items_center()
                        .justify_center()
                        .p(zpx(16.0))
                        .child(
                            view()
                                .px(zpx(14.0))
                                .py(zpx(8.0))
                                .rounded(zpx(8.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.tint_strong))
                                .on_press(move || {
                                    st_open
                                        .active_dialog
                                        .set(ActiveDialog::Settings(SettingsTab::Appearance));
                                })
                                .child(
                                    text(t!("sidebar.settings"))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb_to_hsla(ui.primary_foreground)),
                                ),
                        )
                        .into_element()
                }
            };

            let nav_tabs: &[(MobileTab, String)] = &[
                (MobileTab::Projects, t!("native.nsh.mobileProjects")),
                (MobileTab::Workspace, t!("native.nsh.mobileWorkspace")),
                (MobileTab::Panels, t!("native.nsh.mobilePanels")),
                (MobileTab::Settings, t!("native.nsh.mobileSettings")),
            ];

            let bottom_nav = view()
                .flex_row()
                .items_center()
                .justify_around()
                .py(zpx(8.0))
                .border_t_1()
                .border_color(rgba_to_hsla(ui.border))
                .bg(rgb_to_hsla(ui.background))
                .children(nav_tabs.iter().map(|(nt, label)| {
                    let active = *nt == tab;
                    let st = store.clone();
                    let next_t = *nt;
                    view()
                        .px(zpx(10.0))
                        .py(zpx(4.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(if active {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.background)
                        })
                        .on_press(move || st.mobile_tab.set(next_t))
                        .child(text(label.clone()).text_xs().text_color(if active {
                            rgb_to_hsla(ui.tint_foreground)
                        } else {
                            rgb_to_hsla(ui.muted_foreground)
                        }))
                }));

            view()
                .size_full()
                .flex_col()
                .child(header)
                .child(view().flex_1().overflow_hidden().child(body))
                .when(tab == MobileTab::Workspace, |el| {
                    el.child(render_extra_keys_bar(&store, &resolved))
                })
                .child(bottom_nav)
                .into_element()
        }))
}

fn render_mobile_projects(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let projects = store.projects.get();
    let sessions = store.sessions.get();

    scroll().size_full().child(
        view()
            .flex_col()
            .p(zpx(12.0))
            .gap(zpx(8.0))
            .children(projects.into_iter().map(|p| {
                let proj_sessions: Vec<_> = sessions
                    .iter()
                    .filter(|s| s.project_id == p.id)
                    .cloned()
                    .collect();
                view()
                    .flex_col()
                    .gap(zpx(6.0))
                    .p(zpx(10.0))
                    .rounded(zpx(10.0))
                    .bg(rgb_to_hsla(ui.background))
                    .child(
                        text(p.name)
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .children(proj_sessions.into_iter().map(|s| {
                        let st = store.clone();
                        let sid = s.id.clone();
                        view()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .p(zpx(8.0))
                            .rounded(zpx(6.0))
                            .bg(rgb_to_hsla(ui.card))
                            .cursor_pointer()
                            .on_press(move || {
                                st.focus_session(&sid);
                                st.mobile_tab.set(MobileTab::Workspace);
                            })
                            .child(
                                text(s.display_title())
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            )
                            .child(
                                text(t!("session.open"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.tint_strong)),
                            )
                    }))
            })),
    )
}

fn render_mobile_workspace(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let focused = store.focused_pane_key.get();
    let body = match parse_pane_key(&focused) {
        Some(PaneItem::Terminal { id, .. }) => {
            if store.agent_views.get().contains_key(&id) {
                agent_pane_view()
                    .session_id(id)
                    .focused(true)
                    .into_element()
            } else {
                falcon_terminal()
                    .session_id(id)
                    .focused(true)
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
        None => view()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                text(t!("canvas.empty"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
            .into_element(),
    };

    view()
        .size_full()
        .p(pt_px(4.0))
        .child(
            view()
                .size_full()
                .rounded(zpx(8.0))
                .overflow_hidden()
                .bg(rgb_to_hsla(ui.background))
                .child(body),
        )
}

fn render_mobile_panels(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let cur = store.right_panel_kind.get();

    let mut switcher = view()
        .flex_row()
        .flex_wrap()
        .gap(zpx(6.0))
        .p(zpx(8.0))
        .bg(rgb_to_hsla(ui.background));

    let mut choices: Vec<(RightPanelKind, String)> = vec![
        (RightPanelKind::Files, t!("native.nsh.RightPanelFiles")),
        (RightPanelKind::Git, t!("native.nsh.RightPanelGit")),
    ];
    for p in store.plugin_list.get().plugins {
        if p.enabled && p.error.is_none() {
            choices.push((RightPanelKind::Plugin(p.id), p.name));
        }
    }

    for (k, label) in choices {
        let active = cur == k;
        let st = store.clone();
        let next_k = k.clone();
        switcher = switcher.child(
            view()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(if active {
                    rgb_to_hsla(ui.tint)
                } else {
                    rgb_to_hsla(ui.secondary)
                })
                .on_press(move || st.right_panel_kind.set(next_k.clone()))
                .child(text(label).text_xs().text_color(rgb_to_hsla(ui.foreground))),
        );
    }

    let inner = match cur {
        RightPanelKind::Files => files_panel().into_element(),
        RightPanelKind::Git => git_panel().into_element(),
        RightPanelKind::Plugin(pid) => plugin_panel().plugin_id(pid).into_element(),
    };

    view()
        .size_full()
        .flex_col()
        .child(switcher)
        .child(view().flex_1().bg(rgb_to_hsla(ui.background)).child(inner))
}

/// 移动端终端扩展按键条（`Esc`, `Tab`, `Ctrl`, `Alt`, `/`, `-`, 方向键）。
fn render_extra_keys_bar(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let ctrl_on = store.extra_ctrl_latched.get();
    let alt_on = store.extra_alt_latched.get();
    let focused = store.focused_pane_key.get();

    let send_seq = |st: &WorkspaceStore, pane_key: &str, seq: &str| {
        if let Some(PaneItem::Terminal { id, .. }) = parse_pane_key(pane_key) {
            let sess = st.ensure_terminal_session(&id);
            sess.send_input(seq);
        }
    };

    let keys: &[(&str, &str)] = &[
        ("Esc", "\x1b"),
        ("Tab", "\t"),
        ("/", "/"),
        ("-", "-"),
        ("↑", "\x1b[A"),
        ("↓", "\x1b[B"),
        ("←", "\x1b[D"),
        ("→", "\x1b[C"),
    ];

    let st_ctrl = store.clone();
    let st_alt = store.clone();

    view()
        .flex_row()
        .items_center()
        .gap(zpx(4.0))
        .px(zpx(8.0))
        .py(zpx(4.0))
        .bg(rgb_to_hsla(ui.card))
        .border_t_1()
        .border_color(rgba_to_hsla(ui.border))
        .child(
            view()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(if ctrl_on {
                    rgb_to_hsla(ui.tint_strong)
                } else {
                    rgb_to_hsla(ui.secondary)
                })
                .on_press(move || st_ctrl.extra_ctrl_latched.update(|v| *v = !*v))
                .child(
                    text(t!("native.nsh.extraCtrl"))
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(if ctrl_on {
                            rgb_to_hsla(ui.primary_foreground)
                        } else {
                            rgb_to_hsla(ui.secondary_foreground)
                        }),
                ),
        )
        .child(
            view()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(if alt_on {
                    rgb_to_hsla(ui.tint_strong)
                } else {
                    rgb_to_hsla(ui.secondary)
                })
                .on_press(move || st_alt.extra_alt_latched.update(|v| *v = !*v))
                .child(
                    text(t!("native.nsh.extraAlt"))
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(if alt_on {
                            rgb_to_hsla(ui.primary_foreground)
                        } else {
                            rgb_to_hsla(ui.secondary_foreground)
                        }),
                ),
        )
        .children(keys.iter().map(|&(label, seq)| {
            let st = store.clone();
            let fk = focused.clone();
            let out_seq = seq.to_string();
            view()
                .px(zpx(7.0))
                .py(zpx(4.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || send_seq(&st, &fk, &out_seq))
                .child(
                    text(label)
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                )
        }))
}
