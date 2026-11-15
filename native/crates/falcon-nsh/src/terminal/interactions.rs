//! Terminal actions shared by the pointer surface, IME sink and context menu.
use std::sync::Arc;

use nsh_core::prelude::*;

use super::{TerminalSession, session::ConnectionState};
use crate::{
    i18n::t,
    shell::popups::{PopupAction, ServerUrlGuard},
    store::WorkspaceStore,
    theme::{rgb_to_hsla, use_theme},
    zoom::zpx,
};

pub(super) fn copy(session: &TerminalSession) {
    if let Some(text) = session.core().selection_text() {
        set_clipboard(text);
    }
}

pub(super) fn context_actions(
    store: &WorkspaceStore,
    session: &Arc<TerminalSession>,
) -> Vec<PopupAction> {
    let guard = ServerUrlGuard::capture(store);
    let copy_session = session.clone();
    let paste_session = session.clone();
    let paste_store = store.clone();
    let clear_session = session.clone();
    vec![
        PopupAction::new(
            guard.clone(),
            t!("native.terminal.copy"),
            !session.core().has_selection(),
            false,
            move || copy(&copy_session),
        ),
        PopupAction::new(
            guard.clone(),
            t!("native.terminal.paste"),
            !session.can_input(),
            false,
            move || paste_clipboard(&paste_store, &paste_session),
        ),
        PopupAction::new(
            guard,
            t!("native.terminal.clear"),
            false,
            false,
            move || clear_session.clear_screen(),
        ),
    ]
}

pub(super) fn paste_clipboard(store: &WorkspaceStore, session: &Arc<TerminalSession>) {
    if !session.can_input() {
        return;
    }
    let store = store.clone();
    let session = session.clone();
    let guard = ServerUrlGuard::capture(&store);
    spawn(async move {
        let files = clipboard_files().await;
        let images = files
            .into_iter()
            .filter(|uri| image_mime(uri).is_some())
            .collect::<Vec<_>>();
        if !guard.is_current() {
            return;
        }
        if !images.is_empty() {
            drop_files(&store, &session, images);
            return;
        }
        let text = clipboard_string().await;
        if !guard.is_current() {
            return;
        }
        if !text.is_empty() {
            session.send_paste(&text);
        } else if let Some(image) = clipboard_image().await {
            use base64::Engine;
            if !guard.is_current() {
                return;
            }
            if let Some((header, data)) = image.url.split_once(',')
                && let Some(mime) = header
                    .strip_prefix("data:")
                    .and_then(|h| h.strip_suffix(";base64"))
                && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data)
            {
                upload_images(&store, &session, vec![(bytes, mime.to_string())]);
            }
        }
    });
}

pub(super) fn paste_event(
    store: &WorkspaceStore,
    session: &Arc<TerminalSession>,
    event: &PasteEvent,
) {
    let images = event
        .uris
        .iter()
        .filter(|uri| image_mime(uri).is_some())
        .cloned()
        .collect::<Vec<_>>();
    if !images.is_empty() {
        drop_files(store, session, images);
    } else if !event.text.is_empty() {
        // Rich clipboard sources often carry both text and a bitmap. Prefer text.
        session.send_paste(&event.text);
    } else {
        upload_images(
            store,
            session,
            event
                .files
                .iter()
                .filter(|f| f.mime.starts_with("image/"))
                .map(|f| (f.data.clone(), f.mime.clone()))
                .collect(),
        );
    }
}

pub(super) fn image_mime(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        _ => return None,
    })
}

pub(super) fn drop_files(
    store: &WorkspaceStore,
    session: &Arc<TerminalSession>,
    uris: Vec<String>,
) {
    if !session.can_input() {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut plain = Vec::new();
        let mut images = Vec::new();
        for uri in uris {
            let Some(path) = url::Url::parse(&uri)
                .ok()
                .and_then(|url| url.to_file_path().ok())
            else {
                continue;
            };
            let path_text = path.to_string_lossy();
            if let Some(mime) = image_mime(&path_text) {
                // Bound allocation before reading a file supplied by the desktop.
                match std::fs::metadata(&path) {
                    Ok(meta) if meta.len() > falcon_proto::PASTE_IMAGE_MAX_BYTES => {
                        store.show_toast(t!("native.terminal.pasteImageTooLarge"));
                        continue;
                    }
                    Err(error) => {
                        store.show_toast(format!(
                            "{}: {error}",
                            t!("native.terminal.pasteImageFailed")
                        ));
                        continue;
                    }
                    _ => {}
                }
                match std::fs::read(&path) {
                    Ok(bytes) => images.push((bytes, mime.to_string())),
                    Err(error) => store.show_toast(format!(
                        "{}: {error}",
                        t!("native.terminal.pasteImageFailed")
                    )),
                }
            } else {
                plain.push(falcon_core::paste_image::shell_quote_path(&path_text));
            }
        }
        if !plain.is_empty() {
            session.send_paste(&(plain.join(" ") + " "));
        }
        upload_images(store, session, images);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = uris;
        store.show_toast(t!("native.terminal.transferUnavailable"));
    }
}

pub(super) fn upload_images(
    store: &WorkspaceStore,
    session: &Arc<TerminalSession>,
    images: Vec<(Vec<u8>, String)>,
) {
    if images.is_empty() || !session.can_input() {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(client) = store.client() else {
            store.show_toast(t!("native.terminal.transferUnavailable"));
            return;
        };
        let store = store.clone();
        let session = session.clone();
        let guard = ServerUrlGuard::capture(&store);
        spawn(async move {
            let mut paths = Vec::new();
            for (bytes, mime) in images {
                if !guard.is_current() || !session.can_input() {
                    return;
                }
                if bytes.len() as u64 > falcon_proto::PASTE_IMAGE_MAX_BYTES {
                    store.show_toast(t!("native.terminal.pasteImageTooLarge"));
                    continue;
                }
                match client.paste_image(session.session_id(), bytes, &mime).await {
                    Ok(path) => paths.push(falcon_core::paste_image::quote_for_prompt(&path)),
                    Err(error) if guard.is_current() => store.show_toast(format!(
                        "{}: {error}",
                        t!("native.terminal.pasteImageFailed")
                    )),
                    Err(_) => return,
                }
            }
            if guard.is_current() && !paths.is_empty() {
                session.send_paste(&paths.join(" "));
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    store.show_toast(t!("native.terminal.transferUnavailable"));
}

pub(super) fn banner(
    store: WorkspaceStore,
    session: Arc<TerminalSession>,
    tick: Signal<u64>,
) -> AnyElement {
    let theme = use_theme();
    dynamic(move || {
        let _ = tick.get();
        let meta = session.meta();
        let (title, body, warning) = match meta.connection {
            ConnectionState::Active | ConnectionState::Connecting => return view().into_element(),
            ConnectionState::Disconnected => (
                if meta.reconnecting_attempt.unwrap_or(0) > 0 {
                    t!(
                        "native.terminal.connReconnecting",
                        attempt = meta.reconnecting_attempt.unwrap_or(0)
                    )
                } else {
                    t!("native.terminal.connClosed")
                },
                t!("native.terminal.connClosedBody"),
                false,
            ),
            ConnectionState::Dead => {
                let reason = match meta.dead_reason {
                    Some(falcon_proto::DeadReason::Exited) => {
                        t!("native.terminal.deadReason_exited")
                    }
                    Some(falcon_proto::DeadReason::BackendRestart) => {
                        t!("native.terminal.deadReason_backend_restart")
                    }
                    Some(falcon_proto::DeadReason::LinkLost) => {
                        t!("native.terminal.deadReason_link_lost")
                    }
                    Some(falcon_proto::DeadReason::SessionGone) => {
                        t!("native.terminal.deadReason_session_gone")
                    }
                    _ => String::new(),
                };
                (
                    t!("native.terminal.deadTitle"),
                    t!("native.terminal.deadBody", reason = reason),
                    false,
                )
            }
            ConnectionState::Reconnecting | ConnectionState::Unverified => (
                if meta.reconnecting_attempt.unwrap_or(0) > 0 {
                    t!(
                        "native.terminal.reconnecting",
                        attempt = meta.reconnecting_attempt.unwrap_or(0)
                    )
                } else {
                    t!("native.terminal.unverifiedTitle")
                },
                meta.last_error
                    .map(|error| format!("{}: {error}", t!("native.terminal.attachFailed")))
                    .unwrap_or_else(|| t!("native.terminal.unverifiedBody")),
                true,
            ),
            ConnectionState::Error => (
                t!("native.terminal.attachFailed"),
                meta.last_error.unwrap_or_default(),
                false,
            ),
        };
        let ui = theme.resolved().ui.clone();
        let mut bar = view()
            .test_id("terminal-banner")
            .flex_row()
            .items_center()
            .gap(zpx(8.0))
            .px(zpx(14.0))
            .py(zpx(8.0))
            .bg(rgb_to_hsla(ui.muted))
            .child(text(title).text_sm().text_color(rgb_to_hsla(if warning {
                ui.warning
            } else {
                ui.destructive
            })))
            .child(
                text(body)
                    .flex_1()
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        let st = store.clone();
        let sess = session.clone();
        let guard = ServerUrlGuard::capture(&store);
        let label = match meta.connection {
            ConnectionState::Dead => t!("native.terminal.deadActionNew"),
            ConnectionState::Disconnected => t!("native.terminal.retryNow"),
            ConnectionState::Error => t!("native.terminal.retry"),
            _ => t!("native.terminal.reattach"),
        };
        let connection = meta.connection;
        bar = bar.child(
            view()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .rounded(zpx(4.0))
                .bg(rgb_to_hsla(ui.secondary))
                .cursor_pointer()
                .child(text(label).text_sm())
                .test_id("terminal-banner-retry")
                .on_press(move || {
                    if !guard.is_current() {
                        return;
                    }
                    match connection {
                        ConnectionState::Dead => {
                            if let Some(record) =
                                st.sessions.get().iter().find(|r| r.id == sess.session_id())
                            {
                                st.select_project(&record.project_id);
                            }
                            st.create_terminal_in_active_project();
                        }
                        ConnectionState::Disconnected => sess.reconnect_now(),
                        _ => reattach(&st, &sess),
                    }
                }),
        );
        if meta.connection == ConnectionState::Dead {
            let st = store.clone();
            let id = session.session_id().to_string();
            let guard = ServerUrlGuard::capture(&store);
            bar = bar.child(
                view()
                    .px(zpx(8.0))
                    .py(zpx(4.0))
                    .rounded(zpx(4.0))
                    .bg(rgb_to_hsla(ui.secondary))
                    .cursor_pointer()
                    .child(text(t!("native.terminal.clearRecord")).text_sm())
                    .test_id("terminal-banner-clear")
                    .on_press(move || {
                        if guard.is_current() {
                            st.request_clear_session_record(&id);
                        }
                    }),
            );
        }
        bar.into_element()
    })
    .into_element()
}

fn reattach(store: &WorkspaceStore, session: &Arc<TerminalSession>) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(client) = store.client() {
        let store = store.clone();
        let session = session.clone();
        let guard = ServerUrlGuard::capture(&store);
        spawn(async move {
            match client.reattach_session(session.session_id()).await {
                Ok(updated) if guard.is_current() => {
                    session.set_state(updated.state, updated.dead_reason);
                    store.sessions.update(|list| {
                        if let Some(record) = list.iter_mut().find(|r| r.id == session.session_id())
                        {
                            record.state = updated.state;
                        }
                    });
                    session.reconnect_now();
                }
                Err(error) if guard.is_current() => session.set_error(error.to_string()),
                _ => {}
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = session;
        store.show_toast(t!("native.terminal.transferUnavailable"));
    }
}
