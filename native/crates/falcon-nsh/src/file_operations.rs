use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use falcon_core::file_path::{
    is_valid_file_name_segment, join_host_path, project_host_path_contains, project_nav_root,
};
use futures::channel::oneshot;
use futures::future::{AbortHandle, AbortRegistration, Abortable};
use nsh_core::prelude::{Owner, Signal, signal, spawn, timer};
use parking_lot::Mutex;

use crate::project_operations::MappingGeneration;
use crate::store::{FileEntryItem, ProjectRecord, WorkspaceStore};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileScope {
    pub(crate) profile_id: String,
    pub(crate) server_url: String,
    pub(crate) connection_generation: u64,
    pub(crate) project_id: String,
    pub(crate) directory: String,
    pub(crate) scope_generation: u64,
    pub(crate) request_generation: u64,
    pub(crate) mapping_generation: MappingGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NameAction {
    CreateDirectory,
    Rename { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NameForm {
    pub(crate) scope: FileScope,
    pub(crate) action: NameAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ConfirmationKind {
    Delete { paths: Vec<String> },
    Overwrite { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileConfirmation {
    pub(crate) scope: FileScope,
    pub(crate) kind: ConfirmationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JobKind {
    Rename,
    CreateDirectory,
    Delete,
    Upload,
    Download,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileJob {
    pub(crate) id: u64,
    pub(crate) kind: JobKind,
    pub(crate) completed: usize,
    pub(crate) total: usize,
    pub(crate) failed: usize,
    pub(crate) bytes_done: u64,
    pub(crate) bytes_total: u64,
    pub(crate) current_item: String,
    pub(crate) dispatched: bool,
    pub(crate) cancelled: bool,
    pub(crate) uncertain: bool,
    pub(crate) errors: Vec<String>,
    pub(crate) active: bool,
}

struct ActiveJob {
    id: u64,
    abort: AbortHandle,
    dispatched: bool,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone, PartialEq)]
struct MutationState {
    job_id: u64,
    kind: JobKind,
    scope: FileScope,
    uncertain: bool,
    error: Option<String>,
    listing_generation_after_uncertainty: Option<u64>,
}

#[derive(Clone)]
pub(crate) struct FileOperations {
    pub(crate) selected: Signal<BTreeSet<String>>,
    pub(crate) anchor: Signal<Option<String>>,
    pub(crate) name_form: Signal<Option<NameForm>>,
    pub(crate) name_draft: Signal<String>,
    pub(crate) confirmation: Signal<Option<FileConfirmation>>,
    pub(crate) job: Signal<Option<FileJob>>,
    mutation_state: Signal<Option<MutationState>>,
    pub(crate) task_owner: Owner,
    connection_generation: Rc<Cell<u64>>,
    scope_generation: Rc<Cell<u64>>,
    request_generation: Rc<Cell<u64>>,
    active_job: Rc<Mutex<Option<ActiveJob>>>,
    confirmation_sender: Rc<Mutex<Option<oneshot::Sender<bool>>>>,
    next_job_id: Rc<Cell<u64>>,
}

impl FileOperations {
    pub(crate) fn new(task_owner: Owner) -> Self {
        Self {
            selected: signal(BTreeSet::new()),
            anchor: signal(None),
            name_form: signal(None),
            name_draft: signal(String::new()),
            confirmation: signal(None),
            job: signal(None),
            mutation_state: signal(None),
            task_owner,
            connection_generation: Rc::new(Cell::new(0)),
            scope_generation: Rc::new(Cell::new(0)),
            request_generation: Rc::new(Cell::new(0)),
            active_job: Rc::new(Mutex::new(None)),
            confirmation_sender: Rc::new(Mutex::new(None)),
            next_job_id: Rc::new(Cell::new(0)),
        }
    }

    pub(crate) fn current_scope(&self, store: &WorkspaceStore) -> FileScope {
        let profiles = store.profiles.get();
        let server_url = crate::project_operations::canonical_server_url(&store.server_url.get());
        let project_id = store.active_project_id.get();
        let profile_id = profiles
            .profiles
            .iter()
            .find(|profile| profile.url.trim_end_matches('/') == server_url)
            .map(|profile| profile.id.clone())
            .unwrap_or_else(|| store.workspace_profile_id());
        FileScope {
            profile_id,
            server_url,
            connection_generation: self.connection_generation.get(),
            project_id: project_id.clone(),
            directory: store.file_directory.get(),
            scope_generation: self.scope_generation.get(),
            request_generation: self.request_generation.get(),
            mapping_generation: store.project_operations.mapping_generation(&project_id),
        }
    }

    pub(crate) fn scope_is_current(&self, store: &WorkspaceStore, scope: &FileScope) -> bool {
        self.current_scope(store) == *scope
            && store
                .project_operations
                .mapping_is_current(&scope.project_id, &scope.mapping_generation)
    }

    fn operation_scope_is_current(&self, store: &WorkspaceStore, scope: &FileScope) -> bool {
        let current = self.current_scope(store);
        current.profile_id == scope.profile_id
            && current.server_url == scope.server_url
            && current.connection_generation == scope.connection_generation
            && current.project_id == scope.project_id
            && current.directory == scope.directory
            && current.scope_generation == scope.scope_generation
            && store
                .project_operations
                .mapping_is_current(&scope.project_id, &scope.mapping_generation)
    }

    pub(crate) fn blocks_file_open(&self, store: &WorkspaceStore) -> bool {
        let server_url = crate::project_operations::canonical_server_url(&store.server_url.get());
        self.mutation_state.get().is_some_and(|state| {
            Self::mutation_blocks_file_open(state.kind, &state.scope.server_url, &server_url)
        })
    }

    pub(crate) fn file_open_lock_message(&self, store: &WorkspaceStore) -> Option<String> {
        let state = self.mutation_state.get()?;
        self.blocks_file_open(store)
            .then(|| self.mutation_state_message(&state))
    }

    pub(crate) fn blocks_project_mapping(&self, server_url: &str, project_id: &str) -> bool {
        let server_url = crate::project_operations::canonical_server_url(server_url);
        self.active_job.lock().is_some()
            || self.mutation_state.get().is_some_and(|state| {
                state.scope.server_url == server_url && state.scope.project_id == project_id
            })
    }

    fn mutation_blocks_file_open(
        kind: JobKind,
        mutation_server_url: &str,
        server_url: &str,
    ) -> bool {
        matches!(kind, JobKind::Rename | JobKind::Delete)
            && crate::project_operations::canonical_server_url(mutation_server_url)
                == crate::project_operations::canonical_server_url(server_url)
    }

    fn mutation_state_message(&self, state: &MutationState) -> String {
        let action = mutation_action(state.kind);
        if state.uncertain {
            let error = state.error.clone().unwrap_or_else(|| {
                crate::i18n::t!("files.operations.requestResultUnknown").to_string()
            });
            let uncertain = crate::i18n::t!(
                "files.operations.mutationUncertain",
                action = action,
                error = error
            );
            let refresh = crate::i18n::t!(
                "files.operations.mutationRefreshRequired",
                directory = state.scope.directory.as_str()
            );
            format!("{uncertain}\n{refresh}")
        } else {
            crate::i18n::t!("files.operations.mutationBusy", action = action).to_string()
        }
    }

    fn mutation_uncertain_message(
        &self,
        kind: JobKind,
        scope: &FileScope,
        error: String,
    ) -> String {
        let state = MutationState {
            job_id: 0,
            kind,
            scope: scope.clone(),
            uncertain: true,
            error: Some(error),
            listing_generation_after_uncertainty: None,
        };
        self.mutation_state_message(&state)
    }

    pub(crate) fn can_operate(&self, store: &WorkspaceStore) -> bool {
        if self.is_busy()
            || store.files_loading.get()
            || store.file_error.get().is_some()
            || store.active_project_id.get().is_empty()
            || !store.server_connected.get()
        {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        return store.client().is_some();
        #[cfg(target_arch = "wasm32")]
        false
    }

    pub(crate) fn begin_directory_request(&self, store: &WorkspaceStore, path: &str) -> FileScope {
        let changed = store.file_directory.get() != path;
        self.request_generation
            .set(self.request_generation.get().wrapping_add(1));
        if changed {
            self.scope_generation
                .set(self.scope_generation.get().wrapping_add(1));
            self.cancel_confirmation();
            self.name_form.set(None);
            self.abort_job_and_clear();
            self.clear_selection();
            store.file_entries.set(Vec::new());
        }
        store.file_directory.set(path.to_owned());
        store.file_error.set(None);
        self.current_scope(store)
    }

    pub(crate) fn apply_listing(
        &self,
        store: &WorkspaceStore,
        scope: &FileScope,
        canonical_path: &str,
        entries: &[FileEntryItem],
    ) -> bool {
        if !self.scope_is_current(store, scope) {
            return false;
        }
        self.resolve_mutation_after_listing(scope);
        if canonical_path != scope.directory {
            self.scope_generation
                .set(self.scope_generation.get().wrapping_add(1));
            self.cancel_confirmation();
            self.name_form.set(None);
            self.abort_job_and_clear();
            self.clear_selection();
            store.file_directory.set(canonical_path.to_owned());
        }
        let visible_paths: BTreeSet<String> =
            entries.iter().map(|entry| entry.path.clone()).collect();
        self.selected
            .update(|selected| selected.retain(|path| visible_paths.contains(path)));
        if self
            .anchor
            .get()
            .is_some_and(|anchor| !visible_paths.contains(&anchor))
        {
            self.anchor.set(None);
        }
        true
    }

    pub(crate) fn connection_changed(&self, store: &WorkspaceStore) {
        self.connection_generation
            .set(self.connection_generation.get().wrapping_add(1));
        self.scope_generation
            .set(self.scope_generation.get().wrapping_add(1));
        self.request_generation
            .set(self.request_generation.get().wrapping_add(1));
        self.invalidate_pending(store);
        store.file_directory.set(String::new());
        store.file_entries.set(Vec::new());
        store.files_loading.set(false);
        store.file_error.set(None);
        store.server_connected.set(false);
    }

    pub(crate) fn project_changed(&self, store: &WorkspaceStore) {
        self.request_generation
            .set(self.request_generation.get().wrapping_add(1));
        self.scope_generation
            .set(self.scope_generation.get().wrapping_add(1));
        self.invalidate_pending(store);
        store.file_directory.set(String::new());
        store.file_entries.set(Vec::new());
        store.files_loading.set(false);
        store.file_error.set(None);
    }

    fn invalidate_pending(&self, _store: &WorkspaceStore) {
        self.cancel_confirmation();
        self.name_form.set(None);
        self.clear_selection();
        self.abort_job_and_clear();
    }

    pub(crate) fn cancel_confirmation(&self) {
        self.confirmation.set(None);
        if let Some(sender) = self.confirmation_sender.lock().take() {
            let _ = sender.send(false);
        }
    }

    pub(crate) fn clear_selection(&self) {
        self.selected.set(BTreeSet::new());
        self.anchor.set(None);
    }

    fn abort_job_and_clear(&self) {
        let mut active = self.active_job.lock();
        if active.as_ref().is_some_and(|job| !job.dispatched) {
            let mut cleared_id = None;
            if let Some(job) = active.take() {
                cleared_id = Some(job.id);
                job.cancelled
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                job.abort.abort();
            }
            drop(active);
            if let Some(id) = cleared_id {
                self.clear_mutation_state_for_job(id);
            }
            self.job.set(None);
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.job.get().is_some_and(|job| job.active)
            || self.active_job.lock().is_some()
            || self
                .mutation_state
                .get()
                .is_some_and(|state| state.uncertain)
    }

    pub(crate) fn toggle_selected(&self, path: &str) {
        self.selected.update(|selected| {
            if !selected.remove(path) {
                selected.insert(path.to_owned());
            }
        });
        self.anchor.set(Some(path.to_owned()));
    }

    pub(crate) fn select_range(&self, path: &str, visible_paths: &[String]) {
        let anchor = self.anchor.get().unwrap_or_else(|| path.to_owned());
        let Some(anchor_index) = visible_paths.iter().position(|item| item == &anchor) else {
            self.toggle_selected(path);
            return;
        };
        let Some(path_index) = visible_paths.iter().position(|item| item == path) else {
            self.toggle_selected(path);
            return;
        };
        let start = anchor_index.min(path_index);
        let end = anchor_index.max(path_index);
        self.selected.update(|selected| {
            selected.extend(visible_paths[start..=end].iter().cloned());
        });
        self.anchor.set(Some(anchor));
    }

    pub(crate) fn select_visible(&self, paths: &[String]) {
        let current = self.selected.get();
        let all_selected = !paths.is_empty() && paths.iter().all(|path| current.contains(path));
        self.selected.update(|selected| {
            if all_selected {
                selected.retain(|path| !paths.iter().any(|visible| visible == path));
            } else {
                selected.extend(paths.iter().cloned());
            }
        });
        if self
            .anchor
            .get()
            .is_some_and(|anchor| !self.selected.get().contains(&anchor))
        {
            self.anchor.set(None);
        }
    }

    pub(crate) fn begin_new_directory(&self, store: &WorkspaceStore) {
        if self.ensure_ready(store).is_none() {
            return;
        }
        self.name_draft.set(String::new());
        self.name_form.set(Some(NameForm {
            scope: self.current_scope(store),
            action: NameAction::CreateDirectory,
        }));
    }

    pub(crate) fn begin_rename(&self, store: &WorkspaceStore) {
        if self.ensure_ready(store).is_none() {
            return;
        }
        let selected = self.selected.get();
        if selected.len() != 1 {
            return;
        }
        let path = selected.iter().next().cloned().unwrap_or_default();
        if let Some(dirty) = self.open_file_conflict(store, &path) {
            store.show_toast(if dirty {
                crate::i18n::t!("files.operations.saveCloseFirst").to_string()
            } else {
                crate::i18n::t!("files.operations.closeFirst").to_string()
            });
            return;
        }
        self.name_draft.set(
            store
                .file_entries
                .get()
                .iter()
                .find(|entry| entry.path == path)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path).to_owned()),
        );
        self.name_form.set(Some(NameForm {
            scope: self.current_scope(store),
            action: NameAction::Rename { path },
        }));
    }

    pub(crate) fn cancel_name_form(&self) {
        self.name_form.set(None);
        self.name_draft.set(String::new());
    }

    pub(crate) fn submit_name_form(&self, store: &WorkspaceStore) {
        if self.ensure_ready(store).is_none() {
            return;
        }
        let Some(form) = self.name_form.get() else {
            return;
        };
        let name = self.name_draft.get();
        let validation_directory = effective_remote_directory(store, &form.scope);
        if !is_valid_file_name_segment(&name, &validation_directory) {
            store.show_toast(crate::i18n::t!("files.operations.invalidName").to_string());
            return;
        }
        if !self.operation_scope_is_current(store, &form.scope) {
            self.cancel_name_form();
            return;
        }
        if let NameAction::Rename { path } = &form.action
            && let Some(dirty) = self.open_file_conflict(store, path)
        {
            self.cancel_name_form();
            store.show_toast(if dirty {
                crate::i18n::t!("files.operations.saveCloseFirst").to_string()
            } else {
                crate::i18n::t!("files.operations.closeFirst").to_string()
            });
            return;
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.cancel_name_form();
            store.show_toast(crate::i18n::t!("files.operations.transferUnavailable").to_string());
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let Some(client) = store.client() else {
            store.show_toast(crate::i18n::t!("files.operations.backendUnavailable").to_string());
            return;
        };
        let renaming = matches!(&form.action, NameAction::Rename { .. });
        self.name_form.set(None);
        let Some((id, registration)) = self.begin_job(
            if renaming {
                JobKind::Rename
            } else {
                JobKind::CreateDirectory
            },
            1,
            0,
            Some(form.scope.clone()),
        ) else {
            return;
        };
        let store = store.clone();
        let operations = self.clone();
        #[cfg(not(target_arch = "wasm32"))]
        self.task_owner.run(move || {
            spawn(async move {
                let result = Abortable::new(
                    async {
                        if !operations.operation_scope_is_current(&store, &form.scope) {
                            return None;
                        }
                        let project = &form.scope.project_id;
                        match form.action {
                            NameAction::CreateDirectory => {
                                let path = falcon_core::file_path::join_host_path(
                                    Some(&form.scope.directory),
                                    &name,
                                );
                                if !operations.mark_job_dispatched(id) {
                                    return None;
                                }
                                client.mkdir(project, &path, false).await.map(|_| path)
                            }
                            NameAction::Rename { path } => {
                                if let Some(dirty) = operations.open_file_conflict(&store, &path) {
                                    store.show_toast(if dirty {
                                        crate::i18n::t!("files.operations.saveCloseFirst")
                                            .to_string()
                                    } else {
                                        crate::i18n::t!("files.operations.closeFirst").to_string()
                                    });
                                    return None;
                                }
                                if !operations.mark_job_dispatched(id) {
                                    return None;
                                }
                                client
                                    .rename_file(project, &path, &name)
                                    .await
                                    .map(|_| path)
                            }
                        }
                        .map(Some)
                        .unwrap_or_else(|error| {
                            let error = error.to_string();
                            operations.record_job_failure(id, error.clone(), true);
                            let kind = if renaming {
                                JobKind::Rename
                            } else {
                                JobKind::CreateDirectory
                            };
                            let result =
                                operations.mutation_uncertain_message(kind, &form.scope, error);
                            let message =
                                if operations.operation_scope_is_current(&store, &form.scope) {
                                    result
                                } else {
                                    crate::i18n::t!(
                                        "files.operations.scopeSummary",
                                        action = mutation_action(kind),
                                        directory = form.scope.directory.as_str(),
                                        result = result
                                    )
                                };
                            store.show_toast(message.to_string());
                            Some(String::new())
                        })
                    },
                    registration,
                )
                .await;
                match result {
                    Ok(Some(path)) if !path.is_empty() => {
                        operations.clear_mutation_state_for_job(id);
                        let still_current =
                            operations.operation_scope_is_current(&store, &form.scope);
                        operations.finish_job(id, 1, 0, 0, true);
                        if still_current {
                            store.show_toast(if renaming {
                                crate::i18n::t!("files.operations.renameDone").to_string()
                            } else {
                                crate::i18n::t!("files.operations.directoryCreated").to_string()
                            });
                            if renaming {
                                operations.selected.update(|selected| {
                                    selected.remove(&path);
                                });
                            }
                            store.browse_directory(&form.scope.directory);
                        } else {
                            let result = if renaming {
                                crate::i18n::t!("files.operations.renameDone")
                            } else {
                                crate::i18n::t!("files.operations.directoryCreated")
                            };
                            store.show_toast(crate::i18n::t!(
                                "files.operations.scopeSummary",
                                action = if renaming {
                                    crate::i18n::t!("files.operations.rename")
                                } else {
                                    crate::i18n::t!("files.operations.newFolder")
                                },
                                directory = form.scope.directory.as_str(),
                                result = result
                            ));
                        }
                    }
                    Ok(Some(_)) => operations.finish_job(id, 0, 1, 0, true),
                    Ok(None) | Err(_) => operations.finish_mutation_after_abort(
                        &store,
                        id,
                        if renaming {
                            JobKind::Rename
                        } else {
                            JobKind::CreateDirectory
                        },
                        &form.scope,
                    ),
                }
            });
        });
        #[cfg(target_arch = "wasm32")]
        let _ = (id, registration, store, operations);
    }

    pub(crate) fn delete_selected(&self, store: &WorkspaceStore) {
        if self.ensure_ready(store).is_none() {
            return;
        }
        let selected_before = self.selected.get();
        let paths: Vec<String> = selected_before.iter().cloned().collect();
        if paths.is_empty() {
            return;
        }
        #[cfg(target_arch = "wasm32")]
        {
            store.show_toast(crate::i18n::t!("files.operations.transferUnavailable").to_string());
            return;
        }
        if let Some(dirty) = paths
            .iter()
            .find_map(|path| self.open_file_conflict(store, path))
        {
            store.show_toast(if dirty {
                crate::i18n::t!("files.operations.saveCloseFirst").to_string()
            } else {
                crate::i18n::t!("files.operations.closeFirst").to_string()
            });
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let Some(client) = store.client() else {
            store.show_toast(crate::i18n::t!("files.operations.backendUnavailable").to_string());
            return;
        };
        let scope = self.current_scope(store);
        if !self.operation_scope_is_current(store, &scope) {
            return;
        }
        let Some((id, registration)) =
            self.begin_job(JobKind::Delete, paths.len(), 0, Some(scope.clone()))
        else {
            return;
        };
        let operations = self.clone();
        let store = store.clone();
        #[cfg(not(target_arch = "wasm32"))]
        self.task_owner.run(move || {
            spawn(async move {
                let result = Abortable::new(
                    async {
                        if !operations.operation_scope_is_current(&store, &scope) {
                            return None;
                        }
                        if !operations
                            .ask_confirmation(
                                &store,
                                scope.clone(),
                                ConfirmationKind::Delete {
                                    paths: paths.clone(),
                                },
                            )
                            .await
                            || !operations.operation_scope_is_current(&store, &scope)
                        {
                            return None;
                        }
                        if let Some(dirty) = paths
                            .iter()
                            .find_map(|path| operations.open_file_conflict(&store, path))
                        {
                            store.show_toast(if dirty {
                                crate::i18n::t!("files.operations.saveCloseFirst").to_string()
                            } else {
                                crate::i18n::t!("files.operations.closeFirst").to_string()
                            });
                            return None;
                        }
                        if !operations.mark_job_dispatched(id) {
                            return None;
                        }
                        match client.remove_files(&scope.project_id, &paths).await {
                            Ok(result) => Some(Ok(result)),
                            Err(error) => Some(Err(error.to_string())),
                        }
                    },
                    registration,
                )
                .await;
                match result {
                    Ok(Some(Ok(result))) => {
                        operations.clear_mutation_state_for_job(id);
                        let still_current = operations.operation_scope_is_current(&store, &scope);
                        let error_text = result
                            .errors
                            .iter()
                            .map(|error| format!("{}: {}", error.path, error.error))
                            .collect::<Vec<_>>()
                            .join("\n");
                        let failed = result.errors.len();
                        if failed > 0 {
                            operations.record_job_failure(id, error_text.clone(), false);
                        }
                        operations.finish_job(id, result.removed.len(), failed, 0, true);
                        if still_current {
                            let current_selection = operations.selected.get();
                            operations
                                .selected
                                .set(remaining_after_remove(&current_selection, &result.removed));
                            if failed == 0 {
                                store.show_toast(
                                    crate::i18n::t!(
                                        "files.operations.deleteDone",
                                        count = result.removed.len().to_string()
                                    )
                                    .to_string(),
                                );
                            } else {
                                store.show_toast(
                                    crate::i18n::t!(
                                        "files.operations.deletePartial",
                                        removed = result.removed.len().to_string(),
                                        failed = failed.to_string(),
                                        errors = error_text
                                    )
                                    .to_string(),
                                );
                            }
                            store.browse_directory(&scope.directory);
                        } else {
                            let result_text = if failed == 0 {
                                crate::i18n::t!(
                                    "files.operations.deleteDone",
                                    count = result.removed.len().to_string()
                                )
                            } else {
                                crate::i18n::t!(
                                    "files.operations.deletePartial",
                                    removed = result.removed.len().to_string(),
                                    failed = failed.to_string(),
                                    errors = error_text
                                )
                            };
                            store.show_toast(crate::i18n::t!(
                                "files.operations.scopeSummary",
                                action = crate::i18n::t!("files.operations.delete"),
                                directory = scope.directory.as_str(),
                                result = result_text
                            ));
                        }
                    }
                    Ok(Some(Err(error))) => {
                        operations.record_job_failure(id, error.clone(), true);
                        operations.finish_job(id, 0, 1, 0, true);
                        let result =
                            operations.mutation_uncertain_message(JobKind::Delete, &scope, error);
                        let message = if operations.operation_scope_is_current(&store, &scope) {
                            result
                        } else {
                            crate::i18n::t!(
                                "files.operations.scopeSummary",
                                action = crate::i18n::t!("files.operations.delete"),
                                directory = scope.directory.as_str(),
                                result = result
                            )
                        };
                        store.show_toast(message.to_string());
                    }
                    Ok(None) | Err(_) => {
                        operations.finish_mutation_after_abort(&store, id, JobKind::Delete, &scope)
                    }
                }
            });
        });
        #[cfg(target_arch = "wasm32")]
        let _ = (id, registration, store, paths, scope);
    }

    pub(crate) fn upload_files(&self, store: &WorkspaceStore) {
        self.start_upload_picker(store, false);
    }

    pub(crate) fn upload_folder(&self, store: &WorkspaceStore) {
        self.start_upload_picker(store, true);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start_upload_picker(&self, store: &WorkspaceStore, folders: bool) {
        if self.ensure_ready(store).is_none() {
            return;
        }
        let Some(client) = store.client() else {
            store.show_toast(crate::i18n::t!("files.operations.backendUnavailable").to_string());
            return;
        };
        let scope = self.current_scope(store);
        let Some((id, registration)) = self.begin_job(JobKind::Upload, 0, 0, None) else {
            return;
        };
        let operations = self.clone();
        let store = store.clone();
        self.task_owner.run(move || {
            spawn(async move {
                let progress = Arc::new(Mutex::new(ProgressSnapshot::default()));
                let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let tick_ops = operations.clone();
                let tick_store = store.clone();
                let tick_progress = progress.clone();
                let tick_running = running.clone();
                let ticker = spawn(async move {
                    while tick_running.load(std::sync::atomic::Ordering::Relaxed) {
                        timer(Duration::from_millis(120)).await;
                        tick_ops.update_job_progress(id, &tick_store, &tick_progress);
                    }
                });
                let result = Abortable::new(
                    async {
                        let dialog = if folders {
                            nsh_core::file_dialog::OpenDialog::folders().multiple(true)
                        } else {
                            nsh_core::file_dialog::OpenDialog::files().multiple(true)
                        };
                        let picked = match nsh_core::prelude::pick_paths(dialog).await {
                            Ok(Some(paths)) => paths,
                            Ok(None) => return None,
                            Err(error) => {
                                store.show_toast(
                                    crate::i18n::t!("files.operations.pickerFailed", error = error)
                                        .to_string(),
                                );
                                return None;
                            }
                        };
                        if !operations.operation_scope_is_current(&store, &scope)
                            || picked.is_empty()
                        {
                            return None;
                        }
                        if folders {
                            operations
                                .upload_picked_folders(
                                    &store,
                                    &client,
                                    &scope,
                                    id,
                                    picked,
                                    progress.clone(),
                                )
                                .await
                        } else {
                            operations
                                .upload_picked_files(
                                    &store,
                                    &client,
                                    &scope,
                                    id,
                                    picked,
                                    progress.clone(),
                                )
                                .await
                        }
                    },
                    registration,
                )
                .await;
                running.store(false, std::sync::atomic::Ordering::Relaxed);
                ticker.cancel();
                let was_current = operations.operation_scope_is_current(&store, &scope);
                let summary = match result {
                    Ok(Some(summary)) => Some(summary),
                    Ok(None) => None,
                    Err(_) => Some(summary_from_progress(
                        &progress.lock(),
                        true,
                        operations.job_was_dispatched(id),
                    )),
                };
                if let Some(summary) = summary {
                    operations.finish_transfer_job(id, &summary);
                    operations.show_transfer_summary(&store, &summary, true, &scope, was_current);
                    if summary.changed && was_current {
                        store.browse_directory(&scope.directory);
                    }
                } else {
                    operations.finish_job(id, 0, 0, 0, false);
                }
            });
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn start_upload_picker(&self, store: &WorkspaceStore, _folders: bool) {
        store.show_toast(crate::i18n::t!("files.operations.transferUnavailable").to_string());
    }

    pub(crate) fn download_selected(&self, store: &WorkspaceStore) {
        #[cfg(not(target_arch = "wasm32"))]
        self.start_downloads(store);
        #[cfg(target_arch = "wasm32")]
        store.show_toast(crate::i18n::t!("files.operations.transferUnavailable").to_string());
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start_downloads(&self, store: &WorkspaceStore) {
        if self.ensure_ready(store).is_none() {
            return;
        }
        let selected = self.selected.get();
        let entries = store.file_entries.get();
        let selected_entries: Vec<FileEntryItem> = entries
            .into_iter()
            .filter(|entry| selected.contains(&entry.path))
            .collect();
        let files: Vec<FileEntryItem> = selected_entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .cloned()
            .collect();
        if files.is_empty() {
            store.show_toast(crate::i18n::t!("files.operations.noFilesSelected").to_string());
            return;
        }
        let skipped_dirs = selected_entries.len().saturating_sub(files.len());
        let Some(client) = store.client() else {
            store.show_toast(crate::i18n::t!("files.operations.backendUnavailable").to_string());
            return;
        };
        let scope = self.current_scope(store);
        let bytes_total = files.iter().map(|entry| entry.size_bytes).sum();
        let Some((id, registration)) =
            self.begin_job(JobKind::Download, files.len(), bytes_total, None)
        else {
            return;
        };
        let operations = self.clone();
        let store = store.clone();
        self.task_owner.run(move || {
            spawn(async move {
                let progress = Arc::new(Mutex::new(ProgressSnapshot::default()));
                {
                    let mut snapshot = progress.lock();
                    snapshot.total = files.len();
                    snapshot.skipped = skipped_dirs;
                }
                let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
                let ticker_ops = operations.clone();
                let ticker_store = store.clone();
                let ticker_progress = progress.clone();
                let ticker_running = running.clone();
                let ticker = spawn(async move {
                    while ticker_running.load(std::sync::atomic::Ordering::Relaxed) {
                        timer(Duration::from_millis(120)).await;
                        ticker_ops.update_job_progress(id, &ticker_store, &ticker_progress);
                    }
                });
                let result = Abortable::new(
                    async {
                        let mut summary = JobSummary {
                            skipped: skipped_dirs,
                            ..JobSummary::default()
                        };
                        for entry in files {
                            if !operations.operation_scope_is_current(&store, &scope) {
                                summary.cancelled = true;
                                break;
                            }
                            let name = entry.name.clone();
                            let dest = match nsh_core::prelude::pick_save_path(
                                nsh_core::file_dialog::SaveDialog::new()
                                    .file_name(name.clone())
                                    .directory(falcon_platform::shared().downloads_dir()),
                            )
                            .await
                            {
                                Ok(Some(path)) => path,
                                Ok(None) => {
                                    summary.cancelled = true;
                                    break;
                                }
                                Err(error) => {
                                    progress.lock().complete_item(0, true);
                                    progress.lock().record_error(error.clone());
                                    summary.failed += 1;
                                    summary.errors.push(error);
                                    break;
                                }
                            };
                            if !operations.operation_scope_is_current(&store, &scope) {
                                summary.cancelled = true;
                                break;
                            }
                            progress.lock().begin_item(name.clone());
                            if !operations.mark_job_dispatched(id) {
                                summary.cancelled = true;
                                break;
                            }
                            let progress_cb = progress.clone();
                            match client
                                .download_to_file(
                                    &scope.project_id,
                                    &entry.path,
                                    dest,
                                    move |done, total| {
                                        progress_cb.lock().set_current_bytes(done, total);
                                    },
                                )
                                .await
                            {
                                Ok(bytes) => {
                                    progress.lock().complete_item(bytes, false);
                                    summary.completed += 1;
                                    summary.bytes_done += bytes;
                                    summary.changed = true;
                                }
                                Err(error) => {
                                    progress.lock().complete_item(0, true);
                                    progress
                                        .lock()
                                        .record_error(format!("{}: {}", entry.name, error));
                                    summary.failed += 1;
                                    summary.errors.push(format!("{}: {}", entry.name, error));
                                }
                            }
                        }
                        Some(summary)
                    },
                    registration,
                )
                .await;
                running.store(false, std::sync::atomic::Ordering::Relaxed);
                ticker.cancel();
                let summary = match result {
                    Ok(Some(summary)) => summary,
                    Ok(None) | Err(_) => {
                        let snapshot = progress.lock().clone();
                        summary_from_progress(&snapshot, true, operations.job_was_dispatched(id))
                    }
                };
                let snapshot = progress.lock().clone();
                let summary = JobSummary {
                    completed: snapshot.completed,
                    failed: snapshot.failed,
                    skipped: snapshot.skipped,
                    remaining: snapshot
                        .total
                        .saturating_sub(snapshot.completed + snapshot.failed),
                    bytes_done: snapshot.bytes_done,
                    changed: snapshot.completed > 0,
                    errors: snapshot.errors,
                    ..summary
                };
                let was_current = operations.operation_scope_is_current(&store, &scope);
                operations.finish_transfer_job(id, &summary);
                operations.show_transfer_summary(&store, &summary, false, &scope, was_current);
            });
        });
    }

    pub(crate) fn cancel_job(&self) {
        let active = self.active_job.lock();
        if active.as_ref().is_some_and(|job| {
            job.dispatched
                && self.job.get().is_some_and(|state| {
                    state.id == job.id
                        && matches!(
                            state.kind,
                            JobKind::Rename | JobKind::CreateDirectory | JobKind::Delete
                        )
                })
        }) {
            return;
        }
        if let Some(job) = active.as_ref() {
            job.cancelled
                .store(true, std::sync::atomic::Ordering::Relaxed);
            job.abort.abort();
        }
        drop(active);
        self.cancel_confirmation();
    }

    pub(crate) fn dismiss_job(&self) {
        let uncertain_mutation = self.job.get().is_some_and(|job| {
            job.uncertain
                && matches!(
                    job.kind,
                    JobKind::Rename | JobKind::CreateDirectory | JobKind::Delete
                )
        });
        if !self.is_busy() && !uncertain_mutation {
            self.job.set(None);
        }
    }

    pub(crate) fn answer_confirmation(&self, accept: bool) {
        let Some(confirmation) = self.confirmation.get() else {
            return;
        };
        if !accept {
            self.confirmation.set(None);
            if let Some(sender) = self.confirmation_sender.lock().take() {
                let _ = sender.send(false);
            }
            return;
        }
        self.confirmation.set(None);
        if let Some(sender) = self.confirmation_sender.lock().take() {
            let _ = sender.send(true);
        }
        let _ = confirmation;
    }

    async fn ask_confirmation(
        &self,
        store: &WorkspaceStore,
        scope: FileScope,
        kind: ConfirmationKind,
    ) -> bool {
        if !self.operation_scope_is_current(store, &scope) {
            return false;
        }
        self.cancel_confirmation();
        let (sender, receiver) = oneshot::channel();
        self.confirmation_sender.lock().replace(sender);
        self.confirmation.set(Some(FileConfirmation {
            scope: scope.clone(),
            kind,
        }));
        match receiver.await {
            Ok(true) => self.operation_scope_is_current(store, &scope),
            _ => false,
        }
    }

    fn ensure_ready(&self, store: &WorkspaceStore) -> Option<FileScope> {
        if let Some(state) = self.mutation_state.get().filter(|state| state.uncertain) {
            store.show_toast(self.mutation_state_message(&state));
            return None;
        }
        if self.is_busy() {
            store.show_toast(crate::i18n::t!("files.operations.busy").to_string());
            return None;
        }
        if store.files_loading.get() {
            store.show_toast(crate::i18n::t!("files.operations.listingBusy").to_string());
            return None;
        }
        if let Some(error) = store.file_error.get() {
            store.show_toast(
                crate::i18n::t!("files.operations.listingFailed", error = error).to_string(),
            );
            return None;
        }
        if store.active_project_id.get().is_empty() {
            store.show_toast(crate::i18n::t!("files.operations.noProject").to_string());
            return None;
        }
        if !store.server_connected.get() {
            store.show_toast(crate::i18n::t!("files.operations.backendUnavailable").to_string());
            return None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if store.client().is_none() {
            store.show_toast(crate::i18n::t!("files.operations.backendUnavailable").to_string());
            return None;
        }
        #[cfg(target_arch = "wasm32")]
        {
            store.show_toast(crate::i18n::t!("files.operations.transferUnavailable").to_string());
            None
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            Some(self.current_scope(store))
        }
    }

    fn open_file_conflict(&self, store: &WorkspaceStore, target: &str) -> Option<bool> {
        let project = store.active_project_id.get();
        let projects = store.projects.get();
        let target_record = projects.iter().find(|entry| entry.id == project);
        let target_root = target_record
            .map(|entry| entry.path.as_str())
            .unwrap_or_default();
        let target_host = target_record.and_then(|entry| entry.host_id.as_deref());
        let matches_path = |other_project: &str, candidate: &str| {
            if other_project == project.as_str() {
                return project_host_path_contains(target_root, target, candidate);
            }
            let Some(other_record) = projects.iter().find(|entry| entry.id == other_project) else {
                return false;
            };
            if other_record.host_id.as_deref() != target_host {
                return false;
            }
            let candidate =
                falcon_core::file_path::join_host_path(Some(&other_record.path), candidate);
            project_host_path_contains(target_root, target, &candidate)
        };
        let dirty = store
            .open_files
            .get()
            .values()
            .any(|buffer| buffer.is_dirty() && matches_path(&buffer.project_id, &buffer.path));
        if dirty {
            return Some(true);
        }
        let buffered = store
            .open_files
            .get()
            .values()
            .any(|buffer| matches_path(&buffer.project_id, &buffer.path));
        let pane_open = store
            .columns
            .get()
            .iter()
            .flat_map(|column| &column.panes)
            .any(|pane| {
                matches!(
                    falcon_core::pane_key::parse_pane_key(&pane.key),
                    Some(falcon_core::pane_key::PaneItem::File { project_id, path, .. })
                        if matches_path(&project_id, &path)
                )
            });
        (buffered || pane_open).then_some(false)
    }

    fn begin_job(
        &self,
        kind: JobKind,
        total: usize,
        bytes_total: u64,
        mutation_scope: Option<FileScope>,
    ) -> Option<(u64, AbortRegistration)> {
        if self.is_busy() {
            return None;
        }
        self.next_job_id.set(self.next_job_id.get().wrapping_add(1));
        let id = self.next_job_id.get();
        let (handle, registration) = AbortHandle::new_pair();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.active_job.lock().replace(ActiveJob {
            id,
            abort: handle,
            dispatched: false,
            cancelled,
        });
        if let Some(scope) = mutation_scope {
            self.mutation_state.set(Some(MutationState {
                job_id: id,
                kind,
                scope,
                uncertain: false,
                error: None,
                listing_generation_after_uncertainty: None,
            }));
        }
        self.job.set(Some(FileJob {
            id,
            kind,
            completed: 0,
            total,
            failed: 0,
            bytes_done: 0,
            bytes_total,
            current_item: String::new(),
            dispatched: false,
            cancelled: false,
            uncertain: false,
            errors: Vec::new(),
            active: true,
        }));
        Some((id, registration))
    }

    fn mark_job_dispatched(&self, id: u64) -> bool {
        let mut active = self.active_job.lock();
        let Some(job) = active.as_mut().filter(|job| job.id == id) else {
            return false;
        };
        if job.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        job.dispatched = true;
        drop(active);
        self.job.update(|job| {
            if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                job.dispatched = true;
            }
        });
        true
    }

    fn record_job_failure(&self, id: u64, error: String, uncertain: bool) {
        if uncertain {
            self.mutation_state.update(|state| {
                if let Some(state) = state.as_mut().filter(|state| state.job_id == id) {
                    state.uncertain = true;
                    state.error = Some(error.clone());
                    state.listing_generation_after_uncertainty =
                        Some(self.request_generation.get());
                }
            });
        }
        self.job.update(|job| {
            if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                job.errors.push(error);
                job.uncertain |= uncertain;
            }
        });
    }

    fn clear_mutation_state_for_job(&self, id: u64) {
        self.mutation_state.update(|state| {
            if state.as_ref().is_some_and(|state| state.job_id == id) {
                *state = None;
            }
        });
    }

    fn resolve_mutation_after_listing(&self, scope: &FileScope) {
        let resolved_job_id = self.mutation_state.get().and_then(|state| {
            (state.uncertain
                && same_listing_location(&state.scope, scope)
                && state
                    .listing_generation_after_uncertainty
                    .is_some_and(|generation| scope.request_generation > generation))
            .then_some(state.job_id)
        });
        if let Some(id) = resolved_job_id {
            self.clear_mutation_state_for_job(id);
            self.job.update(|job| {
                if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                    job.uncertain = false;
                }
            });
        }
    }

    fn finish_mutation_after_abort(
        &self,
        store: &WorkspaceStore,
        id: u64,
        kind: JobKind,
        scope: &FileScope,
    ) {
        if !self.job_was_dispatched(id) {
            self.finish_job(id, 0, 0, 0, false);
            return;
        }
        let error = crate::i18n::t!("files.operations.requestResultUnknown").to_string();
        self.record_job_failure(id, error.clone(), true);
        self.finish_job(id, 0, 1, 0, true);
        let result = self.mutation_uncertain_message(kind, scope, error);
        let message = if self.operation_scope_is_current(store, scope) {
            result
        } else {
            crate::i18n::t!(
                "files.operations.scopeSummary",
                action = mutation_action(kind),
                directory = scope.directory.as_str(),
                result = result
            )
        };
        store.show_toast(message.to_string());
    }

    fn job_was_dispatched(&self, id: u64) -> bool {
        self.active_job
            .lock()
            .as_ref()
            .is_some_and(|job| job.id == id && job.dispatched)
    }

    fn job_cancellation_token(&self, id: u64) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        self.active_job
            .lock()
            .as_ref()
            .filter(|job| job.id == id)
            .map(|job| job.cancelled.clone())
    }

    fn finish_job(&self, id: u64, completed: usize, failed: usize, bytes_done: u64, keep: bool) {
        let current_id = self.job.get().map(|job| job.id);
        if !job_id_is_current(current_id, id) {
            return;
        }
        let active_id = self.active_job.lock().as_ref().map(|job| job.id);
        if active_id != Some(id) {
            return;
        }
        self.active_job.lock().take();
        if !keep {
            self.clear_mutation_state_for_job(id);
        }
        self.job.update(|job| {
            if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                job.completed = completed;
                job.failed = failed;
                job.bytes_done = bytes_done;
                job.active = false;
            }
            if !keep {
                *job = None;
            }
        });
    }

    fn finish_transfer_job(&self, id: u64, summary: &JobSummary) {
        if !self.job.get().is_some_and(|job| job.id == id) {
            return;
        }
        self.finish_job(
            id,
            summary.completed,
            summary.failed,
            summary.bytes_done,
            true,
        );
        self.job.update(|job| {
            if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                job.cancelled = summary.cancelled;
                job.uncertain = summary.uncertain;
                job.errors = summary.errors.clone();
            }
        });
    }

    fn update_job_progress(
        &self,
        id: u64,
        _store: &WorkspaceStore,
        progress: &Arc<Mutex<ProgressSnapshot>>,
    ) {
        let snapshot = progress.lock().clone();
        self.job.update(|job| {
            if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                job.completed = snapshot.completed;
                job.failed = snapshot.failed;
                job.bytes_done = snapshot.bytes_done.saturating_add(snapshot.current_bytes);
                job.current_item = snapshot.current_item;
            }
        });
    }

    fn show_transfer_summary(
        &self,
        store: &WorkspaceStore,
        summary: &JobSummary,
        upload: bool,
        scope: &FileScope,
        still_current: bool,
    ) {
        let mut message = if summary.cancelled {
            if upload {
                crate::i18n::t!(
                    "files.operations.uploadCancelled",
                    completed = summary.completed.to_string(),
                    total = (summary.completed + summary.failed + summary.remaining).to_string(),
                    failed = summary.failed.to_string(),
                    uncertain = if summary.uncertain {
                        crate::i18n::t!("files.operations.resultUncertain")
                    } else {
                        String::new()
                    }
                )
            } else {
                crate::i18n::t!(
                    "files.operations.transferCancelled",
                    completed = summary.completed.to_string(),
                    total = (summary.completed + summary.failed + summary.remaining).to_string(),
                    failed = summary.failed.to_string()
                )
            }
        } else if summary.failed > 0 {
            crate::i18n::t!(
                "files.operations.transferPartial",
                completed = summary.completed.to_string(),
                failed = summary.failed.to_string(),
                errors = summary.errors.join("\n")
            )
        } else {
            crate::i18n::t!(
                "files.operations.transferDone",
                completed = summary.completed.to_string()
            )
        };
        if summary.skipped > 0 {
            let skipped = if upload {
                crate::i18n::t!(
                    "files.operations.uploadSkipped",
                    count = summary.skipped.to_string()
                )
            } else {
                crate::i18n::t!(
                    "files.operations.downloadSkippedDirectories",
                    count = summary.skipped.to_string()
                )
            };
            message = format!("{message} {skipped}");
        }
        if !still_current {
            let action = if upload {
                crate::i18n::t!("files.operations.uploadAction")
            } else {
                crate::i18n::t!("files.operations.downloadAction")
            };
            message = crate::i18n::t!(
                "files.operations.scopeSummary",
                action = action,
                directory = scope.directory.as_str(),
                result = message
            );
        }
        store.show_toast(message.to_string());
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn upload_picked_files(
        &self,
        store: &WorkspaceStore,
        client: &falcon_client::FalconClient,
        scope: &FileScope,
        job_id: u64,
        picked: Vec<PathBuf>,
        progress: Arc<Mutex<ProgressSnapshot>>,
    ) -> Option<JobSummary> {
        let mut files = Vec::new();
        let mut skipped = 0;
        for path in picked {
            if !self.operation_scope_is_current(store, scope) {
                return None;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                skipped += 1;
                continue;
            };
            if !metadata.is_file() {
                skipped += 1;
                continue;
            }
            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                skipped += 1;
                continue;
            };
            let validation_directory = effective_remote_directory(store, scope);
            if !is_valid_file_name_segment(&name, &validation_directory) {
                skipped += 1;
                continue;
            }
            files.push(UploadFile {
                local: path,
                remote: falcon_core::file_path::join_host_path(Some(&scope.directory), &name),
                name,
                size: metadata.len(),
            });
        }
        let total = files.len();
        let bytes_total = files.iter().map(|file| file.size).sum();
        self.update_job_total(total, bytes_total);
        {
            let mut snapshot = progress.lock();
            snapshot.total = total;
            snapshot.skipped = skipped;
        }
        let mut summary = JobSummary {
            skipped,
            ..JobSummary::default()
        };
        for file in files {
            if !self.operation_scope_is_current(store, scope) {
                summary.cancelled = true;
                break;
            }
            progress.lock().begin_item(file.name.clone());
            if !self.mark_job_dispatched(job_id) {
                summary.cancelled = true;
                break;
            }
            let progress_cb = progress.clone();
            let mut result = client
                .upload_file(
                    &scope.project_id,
                    &scope.directory,
                    &file.name,
                    file.local.clone(),
                    false,
                    move |done, total| progress_cb.lock().set_current_bytes(done, total),
                )
                .await;
            if let Err(error) = &result
                && error.is_conflict()
            {
                let confirmed = self
                    .ask_confirmation(
                        store,
                        scope.clone(),
                        ConfirmationKind::Overwrite {
                            path: file.remote.clone(),
                        },
                    )
                    .await;
                if !confirmed || !self.operation_scope_is_current(store, scope) {
                    summary.cancelled = true;
                    break;
                }
                if !self.mark_job_dispatched(job_id) {
                    summary.cancelled = true;
                    break;
                }
                let progress_cb = progress.clone();
                result = client
                    .upload_file(
                        &scope.project_id,
                        &scope.directory,
                        &file.name,
                        file.local.clone(),
                        true,
                        move |done, total| progress_cb.lock().set_current_bytes(done, total),
                    )
                    .await;
            }
            match result {
                Ok(uploaded) => {
                    progress.lock().complete_item(uploaded.size, false);
                    summary.completed += 1;
                    summary.bytes_done += uploaded.size;
                    summary.changed = true;
                }
                Err(error) => {
                    progress.lock().complete_item(0, true);
                    progress
                        .lock()
                        .record_error(format!("{}: {}", file.name, error));
                    summary.failed += 1;
                    summary.errors.push(format!("{}: {}", file.name, error));
                }
            }
        }
        let snapshot = progress.lock().clone();
        summary.completed = snapshot.completed;
        summary.failed = snapshot.failed;
        summary.bytes_done = snapshot.bytes_done;
        summary.remaining = snapshot
            .total
            .saturating_sub(summary.completed + summary.failed);
        summary.errors = snapshot.errors;
        Some(summary)
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn upload_picked_folders(
        &self,
        store: &WorkspaceStore,
        client: &falcon_client::FalconClient,
        scope: &FileScope,
        job_id: u64,
        picked: Vec<PathBuf>,
        progress: Arc<Mutex<ProgressSnapshot>>,
    ) -> Option<JobSummary> {
        let cancellation = self.job_cancellation_token(job_id)?;
        let remote_directory = effective_remote_directory(store, scope);
        let scan = nsh_core::prelude::blocking(move || {
            collect_local_folders(picked, &remote_directory, cancellation)
        })
        .await;
        let scan = match scan {
            Ok(scan) => scan,
            Err(error) => {
                let message = error.to_string();
                progress.lock().record_error(message.clone());
                return Some(JobSummary {
                    failed: 1,
                    errors: vec![message],
                    ..JobSummary::default()
                });
            }
        };
        let all_dirs = scan.directories;
        let all_files = scan.files;
        let skipped = scan.skipped;
        let total = all_dirs.len() + all_files.len();
        let bytes_total = all_files.iter().map(|file| file.size).sum();
        self.update_job_total(total, bytes_total);
        {
            let mut snapshot = progress.lock();
            snapshot.total = total;
            snapshot.skipped = skipped;
        }
        let mut summary = JobSummary {
            skipped,
            ..JobSummary::default()
        };
        summary.cancelled = scan.cancelled;
        for directory in all_dirs {
            if summary.cancelled || !self.operation_scope_is_current(store, scope) {
                summary.cancelled = true;
                break;
            }
            let remote = falcon_core::file_path::join_host_path(Some(&scope.directory), &directory);
            progress.lock().begin_item(directory.clone());
            if !self.mark_job_dispatched(job_id) {
                summary.cancelled = true;
                break;
            }
            match client.mkdir(&scope.project_id, &remote, true).await {
                Ok(_) => {
                    progress.lock().complete_item(0, false);
                    summary.completed += 1;
                    summary.changed = true;
                }
                Err(error) => {
                    progress.lock().complete_item(0, true);
                    progress
                        .lock()
                        .record_error(format!("{directory}: {error}"));
                    summary.failed += 1;
                    summary.errors.push(format!("{directory}: {error}"));
                }
            }
        }
        for file in all_files {
            if summary.cancelled || !self.operation_scope_is_current(store, scope) {
                summary.cancelled = true;
                break;
            }
            let name = file
                .relative
                .rsplit('/')
                .next()
                .unwrap_or(&file.relative)
                .to_owned();
            let relative_dir = falcon_core::file_path::rel_dir(&file.relative);
            let remote_dir =
                falcon_core::file_path::join_host_path(Some(&scope.directory), &relative_dir);
            progress.lock().begin_item(file.relative.clone());
            if !self.mark_job_dispatched(job_id) {
                summary.cancelled = true;
                break;
            }
            let progress_cb = progress.clone();
            let mut result = client
                .upload_file(
                    &scope.project_id,
                    &remote_dir,
                    &name,
                    file.local.clone(),
                    false,
                    move |done, total| progress_cb.lock().set_current_bytes(done, total),
                )
                .await;
            if let Err(error) = &result
                && error.is_conflict()
            {
                let remote =
                    falcon_core::file_path::join_host_path(Some(&scope.directory), &file.relative);
                let confirmed = self
                    .ask_confirmation(
                        store,
                        scope.clone(),
                        ConfirmationKind::Overwrite { path: remote },
                    )
                    .await;
                if !confirmed || !self.operation_scope_is_current(store, scope) {
                    summary.cancelled = true;
                    break;
                }
                if !self.mark_job_dispatched(job_id) {
                    summary.cancelled = true;
                    break;
                }
                let progress_cb = progress.clone();
                result = client
                    .upload_file(
                        &scope.project_id,
                        &remote_dir,
                        &name,
                        file.local.clone(),
                        true,
                        move |done, total| progress_cb.lock().set_current_bytes(done, total),
                    )
                    .await;
            }
            match result {
                Ok(uploaded) => {
                    progress.lock().complete_item(uploaded.size, false);
                    summary.completed += 1;
                    summary.bytes_done += uploaded.size;
                    summary.changed = true;
                }
                Err(error) => {
                    progress.lock().complete_item(0, true);
                    progress
                        .lock()
                        .record_error(format!("{}: {error}", file.relative));
                    summary.failed += 1;
                    summary.errors.push(format!("{}: {error}", file.relative));
                }
            }
        }
        let snapshot = progress.lock().clone();
        summary.completed = snapshot.completed;
        summary.failed = snapshot.failed;
        summary.bytes_done = snapshot.bytes_done;
        summary.remaining = snapshot
            .total
            .saturating_sub(summary.completed + summary.failed);
        summary.errors = snapshot.errors;
        Some(summary)
    }

    fn update_job_total(&self, total: usize, bytes_total: u64) {
        self.job.update(|job| {
            if let Some(job) = job.as_mut() {
                job.total = total;
                job.bytes_total = bytes_total;
            }
        });
    }
}

#[derive(Default)]
struct JobSummary {
    completed: usize,
    failed: usize,
    skipped: usize,
    remaining: usize,
    bytes_done: u64,
    cancelled: bool,
    uncertain: bool,
    changed: bool,
    errors: Vec<String>,
}

#[derive(Clone, Default)]
struct ProgressSnapshot {
    completed: usize,
    failed: usize,
    total: usize,
    skipped: usize,
    bytes_done: u64,
    current_bytes: u64,
    current_item: String,
    errors: Vec<String>,
}

impl ProgressSnapshot {
    fn begin_item(&mut self, name: String) {
        self.current_item = name;
        self.current_bytes = 0;
    }

    fn set_current_bytes(&mut self, bytes: u64, _total: Option<u64>) {
        self.current_bytes = bytes;
    }

    fn complete_item(&mut self, bytes: u64, failed: bool) {
        if failed {
            self.failed += 1;
        } else {
            self.completed += 1;
        }
        self.bytes_done = self.bytes_done.saturating_add(bytes);
        self.current_bytes = 0;
        self.current_item.clear();
    }

    fn record_error(&mut self, error: String) {
        self.errors.push(error);
    }
}

fn summary_from_progress(
    progress: &ProgressSnapshot,
    cancelled: bool,
    uncertain: bool,
) -> JobSummary {
    JobSummary {
        completed: progress.completed,
        failed: progress.failed,
        skipped: progress.skipped,
        remaining: progress
            .total
            .saturating_sub(progress.completed + progress.failed),
        bytes_done: progress.bytes_done,
        cancelled,
        uncertain,
        changed: progress.completed > 0,
        errors: progress.errors.clone(),
    }
}

#[derive(Clone)]
struct UploadFile {
    local: PathBuf,
    name: String,
    remote: String,
    size: u64,
}

#[derive(Clone)]
struct LocalFile {
    local: PathBuf,
    relative: String,
    size: u64,
}

#[derive(Default)]
struct FolderScan {
    directories: Vec<String>,
    files: Vec<LocalFile>,
    skipped: usize,
    cancelled: bool,
}

fn collect_local_folders(
    roots: Vec<PathBuf>,
    remote_directory: &str,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> FolderScan {
    let mut scan = FolderScan::default();
    let mut pending = Vec::new();
    for root in roots {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            scan.cancelled = true;
            return scan;
        }
        match std::fs::symlink_metadata(&root) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => {
                scan.skipped += 1;
                continue;
            }
        }
        let Some(name) = root.file_name().and_then(|name| name.to_str()) else {
            scan.skipped += 1;
            continue;
        };
        if !valid_local_name(name, remote_directory) {
            scan.skipped += 1;
            continue;
        }
        let relative = PathBuf::from(name);
        pending.push((root, relative));
    }
    while let Some((directory, relative)) = pending.pop() {
        if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
            scan.cancelled = true;
            break;
        }
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            _ => {
                scan.skipped += 1;
                continue;
            }
        }
        let Some(relative_string) = local_relative_string(&relative) else {
            scan.skipped += 1;
            continue;
        };
        scan.directories.push(relative_string);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => {
                scan.skipped += 1;
                continue;
            }
        };
        for result in entries {
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                scan.cancelled = true;
                break;
            }
            let entry = match result {
                Ok(entry) => entry,
                Err(_) => {
                    scan.skipped += 1;
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    scan.skipped += 1;
                    continue;
                }
            };
            if file_type.is_symlink() {
                scan.skipped += 1;
                continue;
            }
            let name = match entry.file_name().into_string() {
                Ok(name) if valid_local_name(&name, remote_directory) => name,
                _ => {
                    scan.skipped += 1;
                    continue;
                }
            };
            let child_relative = relative.join(&name);
            if file_type.is_dir() {
                match std::fs::symlink_metadata(entry.path()) {
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                        pending.push((entry.path(), child_relative));
                    }
                    _ => scan.skipped += 1,
                }
            } else if file_type.is_file() {
                let Some(relative) = local_relative_string(&child_relative) else {
                    scan.skipped += 1;
                    continue;
                };
                match std::fs::symlink_metadata(entry.path()) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        scan.files.push(LocalFile {
                            local: entry.path(),
                            relative,
                            size: metadata.len(),
                        });
                    }
                    _ => scan.skipped += 1,
                }
            } else {
                scan.skipped += 1;
            }
        }
    }
    scan
}

fn local_relative_string(path: &Path) -> Option<String> {
    let parts: Vec<&str> = path
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<_>>()?;
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn valid_local_name(name: &str, remote_directory: &str) -> bool {
    is_valid_file_name_segment(name, remote_directory)
}

fn effective_remote_directory(store: &WorkspaceStore, scope: &FileScope) -> String {
    let project = store
        .projects
        .get()
        .into_iter()
        .find(|project| project.id == scope.project_id);
    effective_remote_directory_for_project(project.as_ref(), &scope.directory)
}

fn effective_remote_directory_for_project(
    project: Option<&ProjectRecord>,
    directory: &str,
) -> String {
    let root = project.and_then(|project| {
        project
            .source
            .as_ref()
            .and_then(project_nav_root)
            .or_else(|| (!project.path.is_empty()).then(|| project.path.clone()))
    });
    root.map_or_else(
        || directory.to_owned(),
        |root| join_host_path(Some(&root), directory),
    )
}

fn same_listing_location(left: &FileScope, right: &FileScope) -> bool {
    left.profile_id == right.profile_id
        && left.server_url == right.server_url
        && left.project_id == right.project_id
        && left.directory == right.directory
}

fn mutation_action(kind: JobKind) -> String {
    match kind {
        JobKind::Rename => crate::i18n::t!("files.operations.rename").to_string(),
        JobKind::CreateDirectory => crate::i18n::t!("files.operations.newFolder").to_string(),
        JobKind::Delete => crate::i18n::t!("files.operations.delete").to_string(),
        JobKind::Upload => crate::i18n::t!("files.operations.uploadAction").to_string(),
        JobKind::Download => crate::i18n::t!("files.operations.downloadAction").to_string(),
    }
}

fn remaining_after_remove(selected: &BTreeSet<String>, removed: &[String]) -> BTreeSet<String> {
    selected
        .iter()
        .filter(|path| !removed.iter().any(|removed| removed == *path))
        .cloned()
        .collect()
}

fn job_id_is_current(current_id: Option<u64>, finished_id: u64) -> bool {
    current_id == Some(finished_id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use falcon_core::file_path::is_valid_file_name_segment;

    use super::{
        effective_remote_directory_for_project, job_id_is_current, remaining_after_remove,
        FileOperations, JobKind,
    };
    use crate::store::ProjectRecord;

    fn windows_project() -> ProjectRecord {
        ProjectRecord {
            id: "windows-project".into(),
            name: "Windows project".into(),
            path: "C:\\work\\repo".into(),
            host_id: None,
            host_name: None,
            derived_from: None,
            branch: None,
            source: None,
        }
    }

    #[test]
    fn relative_windows_directory_keeps_windows_name_validation() {
        let project = windows_project();
        let root = effective_remote_directory_for_project(Some(&project), "");
        assert_eq!(root, "C:\\work\\repo");
        assert!(!is_valid_file_name_segment("bad:name", &root));
        let directory = effective_remote_directory_for_project(Some(&project), "src");
        assert_eq!(directory, "C:\\work\\repo\\src");
        assert!(!is_valid_file_name_segment("bad\\name", &directory));
        assert!(!is_valid_file_name_segment("bad:name", &directory));
        assert!(is_valid_file_name_segment("good.txt", &directory));

        let source = serde_json::from_value(serde_json::json!({
            "id": "windows-project",
            "name": "Windows project",
            "type": "local",
            "createdAt": 0,
            "workingDir": "D:\\fallback",
            "multi": {"repos": [{"dir": "C:\\multi-root"}]}
        }))
        .unwrap();
        let project = ProjectRecord {
            source: Some(source),
            path: "D:\\fallback".into(),
            ..project
        };
        assert_eq!(
            effective_remote_directory_for_project(Some(&project), "src"),
            "C:\\multi-root\\src"
        );
    }

    #[test]
    fn partial_remove_retains_failed_and_unreported_selection() {
        let selected = BTreeSet::from(["/a".into(), "/b".into(), "/c".into()]);
        let remaining = remaining_after_remove(&selected, &["/a".into()]);
        assert_eq!(remaining, BTreeSet::from(["/b".into(), "/c".into()]));
    }

    #[test]
    fn stale_job_completion_cannot_replace_a_new_job() {
        assert!(!job_id_is_current(Some(8), 7));
        assert!(job_id_is_current(Some(8), 8));
        assert!(!job_id_is_current(None, 8));
    }

    #[test]
    fn rename_and_delete_lock_opens_only_on_mutation_server() {
        assert!(FileOperations::mutation_blocks_file_open(
            JobKind::Rename,
            "https://server-a/",
            "https://server-a"
        ));
        assert!(FileOperations::mutation_blocks_file_open(
            JobKind::Delete,
            "https://server-a",
            "https://server-a/"
        ));
        assert!(!FileOperations::mutation_blocks_file_open(
            JobKind::Rename,
            "https://server-a",
            "https://server-b"
        ));
        assert!(!FileOperations::mutation_blocks_file_open(
            JobKind::CreateDirectory,
            "https://server-a",
            "https://server-a"
        ));
    }
}
