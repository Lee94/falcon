use std::cell::Cell;
use std::rc::Rc;

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::ApiError;
use falcon_proto::{
    MultiRepoProbe, MultiWorktreeInput, ProjectInput, RepoInfo, WorktreeFailure,
    WorktreeInput,
};
#[cfg(not(target_arch = "wasm32"))]
use falcon_proto::{MultiDeriveError, Project};
use nsh_core::element::reactive::Owner;
use nsh_core::prelude::Signal;

use crate::i18n::t;
use crate::store::WorkspaceStore;
#[cfg(not(target_arch = "wasm32"))]
use crate::store::ProjectRecord as WorkspaceProjectRecord;

#[derive(Clone, Copy)]
pub struct FormOperationContext {
    pub(crate) owner: Owner,
    incarnation: u64,
    server_generation: u64,
}

pub struct ProjectOperations {
    task_owner: Owner,
    connection_generation: Rc<Cell<u64>>,
    form_incarnation: Cell<u64>,
}

enum WorktreeRequest {
    Single(WorktreeInput),
    Multi(MultiWorktreeInput),
}

impl ProjectOperations {
    pub fn new(task_owner: Owner, connection_generation: Rc<Cell<u64>>) -> Self {
        Self {
            task_owner,
            connection_generation,
            form_incarnation: Cell::new(0),
        }
    }

    pub fn begin_form(&self) -> FormOperationContext {
        let incarnation = self.bump_form_incarnation();
        FormOperationContext {
            owner: Owner::current(),
            incarnation,
            server_generation: self.connection_generation.get(),
        }
    }

    pub fn is_current(&self, context: FormOperationContext) -> bool {
        self.server_is_current(context)
            && context.owner.is_live()
            && context.incarnation == self.form_incarnation.get()
    }

    fn server_is_current(&self, context: FormOperationContext) -> bool {
        self.task_owner.is_live()
            && context.server_generation == self.connection_generation.get()
    }

    pub fn close_form(&self, store: &WorkspaceStore, context: FormOperationContext) {
        if self.is_current(context) {
            self.bump_form_incarnation();
            store.active_dialog.set(crate::store::ActiveDialog::None);
        }
    }

    fn bump_form_incarnation(&self) -> u64 {
        let incarnation = self
            .form_incarnation
            .get()
            .checked_add(1)
            .expect("form incarnation exhausted");
        self.form_incarnation.set(incarnation);
        incarnation
    }

    pub fn load_repo_probe(
        &self,
        store: WorkspaceStore,
        context: FormOperationContext,
        project_id: String,
        multi: bool,
        loading: Signal<bool>,
        error: Signal<Option<String>>,
        repo: Signal<Option<RepoInfo>>,
        repos: Signal<Option<MultiRepoProbe>>,
    ) {
        if !self.is_current(context) {
            return;
        }
        repo.set(None);
        repos.set(None);
        loading.set(true);
        error.set(None);

        #[cfg(target_arch = "wasm32")]
        {
            let _ = (store, project_id, multi, repo, repos);
            if self.is_current(context) {
                loading.set(false);
                error.set(Some(t!("worktree.unsupported").to_string()));
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if !store.server_connected.get() {
                loading.set(false);
                error.set(Some(t!("project.connectionRequired").to_string()));
                return;
            }
            let Some(client) = store.client() else {
                loading.set(false);
                error.set(Some(t!("project.connectionRequired").to_string()));
                return;
            };
            let operations = store.project_operations.clone();
            store.spawn_root(async move {
                if multi {
                    let result = client.repo_info_multi(&project_id).await;
                    if !operations.is_current(context) {
                        return;
                    }
                    loading.set(false);
                    match result {
                        Ok(result) => {
                            repos.set(Some(result));
                            error.set(None);
                        }
                        Err(api_error) => error.set(Some(api_error.message)),
                    }
                } else {
                    let result = client.repo_info(&project_id).await;
                    if !operations.is_current(context) {
                        return;
                    }
                    loading.set(false);
                    match result {
                        Ok(result) => {
                            repo.set(Some(result));
                            error.set(None);
                        }
                        Err(api_error) => error.set(Some(api_error.message)),
                    }
                }
            });
        }
    }

    pub fn save_project(
        &self,
        store: WorkspaceStore,
        context: FormOperationContext,
        project_id: Option<String>,
        input: ProjectInput,
        busy: Signal<bool>,
        error: Signal<Option<String>>,
    ) {
        if !self.is_current(context) || busy.get() {
            return;
        }
        busy.set(true);
        error.set(None);

        #[cfg(target_arch = "wasm32")]
        {
            let _ = (store, project_id, input);
            if self.is_current(context) {
                busy.set(false);
                error.set(Some(t!("project.unsupported").to_string()));
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if !store.server_connected.get() {
                busy.set(false);
                error.set(Some(t!("project.connectionRequired").to_string()));
                return;
            }
            let Some(client) = store.client() else {
                busy.set(false);
                error.set(Some(t!("project.connectionRequired").to_string()));
                return;
            };
            let operations = store.project_operations.clone();
            let task_store = store.clone();
            store.spawn_root(async move {
                let response = match project_id.as_deref() {
                    Some(id) => client.update_project(id, &input).await,
                    None => client.create_project(&input).await,
                };
                if !operations.server_is_current(context) {
                    return;
                }
                match response {
                    Ok(project) => {
                        let id = project.id.clone();
                        upsert_project(&task_store, &project);
                        if project_id.is_none() && operations.is_current(context) {
                            task_store.select_project(&id);
                        }
                        if operations.is_current(context) {
                            busy.set(false);
                            error.set(None);
                            operations.close_form(&task_store, context);
                        }
                    }
                    Err(api_error) => {
                        if operations.is_current(context) {
                            busy.set(false);
                            error.set(Some(api_error.message));
                        }
                    }
                }
            });
        }
    }

    pub fn derive_worktree(
        &self,
        store: WorkspaceStore,
        context: FormOperationContext,
        project_id: String,
        input: WorktreeInput,
        busy: Signal<bool>,
        error: Signal<Option<String>>,
    ) {
        self.derive(
            store,
            context,
            project_id,
            WorktreeRequest::Single(input),
            busy,
            error,
            false,
        );
    }

    pub fn derive_multi_worktree(
        &self,
        store: WorkspaceStore,
        context: FormOperationContext,
        project_id: String,
        input: MultiWorktreeInput,
        busy: Signal<bool>,
        error: Signal<Option<String>>,
    ) {
        self.derive(
            store,
            context,
            project_id,
            WorktreeRequest::Multi(input),
            busy,
            error,
            true,
        );
    }

    fn derive(
        &self,
        store: WorkspaceStore,
        context: FormOperationContext,
        project_id: String,
        input: WorktreeRequest,
        busy: Signal<bool>,
        error: Signal<Option<String>>,
        multi: bool,
    ) {
        if !self.is_current(context) || busy.get() {
            return;
        }
        busy.set(true);
        error.set(None);

        #[cfg(target_arch = "wasm32")]
        {
            let _ = (store, project_id, multi);
            match input {
                WorktreeRequest::Single(input) => drop(input),
                WorktreeRequest::Multi(input) => drop(input),
            }
            if self.is_current(context) {
                busy.set(false);
                error.set(Some(t!("worktree.unsupported").to_string()));
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if !store.server_connected.get() {
                busy.set(false);
                error.set(Some(t!("project.connectionRequired").to_string()));
                return;
            }
            let Some(client) = store.client() else {
                busy.set(false);
                error.set(Some(t!("project.connectionRequired").to_string()));
                return;
            };
            let operations = store.project_operations.clone();
            let task_store = store.clone();
            store.spawn_root(async move {
                let input: falcon_client::DeriveInput = match input {
                    WorktreeRequest::Single(input) => input.into(),
                    WorktreeRequest::Multi(input) => input.into(),
                };
                let response = client.create_worktree(&project_id, input).await;
                if !operations.server_is_current(context) {
                    return;
                }
                match response {
                    Ok(project) => {
                        let id = project.id.clone();
                        upsert_project(&task_store, &project);
                        if operations.is_current(context) {
                            task_store.select_project(&id);
                            busy.set(false);
                            error.set(None);
                            operations.close_form(&task_store, context);
                        }
                    }
                    Err(api_error) => {
                        if operations.is_current(context) {
                            busy.set(false);
                            error.set(Some(worktree_error(&api_error, multi)));
                        }
                    }
                }
            });
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn upsert_project(store: &WorkspaceStore, project: &Project) {
    let record = project_record(project);
    store.projects.update(|projects| {
        if let Some(existing) = projects.iter_mut().find(|p| p.id == record.id) {
            *existing = record.clone();
        } else {
            projects.push(record);
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn project_record(project: &Project) -> WorkspaceProjectRecord {
    WorkspaceProjectRecord {
        id: project.id.clone(),
        name: project.name.clone(),
        path: project.working_dir.clone().unwrap_or_default(),
        host_id: project.host_id.clone(),
        host_name: project.ssh.as_ref().map(|ssh| ssh.host.clone()),
        derived_from: project.worktree.as_ref().map(|worktree| worktree.source_project_id.clone()),
        branch: project.worktree.as_ref().map(|worktree| worktree.branch.clone()),
        source: Some(project.clone()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn worktree_error(error: &ApiError, multi: bool) -> String {
    let mut message = if error.is_conflict() {
        format!("{}: {}", t!("worktree.conflict"), error.message)
    } else {
        format!("{}: {}", t!("worktree.createFailed"), error.message)
    };
    if multi && let Some(detail) = error.body_as::<MultiDeriveError>() {
        if let Some(member) = detail.member {
            let reason = worktree_reason(&member.reason);
            message.push_str(&format!("\n{}: {} ({reason})", t!("multi.failedMember"), member.dir));
            if let Some(detail) = member.detail {
                message.push_str(&format!(" — {detail}"));
            }
        }
        if let Some(leftovers) = detail.leftover.filter(|paths| !paths.is_empty()) {
            message.push_str(&format!("\n{}: {}", t!("multi.leftovers"), leftovers.join(", ")));
        }
    }
    message
}

pub fn worktree_reason(reason: &WorktreeFailure) -> String {
    match reason {
        WorktreeFailure::GitMissing => t!("worktree.reason_git_missing"),
        WorktreeFailure::NotARepo => t!("worktree.reason_not_a_repo"),
        WorktreeFailure::NoWorkingDir => t!("worktree.reason_no_working_dir"),
        WorktreeFailure::BranchInUse => t!("worktree.reason_branch_in_use"),
        WorktreeFailure::BranchExists => t!("worktree.reason_branch_exists"),
        WorktreeFailure::BranchUnknown => t!("worktree.reason_branch_unknown"),
        WorktreeFailure::PathOccupied => t!("worktree.reason_path_occupied"),
        WorktreeFailure::PathInsideRepo => t!("worktree.reason_path_inside_repo"),
        WorktreeFailure::PathTooLong => t!("worktree.reason_path_too_long"),
        WorktreeFailure::WorktreeAddFailed => t!("worktree.reason_worktree_add_failed"),
        WorktreeFailure::LinkFailed => t!("worktree.reason_link_failed"),
        _ => t!("worktree.reason_unknown"),
    }
}
