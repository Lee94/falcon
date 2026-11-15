use std::collections::HashMap;

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::FalconClient;
use falcon_proto::SessionState;
use nsh_core::prelude::{Owner, Signal, signal, spawn};

use crate::i18n::t;
use crate::store::{ActiveDialog, SessionRecord, WorkspaceStore};

#[derive(Clone)]
pub struct SessionOperations {
    root_owner: Owner,
    connection_epoch: Signal<u64>,
    server_url: Signal<String>,
    next_id: Signal<u64>,
    busy: Signal<HashMap<(String, String), u64>>,
    form_errors: Signal<HashMap<u64, String>>,
    rename_forms: Signal<HashMap<u64, RenameForm>>,
}

#[derive(Clone, PartialEq, Eq)]
struct RenameForm {
    epoch: u64,
    server_url: String,
    identity: SessionIdentity,
    initial_name: String,
}

#[derive(Clone, PartialEq, Eq)]
struct SessionIdentity {
    id: String,
}

impl SessionIdentity {
    fn from_record(record: &SessionRecord) -> Self {
        Self {
            id: record.id.clone(),
        }
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        self.id == record.id
    }
}

impl SessionOperations {
    pub fn new(root_owner: Owner, server_url: Signal<String>) -> Self {
        Self {
            root_owner,
            connection_epoch: signal(0),
            server_url,
            next_id: signal(1),
            busy: signal(HashMap::new()),
            form_errors: signal(HashMap::new()),
            rename_forms: signal(HashMap::new()),
        }
    }

    pub fn begin_connection(&self, server_connected: Signal<bool>) {
        self.connection_epoch.update(|epoch| {
            *epoch = epoch
                .checked_add(1)
                .expect("session connection epoch exhausted");
        });
        self.form_errors.set(HashMap::new());
        server_connected.set(false);
    }

    pub fn open_rename(&self, store: &WorkspaceStore, session_id: &str) {
        let epoch = self.connection_epoch.get_untracked();
        let server_url = crate::project_operations::canonical_server_url(
            &store.server_url.get_untracked(),
        );
        if !self.connection_ready(store, epoch, &server_url) {
            store.show_toast(t!("session.notConnected"));
            return;
        }
        let Some(record) = store
            .sessions
            .get_untracked()
            .into_iter()
            .find(|record| record.id == session_id && record.state != SessionState::Dead)
        else {
            return;
        };
        let form_id = self.next_id();
        self.rename_forms.update(|forms| {
            forms.clear();
            forms.insert(
                form_id,
                RenameForm {
                    epoch,
                    server_url: crate::project_operations::canonical_server_url(
                        &store.server_url.get_untracked(),
                    ),
                    identity: SessionIdentity::from_record(&record),
                    initial_name: record.name,
                },
            );
        });
        self.form_errors.set(HashMap::new());
        store.active_dialog.set(ActiveDialog::RenameSession {
            session_id: session_id.to_string(),
            form_id,
        });
    }

    pub fn initial_name(&self, form_id: u64) -> String {
        self.rename_forms
            .get_untracked()
            .get(&form_id)
            .map(|form| form.initial_name.clone())
            .unwrap_or_default()
    }

    pub fn is_busy(&self, session_id: &str) -> bool {
        let url = crate::project_operations::canonical_server_url(&self.server_url.get());
        self.busy.get().contains_key(&(url, session_id.to_string()))
    }

    pub fn form_error(&self, form_id: u64) -> Option<String> {
        self.form_errors.get().get(&form_id).cloned()
    }

    pub fn clear_form_error(&self, form_id: u64) {
        if self.form_errors.get_untracked().contains_key(&form_id) {
            self.form_errors.update(|errors| {
                errors.remove(&form_id);
            });
        }
    }

    pub fn close_rename(&self, store: &WorkspaceStore, session_id: &str, form_id: u64) {
        if !self.rename_form_is_active(store, session_id, form_id) {
            return;
        }
        self.remove_form_state(form_id);
        store.active_dialog.set(ActiveDialog::None);
    }

    pub fn submit_rename(
        &self,
        store: WorkspaceStore,
        session_id: String,
        form_id: u64,
        raw_name: String,
    ) {
        if !self.rename_form_is_active(&store, &session_id, form_id) {
            return;
        }
        self.clear_form_error(form_id);
        let Some(form) = self.rename_forms.get_untracked().get(&form_id).cloned() else {
            self.set_form_error_if_active(
                &store,
                &session_id,
                form_id,
                t!("session.notConnected"),
            );
            return;
        };
        if form.identity.id != session_id {
            return;
        }
        if !self.connection_ready(&store, form.epoch, &form.server_url) {
            self.set_form_error_if_active(
                &store,
                &session_id,
                form_id,
                t!("session.notConnected"),
            );
            return;
        }
        if !find_record(&store, &form.identity)
            .is_some_and(|record| record.state != SessionState::Dead)
        {
            self.set_form_error_if_active(
                &store,
                &session_id,
                form_id,
                t!("session.renameFailed"),
            );
            return;
        }
        let Some(target) = ApiTarget::capture(&store) else {
            self.set_form_error_if_active(
                &store,
                &session_id,
                form_id,
                t!("session.notConnected"),
            );
            return;
        };
        let operation_id = self.next_id();
        if !self.try_start(&target.server_url, &session_id, operation_id) {
            return;
        }

        let name = raw_name.trim().to_string();
        let operations = self.clone();
        let operation_store = store.clone();
        let identity = form.identity;
        let epoch = form.epoch;
        let server_url = form.server_url;
        self.root_owner.run(move || {
            spawn(async move {
                let record = find_record(&operation_store, &identity);
                if !operations.connection_ready(&operation_store, epoch, &server_url)
                    || !operations.rename_form_is_active(
                        &operation_store,
                        &session_id,
                        form_id,
                    )
                    || !record.is_some_and(|record| record.state != SessionState::Dead)
                {
                    operations.finish(&server_url, &session_id, operation_id);
                    return;
                }

                operation_store.bump_metadata_revision(&server_url);
                let result = target.rename_session(&identity.id, &name).await;
                operation_store.bump_metadata_revision(&server_url);
                if !operations.connection_ready(&operation_store, epoch, &server_url)
                    || find_record(&operation_store, &identity).is_none()
                {
                    operations.finish(&server_url, &session_id, operation_id);
                    return;
                }

                match result {
                    Ok(()) => {
                        let mut updated = false;
                        let saved_name = name.clone();
                        operation_store.sessions.update(|sessions| {
                            if let Some(record) = sessions
                                .iter_mut()
                                .find(|record| identity.matches(record))
                            {
                                record.name = saved_name;
                                updated = true;
                            }
                        });
                        if updated {
                            if operations.rename_form_is_active(
                                &operation_store,
                                &session_id,
                                form_id,
                            ) {
                                operations.close_rename(
                                    &operation_store,
                                    &session_id,
                                    form_id,
                                );
                            } else {
                                operations.remove_form_state(form_id);
                            }
                        }
                    }
                    Err(()) => {
                        let message = t!("session.renameFailed");
                        if operations.rename_form_is_active(
                            &operation_store,
                            &session_id,
                            form_id,
                        ) {
                            operations.set_form_error_if_active(
                                &operation_store,
                                &session_id,
                                form_id,
                                message,
                            );
                        } else {
                            operation_store.show_toast(message);
                        }
                    }
                }
                operations.finish(&server_url, &session_id, operation_id);
            });
        });
    }

    pub fn clear_record(&self, store: WorkspaceStore, session_id: &str) {
        let epoch = self.connection_epoch.get_untracked();
        if !self.connection_ready(&store, epoch, &store.server_url.get_untracked()) {
            store.show_toast(t!("session.notConnected"));
            return;
        }
        let Some(record) = store
            .sessions
            .get_untracked()
            .into_iter()
            .find(|record| record.id == session_id && record.state == SessionState::Dead)
        else {
            return;
        };
        let identity = SessionIdentity::from_record(&record);
        let Some(target) = ApiTarget::capture(&store) else {
            store.show_toast(t!("session.notConnected"));
            return;
        };
        let operation_id = self.next_id();
        let server_url = target.server_url.clone();
        if !self.try_start(&server_url, session_id, operation_id) {
            return;
        }

        let operations = self.clone();
        let operation_store = store.clone();
        let session_id = session_id.to_string();
        let root_owner = self.root_owner;
        root_owner.run(move || {
            spawn(async move {
                let record = find_record(&operation_store, &identity);
                if !operations.connection_ready(&operation_store, epoch, &server_url)
                    || !record.is_some_and(|record| record.state == SessionState::Dead)
                {
                    operations.finish(&server_url, &session_id, operation_id);
                    return;
                }

                operation_store.bump_metadata_revision(&server_url);
                let result = target.clear_record(&identity.id).await;
                operation_store.bump_metadata_revision(&server_url);
                if !operations.connection_ready(&operation_store, epoch, &server_url)
                    || find_record(&operation_store, &identity).is_none()
                {
                    operations.finish(&server_url, &session_id, operation_id);
                    return;
                }

                match result {
                    Ok(()) => {
                        let mut removed = false;
                        operation_store.sessions.update(|sessions| {
                            let is_dead = sessions.iter().any(|record| {
                                identity.matches(record) && record.state == SessionState::Dead
                            });
                            if is_dead {
                                sessions.retain(|record| !identity.matches(record));
                                removed = true;
                            }
                        });
                        if removed {
                            operation_store.detach_pane(&falcon_core::pane_key::term_key(
                                &session_id,
                            ));
                        }
                    }
                    Err(()) => operation_store.show_toast(t!("session.clearRecordFailed")),
                }
                operations.finish(&server_url, &session_id, operation_id);
            });
        });
    }

    fn connection_ready(&self, store: &WorkspaceStore, epoch: u64, server_url: &str) -> bool {
        epoch != 0
            && self.connection_epoch.get_untracked() == epoch
            && store.server_connected.get_untracked()
            && crate::project_operations::canonical_server_url(&store.server_url.get_untracked())
                == crate::project_operations::canonical_server_url(server_url)
    }

    fn rename_form_is_active(&self, store: &WorkspaceStore, session_id: &str, form_id: u64) -> bool {
        store.active_dialog.get_untracked()
            == (ActiveDialog::RenameSession {
                session_id: session_id.to_string(),
                form_id,
            })
            && self.rename_forms.get_untracked().contains_key(&form_id)
    }

    fn set_form_error_if_active(
        &self,
        store: &WorkspaceStore,
        session_id: &str,
        form_id: u64,
        message: String,
    ) {
        if self.rename_form_is_active(store, session_id, form_id) {
            self.form_errors.update(|errors| {
                errors.insert(form_id, message);
            });
        }
    }

    fn remove_form_state(&self, form_id: u64) {
        self.rename_forms.update(|forms| {
            forms.remove(&form_id);
        });
        self.form_errors.update(|errors| {
            errors.remove(&form_id);
        });
    }

    fn next_id(&self) -> u64 {
        let id = self.next_id.get_untracked();
        self.next_id.set(
            id.checked_add(1)
                .expect("session operation id exhausted"),
        );
        id
    }

    fn try_start(&self, server_url: &str, session_id: &str, operation_id: u64) -> bool {
        let key = (
            crate::project_operations::canonical_server_url(server_url),
            session_id.to_string(),
        );
        let mut started = false;
        self.busy.update(|busy| {
            if !busy.contains_key(&key) {
                busy.insert(key, operation_id);
                started = true;
            }
        });
        started
    }

    fn finish(&self, server_url: &str, session_id: &str, operation_id: u64) {
        let key = (
            crate::project_operations::canonical_server_url(server_url),
            session_id.to_string(),
        );
        self.busy.update(|busy| {
            if busy.get(&key) == Some(&operation_id) {
                busy.remove(&key);
            }
        });
    }
}

fn find_record(store: &WorkspaceStore, identity: &SessionIdentity) -> Option<SessionRecord> {
    store
        .sessions
        .get_untracked()
        .into_iter()
        .find(|record| identity.matches(record))
}

#[cfg(not(target_arch = "wasm32"))]
struct ApiTarget {
    client: FalconClient,
    server_url: String,
}

#[cfg(target_arch = "wasm32")]
struct ApiTarget {
    base: String,
    server_url: String,
}

impl ApiTarget {
    fn capture(store: &WorkspaceStore) -> Option<Self> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            store.client().map(|client| Self {
                client,
                server_url: crate::project_operations::canonical_server_url(
                    &store.server_url.get_untracked(),
                ),
            })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let server_url = crate::project_operations::canonical_server_url(
                &store.server_url.get_untracked(),
            );
            (!server_url.is_empty()).then_some(Self {
                base: server_url.clone(),
                server_url,
            })
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn rename_session(&self, session_id: &str, name: &str) -> Result<(), ()> {
        self.client
            .rename_session(session_id, name)
            .await
            .map(|_| ())
            .map_err(|_| ())
    }

    #[cfg(target_arch = "wasm32")]
    async fn rename_session(&self, session_id: &str, name: &str) -> Result<(), ()> {
        let body = serde_json::to_vec(&serde_json::json!({ "name": name })).map_err(|_| ())?;
        self.web_request("PATCH", session_id, Some(body)).await
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn clear_record(&self, session_id: &str) -> Result<(), ()> {
        self.client
            .clear_session(session_id)
            .await
            .map(|_| ())
            .map_err(|_| ())
    }

    #[cfg(target_arch = "wasm32")]
    async fn clear_record(&self, session_id: &str) -> Result<(), ()> {
        self.web_request("DELETE", session_id, None).await
    }

    #[cfg(target_arch = "wasm32")]
    async fn web_request(
        &self,
        method: &str,
        session_id: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(), ()> {
        use nsh_core::prelude::{HttpRequest, fetch};

        let url = format!(
            "{}/api/sessions/{}",
            self.base,
            encode_path_segment(session_id)
        );
        let mut request = HttpRequest::new(method, url);
        if let Some(body) = body {
            request = request.header("Content-Type", "application/json").body(body);
        }
        let response = fetch(request).await.map_err(|_| ())?;
        response.ok().then_some(()).ok_or(())
    }
}

#[cfg(target_arch = "wasm32")]
fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsh_core::element::reactive::{Runtime, create_root};

    #[test]
    fn busy_writes_survive_a_to_b_to_a_and_finish_only_matching_tokens() {
        let runtime = Runtime::default();
        let _entered = runtime.enter();
        let (root, (operations, connected)) = create_root(|| {
            let server_url = signal("http://server-a/".to_string());
            let connected = signal(true);
            let operations = SessionOperations::new(Owner::current(), server_url);
            (operations, connected)
        });

        assert!(operations.try_start("http://server-a", "session-1", 1));

        operations.begin_connection(connected);
        operations.server_url.set("http://server-b".into());
        assert!(!operations.is_busy("session-1"));
        assert!(operations.try_start("http://server-b", "session-1", 2));

        operations.begin_connection(connected);
        operations.server_url.set("http://server-a".into());
        assert!(operations.is_busy("session-1"));
        assert!(!operations.try_start("http://server-a/", "session-1", 3));
        operations.finish("http://server-a/", "session-1", 3);
        assert!(operations.is_busy("session-1"));

        operations.finish("http://server-a", "session-1", 1);
        assert!(!operations.is_busy("session-1"));
        operations.server_url.set("http://server-b".into());
        assert!(operations.is_busy("session-1"));
        operations.finish("http://server-b", "session-1", 2);
        assert!(!operations.is_busy("session-1"));
        root.dispose();
    }
}
