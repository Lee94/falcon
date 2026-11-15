//! Upgrade jobs belong to a gateway/node, not to the modal that displays them.
use std::collections::HashMap;
use std::time::Duration;

use falcon_proto::{ServerUpdateCheck, ServerUpdatePhase, ServerUpdateTask};
use nsh_core::prelude::{Owner, Signal, signal, spawn, timer};

use crate::i18n::t;
use crate::store::{ActiveDialog, WorkspaceStore};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerUpdateKey {
    pub gateway: String,
    pub host_id: Option<String>,
}

#[derive(Clone, Default, PartialEq)]
pub(crate) struct UpdateState {
    pub label: String,
    pub check: Option<ServerUpdateCheck>,
    pub task: Option<ServerUpdateTask>,
    pub checking: bool,
    pub running: bool,
    pub reconnecting: bool,
    pub error: Option<String>,
    pub confirming: bool,
}

impl UpdateState {
    pub fn available_version(&self) -> Option<String> {
        if self.checking || self.running {
            return None;
        }
        self.check
            .as_ref()
            .filter(|check| check.available && check.error.is_none())
            .and_then(|check| check.latest_version.clone())
    }
}

#[derive(Clone)]
pub(crate) struct ServerUpdates {
    owner: Owner,
    states: Signal<HashMap<ServerUpdateKey, UpdateState>>,
}

fn active(phase: ServerUpdatePhase) -> bool {
    !matches!(
        phase,
        ServerUpdatePhase::Idle | ServerUpdatePhase::Succeeded | ServerUpdatePhase::Failed
    )
}

impl ServerUpdates {
    pub fn new(owner: Owner) -> Self {
        Self {
            owner,
            states: signal(HashMap::new()),
        }
    }

    pub fn state(&self, key: &ServerUpdateKey) -> UpdateState {
        self.states.get().get(key).cloned().unwrap_or_default()
    }

    fn update(&self, key: &ServerUpdateKey, f: impl FnOnce(&mut UpdateState)) {
        self.states
            .update(|states| f(states.entry(key.clone()).or_default()));
    }

    pub fn open(&self, store: &WorkspaceStore, host_id: Option<String>) {
        let gateway =
            crate::project_operations::canonical_server_url(&store.server_url.get_untracked());
        let label = if let Some(id) = &host_id {
            let Some(host) = store
                .hosts
                .get_untracked()
                .into_iter()
                .find(|host| &host.id == id)
            else {
                return;
            };
            format!("{} · {}", host.name, host.destination)
        } else {
            gateway.clone()
        };
        let key = ServerUpdateKey { gateway, host_id };
        self.update(&key, |state| {
            state.label = label;
            state.confirming = false;
        });
        store
            .active_dialog
            .set(ActiveDialog::ServerUpdate(key.clone()));
        self.refresh(store, &key);
    }

    pub fn confirm(&self, key: &ServerUpdateKey, confirming: bool) {
        self.update(key, |state| {
            state.confirming = confirming && state.available_version().is_some()
        });
    }

    pub fn refresh(&self, store: &WorkspaceStore, key: &ServerUpdateKey) {
        let state = self.state(key);
        if state.checking || state.running {
            return;
        }
        let Some(target) = Target::capture(store, key) else {
            self.update(key, |state| {
                state.check = None;
                state.error = Some(t!("native.nsh.serverNotConnected"));
            });
            return;
        };
        self.update(key, |state| {
            state.checking = true;
            state.error = None;
            state.confirming = false;
        });
        let (updates, store) = (self.clone(), store.clone());
        // ModalLayer's dynamic owner is disposed on close. Spawn under the workspace owner instead.
        self.owner.run(move || {
            spawn(async move {
                match target.status().await {
                    Ok(task) if active(task.phase) => {
                        updates.update(&target.key, |state| {
                            state.checking = false;
                            state.running = true;
                            state.reconnecting = false;
                            state.task = Some(task);
                        });
                        updates.poll(&store, &target).await;
                    }
                    Ok(task) => {
                        let check = target.check().await;
                        updates.update(&target.key, |state| {
                            state.task = Some(task);
                            state.checking = false;
                            state.reconnecting = false;
                            apply_check(state, check);
                        });
                    }
                    Err(error) => updates.update(&target.key, |state| {
                        state.checking = false;
                        state.check = None;
                        state.error = Some(error.message);
                    }),
                }
            })
        });
    }

    pub fn install(&self, store: &WorkspaceStore, key: &ServerUpdateKey, version: &str) {
        let state = self.state(key);
        if !state.confirming || state.available_version().as_deref() != Some(version) {
            return;
        }
        let Some(target) = Target::capture(store, key) else {
            return;
        };
        let version = version.to_owned();
        self.update(key, |state| {
            state.running = true;
            state.confirming = false;
            state.reconnecting = false;
            state.error = None;
            state.task = Some(ServerUpdateTask {
                phase: ServerUpdatePhase::Downloading,
                target_version: Some(version.clone()),
                error: None,
            });
        });
        let (updates, store) = (self.clone(), store.clone());
        self.owner.run(move || {
            spawn(async move {
                // A lost response may mean the server already restarted. Never replay this POST.
                match target.start(&version).await {
                    Ok(task) => {
                        if !updates.observe(&store, &target, task).await {
                            return;
                        }
                    }
                    Err(error)
                        if error
                            .status
                            .is_some_and(|s| (400..500).contains(&s) && s != 409) =>
                    {
                        updates.update(&target.key, |state| {
                            state.running = false;
                            state.check = None;
                            state.task = None;
                            state.error = Some(error.message);
                        });
                        return;
                    }
                    Err(error) => updates.disconnected(&target.key, error),
                }
                updates.poll(&store, &target).await;
            })
        });
    }

    async fn poll(&self, store: &WorkspaceStore, target: &Target) {
        loop {
            timer(Duration::from_secs(2)).await;
            match target.status().await {
                Ok(task) => {
                    if !self.observe(store, target, task).await {
                        break;
                    }
                }
                Err(error) => self.disconnected(&target.key, error),
            }
        }
    }

    fn disconnected(&self, key: &ServerUpdateKey, error: UpdateError) {
        self.update(key, |state| {
            state.reconnecting = true;
            state.error = Some(error.message);
        });
    }

    async fn observe(
        &self,
        store: &WorkspaceStore,
        target: &Target,
        task: ServerUpdateTask,
    ) -> bool {
        let phase = task.phase;
        self.update(&target.key, |state| {
            state.reconnecting = false;
            state.error = None;
            state.task = Some(task.clone());
        });
        if active(phase) {
            return true;
        }
        if phase == ServerUpdatePhase::Idle {
            self.update(&target.key, |state| {
                state.running = false;
                state.check = None;
                state.task = None;
                state.error = Some(t!("native.serverUpdate.missingTask"));
            });
            return false;
        }
        let check = target.check().await;
        let version = check
            .as_ref()
            .ok()
            .map(|check| check.current_version.clone());
        self.update(&target.key, |state| {
            state.running = false;
            apply_check(state, check);
        });
        // An old gateway finishing must not overwrite the currently connected gateway's metadata.
        if crate::project_operations::canonical_server_url(&store.server_url.get_untracked())
            == target.key.gateway
        {
            if target.key.host_id.is_none() {
                if let Some(version) = version {
                    store.server_info.update(|info| {
                        if let Some(info) = info {
                            info.version = version;
                        }
                    });
                }
            }
            let message = t!(
                phase_key(phase),
                version = task.target_version.unwrap_or_else(|| "—".into())
            );
            let label = self.state(&target.key).label;
            store.show_toast(format!(
                "{label}\n{message}{}",
                task.error
                    .map(|error| format!("\n{error}"))
                    .unwrap_or_default()
            ));
        }
        false
    }
}

fn apply_check(state: &mut UpdateState, check: Result<ServerUpdateCheck, UpdateError>) {
    match check {
        Ok(check) => {
            state.error = check.error.clone();
            state.check = Some(check);
        }
        Err(error) => {
            state.error = Some(error.message);
            state.check = None;
        }
    }
}

pub(crate) fn phase_key(phase: ServerUpdatePhase) -> &'static str {
    match phase {
        ServerUpdatePhase::Idle => "native.serverUpdate.phase_idle",
        ServerUpdatePhase::Downloading => "native.serverUpdate.phase_downloading",
        ServerUpdatePhase::Verifying => "native.serverUpdate.phase_verifying",
        ServerUpdatePhase::Installing => "native.serverUpdate.phase_installing",
        ServerUpdatePhase::Restarting => "native.serverUpdate.phase_restarting",
        ServerUpdatePhase::Succeeded => "native.serverUpdate.phase_succeeded",
        ServerUpdatePhase::Failed => "native.serverUpdate.phase_failed",
        ServerUpdatePhase::Unknown => "native.serverUpdate.phase_unknown",
    }
}

struct UpdateError {
    status: Option<u16>,
    message: String,
}

struct Target {
    key: ServerUpdateKey,
    #[cfg(not(target_arch = "wasm32"))]
    client: falcon_client::FalconClient,
}

impl Target {
    fn capture(store: &WorkspaceStore, key: &ServerUpdateKey) -> Option<Self> {
        if crate::project_operations::canonical_server_url(&store.server_url.get_untracked())
            != key.gateway
        {
            return None;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let client = store.client()?;
            if crate::project_operations::canonical_server_url(client.base_url()) != key.gateway {
                return None;
            }
            Some(Self {
                key: key.clone(),
                client,
            })
        }
        #[cfg(target_arch = "wasm32")]
        Some(Self { key: key.clone() })
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn status(&self) -> Result<ServerUpdateTask, UpdateError> {
        self.client
            .server_update_status(self.key.host_id.as_deref())
            .await
            .map_err(Into::into)
    }
    #[cfg(not(target_arch = "wasm32"))]
    async fn check(&self) -> Result<ServerUpdateCheck, UpdateError> {
        self.client
            .check_server_update(self.key.host_id.as_deref())
            .await
            .map_err(Into::into)
    }
    #[cfg(not(target_arch = "wasm32"))]
    async fn start(&self, version: &str) -> Result<ServerUpdateTask, UpdateError> {
        self.client
            .start_server_update(self.key.host_id.as_deref(), version)
            .await
            .map_err(Into::into)
    }

    #[cfg(target_arch = "wasm32")]
    async fn status(&self) -> Result<ServerUpdateTask, UpdateError> {
        self.request("GET", true, None).await
    }
    #[cfg(target_arch = "wasm32")]
    async fn check(&self) -> Result<ServerUpdateCheck, UpdateError> {
        self.request("GET", false, None).await
    }
    #[cfg(target_arch = "wasm32")]
    async fn start(&self, version: &str) -> Result<ServerUpdateTask, UpdateError> {
        self.request(
            "POST",
            false,
            Some(
                serde_json::json!({ "version": version })
                    .to_string()
                    .into_bytes(),
            ),
        )
        .await
    }

    #[cfg(target_arch = "wasm32")]
    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        status: bool,
        body: Option<Vec<u8>>,
    ) -> Result<T, UpdateError> {
        use nsh_core::prelude::{HttpRequest, fetch};
        let failed = || UpdateError {
            status: None,
            message: t!("native.serverUpdate.requestFailed"),
        };
        let mut url =
            url::Url::parse(&format!("{}/api/", self.key.gateway)).map_err(|_| failed())?;
        {
            let mut path = url.path_segments_mut().map_err(|_| failed())?;
            path.pop_if_empty();
            if let Some(host) = &self.key.host_id {
                path.extend(["hosts", host.as_str(), "node"]);
            }
            path.extend(["system", "update"]);
            if status {
                path.push("status");
            }
        }
        let mut request = HttpRequest::new(method, url.to_string());
        if let Some(body) = body {
            request = request
                .header("Content-Type", "application/json")
                .body(body);
        }
        let response = fetch(request).await.map_err(|_| failed())?;
        if !response.ok() {
            let message = serde_json::from_slice::<serde_json::Value>(&response.body)
                .ok()
                .and_then(|body| {
                    body.get("error")
                        .and_then(|error| error.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| t!("native.serverUpdate.requestFailed"));
            return Err(UpdateError {
                status: Some(response.status()),
                message,
            });
        }
        serde_json::from_slice(&response.body).map_err(|_| failed())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<falcon_client::ApiError> for UpdateError {
    fn from(error: falcon_client::ApiError) -> Self {
        Self {
            status: error.status,
            message: error.message,
        }
    }
}
