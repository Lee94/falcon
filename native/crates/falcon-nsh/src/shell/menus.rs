use crate::{
    i18n::t,
    store::{ActiveDialog, SessionRecord, SettingsTab, WorkspaceStore},
    shell::popups::PopupAction,
};
use falcon_core::pane_key::term_key;
use falcon_proto::SessionState;
use nsh_core::prelude::*;
use url::Url;

pub(crate) fn project_menu_actions(store: &WorkspaceStore, project_id: &str) -> Vec<PopupAction> {
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
    let mut actions = vec![PopupAction::new(t!("session.open"), false, false, move || {
        select.select_project(&id);
    })];

    let id = project.id.clone();
    let create = store.clone();
    actions.push(PopupAction::new(
        t!("sidebar.newTerminal"),
        false,
        false,
        move || {
            create.select_project(&id);
            create.create_terminal_in_active_project();
        },
    ));

    if let Some(path) = project_path(&project) {
        let copy = store.clone();
        let copied_path = path.clone();
        actions.push(PopupAction::new(t!("files.copyPath"), false, false, move || {
            set_clipboard(copied_path.clone());
            copy.show_toast(t!("files.copiedPath"));
        }));

        let base = store.server_url.get();
        let id = project.id.clone();
        let target = format!("{}{}", base.trim_end_matches('/'), falcon_core::px0::px0_base_path(&id));
        if Url::parse(&target).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some()
        }) {
            actions.push(PopupAction::new(t!("project.px0Review"), false, false, move || {
                let _ = open_url(target.clone());
            }));
        }
    }

    actions
}

pub(crate) fn session_menu_actions(
    store: &WorkspaceStore,
    session: &SessionRecord,
) -> Vec<PopupAction> {
    let mut actions = Vec::new();
    let dead = session.state == SessionState::Dead;
    let attached = store.is_session_attached(&session.id);

    if !dead {
        let st = store.clone();
        let id = session.id.clone();
        actions.push(PopupAction::new(
            if attached {
                t!("session.open")
            } else {
                t!("session.reattachMenu")
            },
            false,
            false,
            move || st.focus_session(&id),
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
        actions.push(PopupAction::new(t!("native.nsh.DetachWindow"), false, false, move || {
            st.detach_pane(&pane);
        }));
    }

    if !dead {
        let st = store.clone();
        let id = session.id.clone();
        actions.push(PopupAction::new(t!("session.terminate"), false, true, move || {
            st.request_terminate_session(&id);
        }));
    }

    actions
}

pub(crate) fn host_menu_actions(
    store: &WorkspaceStore,
    host_id: Option<&str>,
) -> Vec<PopupAction> {
    let mut actions = Vec::new();
    if let Some(host_id) = host_id {
        let st = store.clone();
        let id = host_id.to_string();
        actions.push(PopupAction::new(t!("host.test"), false, false, move || {
            st.test_host_connection(&id);
        }));
    }
    let st = store.clone();
    actions.push(PopupAction::new(
        t!("settings.tab_hosts"),
        false,
        false,
        move || st.active_dialog.set(ActiveDialog::Settings(SettingsTab::Hosts)),
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
