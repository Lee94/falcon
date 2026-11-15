use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::ApiErrorKind;
#[cfg(not(target_arch = "wasm32"))]
use falcon_client::ApiError;
use falcon_core::project_form::mapping_changed;
use falcon_proto::{
    MultiRepoProbe, MultiWorktreeInput, Project, ProjectInput, RepoInfo, WorktreeFailure,
    WorktreeInput,
};
#[cfg(not(target_arch = "wasm32"))]
use falcon_proto::MultiDeriveError;
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
    server_url: Signal<String>,
    form_incarnation: Cell<u64>,
    mapping_state: RefCell<MappingState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappingGeneration {
    server_url: String,
    connection_generation: u64,
    project_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectUpdateToken {
    server_url: String,
    connection_generation: u64,
    project_generation: u64,
    request_id: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingUpdateState {
    InFlight,
    NeedsReconciliation,
}

struct PendingProjectUpdate {
    token: ProjectUpdateToken,
    mapping_changed: bool,
    state: PendingUpdateState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProjectUpdateOutcome {
    Applied,
    DefiniteFailure,
    Uncertain,
}

#[derive(Default)]
struct MappingState {
    generations: HashMap<(String, String), u64>,
    pending: HashMap<(String, String), PendingProjectUpdate>,
    next_request_id: u64,
}

enum WorktreeRequest {
    Single(WorktreeInput),
    Multi(MultiWorktreeInput),
}

impl ProjectOperations {
    pub fn new(
        task_owner: Owner,
        connection_generation: Rc<Cell<u64>>,
        server_url: Signal<String>,
    ) -> Self {
        Self {
            task_owner,
            connection_generation,
            server_url,
            form_incarnation: Cell::new(0),
            mapping_state: RefCell::new(MappingState::default()),
        }
    }

    pub fn mapping_change_pending(&self, project_id: &str) -> bool {
        let key = (canonical_server_url(&self.server_url.get()), project_id.to_string());
        self.mapping_state
            .borrow()
            .pending
            .get(&key)
            .is_some_and(|pending| pending.mapping_changed)
    }

    pub fn update_pending(&self, project_id: &str) -> bool {
        let key = (canonical_server_url(&self.server_url.get()), project_id.to_string());
        self.mapping_state.borrow().pending.contains_key(&key)
    }

    pub fn mapping_generation(&self, project_id: &str) -> MappingGeneration {
        let server_url = canonical_server_url(&self.server_url.get());
        let connection_generation = self.connection_generation.get();
        let project_generation = self
            .mapping_state
            .borrow()
            .generations
            .get(&(server_url.clone(), project_id.to_string()))
            .copied()
            .unwrap_or(0);
        MappingGeneration {
            server_url,
            connection_generation,
            project_generation,
        }
    }

    pub fn mapping_is_current(&self, project_id: &str, generation: &MappingGeneration) -> bool {
        self.mapping_server_is_current(generation)
            && !self.mapping_change_pending(project_id)
            && self.mapping_generation(project_id) == *generation
    }

    pub fn mapping_server_is_current(&self, generation: &MappingGeneration) -> bool {
        generation.server_url == canonical_server_url(&self.server_url.get())
            && generation.connection_generation == self.connection_generation.get()
    }

    fn project_update_server_is_current(&self, token: &ProjectUpdateToken) -> bool {
        token.server_url == canonical_server_url(&self.server_url.get())
            && token.connection_generation == self.connection_generation.get()
    }

    pub fn current_connection_generation(&self) -> u64 {
        self.connection_generation.get()
    }

    fn begin_project_update(
        &self,
        project_id: &str,
        mapping_changed: bool,
    ) -> Option<ProjectUpdateToken> {
        let server_url = canonical_server_url(&self.server_url.get());
        let key = (server_url.clone(), project_id.to_string());
        let connection_generation = self.connection_generation.get();
        let mut state = self.mapping_state.borrow_mut();
        if state.pending.contains_key(&key) {
            return None;
        }
        let current_generation = state
            .generations
            .get(&key)
            .copied()
            .unwrap_or(0);
        let project_generation = if mapping_changed {
            current_generation
                .checked_add(1)
                .expect("project mapping generation exhausted")
        } else {
            current_generation
        };
        state.next_request_id = state
            .next_request_id
            .checked_add(1)
            .expect("project mapping request id exhausted");
        let token = ProjectUpdateToken {
            server_url,
            connection_generation,
            project_generation,
            request_id: state.next_request_id,
        };
        if mapping_changed {
            state.generations.insert(key.clone(), project_generation);
        }
        state.pending.insert(
            key,
            PendingProjectUpdate {
                token: token.clone(),
                mapping_changed,
                state: PendingUpdateState::InFlight,
            },
        );
        Some(token)
    }

    fn finish_project_update(
        &self,
        project_id: &str,
        token: &ProjectUpdateToken,
        outcome: ProjectUpdateOutcome,
    ) {
        let key = (token.server_url.clone(), project_id.to_string());
        let current_connection = token.server_url == canonical_server_url(&self.server_url.get())
            && token.connection_generation == self.connection_generation.get();
        let mut state = self.mapping_state.borrow_mut();
        let Some(pending) = state.pending.get(&key) else {
            return;
        };
        if pending.token != *token || pending.state != PendingUpdateState::InFlight {
            return;
        }
        let mapping_changed = pending.mapping_changed;
        if mapping_changed {
            let next_generation = state
                .generations
                .get(&key)
                .copied()
                .unwrap_or(token.project_generation)
                .checked_add(1)
                .expect("project mapping generation exhausted");
            state.generations.insert(key.clone(), next_generation);
        }
        if outcome == ProjectUpdateOutcome::DefiniteFailure
            || (outcome == ProjectUpdateOutcome::Applied && current_connection)
        {
            state.pending.remove(&key);
        } else if let Some(pending) = state.pending.get_mut(&key) {
            pending.state = PendingUpdateState::NeedsReconciliation;
        }
    }

    pub fn reconcile_mapping_snapshot(
        &self,
        server_url: &str,
        project_ids: &[String],
        connection_generation: u64,
    ) {
        let server_url = canonical_server_url(server_url);
        if server_url != canonical_server_url(&self.server_url.get())
            || connection_generation != self.connection_generation.get()
        {
            return;
        }
        let mut state = self.mapping_state.borrow_mut();
        state.pending.retain(|(pending_url, project_id), pending| {
            pending_url != &server_url
                || !project_ids.iter().any(|id| id == project_id)
                || pending.state != PendingUpdateState::NeedsReconciliation
                || pending.token.connection_generation >= connection_generation
        });
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
        if let Some(id) = project_id.as_deref() {
            if self.update_pending(id) {
                error.set(Some(t!("project.updatePending").to_string()));
                return;
            }
        }

        let mut mapping_changed = false;
        if let Some(id) = project_id.as_deref() {
            let Some(original) = store
                .projects
                .get()
                .into_iter()
                .find(|record| record.id == id)
                .and_then(|record| record.source)
            else {
                error.set(Some(t!("project.sourceUnavailable").to_string()));
                return;
            };
            mapping_changed = mapping_changed_in_store(&store, &original, &input);
            if mapping_changed && store.has_open_file_mapping(id) {
                error.set(Some(t!("project.closeFilesBeforeMappingChange").to_string()));
                return;
            }
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
            let update_token = if let Some(id) = project_id.as_deref() {
                let Some(token) = self.begin_project_update(id, mapping_changed) else {
                    busy.set(false);
                    error.set(Some(t!("project.updatePending").to_string()));
                    return;
                };
                if mapping_changed {
                    store.on_project_mapping_change_started(id);
                }
                Some(token)
            } else {
                None
            };
            let operations = store.project_operations.clone();
            let task_store = store.clone();
            store.spawn_root(async move {
                let response = match project_id.as_deref() {
                    Some(id) => client.update_project(id, &input).await,
                    None => client.create_project(&input).await,
                };
                let server_is_current = operations.server_is_current(context)
                    && update_token.as_ref().map_or(true, |token| {
                        operations.project_update_server_is_current(token)
                    });
                match response {
                    Ok(project) => {
                        let id = project.id.clone();
                        if let (Some(edit_id), Some(token)) =
                            (project_id.as_deref(), update_token.as_ref())
                        {
                            operations.finish_project_update(
                                edit_id,
                                token,
                                ProjectUpdateOutcome::Applied,
                            );
                        }
                        if !server_is_current {
                            return;
                        }
                        task_store.on_project_saved(&project, mapping_changed);
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
                        let definite_failure = api_error.kind == ApiErrorKind::Http
                            && api_error.status.is_some_and(|status| {
                                (400..500).contains(&status) && status != 408
                            })
                            || api_error.kind == ApiErrorKind::Internal;
                        if let (Some(edit_id), Some(token)) =
                            (project_id.as_deref(), update_token.as_ref())
                        {
                            operations.finish_project_update(
                                edit_id,
                                token,
                                if definite_failure {
                                    ProjectUpdateOutcome::DefiniteFailure
                                } else {
                                    ProjectUpdateOutcome::Uncertain
                                },
                            );
                        }
                        if !server_is_current {
                            return;
                        }
                        if operations.is_current(context) {
                            busy.set(false);
                            let message = if update_token.is_some() && !definite_failure {
                                if mapping_changed {
                                    t!("project.mappingChangeUncertain").to_string()
                                } else {
                                    t!("project.updateUncertain").to_string()
                                }
                            } else {
                                api_error.message
                            };
                            error.set(Some(message));
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

fn mapping_changed_in_store(
    store: &WorkspaceStore,
    original: &Project,
    input: &ProjectInput,
) -> bool {
    if mapping_changed(original, input) {
        return true;
    }
    let Some(host_id) = original
        .host_id
        .as_deref()
        .filter(|host_id| input.host_id.as_deref() == Some(*host_id))
    else {
        return false;
    };
    let Some(original_ssh) = original.ssh.as_ref() else {
        return true;
    };
    let Some(host) = store
        .hosts
        .get()
        .into_iter()
        .find(|host| host.id == host_id)
        .and_then(|host| host.source)
    else {
        return true;
    };
    host.host != original_ssh.host
        || host.port != original_ssh.port
        || host.username != original_ssh.username
        || host.auth_method != original_ssh.auth_method
        || host.key_path != original_ssh.key_path
        || host.has_secret != original_ssh.has_secret
}

fn canonical_server_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
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
