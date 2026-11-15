use crate::{
    i18n::t,
    shell::popups::ServerUrlGuard,
    store::{ActiveDialog, ProjectRecord, SessionRecord, WorkspaceStore},
    theme::{rgb_to_hsla, rgba_to_hsla},
    zoom::zpx,
};
use falcon_core::project_form::ProjectFormDraft;
use falcon_proto::{Project, ProjectType, SessionState, WorktreeStatus};
use nsh_core::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPreset {
    pub local: bool,
    pub host_id: Option<String>,
    pub multi: bool,
}

impl ProjectPreset {
    pub(crate) fn draft(self) -> ProjectFormDraft {
        ProjectFormDraft {
            project_type: if self.local {
                ProjectType::Local
            } else {
                ProjectType::Ssh
            },
            host_id: self.host_id,
            multi: self.multi,
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CleanupWarning {
    pub server_url: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmationItem {
    pub name: String,
    pub meta: String,
    pub state: Option<SessionState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectConfirmation {
    pub request_id: u64,
    pub project_id: String,
    pub project_snapshot: String,
    pub affected_snapshot: String,
    pub server_url: String,
    pub server_epoch: u64,
    pub affected_ids: Vec<String>,
    pub archive: bool,
    pub title: String,
    pub body: String,
    pub items: Vec<ConfirmationItem>,
    pub footnote: String,
    pub confirm_label: String,
}

pub(crate) fn affected_projects(project: &Project, projects: &[ProjectRecord]) -> Vec<String> {
    let mut ids = vec![project.id.clone()];
    if project.worktree.is_none() {
        ids.extend(
            projects
                .iter()
                .filter(|p| p.derived_from.as_deref() == Some(&project.id))
                .map(|p| p.id.clone()),
        );
    }
    ids
}

pub(crate) fn build_confirmation(
    request_id: u64,
    project: &Project,
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
    archive: bool,
    statuses: &[(Project, Option<WorktreeStatus>)],
) -> ProjectConfirmation {
    let affected_ids = affected_projects(project, projects);
    let live: Vec<_> = sessions
        .iter()
        .filter(|s| affected_ids.contains(&s.project_id) && s.state != SessionState::Dead)
        .collect();
    let mut items = live
        .iter()
        .map(|s| ConfirmationItem {
            name: s.display_title(),
            meta: s.cwd.clone(),
            state: Some(s.state),
        })
        .collect::<Vec<_>>();
    let dir = project.working_dir.clone().unwrap_or_default();
    let branch = project
        .worktree
        .as_ref()
        .map(|wt| wt.branch.clone())
        .unwrap_or_default();
    let mut foot = Vec::new();
    for (child, status) in statuses {
        items.push(ConfirmationItem {
            name: child
                .working_dir
                .clone()
                .unwrap_or_else(|| child.name.clone()),
            meta: child
                .worktree
                .as_ref()
                .map(|wt| wt.branch.clone())
                .unwrap_or_default(),
            state: None,
        });
        if let Some(status) = status {
            items.extend(status.dirty_sample.iter().map(|path| ConfirmationItem {
                name: path.clone(),
                meta: t!("worktree.dirtyFile"),
                state: None,
            }));
            if status.error.is_some() {
                foot.push(t!("worktree.deleteStatusUnknown"));
            }
            if !status.present {
                foot.push(t!("worktree.deleteMissing"));
            }
            let remaining = (status.dirty_count as usize).saturating_sub(status.dirty_sample.len());
            if remaining > 0 {
                foot.push(t!("worktree.deleteDirtyMore", n = remaining));
            }
            if status.ignored_count > 0 {
                foot.push(t!("worktree.deleteDirtyIgnored", n = status.ignored_count));
            }
            if status.ahead.unwrap_or(0) > 0 {
                foot.push(t!("worktree.deleteAhead", n = status.ahead.unwrap_or(0)));
            }
        } else {
            foot.push(t!("worktree.deleteStatusUnknown"));
        }
    }
    let clean = statuses
        .first()
        .and_then(|(_, s)| s.as_ref())
        .is_some_and(|s| s.error.is_none() && s.dirty_count == 0 && s.ignored_count == 0);
    let mut body = if archive {
        foot.push(t!("worktree.archiveFootnote", dir = dir));
        t!(
            "worktree.archiveBody",
            days = falcon_proto::WORKTREE_ARCHIVE_TTL_DAYS,
            branch = branch
        )
    } else if project.worktree.is_some() {
        if let Some(multi) = &project.multi {
            foot.push(t!("multi.deleteFootnote", n = multi.repos.len()));
        }
        if clean {
            t!("worktree.deleteBodyClean", dir = dir, branch = branch)
        } else {
            t!("worktree.deleteBodyDirty", dir = dir, branch = branch)
        }
    } else {
        if affected_ids.len() > 1 {
            foot.push(t!("worktree.cascadeFootnote", n = affected_ids.len() - 1));
        }
        if project.multi.is_some() {
            foot.push(t!("multi.deleteVirtualDirFootnote"));
        } else if project.project_type == ProjectType::Ssh {
            foot.push(t!("project.deleteFootnote"));
        }
        if live.is_empty() {
            t!("project.deleteBodyEmpty")
        } else {
            t!("project.deleteBody", n = live.len())
        }
    };
    if (archive || project.worktree.is_some()) && !live.is_empty() {
        body.push_str(&format!(
            " {}",
            t!("worktree.deleteBodySessions", n = live.len())
        ));
    }
    ProjectConfirmation {
        request_id,
        project_id: project.id.clone(),
        project_snapshot: serde_json::to_string(project).unwrap_or_default(),
        affected_snapshot: affected_snapshot(&affected_ids, projects, sessions),
        server_url: String::new(),
        server_epoch: 0,
        affected_ids,
        archive,
        title: if archive {
            t!("worktree.archiveTitle", name = project.name.clone())
        } else if project.worktree.is_some() {
            t!("worktree.deleteTitle", name = project.name.clone())
        } else {
            t!("project.deleteTitle", name = project.name.clone())
        },
        body,
        items,
        footnote: foot.join(" · "),
        confirm_label: if archive {
            t!("worktree.archiveConfirm")
        } else if project.worktree.is_some() {
            t!("worktree.deleteConfirm")
        } else {
            t!("project.deleteConfirm")
        },
    }
}

fn affected_snapshot(
    ids: &[String],
    projects: &[ProjectRecord],
    sessions: &[SessionRecord],
) -> String {
    let project_sources: Vec<_> = projects
        .iter()
        .filter(|p| ids.contains(&p.id))
        .map(|p| &p.source)
        .collect();
    let live: Vec<_> = sessions
        .iter()
        .filter(|s| ids.contains(&s.project_id) && s.state != SessionState::Dead)
        .map(|s| (&s.id, &s.project_id))
        .collect();
    serde_json::to_string(&(project_sources, live)).unwrap_or_default()
}

impl WorkspaceStore {
    pub(crate) fn refresh_menu_agents(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self.client() {
            let store = self.clone();
            let server = ServerUrlGuard::capture(self);
            let projects = self.projects.get_untracked();
            let hosts = self.hosts.get_untracked();
            let mut targets = projects
                .iter()
                .filter(|project| !falcon_proto::is_standalone_project_id(&project.id))
                .map(|project| {
                    (
                        project.id.clone(),
                        falcon_client::ProbeTarget::Project(project.id.clone()),
                    )
                })
                .collect::<Vec<_>>();
            targets.push((
                falcon_proto::STANDALONE_LOCAL.to_string(),
                falcon_client::ProbeTarget::Backend,
            ));
            targets.extend(hosts.iter().map(|host| {
                (
                    falcon_proto::standalone_for_host(&host.id),
                    falcon_client::ProbeTarget::SshHost(host.id.clone()),
                )
            }));
            let request = self.menu_agents_request.get_untracked().wrapping_add(1);
            self.menu_agents_request.set(request);
            self.spawn_root(async move {
                let results = futures::future::join_all(targets.into_iter().map(|(id, target)| {
                    let client = client.clone();
                    async move {
                        let agents = client.agents(&target).await.unwrap_or_default();
                        (id, target, agents)
                    }
                }))
                .await;
                if server.is_current() && store.menu_agents_request.get_untracked() == request {
                    let current = store.projects.get_untracked();
                    let current_hosts = store.hosts.get_untracked();
                    store.project_agents.set(
                        results
                            .into_iter()
                            .filter(|(id, target, _)| match target {
                                falcon_client::ProbeTarget::Backend => true,
                                falcon_client::ProbeTarget::SshHost(host_id) => {
                                    hosts.iter().find(|h| &h.id == host_id).is_some_and(|host| {
                                        current_hosts
                                            .iter()
                                            .any(|h| h.id == host.id && h.source == host.source)
                                    })
                                }
                                falcon_client::ProbeTarget::Project(_) => {
                                    projects
                                        .iter()
                                        .find(|p| &p.id == id)
                                        .is_some_and(|project| {
                                            current.iter().any(|p| {
                                                p.id == project.id && p.source == project.source
                                            })
                                        })
                                        && !store.project_operations.update_pending(id)
                                }
                            })
                            .map(|(id, _, agents)| (id, agents))
                            .collect(),
                    );
                }
            });
        }
    }

    pub(crate) fn request_project_action(&self, project: &Project, archive: bool) {
        if self.project_menu_busy.get_untracked() {
            return;
        }
        let ids = affected_projects(project, &self.projects.get_untracked());
        if ids
            .iter()
            .any(|id| self.has_open_file_mapping(id) || self.project_operations.update_pending(id))
        {
            self.show_toast(t!("native.menus.closeProjectFiles"));
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self
            .client()
            .filter(|_| self.server_connected.get_untracked())
        {
            let request_id = self.menu_request_id.get_untracked().wrapping_add(1);
            self.menu_request_id.set(request_id);
            let pending = ActiveDialog::ProjectActionLoading { request_id };
            self.active_dialog.set(pending.clone());
            let project = project.clone();
            let projects = self.projects.get_untracked();
            let store = self.clone();
            let server = ServerUrlGuard::capture(self);
            self.spawn_root(async move {
                let mut statuses = Vec::new();
                for candidate in projects
                    .iter()
                    .filter(|p| ids.contains(&p.id))
                    .filter_map(|p| p.source.clone())
                    .filter(|p| p.worktree.is_some())
                {
                    let status = client.worktree_status(&candidate.id).await.ok();
                    if !server.is_current() || store.active_dialog.get_untracked() != pending {
                        return;
                    }
                    statuses.push((candidate, status));
                }
                if !server.is_current() || store.active_dialog.get_untracked() != pending {
                    return;
                }
                let mut confirmation = build_confirmation(
                    request_id,
                    &project,
                    &projects,
                    &store.sessions.get_untracked(),
                    archive,
                    &statuses,
                );
                confirmation.server_url = server.origin_url().to_string();
                confirmation.server_epoch = store.connection_generation();
                store
                    .active_dialog
                    .set(ActiveDialog::ConfirmProjectAction(confirmation));
            });
            return;
        }
        self.show_toast(t!("native.nsh.serverNotConnected"));
    }

    pub(crate) fn confirm_project_action(&self, confirmation: &ProjectConfirmation) {
        if self.project_menu_busy.get_untracked()
            || self.active_dialog.get_untracked()
                != ActiveDialog::ConfirmProjectAction(confirmation.clone())
        {
            return;
        }
        let current = self
            .projects
            .get_untracked()
            .into_iter()
            .find(|p| p.id == confirmation.project_id)
            .and_then(|p| p.source);
        let ids = current
            .as_ref()
            .map(|p| affected_projects(p, &self.projects.get_untracked()))
            .unwrap_or_default();
        if self.connection_generation() != confirmation.server_epoch
            || self.server_url.get_untracked().trim_end_matches('/') != confirmation.server_url
            || ids != confirmation.affected_ids
            || affected_snapshot(
                &ids,
                &self.projects.get_untracked(),
                &self.sessions.get_untracked(),
            ) != confirmation.affected_snapshot
            || !current.as_ref().is_some_and(|p| {
                serde_json::to_string(p).ok().as_deref() == Some(&confirmation.project_snapshot)
            })
            || confirmation.affected_ids.iter().any(|id| {
                self.has_open_file_mapping(id) || self.project_operations.update_pending(id)
            })
        {
            self.active_dialog.set(ActiveDialog::None);
            self.show_toast(t!("native.menus.projectChanged"));
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self
            .client()
            .filter(|_| self.server_connected.get_untracked())
        {
            self.project_menu_busy.set(true);
            let store = self.clone();
            let server = ServerUrlGuard::capture(self);
            let confirmation = confirmation.clone();
            let project = current.expect("validated project");
            self.spawn_root(async move {
                store.bump_metadata_revision(server.origin_url());
                let result = if confirmation.archive {
                    client
                        .archive_project(&confirmation.project_id)
                        .await
                        .map(|p| (Some(p), Vec::new()))
                } else {
                    client
                        .delete_project(&confirmation.project_id, true)
                        .await
                        .map(|r| (None, r.warnings.unwrap_or_default()))
                };
                store.bump_metadata_revision(server.origin_url());
                if !server.is_current() {
                    return;
                }
                store.project_menu_busy.set(false);
                match result {
                    Ok((archived, warnings)) => {
                        for session in store
                            .sessions
                            .get_untracked()
                            .iter()
                            .filter(|s| confirmation.affected_ids.contains(&s.project_id))
                        {
                            store.detach_pane(&falcon_core::pane_key::term_key(&session.id));
                        }
                        store.sessions.update(|sessions| {
                            sessions.retain(|s| !confirmation.affected_ids.contains(&s.project_id))
                        });
                        if let Some(archived) = archived {
                            store.on_project_saved(&archived, false);
                        } else {
                            store.projects.update(|projects| {
                                projects.retain(|p| !confirmation.affected_ids.contains(&p.id))
                            });
                        }
                        if confirmation
                            .affected_ids
                            .contains(&store.active_project_id.get_untracked())
                        {
                            if let Some(source) = project
                                .worktree
                                .as_ref()
                                .map(|wt| &wt.source_project_id)
                                .filter(|id| {
                                    store.projects.get_untracked().iter().any(|p| &p.id == *id)
                                })
                            {
                                store.select_project(source);
                            } else {
                                store.select_project("");
                                store.overview_open.set(true);
                            }
                        }
                        if store
                            .overview_project
                            .get_untracked()
                            .as_ref()
                            .is_some_and(|id| confirmation.affected_ids.contains(id))
                        {
                            store.overview_project.set(None);
                        }
                        if store.active_dialog.get_untracked()
                            == ActiveDialog::ConfirmProjectAction(confirmation.clone())
                        {
                            store.active_dialog.set(ActiveDialog::None);
                        }
                        store.show_toast(if confirmation.archive {
                            t!("worktree.archivedToast", name = project.name)
                        } else if project.worktree.is_some() {
                            t!("worktree.deleted", name = project.name)
                        } else {
                            t!("project.deleted", name = project.name)
                        });
                        if !warnings.is_empty() {
                            store.project_cleanup_warnings.update(|retained| {
                                retained.extend(warnings.iter().map(|message| CleanupWarning {
                                    server_url: server.origin_url().to_string(),
                                    message: message.clone(),
                                }));
                            });
                            store.show_toast(format!(
                                "{}\n{}",
                                t!("worktree.leftoverTitle"),
                                warnings.join("\n")
                            ));
                        }
                        store.connect_live_server(server.origin_url());
                    }
                    Err(err) => store.show_toast(err.to_string()),
                }
            });
            return;
        }
        self.show_toast(t!("native.nsh.serverNotConnected"));
    }

    pub(crate) fn restore_menu_project(&self, project_id: &str) {
        if self.project_menu_busy.get_untracked() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(client) = self
            .client()
            .filter(|_| self.server_connected.get_untracked())
        {
            self.project_menu_busy.set(true);
            let id = project_id.to_string();
            let store = self.clone();
            let server = ServerUrlGuard::capture(self);
            self.spawn_root(async move {
                store.bump_metadata_revision(server.origin_url());
                let result = client.restore_project(&id).await;
                store.bump_metadata_revision(server.origin_url());
                if !server.is_current() {
                    return;
                }
                store.project_menu_busy.set(false);
                match result {
                    Ok(project) => {
                        store.on_project_saved(&project, false);
                        store.show_toast(t!("worktree.restored", name = project.name));
                    }
                    Err(err) => store.show_toast(err.to_string()),
                }
            });
            return;
        }
        let _ = project_id;
        self.show_toast(t!("native.nsh.serverNotConnected"));
    }
}

pub(crate) fn render_cleanup_warnings(
    store: &WorkspaceStore,
    theme: &falcon_theme::ResolvedTheme,
) -> AnyElement {
    let warnings = store.project_cleanup_warnings.get();
    if warnings.is_empty() {
        return view().into_element();
    }
    let ui = &theme.ui;
    let mut list = view().flex_col().gap(zpx(8.0));
    for warning in &warnings {
        list = list.child(
            view()
                .flex_col()
                .gap(zpx(4.0))
                .child(
                    text(warning.server_url.clone())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(text(warning.message.clone()).text_xs()),
        );
    }
    let dismiss = store.clone();
    view()
        .absolute()
        .left(zpx(16.0))
        .right(zpx(16.0))
        .bottom(zpx(64.0))
        .flex_row()
        .justify_end()
        .child(
            view()
                .label("project-cleanup-warning")
                .max_w(zpx(520.0))
                .p(zpx(12.0))
                .rounded(zpx(8.0))
                .border_1()
                .border_color(rgba_to_hsla(ui.border))
                .bg(rgb_to_hsla(ui.popover))
                .text_color(rgb_to_hsla(ui.popover_foreground))
                .flex_col()
                .gap(zpx(8.0))
                .child(
                    text(t!("worktree.leftoverTitle"))
                        .text_sm()
                        .text_color(rgb_to_hsla(ui.warning)),
                )
                .child(scroll().max_h(zpx(200.0)).child(list))
                .child(
                    view()
                        .flex_row()
                        .gap(zpx(8.0))
                        .justify_end()
                        .child(
                            view()
                                .label("project-cleanup-warning-copy")
                                .cursor_pointer()
                                .on_press(move || {
                                    set_clipboard(
                                        warnings
                                            .iter()
                                            .map(|w| format!("{}\n{}", w.server_url, w.message))
                                            .collect::<Vec<_>>()
                                            .join("\n\n"),
                                    )
                                })
                                .child(text(t!("native.menus.copyWarnings")).text_xs()),
                        )
                        .child(
                            view()
                                .label("project-cleanup-warning-dismiss")
                                .cursor_pointer()
                                .on_press(move || dismiss.project_cleanup_warnings.set(Vec::new()))
                                .child(text(t!("native.menus.dismissWarnings")).text_xs()),
                        ),
                ),
        )
        .into_element()
}

pub(crate) fn render_confirmation(
    store: &WorkspaceStore,
    theme: &falcon_theme::ResolvedTheme,
    confirmation: ProjectConfirmation,
) -> AnyElement {
    let ui = &theme.ui;
    let busy = store.project_menu_busy.get();
    let mut list = view().flex_col().gap(zpx(4.0));
    for item in &confirmation.items {
        let state_label = item.state.map(|state| {
            t!(match state {
                SessionState::Active => "session.state_active",
                SessionState::Unverified => "session.state_unverified",
                SessionState::Dead => "session.state_dead",
                SessionState::Unknown => "session.state_unknown",
            })
        });
        list = list.child(
            view()
                .p(zpx(8.0))
                .rounded(zpx(6.0))
                .bg(rgb_to_hsla(ui.muted))
                .child(text(item.name.clone()).text_xs())
                .child(
                    text(item.meta.clone())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .when_some(state_label, |v, label| {
                    v.child(text(label).text_xs().text_color(rgb_to_hsla(ui.warning)))
                }),
        );
    }
    let cancel = store.clone();
    let confirm = store.clone();
    let label = confirmation.confirm_label.clone();
    let danger = !confirmation.archive;
    view()
        .label("project-action-confirmation")
        .w_full()
        .p(zpx(20.0))
        .flex_col()
        .gap(zpx(14.0))
        .child(text(confirmation.title.clone()).text_lg())
        .child(text(confirmation.body.clone()).text_sm())
        .child(scroll().max_h(zpx(260.0)).child(list))
        .child(
            text(confirmation.footnote.clone())
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(zpx(8.0))
                .child(
                    view()
                        .label("project-action-cancel")
                        .px(zpx(12.0))
                        .py(zpx(8.0))
                        .rounded(zpx(6.0))
                        .border_1()
                        .border_color(rgba_to_hsla(ui.border))
                        .cursor_pointer()
                        .on_press(move || cancel.active_dialog.set(ActiveDialog::None))
                        .child(text(t!("common.cancel")).text_xs()),
                )
                .child(
                    view()
                        .label("project-action-confirm")
                        .px(zpx(12.0))
                        .py(zpx(8.0))
                        .rounded(zpx(6.0))
                        .bg(rgb_to_hsla(if danger { ui.destructive } else { ui.tint }))
                        .text_color(rgb_to_hsla(if danger {
                            ui.destructive_foreground
                        } else {
                            ui.tint_foreground
                        }))
                        .when(busy, |v| v.opacity(0.55))
                        .cursor_pointer()
                        .on_press(move || confirm.confirm_project_action(&confirmation))
                        .child(text(label).text_xs()),
                ),
        )
        .into_element()
}
