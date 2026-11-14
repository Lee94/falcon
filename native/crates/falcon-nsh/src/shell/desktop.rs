//! 桌面端、平板端与 Web 端浮动岛列式工作区壳层（ADR 0011 + ADR 0012 + ADR 0020）。
//!
//! - 外框底板着 `ResolvedTheme::ui.app`，侧栏、每扇工作区窗口与右侧栏均为带圆角的浮动岛，由缝隙自然分隔，
//!   **不画跨屏 1px 分割线**；
//! - 列宽与列内窗口高度统一由 `falcon_core::layout::layout_frames` 计算；
//! - 终端会话的 `ProgramStatus` 胶囊严格画在窗口标题栏上，绝不在终端内插入任何会改变 PTY 行数的动态栏；
//! - 显式区分「收起窗口」（Detach，仅移除视图，后台持久会话继续跑）与「终止会话」（Terminate，忙时先弹确认框）。

use falcon_core::layout::{
    CANVAS_GAP_PX, Viewport, canvas_groups, canvas_index, is_pinned, layout_frames, move_to_canvas,
    pin_pane, unpin_all,
};
use falcon_core::pane_key::{PaneItem, parse_pane_key, term_key};
use falcon_proto::ProgramState;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{ActiveDialog, RightPanelKind, SettingsTab, WorkspaceStore, use_workspace};
use crate::terminal::falcon_terminal;
use crate::theme::{rgb_to_hsla, use_theme};
use crate::views::{
    agent_pane_view, diff_pane_view, file_pane_view, files_panel, git_panel, plugin_panel,
};
use crate::zoom::{pt_px, zpx};

#[component]
pub fn DesktopShell() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let canvas_area_size = signal((960.0_f64, 640.0_f64));

    view()
        .size_full()
        .flex_row()
        .p(zpx(8.0))
        .gap(zpx(8.0))
        .bg(move || rgb_to_hsla(theme_ctrl.resolved().ui.app))
        .child(dynamic({
            let st = store.clone();
            move || {
                let resolved = theme_ctrl.resolved();
                if !st.sidebar_open.get() {
                    return view().into_element();
                }
                render_sidebar_island(&st, &resolved).into_element()
            }
        }))
        .child(
            view()
                .flex_1()
                .h_full()
                .flex_col()
                .gap(zpx(8.0))
                .overflow_hidden()
                .child(dynamic({
                    let st = store.clone();
                    move || {
                        let resolved = theme_ctrl.resolved();
                        render_workspace_topbar(&st, &resolved).into_element()
                    }
                }))
                .child(
                    view()
                        .flex_1()
                        .w_full()
                        .relative()
                        .overflow_hidden()
                        .on_layout(move |b: &Bounds<Points>| {
                            let w = (b.size.width.0 as f64).max(320.0);
                            let h = (b.size.height.0 as f64).max(200.0);
                            canvas_area_size.set((w, h));
                        })
                        .child(dynamic({
                            let st = store.clone();
                            move || {
                                let resolved = theme_ctrl.resolved();
                                let (vw, vh) = canvas_area_size.get();
                                render_canvas_columns(&st, &resolved, vw, vh).into_element()
                            }
                        })),
                ),
        )
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            if !store.right_panel_open.get() {
                return view().into_element();
            }
            render_right_panel_island(&store, &resolved).into_element()
        }))
}

fn render_sidebar_island(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let width = store.sidebar_width.get();
    let projects = store.projects.get();
    let sessions = store.sessions.get();
    let active_proj = store.active_project_id.get();
    let focused_key = store.focused_pane_key.get();
    let connected = store.server_connected.get();
    let plaintext = store.plaintext_warning.get();

    let header = {
        let st_prof = store.clone();
        let st_set = store.clone();
        let st_new_proj = store.clone();
        let st_new_term = store.clone();
        let appearance_tag = if resolved.is_dark {
            t!("native.nsh.themeDerivedDark")
        } else {
            t!("native.nsh.themeDerivedLight")
        };

        view()
            .flex_col()
            .gap(zpx(6.0))
            .p(zpx(10.0))
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
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
                                    .text_color(rgb_to_hsla(ui.sidebar_foreground)),
                            )
                            .child(
                                view()
                                    .px(zpx(6.0))
                                    .py(zpx(1.5))
                                    .rounded(zpx(4.0))
                                    .bg(rgb_to_hsla(ui.sidebar_accent))
                                    .child(
                                        text(if connected {
                                            t!("native.nsh.serverOnline")
                                        } else {
                                            t!("native.nsh.serverOffline")
                                        })
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.sidebar_accent_foreground)),
                                    ),
                            ),
                    )
                    .child(
                        view()
                            .px(zpx(7.0))
                            .py(zpx(3.0))
                            .rounded(zpx(5.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.sidebar_accent))
                            .on_press(move || {
                                st_set
                                    .active_dialog
                                    .set(ActiveDialog::Settings(SettingsTab::Appearance));
                            })
                            .child(
                                text(t!("sidebar.settings"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.sidebar_accent_foreground)),
                            ),
                    ),
            )
            .child(
                text(appearance_tag)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
            .when(plaintext, |el| {
                el.child(
                    text(t!("native.server.plaintext"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.warning)),
                )
            })
            .child(
                view()
                    .flex_row()
                    .gap(zpx(6.0))
                    .child(
                        view()
                            .flex_1()
                            .items_center()
                            .py(zpx(5.0))
                            .rounded(zpx(6.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.sidebar_accent))
                            .on_press(move || {
                                st_new_proj.active_dialog.set(ActiveDialog::NewProject);
                            })
                            .child(
                                text(t!("sidebar.newProject"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.sidebar_accent_foreground)),
                            ),
                    )
                    .child(
                        view()
                            .flex_1()
                            .items_center()
                            .py(zpx(5.0))
                            .rounded(zpx(6.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.tint))
                            .on_press(move || {
                                st_new_term.create_terminal_in_active_project();
                            })
                            .child(
                                text(t!("sidebar.newTerminal"))
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb_to_hsla(ui.tint_foreground)),
                            ),
                    ),
            )
    };

    let mut project_blocks = view().flex_col().px(zpx(8.0)).pb(zpx(10.0)).gap(zpx(8.0));

    for proj in projects {
        let is_active_proj = proj.id == active_proj;
        let proj_sessions: Vec<_> = sessions
            .iter()
            .filter(|s| s.project_id == proj.id)
            .cloned()
            .collect();

        let location_line = match &proj.host_name {
            Some(host) => t!("native.nsh.hostPrefix", name = host.as_str()),
            None => t!("native.nsh.localMachine"),
        };

        let st_pick_proj = store.clone();
        let pid_pick = proj.id.clone();

        let mut card = view()
            .flex_col()
            .gap(zpx(4.0))
            .p(zpx(8.0))
            .rounded(zpx(8.0))
            .bg(if is_active_proj {
                rgb_to_hsla(ui.sidebar_accent)
            } else {
                rgb_to_hsla(ui.sidebar)
            })
            .child(
                view()
                    .flex_col()
                    .gap(zpx(2.0))
                    .cursor_pointer()
                    .on_press(move || st_pick_proj.active_project_id.set(pid_pick.clone()))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(zpx(4.0))
                            .child(
                                text(proj.name.clone())
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb_to_hsla(ui.sidebar_foreground)),
                            )
                            .when(proj.derived_from.is_some(), |el| {
                                el.child(
                                    view()
                                        .px(zpx(5.0))
                                        .py(zpx(1.0))
                                        .rounded(zpx(4.0))
                                        .bg(rgb_to_hsla(ui.tint))
                                        .child(
                                            text(t!("native.nsh.derivedBadge"))
                                                .text_xs()
                                                .text_color(rgb_to_hsla(ui.tint_foreground)),
                                        ),
                                )
                            }),
                    )
                    .when_some(proj.derived_from.clone(), |el, src_name| {
                        el.child(
                            text(t!(
                                "native.nsh.sourceProjectPrefix",
                                name = src_name.as_str()
                            ))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                    })
                    .child(
                        text(location_line)
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    ),
            );

        for sess in proj_sessions {
            let pane_k = term_key(&sess.id);
            let attached = store.is_session_attached(&sess.id);
            let is_focused = focused_key == pane_k;
            let st_focus = store.clone();
            let sid_focus = sess.id.clone();

            let mut row = view()
                .flex_col()
                .gap(zpx(3.0))
                .p(zpx(6.0))
                .rounded(zpx(6.0))
                .bg(if is_focused {
                    rgb_to_hsla(ui.tint)
                } else {
                    rgb_to_hsla(ui.card)
                })
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(zpx(4.0))
                        .cursor_pointer()
                        .on_press(move || st_focus.focus_session(&sid_focus))
                        .child(
                            text(sess.display_title())
                                .text_xs()
                                .number_of_lines(1)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            text(if sess.durable {
                                t!("native.nsh.durableBadge")
                            } else {
                                t!("native.nsh.ephemeralBadge")
                            })
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                );

            if !attached {
                let st_reattach = store.clone();
                let sid_reattach = sess.id.clone();
                let reattach_label = format!("reattach-{}", sess.id);
                row = row.child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .child(
                            text(t!("native.nsh.detachedBadge"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.tint_strong)),
                        )
                        .child(
                            view()
                                .label(reattach_label)
                                .px(zpx(6.0))
                                .py(zpx(2.0))
                                .rounded(zpx(4.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.tint_strong))
                                .on_press(move || st_reattach.focus_session(&sid_reattach))
                                .child(
                                    text(t!("native.nsh.reattachSession"))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb_to_hsla(ui.primary_foreground)),
                                ),
                        ),
                );
            }

            card = card.child(row);
        }

        project_blocks = project_blocks.child(card);
    }

    view()
        .w(zpx(width))
        .h_full()
        .flex_col()
        .rounded(zpx(10.0))
        .overflow_hidden()
        .bg(rgb_to_hsla(ui.sidebar))
        .child(header)
        .child(scroll().flex_1().child(project_blocks))
}

fn render_workspace_topbar(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let cols = store.columns.get();
    let focused = store.focused_pane_key.get();
    let remembered = store.active_canvas_id.get();
    let groups = canvas_groups(&cols);
    let active_idx = canvas_index(
        &groups,
        (!focused.is_empty()).then_some(focused.as_str()),
        Some(remembered.as_str()),
    );

    let mut left = view().flex_row().items_center().gap(zpx(6.0));

    {
        let st_sb = store.clone();
        left = left.child(
            view()
                .px(zpx(7.0))
                .py(zpx(3.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || st_sb.sidebar_open.update(|v| *v = !*v))
                .child(
                    text(t!("sidebar.collapse"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        );
    }

    for (idx, group) in groups.iter().enumerate() {
        let selected = idx == active_idx;
        let st = store.clone();
        let gid = group.id.clone();
        let first_key = group
            .columns
            .first()
            .and_then(|c| c.panes.first())
            .map(|p| p.key.clone());
        let label = t!(
            "native.nsh.canvasLabel",
            index = (idx + 1).to_string().as_str()
        );
        left = left.child(
            view()
                .px(zpx(8.0))
                .py(zpx(3.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(if selected {
                    rgb_to_hsla(ui.tint)
                } else {
                    rgb_to_hsla(ui.secondary)
                })
                .on_press(move || {
                    st.active_canvas_id.set(gid.clone());
                    if let Some(k) = &first_key {
                        st.focused_pane_key.set(k.clone());
                    }
                })
                .child(text(label).text_xs().text_color(if selected {
                    rgb_to_hsla(ui.tint_foreground)
                } else {
                    rgb_to_hsla(ui.secondary_foreground)
                })),
        );
    }

    if !focused.is_empty() {
        let st_move = store.clone();
        let fk = focused.clone();
        let after = remembered.clone();
        left = left.child(
            view()
                .px(zpx(7.0))
                .py(zpx(3.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || {
                    let cur = st_move.columns.get();
                    if let Some(next) = move_to_canvas(&cur, &fk, None, Some(&after)) {
                        st_move.columns.set(next);
                    }
                })
                .child(
                    text(t!("native.nsh.newCanvas"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        );
    }

    let mut right = view().flex_row().items_center().gap(zpx(6.0));
    {
        let st_pal = store.clone();
        right = right.child(
            view()
                .px(zpx(8.0))
                .py(zpx(3.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || st_pal.active_dialog.set(ActiveDialog::Palette))
                .child(
                    text(t!("palette.title"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        );
    }

    let current_rp = store.right_panel_kind.get();
    let rp_open = store.right_panel_open.get();

    let mut panel_tabs: Vec<(RightPanelKind, String)> = vec![
        (RightPanelKind::Files, t!("native.nsh.RightPanelFiles")),
        (RightPanelKind::Git, t!("native.nsh.RightPanelGit")),
    ];
    for p in store.plugin_list.get().plugins {
        if p.enabled && p.error.is_none() {
            panel_tabs.push((RightPanelKind::Plugin(p.id), p.name));
        }
    }

    for (kind, label) in panel_tabs {
        let active = rp_open && current_rp == kind;
        let st = store.clone();
        let target_kind = kind.clone();
        right = right.child(
            view()
                .px(zpx(8.0))
                .py(zpx(3.0))
                .rounded(zpx(5.0))
                .cursor_pointer()
                .bg(if active {
                    rgb_to_hsla(ui.tint)
                } else {
                    rgb_to_hsla(ui.secondary)
                })
                .on_press(move || {
                    if st.right_panel_open.get() && st.right_panel_kind.get() == target_kind {
                        st.right_panel_open.set(false);
                    } else {
                        st.right_panel_kind.set(target_kind.clone());
                        st.right_panel_open.set(true);
                    }
                })
                .child(text(label).text_xs().text_color(if active {
                    rgb_to_hsla(ui.tint_foreground)
                } else {
                    rgb_to_hsla(ui.secondary_foreground)
                })),
        );
    }

    view()
        .flex_row()
        .items_center()
        .justify_between()
        .px(zpx(10.0))
        .py(zpx(6.0))
        .rounded(zpx(10.0))
        .bg(rgb_to_hsla(ui.background))
        .child(left)
        .child(right)
}

fn render_canvas_columns(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    viewport_w: f64,
    viewport_h: f64,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let all_cols = store.columns.get();
    let focused = store.focused_pane_key.get();
    let remembered = store.active_canvas_id.get();
    let groups = canvas_groups(&all_cols);

    if groups.is_empty() {
        return view()
            .size_full()
            .items_center()
            .justify_center()
            .rounded(zpx(10.0))
            .bg(rgb_to_hsla(ui.background))
            .child(
                text(t!("canvas.empty"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
    }

    let idx = canvas_index(
        &groups,
        (!focused.is_empty()).then_some(focused.as_str()),
        Some(remembered.as_str()),
    );
    let active_cols = &groups[idx].columns;
    let frames = layout_frames(
        active_cols,
        Viewport {
            width: viewport_w,
            height: viewport_h,
        },
        CANVAS_GAP_PX,
    );

    let mut stage = view().size_full().relative();
    for col_frame in frames.columns {
        let pane_count = col_frame.panes.len();
        for (p_idx, pf) in col_frame.panes.into_iter().enumerate() {
            let bottom_gap = if p_idx + 1 < pane_count {
                CANVAS_GAP_PX
            } else {
                0.0
            };
            let h = (pf.height - bottom_gap).max(64.0);
            stage = stage.child(
                view()
                    .key(pf.key.clone())
                    .absolute()
                    .left(pt_px(col_frame.x as f32))
                    .top(pt_px(pf.y as f32))
                    .w(pt_px(col_frame.width as f32))
                    .h(pt_px(h as f32))
                    .rounded(zpx(10.0))
                    .overflow_hidden()
                    .bg(rgb_to_hsla(ui.background))
                    .child(render_pane_island(
                        store,
                        resolved,
                        &all_cols,
                        &pf.key,
                        focused == pf.key,
                    )),
            );
        }
    }
    stage
}

fn render_pane_island(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    all_cols: &[falcon_core::layout::ColumnLayout],
    pane_key: &str,
    is_focused: bool,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let sessions = store.sessions.get();
    let parsed = parse_pane_key(pane_key);
    let pinned = is_pinned(all_cols, pane_key);

    let (title_text, prog_capsule, term_session_id) = match &parsed {
        Some(PaneItem::Terminal { id, .. }) => {
            let rec = sessions.iter().find(|s| &s.id == id);
            let title = rec
                .map(|r| r.display_title())
                .unwrap_or_else(|| id.clone());
            let capsule = rec.and_then(|r| r.program_status.clone());
            (title, capsule, Some(id.clone()))
        }
        Some(PaneItem::File { path, .. }) => (path.clone(), None, None),
        Some(PaneItem::Diff { .. }) => (t!("tab.diff"), None, None),
        None => (pane_key.to_string(), None, None),
    };

    let st_focus = store.clone();
    let pk_focus = pane_key.to_string();

    let mut left = view()
        .flex_1()
        .flex_row()
        .items_center()
        .gap(zpx(8.0))
        .overflow_hidden()
        .cursor_pointer()
        .on_press(move || st_focus.focused_pane_key.set(pk_focus.clone()))
        .child(
            view()
                .flex_none()
                .w(zpx(7.0))
                .h(zpx(7.0))
                .rounded_full()
                .bg(if is_focused {
                    rgb_to_hsla(ui.tint_strong)
                } else {
                    rgb_to_hsla(ui.muted_foreground)
                }),
        )
        .child(
            text(title_text)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .number_of_lines(1)
                .text_color(rgb_to_hsla(ui.foreground)),
        );

    // ADR 0020：程序状态胶囊严格画在窗口标题栏上，绝不在终端内增加动态状态行
    if let Some((st_kind, summary)) = prog_capsule {
        let capsule_fg = match st_kind {
            ProgramState::Working => rgb_to_hsla(ui.tint_strong),
            ProgramState::Blocked => rgb_to_hsla(ui.warning),
            ProgramState::Done => rgb_to_hsla(ui.success),
            ProgramState::Error => rgb_to_hsla(ui.destructive),
            ProgramState::Idle | ProgramState::Unknown => rgb_to_hsla(ui.muted_foreground),
        };
        left = left.child(
            view()
                .flex_none()
                .px(zpx(6.0))
                .py(zpx(1.5))
                .rounded(zpx(4.0))
                .bg(rgb_to_hsla(ui.muted))
                .child(
                    text(summary)
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .number_of_lines(1)
                        .text_color(capsule_fg),
                ),
        );
    }

    let mut right = view().flex_none().flex_row().items_center().gap(zpx(4.0));

    {
        let st_pin = store.clone();
        let pk_pin = pane_key.to_string();
        right = right.child(
            view()
                .px(zpx(6.0))
                .py(zpx(2.0))
                .rounded(zpx(4.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || {
                    let cur = st_pin.columns.get();
                    let next = if pinned {
                        unpin_all(&cur)
                    } else {
                        pin_pane(&cur, &pk_pin)
                    };
                    st_pin.columns.set(next);
                })
                .child(
                    text(if pinned {
                        t!("native.nsh.unpinColumn")
                    } else {
                        t!("native.nsh.pinColumn")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        );
    }

    // 显式「收起窗口」(Detach) 按钮：仅摘除视图，不杀后台会话
    {
        let st_detach = store.clone();
        let pk_detach = pane_key.to_string();
        let detach_a11y = format!("detach-{pane_key}");
        right = right.child(
            view()
                .label(detach_a11y)
                .px(zpx(6.0))
                .py(zpx(2.0))
                .rounded(zpx(4.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || st_detach.detach_pane(&pk_detach))
                .child(
                    text(t!("native.nsh.DetachWindow"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        );
    }

    // 显式「终止会话」(Terminate) 按钮：前台忙时触发确认护栏
    if let Some(sid) = term_session_id {
        let st_term = store.clone();
        let terminate_a11y = format!("terminate-{sid}");
        right = right.child(
            view()
                .label(terminate_a11y)
                .px(zpx(6.0))
                .py(zpx(2.0))
                .rounded(zpx(4.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.muted))
                .on_press(move || st_term.request_terminate_session(&sid))
                .child(
                    text(t!("native.nsh.TerminateSession"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.destructive)),
                ),
        );
    }

    let titlebar = view()
        .flex_row()
        .items_center()
        .justify_between()
        .px(zpx(10.0))
        .py(zpx(5.0))
        .bg(rgb_to_hsla(ui.card))
        .child(left)
        .child(right);

    let body = match parsed {
        Some(PaneItem::Terminal { id, .. }) => {
            if store.agent_views.get().contains_key(&id) {
                agent_pane_view()
                    .session_id(id)
                    .focused(is_focused)
                    .into_element()
            } else {
                falcon_terminal()
                    .session_id(id)
                    .focused(is_focused)
                    .tui_passthrough(store.tui_passthrough.get())
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

    view()
        .size_full()
        .flex_col()
        .child(titlebar)
        .child(view().flex_1().overflow_hidden().child(body))
}

fn render_right_panel_island(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let width = store.right_panel_width.get();
    let kind = store.right_panel_kind.get();

    let inner = match kind {
        RightPanelKind::Files => files_panel().into_element(),
        RightPanelKind::Git => git_panel().into_element(),
        RightPanelKind::Plugin(pid) => plugin_panel().plugin_id(pid).into_element(),
    };

    view()
        .w(zpx(width))
        .h_full()
        .rounded(zpx(10.0))
        .overflow_hidden()
        .bg(rgb_to_hsla(ui.background))
        .child(inner)
}
