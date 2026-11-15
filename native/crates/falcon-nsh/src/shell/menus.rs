use crate::{
    i18n::t,
    store::{ActiveDialog, SessionRecord, SettingsTab, WorkspaceStore},
    shell::popups::{PopupAction, ServerUrlGuard},
};
use falcon_core::pane_key::term_key;
use falcon_proto::SessionState;
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

    let id = project.id.clone();
    let select = store.clone();
    let mut actions = vec![PopupAction::new(
        server.clone(),
        t!("session.open"),
        false,
        false,
        move || select.select_project(&id),
    )];

    let id = project.id.clone();
    let create = store.clone();
    actions.push(PopupAction::new(
        server.clone(),
        t!("sidebar.newTerminal"),
        false,
        false,
        move || {
            create.select_project(&id);
            create.create_terminal_in_active_project();
        },
    ));

    let id = project.id.clone();
    let edit = store.clone();
    actions.push(PopupAction::new(
        server.clone(),
        t!("sidebar.editProject"),
        false,
        false,
        move || {
            edit.select_project(&id);
            edit.active_dialog.set(ActiveDialog::EditProject(id.clone()));
        },
    ));

    let id = project.id.clone();
    let derive = store.clone();
    actions.push(PopupAction::new(
        server.clone(),
        t!("sidebar.deriveProject"),
        false,
        false,
        move || {
            derive.select_project(&id);
            derive.active_dialog.set(ActiveDialog::DeriveProject(id.clone()));
        },
    ));

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
        if Url::parse(&target).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
        }) {
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
        actions.push(PopupAction::new(
            server.clone(),
            if attached {
                t!("session.open")
            } else {
                t!("session.reattachMenu")
            },
            false,
            false,
            open,
        ));
    }

    let has_project = store
        .projects
        .get()
        .iter()
        .any(|project| project.id == session.project_id);
    if has_project {
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

pub(crate) fn host_menu_actions(
    store: &WorkspaceStore,
    host_id: Option<&str>,
) -> Vec<PopupAction> {
    let server = ServerUrlGuard::capture(store);
    let mut actions = Vec::new();
    // Preserve the existing host actions' keyboard order; append creation presets below.
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
                    st.active_dialog.set(ActiveDialog::Settings(SettingsTab::Hosts));
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
        server.clone(),
        t!("settings.tab_hosts"),
        false,
        false,
        move || st.active_dialog.set(ActiveDialog::Settings(SettingsTab::Hosts)),
    ));
    for multi in [false, true] {
        let st = store.clone();
        let host_id = host_id.map(str::to_owned);
        actions.push(PopupAction::new(server.clone(), if multi { t!("multi.newProject") } else { t!("sidebar.newProject") }, false, false, move || {
            st.active_dialog.set(ActiveDialog::NewProjectPreset { host_id: host_id.clone(), remote: host_id.is_some(), multi });
        }));
    }
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
