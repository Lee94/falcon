//! 桌面端、平板端与 Web 端浮动岛列式工作区壳层（ADR 0011 + ADR 0012 + ADR 0020）。
//!
//! - 外框底板着 `ResolvedTheme::ui.app`，侧栏、每扇工作区窗口与右侧栏均为带圆角的浮动岛，由缝隙自然分隔，
//!   **不画跨屏 1px 分割线**；
//! - 列宽与列内窗口高度统一由 `falcon_core::layout::layout_frames` 计算；
//! - 终端会话的 `ProgramStatus` 胶囊严格画在窗口标题栏上，绝不在终端内插入任何会改变 PTY 行数的动态栏；
//! - 显式区分「收起窗口」（Detach，仅移除视图，后台持久会话继续跑）与「终止会话」（Terminate，忙时先弹确认框）。

use falcon_core::layout::{
    Viewport, canvas_groups, canvas_index, is_pinned, layout_frames, move_to_canvas, pin_pane,
    unpin_all,
};
use falcon_core::pane_key::{PaneItem, parse_pane_key};
use nsh_core::element::desktop::title_bar;
use nsh_core::prelude::*;

use super::sidebar::sidebar_island;
use super::widgets::{Glyph, icon_button};
use super::title_status::title_status;
use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::shell::popups::themed_tooltip;
use crate::store::{ActiveDialog, RightPanelKind, WorkspaceStore, use_workspace};
use crate::terminal::falcon_terminal;
use crate::theme::{rgb_to_hsla, use_theme};
use crate::views::{
    agent_pane_view, diff_pane_view, file_pane_view, files_panel, git_panel, plugin_panel,
};
use crate::zoom::{current_zoom, pt_px, zpx};

#[component]
pub fn DesktopShell() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let canvas_area_size = signal((960.0_f64, 640.0_f64));
    let identity = store.clone();

    let shell = view()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex_row()
        .p(zpx(6.0))
        .gap(zpx(6.0))
        .child(dynamic({
            let st = store.clone();
            move || {
                let resolved = theme_ctrl.resolved();
                if !st.sidebar_open.get() {
                    return view().into_element();
                }
                view()
                    .h_full()
                    .flex_none()
                    .relative()
                    .child(sidebar_island(&st, &resolved))
                    .child(panel_resize_handle(&st, false))
                    .into_element()
            }
        }))
        .child(
            view()
                .flex_1()
                .min_w_0()
                .h_full()
                .flex_col()
                .gap(zpx(0.0))
                .overflow_hidden()
                .child(dynamic({
                    let st = store.clone();
                    move || {
                        let resolved = theme_ctrl.resolved();
                        if canvas_groups(&project_columns(&st)).len() > 1 {
                            render_workspace_topbar(&st, &resolved).into_element()
                        } else {
                            view().into_element()
                        }
                    }
                }))
                .child(
                    view()
                        .flex_1()
                        .w_full()
                        .relative()
                        .overflow_hidden()
                        .on_layout(move |b: &Bounds<Points>| {
                            let w = (b.size.width.0 as f64).max(1.0);
                            let h = (b.size.height.0 as f64).max(1.0);
                            if canvas_area_size.get() != (w, h) {
                                canvas_area_size.set((w, h));
                            }
                        })
                        .child(dynamic({
                            let st = store.clone();
                            move || {
                                let resolved = theme_ctrl.resolved();
                                let (vw, vh) = canvas_area_size.get();
                                if st.overview_open.get() {
                                    super::overview::session_overview().into_element()
                                } else {
                                    render_canvas_columns(&st, &resolved, vw, vh).into_element()
                                }
                            }
                        })),
                ),
        )
        .child(dynamic({
            let store = store.clone();
            move || {
                let resolved = theme_ctrl.resolved();
                if !store.right_panel_open.get() {
                    return view().into_element();
                }
                view()
                    .h_full()
                    .flex_none()
                    .relative()
                    .child(render_right_panel_island(&store, &resolved))
                    .child(panel_resize_handle(&store, true))
                    .into_element()
            }
        }))
        .child(dynamic(move || {
            activity_bar(&store, &theme_ctrl.resolved()).into_element()
        }));
    view()
        .size_full()
        .flex_col()
        .child(dynamic(move || {
            identity_strip(&identity, &theme_ctrl.resolved()).into_element()
        }))
        .child(shell)
}

fn render_workspace_topbar(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let cols = project_columns(store);
    let focused = store.focused_pane_key.get();
    let remembered = store.active_canvas_id.get();
    let groups = canvas_groups(&cols);
    let active_idx = canvas_index(
        &groups,
        (!focused.is_empty()).then_some(focused.as_str()),
        Some(remembered.as_str()),
    );

    let mut left = view().flex_row().items_center().gap(zpx(6.0));

    if !store.sidebar_open.get() {
        let st = store.clone();
        left = left.child(icon_button(
            "sidebar-expand",
            t!("native.nsh.expandSidebar"),
            Glyph::Sidebar,
            false,
            resolved,
            move || st.sidebar_open.set(true),
        ));
    }
    let project_name = store
        .projects
        .get()
        .into_iter()
        .find(|p| p.id == store.active_project_id.get())
        .map(|p| p.name)
        .unwrap_or_default();
    left = left.child(
        text(project_name)
            .text_xs()
            .number_of_lines(1)
            .text_color(rgb_to_hsla(ui.muted_foreground)),
    );
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
                .rounded(zpx(12.0))
                .cursor_pointer()
                .when(selected, |v| v.bg(rgb_to_hsla(ui.background)))
                .hover(|s| s.bg(rgb_to_hsla(ui.muted)))
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
        let st = store.clone();
        let fk = focused.clone();
        let after = remembered.clone();
        left = left.child(icon_button(
            "move-new-canvas",
            t!("pane.moveToNewCanvas"),
            Glyph::Plus,
            false,
            resolved,
            move || {
                if let Some(cols) = move_to_canvas(&st.columns.get(), &fk, None, Some(&after)) {
                    st.columns.set(cols);
                }
            },
        ));
    }
    let st_new = store.clone();
    let st_pal = store.clone();
    view()
        .label("workspace-topbar")
        .h(zpx(32.0))
        .flex_none()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(zpx(4.0))
        .child(left.flex_1().min_w_0())
        .child(icon_button(
            "new-terminal",
            t!("sidebar.newTerminal"),
            Glyph::Plus,
            false,
            resolved,
            move || {
                st_new.create_terminal_in_active_project();
            },
        ))
        .child(icon_button(
            "open-palette",
            t!("palette.title"),
            Glyph::Search,
            false,
            resolved,
            move || st_pal.active_dialog.set(ActiveDialog::Palette),
        ))
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
    let visible = project_columns(store);
    let groups = canvas_groups(&visible);

    if groups.is_empty() {
        return view()
            .size_full()
            .items_center()
            .justify_center()
            .rounded(zpx(12.0))
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
    let zoomed = if store.term_zoomed.get() {
        groups[idx]
            .columns
            .iter()
            .find(|c| c.panes.iter().any(|p| p.key == focused))
            .cloned()
            .map(|mut c| {
                c.basis = None;
                vec![c]
            })
    } else {
        None
    };
    let active_cols = zoomed.as_ref().unwrap_or(&groups[idx].columns);
    let gap = zpx(6.0).0 as f64;
    let frames = layout_frames(
        active_cols,
        Viewport {
            width: viewport_w,
            height: viewport_h,
        },
        gap,
    );

    let mut stage = view().size_full().relative();
    for (col_idx, col_frame) in frames.columns.iter().enumerate() {
        let pane_count = col_frame.panes.len();
        for (p_idx, pf) in col_frame.panes.iter().enumerate() {
            let bottom_gap = if p_idx + 1 < pane_count { gap } else { 0.0 };
            let h = (pf.height - bottom_gap).max(64.0);
            stage = stage.child(
                view()
                    .label(format!("pane-{}", pf.key))
                    .key(pf.key.clone())
                    .absolute()
                    .left(pt_px(col_frame.x as f32))
                    .top(pt_px(pf.y as f32))
                    .w(pt_px(col_frame.width as f32))
                    .h(pt_px(h as f32))
                    .rounded(zpx(12.0))
                    .overflow_hidden()
                    .bg(rgb_to_hsla(ui.background))
                    .border_1()
                    .border_color(if focused == pf.key {
                        rgb_to_hsla(ui.tint_strong)
                    } else {
                        crate::theme::rgba_to_hsla(ui.app_border)
                    })
                    .child(render_pane_island(
                        store,
                        resolved,
                        &all_cols,
                        &pf.key,
                        focused == pf.key,
                        &frames,
                        active_cols,
                        (col_frame.x, pf.y),
                    )),
            );
            if bottom_gap > 0.0 {
                let st = store.clone();
                let reset = store.clone();
                let key = pf.key.clone();
                let reset_key = key.clone();
                let initial = pf.height;
                let max = (viewport_h - pf.y - falcon_core::layout::PANE_MIN_PX)
                    .max(falcon_core::layout::PANE_MIN_PX);
                stage = stage.child(
                    view()
                        .label(format!("resize-pane-{}", pf.key))
                        .absolute()
                        .left(pt_px(col_frame.x as f32))
                        .top(pt_px((pf.y + h) as f32))
                        .w(pt_px(col_frame.width as f32))
                        .h(pt_px(gap as f32))
                        .cursor(nsh_core::Cursor::RowResize)
                        .pan_enabled(true)
                        .on_pan_end(move |e: &PanEvent| {
                            if !e.cancelled {
                                st.columns.set(falcon_core::layout::set_pane_basis(
                                    &st.columns.get(),
                                    &key,
                                    Some(falcon_core::layout::clamp_pane_height(
                                        initial + e.translation.y.0 as f64,
                                        max,
                                    )),
                                ));
                            }
                        })
                        .on_press(move |e: &PressEvent| {
                            if e.clicks == 2 {
                                reset.columns.set(falcon_core::layout::set_pane_basis(
                                    &reset.columns.get(),
                                    &reset_key,
                                    None,
                                ));
                            }
                        }),
                );
            }
        }
        if col_idx + 1 < frames.columns.len() {
            let st = store.clone();
            let reset = store.clone();
            let id = col_frame.id.clone();
            let reset_id = id.clone();
            let initial = col_frame.width;
            let max = (viewport_w - col_frame.x - falcon_core::layout::COLUMN_MIN_PX - gap)
                .max(falcon_core::layout::COLUMN_MIN_PX);
            stage = stage.child(
                view()
                    .label(format!("resize-column-{}", col_frame.id))
                    .tooltip(themed_tooltip(text(t!("pane.resizeColumn")), None, resolved))
                    .absolute()
                    .left(pt_px((col_frame.x + initial) as f32))
                    .top(pt_px(0.0))
                    .w(pt_px(gap as f32))
                    .h_full()
                    .cursor(nsh_core::Cursor::ColResize)
                    .pan_enabled(true)
                    .on_pan_end(move |e: &PanEvent| {
                        if !e.cancelled {
                            st.columns.set(falcon_core::layout::set_column_basis(
                                &st.columns.get(),
                                &id,
                                Some(falcon_core::layout::clamp_column_width(
                                    initial + e.translation.x.0 as f64,
                                    max,
                                )),
                            ));
                        }
                    })
                    .on_press(move |e: &PressEvent| {
                        if e.clicks == 2 {
                            reset.columns.set(falcon_core::layout::set_column_basis(
                                &reset.columns.get(),
                                &reset_id,
                                None,
                            ));
                        }
                    }),
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
    frames: &falcon_core::layout::CanvasFrames,
    visible: &[falcon_core::layout::ColumnLayout],
    origin: (f64, f64),
) -> impl IntoElement {
    let ui = &resolved.ui;
    let sessions = store.sessions.get();
    let parsed = parse_pane_key(pane_key);
    let pinned = is_pinned(all_cols, pane_key);

    let (title_text, term_session_id) = match &parsed {
        Some(PaneItem::Terminal { id, .. }) => {
            let rec = sessions.iter().find(|s| &s.id == id);
            let title = rec
                .map(|r| {
                    if !r.name.trim().is_empty() {
                        r.name.clone()
                    } else if !r.auto_title.trim().is_empty() {
                        r.auto_title.clone()
                    } else {
                        r.agent.clone().unwrap_or_default()
                    }
                })
                .unwrap_or_default();
            (title, Some(id.clone()))
        }
        Some(PaneItem::File { path, .. }) => (path.clone(), None),
        Some(PaneItem::Diff { .. }) => (t!("tab.diff"), None),
        None => (pane_key.to_string(), None),
    };

    let st_focus = store.clone();
    let pk_focus = pane_key.to_string();
    let drag = store.clone();
    let drag_key = pane_key.to_string();
    let drag_visible = visible.to_vec();
    let rects: Vec<_> = frames
        .columns
        .iter()
        .map(|c| falcon_core::layout::ColumnRect {
            left: c.x,
            right: c.x + c.width,
            panes: c
                .panes
                .iter()
                .map(|p| falcon_core::layout::PaneRect {
                    top: p.y,
                    bottom: p.y + p.height,
                })
                .collect(),
        })
        .collect();
    let drag_origin = signal(origin);
    let press_zoom = store.term_zoomed;

    let mut left = view()
        .flex_1()
        .min_w_0()
        .flex_row()
        .items_center()
        .gap(zpx(8.0))
        .overflow_hidden()
        .cursor_pointer()
        .tooltip(themed_tooltip(text(t!("pane.dragHint")), None, resolved))
        .cursor(nsh_core::Cursor::Grab)
        .on_press_down(move |e: &PressEvent| {
            drag_origin.set((
                origin.0 + e.position.x.0 as f64,
                origin.1 + e.position.y.0 as f64,
            ))
        })
        .pan_enabled(true)
        .on_pan_end(move |e: &PanEvent| {
            if !e.cancelled && !drag.term_zoomed.get() {
                use falcon_core::layout::{
                    COLUMN_EDGE_PX, Point, apply_drop, drop_spot, resolve_spot,
                };
                let start = drag_origin.get();
                let spot = drop_spot(
                    Point {
                        x: start.0 + e.translation.x.0 as f64,
                        y: start.1 + e.translation.y.0 as f64,
                    },
                    &rects,
                    COLUMN_EDGE_PX,
                );
                let all = drag.columns.get();
                if let Some(next) =
                    apply_drop(&all, &drag_key, resolve_spot(&all, &drag_visible, spot))
                {
                    drag.columns.set(next);
                }
            }
        })
        .on_press(move |e: &PressEvent| {
            st_focus.focused_pane_key.set(pk_focus.clone());
            if e.clicks == 2 {
                press_zoom.update(|v| *v = !*v);
            }
        })
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
        );

    // ADR 0020：程序状态胶囊严格画在窗口标题栏上，绝不在终端内增加动态状态行
    if let Some(id) = &term_session_id {
        left = left.child(title_status().session_id(id.clone()));
    }
    left = left.child(text(title_text).min_w_0().text_xs()
        .font_weight(FontWeight::SEMIBOLD).number_of_lines(1)
        .text_color(rgb_to_hsla(ui.foreground)));

    if let Some(sid) = &term_session_id {
        if let Some(rec) = sessions.iter().find(|s| &s.id == sid) {
            left = left.child(
                text(rec.cwd.clone())
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .number_of_lines(1)
                    .font_family(FONT_FAMILY_MONO)
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
    }
    let pin = store.clone();
    let pin_key = pane_key.to_string();
    let detach = store.clone();
    let detach_key = pane_key.to_string();
    let zoom = store.term_zoomed;
    let mut right = view()
        .flex_none()
        .flex_row()
        .items_center()
        .gap(zpx(2.0));
    // 对照 GPUI canvas.rs 的 pane-agent-toggle：Claude 终端（含只读镜像）点击经
    // enableGui* 确认框发起协议转换，可交互 GUI 下只切本地排版样式。
    if let Some(id) = &term_session_id {
        let agent_kind = sessions
            .iter()
            .find(|s| &s.id == id)
            .and_then(|s| s.agent.as_deref())
            .and_then(falcon_proto::SessionAgent::from_wire);
        let agent_view = store.agent_views.get().get(id).cloned();
        if let Some(badge) =
            crate::views::agent::AgentModeBadge::new(agent_kind, agent_view.as_ref())
        {
            let switch_mode = badge.switch_mode;
            let dimmed = badge.switching;
            let chip = store.clone();
            let sid = id.clone();
            right = right.child(crate::views::agent::agent_mode_chip(
                format!("pane-agent-toggle-{pane_key}"),
                badge.label(),
                badge.tooltip(),
                dimmed,
                resolved,
                move || {
                    if switch_mode {
                        chip.request_agent_gui(&sid);
                    } else {
                        chip.toggle_agent_view_style(&sid);
                    }
                },
            ));
        }
    }
    right = right
        .child(icon_button(
            format!("pin-{pane_key}"),
            t!("pane.pinRight"),
            Glyph::Pin,
            pinned,
            resolved,
            move || {
                let cols = pin.columns.get();
                pin.columns.set(if pinned {
                    unpin_all(&cols)
                } else {
                    pin_pane(&cols, &pin_key)
                });
            },
        ))
        .child(icon_button(
            format!("maximize-{pane_key}"),
            if zoom.get() {
                t!("pane.restore")
            } else {
                t!("pane.maximize")
            },
            if zoom.get() {
                Glyph::Restore
            } else {
                Glyph::Maximize
            },
            false,
            resolved,
            move || zoom.update(|v| *v = !*v),
        ))
        .child(icon_button(
            format!("detach-{pane_key}"),
            t!("native.nsh.DetachWindow"),
            Glyph::Detach,
            false,
            resolved,
            move || detach.detach_pane(&detach_key),
        ));
    let close = store.clone();
    let close_key = pane_key.to_string();
    let edge_session_id = term_session_id.clone();
    right = right.child(icon_button(
        term_session_id
            .as_ref()
            .map(|id| format!("terminate-{id}"))
            .unwrap_or_else(|| format!("close-{pane_key}")),
        if term_session_id.is_some() {
            t!("tab.closeHint")
        } else {
            t!("tab.closeView")
        },
        Glyph::Close,
        false,
        resolved,
        move || {
            if let Some(id) = &term_session_id {
                close.request_terminate_session(id);
            } else {
                close.detach_pane(&close_key);
            }
        },
    ));
    let titlebar = view()
        .relative()
        .flex_row()
        .items_center()
        .justify_between()
        .label(format!("titlebar-{pane_key}"))
        .test_id(&format!(
            "{}-pane-{pane_key}",
            if is_focused { "active" } else { "inactive" }
        ))
        .h(zpx(34.0))
        .flex_none()
        .bg(rgb_to_hsla(if is_focused {
            ui.tint
        } else {
            ui.background
        }))
        .px(zpx(8.0))
        .child(left)
        .child(right)
        .when_some(edge_session_id, |v, id| v.child(title_status().session_id(id).edge(true)));

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

    view().size_full().flex_col().child(titlebar).child(
        view()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .child(body),
    )
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
        .label("right-panel-island")
        .test_id("right-panel-island")
        .flex_none()
        .w(zpx(width))
        .h_full()
        .rounded(zpx(12.0))
        .overflow_hidden()
        .bg(rgb_to_hsla(ui.background))
        .border_1()
        .border_color(crate::theme::rgba_to_hsla(ui.app_border))
        .child(inner)
}

fn project_columns(store: &WorkspaceStore) -> Vec<falcon_core::layout::ColumnLayout> {
    let selected = store.active_project_id.get();
    let sessions = store.sessions.get();
    falcon_core::layout::visible_columns(&store.columns.get(), |key| match parse_pane_key(key) {
        Some(PaneItem::Terminal { id, .. }) => sessions
            .iter()
            .any(|s| s.id == id && (selected.is_empty() || s.project_id == selected)),
        Some(PaneItem::File { project_id, .. }) => selected.is_empty() || project_id == selected,
        Some(PaneItem::Diff { .. }) => true,
        None => false,
    })
}

fn panel_resize_handle(store: &WorkspaceStore, right: bool) -> impl IntoElement {
    let width = if right {
        store.right_panel_width
    } else {
        store.sidebar_width
    };
    let start = width.get();
    let zoom = current_zoom();
    view()
        .label(if right {
            "resize-right-panel"
        } else {
            "resize-sidebar"
        })
        .absolute()
        .top(pt_px(0.0))
        .h_full()
        .w(zpx(6.0))
        .when(right, |v| v.left(zpx(-6.0)))
        .when(!right, |v| v.right(zpx(-6.0)))
        .cursor(nsh_core::Cursor::ColResize)
        .pan_enabled(true)
        .on_pan_end(move |event: &PanEvent| {
            if !event.cancelled {
                let dx = event.translation.x.0 / zoom;
                width.set(falcon_core::panel_width::clamp_panel_width_default(
                    (start + if right { -dx } else { dx }) as f64,
                ) as f32);
            }
        })
        .on_press(move |event: &PressEvent| {
            if event.clicks == 2 {
                width.set(falcon_core::panel_width::PANEL_WIDTH_DEFAULT as f32);
            }
        })
}

fn activity_bar(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let mut bar = view()
        .label("right-activity-bar")
        .w(zpx(34.0))
        .h_full()
        .flex_none()
        .flex_col()
        .items_center()
        .gap(zpx(4.0))
        .py(zpx(4.0));
    for (kind, icon, label) in [
        (RightPanelKind::Files, Glyph::Folder, t!("files.panel")),
        (RightPanelKind::Git, Glyph::Changes, t!("changes.panel")),
    ] {
        let active = store.right_panel_open.get() && store.right_panel_kind.get() == kind;
        let id = if kind == RightPanelKind::Files {
            "activity-files"
        } else {
            "activity-changes"
        };
        let st = store.clone();
        bar = bar.child(icon_button(id, label, icon, active, resolved, move || {
            st.toggle_right_panel(kind.clone())
        }));
    }
    let plugins: Vec<_> = store
        .plugin_list
        .get()
        .plugins
        .into_iter()
        .filter(|p| p.enabled && p.error.is_none())
        .collect();
    if !plugins.is_empty() {
        bar = bar.child(
            view()
                .w(zpx(14.0))
                .h(pt_px(1.0))
                .my(zpx(4.0))
                .bg(crate::theme::rgba_to_hsla(resolved.ui.border)),
        );
    }
    for p in plugins {
        let kind = RightPanelKind::Plugin(p.id.clone());
        let st = store.clone();
        let active = store.right_panel_open.get() && store.right_panel_kind.get() == kind;
        bar = bar.child(icon_button(
            format!("activity-plugin-{}", p.id),
            p.name,
            if p.id == "sysmon" {
                Glyph::Activity
            } else if p.id == "tokens" {
                Glyph::Link
            } else {
                Glyph::Box
            },
            active,
            resolved,
            move || st.toggle_right_panel(kind.clone()),
        ));
    }
    bar
}

fn identity_strip(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let profiles = store.profiles.get();
    let url = store.server_url.get();
    let name = profiles
        .profiles
        .iter()
        .find(|p| p.url.trim_end_matches('/') == url.trim_end_matches('/'))
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let profiles_action = store.clone();
    let create = store.clone();
    let palette = store.clone();
    let overview = store.overview_open;
    // 这条兼任桌面端的窗口标题栏（TitleBarStyle::Hidden，window_drag 提供拖动 /
    // 双击最大化 / 右键系统菜单）。系统窗口控件由 NSH 画在这条的两端：
    // macOS 红绿灯在左、Windows / GTK 的 caption 按钮在右，宽度按 title_bar()
    // 环境预留（不随界面缩放——系统控件尺寸是窗口外框，固定点值）；
    // 系统标题栏可见时与全屏下环境为 0，只剩基础边距。
    let chrome = title_bar();
    let mut strip = view()
        .label("server-identity-strip")
        .h(zpx(34.0))
        .flex_none()
        .flex_row()
        .items_center()
        .gap(zpx(8.0))
        .pr(Points(zpx(8.0).0 + chrome.right.0))
        .pl(Points(zpx(14.0).0 + chrome.left.0))
        .window_drag(true);
    if !store.sidebar_open.get() {
        let sidebar = store.sidebar_open;
        strip = strip.child(icon_button(
            "sidebar-expand",
            t!("native.nsh.expandSidebar"),
            Glyph::Sidebar,
            false,
            resolved,
            move || sidebar.set(true),
        ));
    }
    strip
        .child(
            image(crate::appearance::app_icon::selected_source(
                &store.appearance.app_icon,
            ))
            .w(zpx(18.0))
            .h(zpx(18.0)),
        )
        .child(
            text("Falcon")
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(resolved.ui.foreground)),
        )
        .child(
            view()
                .label("identity-server-profiles")
                .flex_1()
                .min_w_0()
                .flex_row()
                .gap(zpx(8.0))
                .cursor_pointer()
                .on_press(move || profiles_action.active_dialog.set(ActiveDialog::Profiles))
                .child(
                    text(name)
                        .text_xs()
                        .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                )
                .child(
                    text(url)
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                ),
        )
        .child(icon_button(
            "open-overview",
            t!("overview.title"),
            Glyph::Monitor,
            overview.get(),
            resolved,
            move || overview.set(true),
        ))
        .child(icon_button(
            "identity-new-terminal",
            t!("sidebar.newTerminal"),
            Glyph::Plus,
            false,
            resolved,
            move || {
                create.create_terminal_in_active_project();
            },
        ))
        .child(icon_button(
            "identity-palette",
            t!("palette.title"),
            Glyph::Search,
            false,
            resolved,
            move || palette.active_dialog.set(ActiveDialog::Palette),
        ))
}
