use crate::{
    i18n::t,
    shell::popups::{PopupAction, ServerUrlGuard},
    store::{ActiveDialog, SessionRecord, SettingsTab, WorkspaceStore},
};
use falcon_core::editor::{Editor, EditorTarget, editor_target, editor_url};
use falcon_core::pane_key::term_key;
use falcon_core::shortcuts::{Command, chord};
use falcon_proto::{CreateSessionRequest, SessionState};
use nsh_core::prelude::*;
use url::Url;

pub(crate) fn project_menu_actions(store: &WorkspaceStore, project_id: &str) -> Vec<PopupAction> {
    let server = ServerUrlGuard::capture(store);
    let Some(project) = store
        .projects
        .get()
        .into_iter()
        .find(|project| project.id == project_id)
    else {
        return Vec::new();
    };

    let source = project.source.as_ref();
    let archived = source
        .and_then(|p| p.worktree.as_ref())
        .is_some_and(|wt| wt.archived_at.is_some());
    if archived {
        let restore = store.clone();
        let id = project.id.clone();
        let delete = store.clone();
        let p = source.expect("archived worktree").clone();
        return vec![
            PopupAction::new(
                server.clone(),
                t!("worktree.restore"),
                false,
                false,
                move || restore.restore_menu_project(&id),
            ),
            PopupAction::new(server, t!("worktree.deleteNow"), false, true, move || {
                delete.request_project_action(&p, false)
            })
            .sep(),
        ];
    }
    let mut actions = new_session_actions(store, &project.id);

    let copy = store.clone();
    let conn_project = source.cloned();
    actions.push(
        PopupAction::new(
            server.clone(),
            t!("session.copyConn"),
            false,
            false,
            move || {
                copy_connection(&copy, conn_project.as_ref());
            },
        )
        .sep(),
    );

    let id = project.id.clone();
    let edit = store.clone();
    actions.push(PopupAction::new(
        server.clone(),
        t!("sidebar.editProject"),
        false,
        false,
        move || {
            edit.select_project(&id);
            edit.active_dialog
                .set(ActiveDialog::EditProject(id.clone()));
        },
    ));

    if project.derived_from.is_none() {
        let id = project.id.clone();
        let derive = store.clone();
        actions.push(PopupAction::new(
            server.clone(),
            if source.is_some_and(|p| p.multi.is_some()) {
                t!("multi.derive")
            } else {
                t!("project.derive")
            },
            false,
            false,
            move || {
                derive.select_project(&id);
                derive
                    .active_dialog
                    .set(ActiveDialog::DeriveProject(id.clone()));
            },
        ));
        let archived_count = store
            .projects
            .get()
            .iter()
            .filter(|p| {
                p.source
                    .as_ref()
                    .and_then(|p| p.worktree.as_ref())
                    .is_some_and(|wt| {
                        wt.source_project_id == project.id && wt.archived_at.is_some()
                    })
            })
            .count();
        if archived_count > 0 || store.show_archived.get() {
            let toggle = store.clone();
            actions.push(
                PopupAction::new(
                    server.clone(),
                    t!("worktree.showArchived", n = archived_count),
                    false,
                    false,
                    move || toggle.show_archived.update(|value| *value = !*value),
                )
                .checked(store.show_archived.get()),
            );
        }
    }

    if let Some(path) = project_path(&project) {
        let copy = store.clone();
        let copied_path = path.clone();
        actions.push(PopupAction::new(
            server.clone(),
            t!("files.copyPath"),
            false,
            false,
            move || {
                set_clipboard(copied_path.clone());
                copy.show_toast(t!("files.copiedPath"));
            },
        ));

        let id = project.id.clone();
        let target = format!(
            "{}{}",
            server.origin_url(),
            falcon_core::px0::px0_base_path(&id)
        );
        if Url::parse(&target)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        {
            actions.push(PopupAction::new(
                server.clone(),
                t!("project.px0Review"),
                false,
                false,
                move || {
                    let _ = open_url(target.clone());
                },
            ));
        }
    }

    if let Some(source) = source {
        actions.extend(editor_items(store, source));
        if source.worktree.is_some() {
            let archive = store.clone();
            let p = source.clone();
            actions.push(
                PopupAction::new(
                    server.clone(),
                    t!("worktree.archive"),
                    false,
                    false,
                    move || archive.request_project_action(&p, true),
                )
                .sep(),
            );
        }
        let delete = store.clone();
        let p = source.clone();
        actions.push(
            PopupAction::new(
                server.clone(),
                if source.worktree.is_some() {
                    t!("worktree.deleteConfirm")
                } else {
                    t!("project.delete")
                },
                false,
                true,
                move || delete.request_project_action(&p, false),
            )
            .sep(),
        );
    }
    let filter = store.clone();
    let id = project.id.clone();
    actions.push(
        PopupAction::new(
            server,
            t!("project.filterInOverview"),
            false,
            false,
            move || {
                filter.overview_project.set(Some(id.clone()));
                filter.overview_filter.set(None);
                filter.overview_open.set(true);
            },
        )
        .sep(),
    );

    actions
}

pub(crate) fn session_menu_actions(
    store: &WorkspaceStore,
    session: &SessionRecord,
) -> Vec<PopupAction> {
    let server = ServerUrlGuard::capture(store);
    let mut actions = Vec::new();
    let dead = session.state == SessionState::Dead;
    let attached = store.is_session_attached(&session.id);

    if !dead {
        let st = store.clone();
        let id = session.id.clone();
        let open = move || st.focus_palette_session(&id);
        actions.push(
            PopupAction::new(
                server.clone(),
                if attached {
                    t!("session.open")
                } else {
                    t!("session.reattachMenu")
                },
                false,
                false,
                open,
            )
            .kbd(if session.state == SessionState::Unverified {
                chord(Command::Reattach, apple_keys())
            } else {
                String::new()
            }),
        );
    }

    let has_project = store
        .projects
        .get()
        .iter()
        .any(|project| project.id == session.project_id);
    if has_project {
        let st = store.clone();
        let project = store
            .projects
            .get()
            .into_iter()
            .find(|p| p.id == session.project_id)
            .and_then(|p| p.source);
        actions.push(PopupAction::new(
            server.clone(),
            t!("session.copyConn"),
            false,
            false,
            move || copy_connection(&st, project.as_ref()),
        ));
        let st = store.clone();
        let project_id = session.project_id.clone();
        actions.push(PopupAction::new(
            server.clone(),
            t!("session.duplicate"),
            false,
            false,
            move || {
                st.select_project(&project_id);
                st.create_terminal_in_active_project();
            },
        ));
    }

    if attached {
        let st = store.clone();
        let pane = term_key(&session.id);
        actions.push(PopupAction::new(
            server.clone(),
            t!("native.nsh.DetachWindow"),
            false,
            false,
            move || st.detach_pane(&pane),
        ));
    }

    if !dead {
        let st = store.clone();
        let id = session.id.clone();
        actions.push(PopupAction::new(
            server.clone(),
            t!("session.rename"),
            false,
            false,
            move || st.request_rename_session(&id),
        ));
        let st = store.clone();
        let id = session.id.clone();
        actions.push(PopupAction::new(
            server.clone(),
            t!("session.terminate"),
            false,
            true,
            move || st.request_terminate_session(&id),
        ));
    } else {
        let st = store.clone();
        let id = session.id.clone();
        actions.push(PopupAction::new(
            server.clone(),
            t!("session.clearRecord"),
            false,
            true,
            move || st.request_clear_session_record(&id),
        ));
    }

    actions
}

pub(crate) fn host_menu_actions(store: &WorkspaceStore, host_id: Option<&str>) -> Vec<PopupAction> {
    let server = ServerUrlGuard::capture(store);
    let mut actions = server_menu_actions(store, host_id.is_none(), host_id);
    if let Some(host_id) = host_id {
        let st = store.clone();
        let id = host_id.to_string();
        actions.push(PopupAction::new(
            server.clone(),
            t!("host.test"),
            false,
            false,
            move || st.test_host_connection(&id),
        ));
        if let Some(host) = store
            .hosts
            .get()
            .into_iter()
            .find(|host| host.id == host_id)
            .and_then(|host| host.source)
        {
            let st = store.clone();
            let edit_host = host.clone();
            actions.push(PopupAction::new(
                server.clone(),
                t!("host.edit"),
                false,
                false,
                move || {
                    st.begin_host_edit(edit_host.clone());
                    st.active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Hosts));
                },
            ));

            let st = store.clone();
            let delete_host = host.id.clone();
            let host_name = host.name.clone();
            let project_count = host.project_count;
            actions.push(PopupAction::new(
                server.clone(),
                t!("host.delete"),
                false,
                true,
                move || {
                    st.host_delete_error.set(None);
                    st.active_dialog.set(ActiveDialog::ConfirmDeleteHost {
                        host_id: delete_host.clone(),
                        host_name: host_name.clone(),
                        project_count,
                    });
                },
            ));
        }
    }
    let st = store.clone();
    actions.push(PopupAction::new(
        server,
        t!("settings.tab_hosts"),
        false,
        false,
        move || {
            st.active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Hosts))
        },
    ));
    actions
}

fn project_path(project: &crate::store::ProjectRecord) -> Option<String> {
    let source = project.source.as_ref();
    source
        .and_then(|project| {
            project
                .working_dir
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(ToOwned::to_owned)
                .or_else(|| {
                    project.multi.as_ref().and_then(|multi| {
                        multi.repos.iter().find_map(|repo| {
                            (!repo.dir.trim().is_empty()).then(|| repo.dir.clone())
                        })
                    })
                })
        })
        .or_else(|| (!project.path.trim().is_empty()).then(|| project.path.clone()))
}

pub(crate) fn new_session_actions(store: &WorkspaceStore, project_id: &str) -> Vec<PopupAction> {
    let server = ServerUrlGuard::capture(store);
    let create = store.clone();
    let id = project_id.to_string();
    let mut items = vec![
        PopupAction::new(
            server.clone(),
            t!("agent.terminal"),
            false,
            false,
            move || {
                create.select_project(&id);
                create.create_terminal_in_active_project();
            },
        )
        .kbd(chord(Command::NewTerminal, apple_keys()))
        .session_icon(None),
    ];
    let agents = store
        .project_agents
        .get()
        .get(project_id)
        .cloned()
        .unwrap_or_else(|| {
            if store.is_offline_demo() && store.active_project_id.get() == project_id {
                store
                    .detected_agents
                    .get()
                    .iter()
                    .filter_map(|agent| {
                        serde_json::from_value(serde_json::Value::String(agent.clone())).ok()
                    })
                    .collect()
            } else {
                Vec::new()
            }
        });
    for (index, agent) in agents.into_iter().enumerate() {
        let create = store.clone();
        let id = project_id.to_string();
        let item = PopupAction::new(
            server.clone(),
            t!(&format!("agent.{}", agent.as_str())),
            false,
            false,
            move || {
                create.select_project(&id);
                create.create_terminal_with_request(
                    CreateSessionRequest {
                        agent: Some(agent),
                        ..Default::default()
                    },
                    crate::store::SessionPlacement::Focus,
                );
            },
        )
        .session_icon(Some(agent));
        items.push(if index == 0 { item.sep() } else { item });
    }
    items
}

fn copy_connection(store: &WorkspaceStore, project: Option<&falcon_proto::Project>) {
    let conn = falcon_core::host_color::conn_label(
        project,
        store.server_info.get().as_ref(),
        &t!("forward.local"),
    );
    set_clipboard(conn);
    store.show_toast(t!("toast.copied"));
}

pub(crate) fn server_menu_actions(
    store: &WorkspaceStore,
    local: bool,
    host_id: Option<&str>,
) -> Vec<PopupAction> {
    let mut items = Vec::new();
    for multi in [false, true] {
        // GPUI does not offer multi-repo projects for unbound legacy SSH connections.
        if multi && !local && host_id.is_none() {
            continue;
        }
        let store = store.clone();
        let server = ServerUrlGuard::capture(&store);
        let preset = crate::menu_operations::ProjectPreset {
            local,
            host_id: host_id.map(str::to_owned),
            multi,
        };
        items.push(PopupAction::new(
            server,
            if multi {
                t!("multi.newProject")
            } else {
                t!("sidebar.newProject")
            },
            false,
            false,
            move || {
                store
                    .active_dialog
                    .set(ActiveDialog::NewProjectPreset(preset.clone()))
            },
        ));
    }
    items
}

pub(crate) fn editor_items(
    store: &WorkspaceStore,
    project: &falcon_proto::Project,
) -> Vec<PopupAction> {
    let local = Url::parse(&store.server_url.get()).ok().is_some_and(|url| {
        url.host_str()
            .is_some_and(|host| matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1"))
    });
    let Some(target) = editor_target(project, local) else {
        return Vec::new();
    };
    Editor::ALL
        .into_iter()
        .map(|editor| {
            let store = store.clone();
            let server = ServerUrlGuard::capture(&store);
            let target = target.clone();
            let id = project.id.clone();
            PopupAction::new(
                server,
                t!("project.openIn", editor = editor.name()),
                false,
                false,
                move || open_in_editor(&store, &id, editor, &target),
            )
        })
        .collect()
}

fn open_in_editor(store: &WorkspaceStore, project_id: &str, editor: Editor, target: &EditorTarget) {
    if let Some(url) = editor_url(editor, target) {
        hand_to_editor(store, editor, url);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(client) = store.client() {
        let probe = match target {
            EditorTarget::Local { .. } => falcon_client::ProbeTarget::Backend,
            EditorTarget::Ssh { .. } => falcon_client::ProbeTarget::Project(project_id.to_string()),
        };
        let future = client.list_dir(Some(target.dir()), &probe);
        let store = store.clone();
        let target = target.clone();
        let server = ServerUrlGuard::capture(&store);
        store.clone().spawn_root(async move {
            let result = future.await;
            if !server.is_current() {
                return;
            }
            match result {
                Ok(listing) => match editor_url(editor, &target.with_dir(listing.path)) {
                    Some(url) => hand_to_editor(&store, editor, url),
                    None => store.show_toast(t!("project.openInFailed", editor = editor.name())),
                },
                Err(err) => store.show_toast(err.to_string()),
            }
        });
        return;
    }
    let _ = project_id;
    store.show_toast(t!("native.nsh.serverNotConnected"));
}

fn hand_to_editor(store: &WorkspaceStore, editor: Editor, url: String) {
    match falcon_platform::shared().open_app_link(&url) {
        Some(true) => {}
        Some(false) => store.show_toast(format!(
            "{}\n{}",
            t!("project.editorMissing", editor = editor.name()),
            t!("project.editorMissingBody", editor = editor.name())
        )),
        None => {
            let answer = open_url(url);
            let store = store.clone();
            let server = ServerUrlGuard::capture(&store);
            store.clone().spawn_root(async move {
                if !answer.await && server.is_current() {
                    store.show_toast(t!("project.editorMissing", editor = editor.name()));
                }
            });
        }
    }
}
