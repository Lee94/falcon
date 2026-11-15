use falcon_proto::ServerUpdatePhase;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::server_updates::{ServerUpdateKey, phase_key};
use crate::store::{WorkspaceStore, use_workspace};
use crate::theme::{rgb_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone, PartialEq, Props)]
pub struct ServerUpdateProps {
    pub gateway: String,
    pub host_id: Option<String>,
}

#[component]
pub fn ServerUpdate(props: ServerUpdateProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let resolved = theme.resolved();
    let key = ServerUpdateKey {
        gateway: props.gateway,
        host_id: props.host_id,
    };
    view()
        .test_id("server-update-dialog")
        .flex_col()
        .overflow_hidden()
        .child(super::dialogs::modal_header(
            &store,
            &resolved,
            t!("native.serverUpdate.title"),
        ))
        .child(scroll().min_h(zpx(0.0)).child(dynamic(move || {
            render_body(&store, &theme.resolved(), &key).into_element()
        })))
}

fn render_body(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    key: &ServerUpdateKey,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let state = store.server_updates.state(key);
    let mut body = view()
        .flex_col()
        .p(zpx(16.0))
        .gap(zpx(12.0))
        .child(
            text(state.label.clone())
                .text_sm()
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            text(t!("native.serverUpdate.intro"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    if let Some(check) = &state.check {
        body = body.child(
            text(t!(
                "native.serverUpdate.versions",
                current = check.current_version.clone(),
                latest = check.latest_version.clone().unwrap_or_else(|| "—".into()),
                platform = check.platform.clone(),
            ))
            .text_xs()
            .text_color(rgb_to_hsla(ui.foreground)),
        );
    }
    if let Some(task) = &state.task {
        body = body.child(
            view().test_id("server-update-phase").child(
                text(t!(
                    phase_key(task.phase),
                    version = task.target_version.clone().unwrap_or_else(|| "—".into())
                ))
                .text_xs()
                .text_color(rgb_to_hsla(
                    if task.phase == ServerUpdatePhase::Failed {
                        ui.destructive
                    } else {
                        ui.muted_foreground
                    },
                )),
            ),
        );
        if let Some(error) = &task.error {
            body = body.child(
                text(error.clone())
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            );
        }
    }
    if state.reconnecting {
        body = body.child(
            view().test_id("server-update-reconnecting").child(
                text(t!("native.serverUpdate.reconnecting"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            ),
        );
    }
    if let Some(error) = &state.error {
        body = body.child(
            view().test_id("server-update-error").child(
                text(error.clone())
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            ),
        );
    }
    if let Some(version) = state.available_version() {
        let st = store.clone();
        let target = key.clone();
        if state.confirming {
            body = body.child(
                text(t!(
                    "native.serverUpdate.confirm",
                    target = state.label,
                    version = version.clone()
                ))
                .text_xs()
                .text_color(rgb_to_hsla(ui.foreground)),
            );
            let cancel = store.server_updates.clone();
            let cancel_target = key.clone();
            body = body.child(
                view()
                    .flex_row()
                    .gap(zpx(8.0))
                    .child(action(
                        "server-update-cancel",
                        t!("native.serverUpdate.cancel"),
                        false,
                        resolved,
                        move || cancel.confirm(&cancel_target, false),
                    ))
                    .child(action(
                        "server-update-confirm",
                        t!("native.serverUpdate.install", version = version.clone()),
                        false,
                        resolved,
                        move || st.server_updates.install(&st, &target, &version),
                    )),
            );
        } else {
            body = body.child(action(
                "server-update-install",
                t!("native.serverUpdate.install", version = version),
                false,
                resolved,
                move || st.server_updates.confirm(&target, true),
            ));
        }
    }
    let st = store.clone();
    let target = key.clone();
    body = body.child(action(
        "server-update-check",
        if state.checking {
            t!("native.serverUpdate.checking")
        } else {
            t!("native.serverUpdate.check")
        },
        state.checking || state.running,
        resolved,
        move || st.server_updates.refresh(&st, &target),
    ));
    body
}

pub(crate) fn action(
    id: &str,
    label: String,
    disabled: bool,
    resolved: &falcon_theme::ResolvedTheme,
    callback: impl Fn() + 'static,
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .test_id(id)
        .disabled(disabled)
        .px(zpx(10.0))
        .py(zpx(6.0))
        .rounded(zpx(8.0))
        .cursor_pointer()
        .bg(rgb_to_hsla(ui.secondary))
        .on_press(callback)
        .child(
            text(label)
                .text_xs()
                .text_color(rgb_to_hsla(ui.secondary_foreground)),
        )
}
