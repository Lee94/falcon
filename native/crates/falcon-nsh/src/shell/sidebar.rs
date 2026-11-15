//! 与 GPUI 共用宿主机 → 项目 → 检出 → 会话的分组规则；会话默认收起。
use super::widgets::{Glyph, glyph, icon_button};
use crate::i18n::t;
use crate::store::{ActiveDialog, ProjectRecord, SessionRecord, SettingsTab, WorkspaceStore};
use crate::theme::{rgb_to_hsla, rgba_to_hsla};
use crate::zoom::zpx;
use falcon_core::project_tree::{ServerKind, checkout_label, folder_key, group_servers};
use falcon_proto::{
    ProgramState, Project, ProjectType, SessionState, SshAuthMethod, SshHost, WorktreeInfo,
};
use nsh_core::prelude::*;

// 线上保留完整协议对象；离线示例和尚未提交的本地表单摘要也经过相同的树分组函数。
fn project_for_tree(p: &ProjectRecord) -> Project {
    p.source.clone().unwrap_or_else(|| Project {
        id: p.id.clone(),
        name: p.name.clone(),
        project_type: if p.host_id.is_some() || p.host_name.is_some() {
            ProjectType::Ssh
        } else {
            ProjectType::Local
        },
        working_dir: Some(p.path.clone()),
        shell: None,
        ssh: None,
        host_id: p.host_id.clone(),
        worktree: p.derived_from.as_ref().map(|id| WorktreeInfo {
            source_project_id: id.clone(),
            branch: p.branch.clone().unwrap_or_default(),
            repo_dir: p.path.clone(),
            created_by_falcon: false,
            archived_at: None,
        }),
        multi: None,
        default_worktree_branch: None,
        created_at: 0,
    })
}

pub fn sidebar_island(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let projects: Vec<_> = store.projects.get().iter().map(project_for_tree).collect();
    let hosts: Vec<_> = store
        .hosts
        .get()
        .iter()
        .map(|h| {
            h.source.clone().unwrap_or_else(|| SshHost {
                id: h.id.clone(),
                name: h.name.clone(),
                host: h.destination.clone(),
                port: 22,
                username: String::new(),
                auth_method: SshAuthMethod::Agent,
                key_path: None,
                has_secret: false,
                project_count: 0,
                created_at: 0,
            })
        })
        .collect();
    let sessions = store.sessions.get();
    let collapsed = store.collapsed.get();
    let mut tree = view().flex_col().p(zpx(8.0)).gap(zpx(2.0));
    for group in group_servers(&projects, &hosts, &t!("forward.local"), false) {
        let open = !collapsed.get(&group.key).copied().unwrap_or(false);
        let st = store.clone();
        let key = group.key.clone();
        let new_project = store.clone();
        let mut block = view().flex_col().gap(zpx(2.0)).child(
            view()
                .h(zpx(30.0))
                .flex_row()
                .items_center()
                .gap(zpx(4.0))
                .rounded(zpx(8.0))
                .child(
                    view()
                        .label(format!("tree-{}", group.key))
                        .tooltip(nsh_base::tooltip().child(text(
                            group.conn.clone().unwrap_or_else(|| group.name.clone()),
                        )))
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex_row()
                        .items_center()
                        .gap(zpx(6.0))
                        .cursor_pointer()
                        .text_color(rgb_to_hsla(ui.muted_foreground))
                        .hover(|s| s.bg(rgb_to_hsla(ui.muted)))
                        .on_press(move || {
                            st.collapsed.update(|m| {
                                m.insert(key.clone(), open);
                            })
                        })
                        .child(glyph(if open {
                            Glyph::ChevronDown
                        } else {
                            Glyph::ChevronRight
                        }))
                        .child(glyph(if group.kind == ServerKind::Local {
                            Glyph::Monitor
                        } else {
                            Glyph::Server
                        }))
                        .child(
                            text(group.name.clone())
                                .text_xs()
                                .number_of_lines(1)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                )
                .child(icon_button(
                    format!("new-project-{}", group.key),
                    t!("sidebar.newProject"),
                    Glyph::Plus,
                    false,
                    resolved,
                    move || new_project.active_dialog.set(ActiveDialog::NewProject),
                )),
        );
        if open {
            if let Some(id) = group.standalone_id.as_ref().filter(|id| {
                sessions.iter().any(|s| &s.project_id == *id)
                    || store.active_project_id.get() == **id
            }) {
                block = block.child(checkout_row(
                    store,
                    resolved,
                    id,
                    &t!("sidebar.standalone"),
                    None,
                    1,
                ));
            }
            if group.folders.is_empty() {
                block = block.child(
                    text(t!("sidebar.emptyServer"))
                        .text_xs()
                        .pl(zpx(24.0))
                        .py(zpx(6.0))
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                );
            }
            for folder in group.folders {
                let p = &folder.project;
                let k = folder_key(&p.id);
                let expanded = !collapsed.get(&k).copied().unwrap_or(false);
                let st = store.clone();
                let pid = p.id.clone();
                let toggle = store.clone();
                let mut folder_view = view().flex_col().gap(zpx(2.0)).child(
                    view()
                        .h(zpx(30.0))
                        .flex_row()
                        .items_center()
                        .gap(zpx(4.0))
                        .pl(zpx(12.0))
                        .rounded(zpx(8.0))
                        .text_color(rgb_to_hsla(ui.muted_foreground))
                        .child(
                            view()
                                .label(format!("folder-toggle-{}", p.id))
                                .w(zpx(18.0))
                                .cursor_pointer()
                                .on_press(move || {
                                    toggle.collapsed.update(|m| {
                                        m.insert(k.clone(), expanded);
                                    })
                                })
                                .child(glyph(if expanded {
                                    Glyph::ChevronDown
                                } else {
                                    Glyph::ChevronRight
                                })),
                        )
                        .child(
                            view()
                                .label(format!("project-{}", p.id))
                                .tooltip(
                                    nsh_base::tooltip()
                                        .child(text(p.working_dir.clone().unwrap_or_default())),
                                )
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .flex_row()
                                .items_center()
                                .gap(zpx(6.0))
                                .cursor_pointer()
                                .on_press(move || st.select_project(&pid))
                                .child(glyph(Glyph::Folder))
                                .child(
                                    text(p.name.clone())
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .number_of_lines(1)
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                ),
                        ),
                );
                if expanded {
                    let branch = store
                        .projects
                        .get()
                        .iter()
                        .find(|r| r.id == p.id)
                        .and_then(|r| r.branch.clone());
                    let label = branch.unwrap_or_else(|| checkout_label(p, None));
                    folder_view = folder_view.child(checkout_row(
                        store,
                        resolved,
                        &p.id,
                        if p.multi.is_some() { &p.name } else { &label },
                        None,
                        2,
                    ));
                    for wt in folder.worktrees {
                        let source = t!("native.nsh.sourceProjectPrefix", name = p.name.as_str());
                        folder_view = folder_view.child(checkout_row(
                            store,
                            resolved,
                            &wt.id,
                            &checkout_label(&wt, None),
                            Some(source),
                            3,
                        ));
                    }
                }
                block = block.child(folder_view);
            }
        }
        tree = tree.child(block);
    }
    let settings = store.clone();
    let collapse = store.clone();
    let settings_label = store.clone();
    view()
        .label("sidebar-island")
        .test_id("sidebar-island")
        .w(zpx(store.sidebar_width.get()))
        .flex_none()
        .h_full()
        .flex_col()
        .rounded(zpx(12.0))
        .overflow_hidden()
        .bg(rgb_to_hsla(ui.background))
        .border_1()
        .border_color(rgba_to_hsla(ui.app_border))
        .child(scroll().flex_1().min_h_0().child(tree))
        .child(
            view()
                .h(zpx(40.0))
                .flex_none()
                .flex_row()
                .items_center()
                .px(zpx(8.0))
                .gap(zpx(4.0))
                .child(icon_button(
                    "sidebar-settings",
                    t!("sidebar.settings"),
                    Glyph::Settings,
                    false,
                    resolved,
                    move || {
                        settings
                            .active_dialog
                            .set(ActiveDialog::Settings(SettingsTab::Appearance))
                    },
                ))
                .child(
                    view()
                        .flex_1()
                        .min_w_0()
                        .cursor_pointer()
                        .label("sidebar-settings-label")
                        .tooltip(nsh_base::tooltip().child(text(store.server_url.get())))
                        .on_press(move || {
                            settings_label
                                .active_dialog
                                .set(ActiveDialog::Settings(SettingsTab::Appearance))
                        })
                        .child(
                            text(t!("sidebar.settings"))
                                .text_xs()
                                .text_color(rgb_to_hsla(if store.server_connected.get() {
                                    ui.muted_foreground
                                } else {
                                    ui.warning
                                })),
                        ),
                )
                .child(icon_button(
                    "sidebar-collapse",
                    t!("sidebar.collapse"),
                    Glyph::Sidebar,
                    false,
                    resolved,
                    move || collapse.sidebar_open.set(false),
                )),
        )
}

fn status_color(s: &SessionRecord, ui: &falcon_theme::UiTokens) -> falcon_theme::Rgb {
    if s.state == SessionState::Dead {
        return ui.destructive;
    }
    if s.state == SessionState::Unverified {
        return ui.warning;
    }
    match s.program_status.as_ref().map(|s| s.0) {
        Some(ProgramState::Blocked) => ui.warning,
        Some(ProgramState::Error) => ui.destructive,
        Some(ProgramState::Done) => ui.success,
        Some(ProgramState::Working) => ui.tint_strong,
        _ => ui.muted_foreground,
    }
}

fn checkout_row(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    id: &str,
    name: &str,
    hint: Option<String>,
    depth: usize,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let sessions: Vec<_> = store
        .sessions
        .get()
        .into_iter()
        .filter(|s| s.project_id == id)
        .collect();
    let expanded = store.sessions_open.get().get(id).copied().unwrap_or(false);
    let selected = store.active_project_id.get() == id && store.focused_pane_key.get().is_empty();
    let st = store.clone();
    let pid = id.to_string();
    let select = store.clone();
    let select_id = id.to_string();
    let create = store.clone();
    let create_id = id.to_string();
    let mut block = view().flex_col().gap(zpx(2.0)).child(
        view()
            .h(zpx(30.0))
            .flex_row()
            .items_center()
            .gap(zpx(4.0))
            .pl(zpx(4.0 + depth as f32 * 12.0))
            .pr(zpx(4.0))
            .rounded(zpx(8.0))
            .text_color(rgb_to_hsla(ui.muted_foreground))
            .when(selected, |v| v.bg(rgb_to_hsla(ui.tint)))
            .child(
                view()
                    .label(format!("sessions-toggle-{id}"))
                    .tooltip(nsh_base::tooltip().child(text(t!(
                        "sidebar.sessionCount",
                        n = sessions.len().to_string().as_str()
                    ))))
                    .w(zpx(18.0))
                    .cursor_pointer()
                    .on_press(move || {
                        st.sessions_open.update(|m| {
                            m.insert(pid.clone(), !expanded);
                        })
                    })
                    .child(glyph(if expanded {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronRight
                    })),
            )
            .child(
                view()
                    .label(format!("checkout-{id}"))
                    .tooltip(
                        nsh_base::tooltip().child(text(hint.unwrap_or_else(|| name.to_string()))),
                    )
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.0))
                    .cursor_pointer()
                    .on_press(move || select.select_project(&select_id))
                    .child(glyph(Glyph::Branch))
                    .child(
                        text(name.to_string())
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .number_of_lines(1)
                            .text_color(rgb_to_hsla(if selected {
                                ui.tint_foreground
                            } else {
                                ui.foreground
                            })),
                    ),
            )
            .child(
                text(sessions.len().to_string())
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
            // 折叠时仍显出最需要关注的会话状态。
            .when(
                sessions
                    .iter()
                    .any(|s| s.state != SessionState::Active || s.program_status.is_some()),
                |v| {
                    let worst = sessions
                        .iter()
                        .max_by_key(
                            |s| match (s.state, s.program_status.as_ref().map(|s| s.0)) {
                                (SessionState::Dead, _) | (_, Some(ProgramState::Error)) => 5,
                                (_, Some(ProgramState::Blocked)) => 4,
                                (SessionState::Unverified, _) => 3,
                                (_, Some(ProgramState::Working)) => 2,
                                _ => 1,
                            },
                        )
                        .unwrap();
                    v.child(
                        view()
                            .w(zpx(5.0))
                            .h(zpx(5.0))
                            .rounded_full()
                            .bg(rgb_to_hsla(status_color(worst, ui))),
                    )
                },
            )
            .child(icon_button(
                format!("new-terminal-{id}"),
                t!("sidebar.newTerminal"),
                Glyph::Plus,
                false,
                resolved,
                move || {
                    create.active_project_id.set(create_id.clone());
                    create.create_terminal_in_active_project();
                },
            )),
    );
    if expanded {
        for session in sessions {
            let st = store.clone();
            let sid = session.id.clone();
            let focused = store.focused_pane_key.get() == falcon_core::pane_key::term_key(&sid);
            let attached = store.is_session_attached(&sid);
            let mut row = view()
                .label(format!("session-{sid}"))
                .h(zpx(28.0))
                .flex_row()
                .items_center()
                .gap(zpx(6.0))
                .pl(zpx(16.0 + depth as f32 * 12.0))
                .pr(zpx(6.0))
                .rounded(zpx(8.0))
                .cursor_pointer()
                .when(focused, |v| v.bg(rgb_to_hsla(ui.tint)))
                .hover(|s| s.bg(rgb_to_hsla(ui.muted)))
                .tooltip(
                    nsh_base::tooltip().child(text(
                        session
                            .program_status
                            .as_ref()
                            .map(|s| s.1.clone())
                            .unwrap_or_else(|| session.cwd.clone()),
                    )),
                )
                .on_press(move || st.focus_session(&sid))
                .child(
                    view()
                        .w(zpx(5.0))
                        .h(zpx(5.0))
                        .rounded_full()
                        .bg(rgb_to_hsla(status_color(&session, ui))),
                )
                .child(
                    text(session.display_title())
                        .flex_1()
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(ui.foreground)),
                );
            if !attached {
                let st = store.clone();
                let sid = session.id.clone();
                row = row.child(icon_button(
                    format!("reattach-{}", session.id),
                    t!("native.nsh.reattachSession"),
                    Glyph::Terminal,
                    false,
                    resolved,
                    move || st.focus_session(&sid),
                ));
            }
            if session.state == SessionState::Dead {
                let st = store.clone();
                let sid = session.id.clone();
                row = row.child(icon_button(
                    format!("clear-session-record-{}", session.id),
                    t!("session.clearRecord"),
                    Glyph::Trash,
                    false,
                    resolved,
                    move || st.request_clear_session_record(&sid),
                ));
            } else {
                let st = store.clone();
                let sid = session.id.clone();
                row = row.child(icon_button(
                    format!("rename-session-{}", session.id),
                    t!("session.rename"),
                    Glyph::Edit,
                    false,
                    resolved,
                    move || st.request_rename_session(&sid),
                ));
            }
            block = block.child(row);
        }
    }
    block
}
