//! WS 与 REST 兜底共用队列；密码仅留在当前弹窗与答复请求中。

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use falcon_proto::AskpassPrompt;
use nsh_core::element::desktop::main_window;
use nsh_core::platform::Lifecycle;
use nsh_core::prelude::*;

use super::{ActiveDialog, WorkspaceStore};

#[derive(Clone)]
pub(crate) struct AskpassState {
    queue: Signal<VecDeque<AskpassPrompt>>,
    pub secret: Signal<String>,
    pub busy: Signal<bool>,
    pub error: Signal<Option<String>>,
    completed: Rc<RefCell<HashSet<String>>>,
    epoch: Rc<Cell<u64>>,
    poll_in_flight: Rc<Cell<Option<u64>>>,
}

impl AskpassState {
    pub fn new() -> Self {
        Self {
            queue: signal(VecDeque::new()),
            secret: signal(String::new()),
            busy: signal(false),
            error: signal(None),
            completed: Rc::new(RefCell::new(HashSet::new())),
            epoch: Rc::new(Cell::new(0)),
            poll_in_flight: Rc::new(Cell::new(None)),
        }
    }
}

impl WorkspaceStore {
    pub(super) fn install_askpass(&self) {
        let store = self.clone();
        effect(move || {
            let active = store.server_connected.get()
                && lifecycle() == Lifecycle::Active
                && main_window()
                    .state()
                    .is_none_or(|s| s.focused && s.visible && !s.minimized);
            if active {
                store.pull_askpass();
                let poll = store.clone();
                set_interval(Duration::from_millis(1500), move || poll.pull_askpass());
                let ws = store.clone();
                set_interval(Duration::from_millis(100), move || {
                    ws.drain_askpass_events()
                });
            }
        });
        let store = self.clone();
        effect(move || {
            // 不抢占其他表单；它关闭后再展示队头，避免丢掉未保存的编辑。
            let dialog = store.active_dialog.get();
            if !matches!(dialog, ActiveDialog::Askpass { .. }) {
                store.askpass.secret.set(String::new());
            }
            if dialog == ActiveDialog::None {
                store.open_askpass_head();
            }
        });
    }

    pub(crate) fn enqueue_askpass(&self, id: String, prompt: String) {
        if self.askpass.completed.borrow().contains(&id)
            || self
                .askpass
                .queue
                .get_untracked()
                .iter()
                .any(|p| p.id == id)
        {
            return;
        }
        self.askpass
            .queue
            .update(|queue| queue.push_back(AskpassPrompt { id, prompt }));
        self.open_askpass_head();
    }

    fn open_askpass_head(&self) {
        if self.active_dialog.get_untracked() != ActiveDialog::None {
            return;
        }
        if let Some(head) = self.askpass.queue.get().front().cloned() {
            self.askpass.secret.set(String::new());
            self.askpass.error.set(None);
            self.active_dialog.set(ActiveDialog::Askpass {
                id: head.id,
                prompt: head.prompt,
            });
        }
    }

    pub(crate) fn drain_askpass_events(&self) {
        let pending: Vec<_> = self
            .terminals
            .map
            .lock()
            .values()
            .flat_map(|session| session.take_askpass_pending())
            .collect();
        for (id, prompt) in pending {
            self.enqueue_askpass(id, prompt);
        }
    }

    fn pull_askpass(&self) {
        let epoch = self.askpass.epoch.get();
        if self.askpass.poll_in_flight.get() == Some(epoch) {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let Some(client) = self.client() else { return };
        #[cfg(target_arch = "wasm32")]
        let base = self
            .server_url
            .get_untracked()
            .trim_end_matches('/')
            .to_string();
        self.askpass.poll_in_flight.set(Some(epoch));
        let store = self.clone();
        self.spawn_root(async move {
            #[cfg(not(target_arch = "wasm32"))]
            let result = client.pending_askpass().await.map_err(|_| ());
            #[cfg(target_arch = "wasm32")]
            let result = async {
                let response = fetch(HttpRequest::get(format!("{base}/api/askpass/pending")))
                    .await
                    .map_err(|_| ())?;
                if !response.ok() {
                    return Err(());
                }
                serde_json::from_slice::<Vec<AskpassPrompt>>(&response.body).map_err(|_| ())
            }
            .await;
            if store.askpass.poll_in_flight.get() == Some(epoch) {
                store.askpass.poll_in_flight.set(None);
            }
            if store.askpass.epoch.get() != epoch
                || !store.server_connected.get_untracked()
                || lifecycle() != Lifecycle::Active
                || main_window()
                    .state()
                    .is_some_and(|s| !s.focused || !s.visible || s.minimized)
            {
                return;
            }
            if let Ok(prompts) = result {
                for prompt in prompts {
                    store.enqueue_askpass(prompt.id, prompt.prompt);
                }
            }
        });
    }

    pub(crate) fn answer_askpass(&self, id: &str, cancel: bool) {
        if self.askpass.busy.get_untracked()
            || !matches!(self.active_dialog.get_untracked(), ActiveDialog::Askpass { id: current, .. } if current == id)
            || !self
                .askpass
                .queue
                .get_untracked()
                .front()
                .is_some_and(|p| p.id == id)
        {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        let Some(client) = self.client() else { return };
        #[cfg(target_arch = "wasm32")]
        let base = self
            .server_url
            .get_untracked()
            .trim_end_matches('/')
            .to_string();
        let password = self.askpass.secret.get_untracked();
        self.askpass.secret.set(String::new());
        self.askpass.busy.set(true);
        self.askpass.error.set(None);
        let epoch = self.askpass.epoch.get();
        let id = id.to_string();
        let store = self.clone();
        self.spawn_root(async move {
            #[cfg(not(target_arch = "wasm32"))]
            let result = {
                let result = if cancel {
                    client.cancel_askpass(&id).await
                } else {
                    client.answer_askpass(&id, &password).await
                };
                match result {
                    Ok(_) => Ok(()),
                    Err(error) if error.is_not_found() => Ok(()),
                    Err(_) => Err(()),
                }
            };
            #[cfg(target_arch = "wasm32")]
            let result = async {
                let mut endpoint =
                    url::Url::parse(&format!("{base}/api/askpass/")).map_err(|_| ())?;
                endpoint
                    .path_segments_mut()
                    .map_err(|_| ())?
                    .pop_if_empty()
                    .push(&id)
                    .push("answer");
                let body = if cancel {
                    serde_json::json!({"cancel": true})
                } else {
                    serde_json::json!({"password": password})
                };
                let response = fetch(
                    HttpRequest::post(endpoint.to_string(), body.to_string().into_bytes())
                        .header("Content-Type", "application/json"),
                )
                .await
                .map_err(|_| ())?;
                if response.ok() || response.status() == 404 {
                    Ok(())
                } else {
                    Err(())
                }
            }
            .await;
            if store.askpass.epoch.get() != epoch {
                return;
            }
            store.askpass.busy.set(false);
            if result.is_err() {
                store
                    .askpass
                    .error
                    .set(Some(crate::i18n::t!("native.askpass.failed")));
                return;
            }
            store.askpass.completed.borrow_mut().insert(id);
            store.askpass.queue.update(|queue| {
                queue.pop_front();
            });
            if matches!(
                store.active_dialog.get_untracked(),
                ActiveDialog::Askpass { .. }
            ) {
                store.active_dialog.set(ActiveDialog::None);
            }
            store.open_askpass_head();
        });
    }

    pub(super) fn reset_askpass(&self) {
        self.askpass
            .epoch
            .set(self.askpass.epoch.get().wrapping_add(1));
        self.askpass.queue.set(VecDeque::new());
        self.askpass.secret.set(String::new());
        self.askpass.busy.set(false);
        self.askpass.error.set(None);
        self.askpass.completed.borrow_mut().clear();
        if matches!(
            self.active_dialog.get_untracked(),
            ActiveDialog::Askpass { .. }
        ) {
            self.active_dialog.set(ActiveDialog::None);
        }
    }
}
