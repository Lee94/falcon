//! Relay settings: machine ownership and full snapshots after writes (ADR 0016).
use crate::i18n::t;
use crate::shell::popups::themed_tooltip;
use crate::shell::widgets::{Glyph, icon_button};
use crate::store::{ActiveDialog, RelayChange, SettingsTab, use_workspace};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;
use falcon_core::relay::{endpoint_label, parse_forward_dest};
use falcon_proto::{ForwardState, PortForwardPatch, PublicSharePatch};
use nsh_core::element::input;
use nsh_core::prelude::*;
use std::time::Duration;

mod form;
pub use form::relay_editor;

#[component]
pub fn RelaysPane() -> impl IntoElement {
    let store = use_workspace();
    let poll = store.clone();
    // The pane owner cancels the timer immediately on navigation or closure.
    effect(move || {
        if poll.server_connected.get() {
            let store = poll.clone();
            untracked(move || {
                if !store.relay_actions_available() {
                    return;
                }
                store.refresh_relays();
                spawn(async move {
                    loop {
                        timer(Duration::from_secs(3)).await;
                        store.refresh_relays();
                    }
                });
            });
        }
    });
    view()
        .flex_col()
        .gap(zpx(24.0))
        .child(
            relay_machine()
                .machine_id(String::new())
                .name(t!("forward.local"))
                .destination(t!("forward.localConn")),
        )
        .child(dynamic(move || {
            let mut groups = view().flex_col().gap(zpx(24.0));
            let hosts = store.hosts.get();
            for host in &hosts {
                groups = groups.child(
                    relay_machine()
                        .key(host.id.clone())
                        .machine_id(host.id.clone())
                        .name(host.name.clone())
                        .destination(host.destination.clone()),
                );
            }
            if hosts.is_empty() {
                let resolved = use_theme().resolved();
                let go = store.clone();
                groups = groups.child(
                    view()
                        .flex_col()
                        .gap(zpx(8.0))
                        .p(zpx(16.0))
                        .rounded(zpx(8.0))
                        .bg(rgb_to_hsla(resolved.ui.app))
                        .child(
                            text(t!("forward.noHosts"))
                                .text_xs()
                                .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                        )
                        .child(
                            view()
                                .test_id("relays-go-hosts")
                                .focusable(true)
                                .on_press(move || {
                                    go.active_dialog
                                        .set(ActiveDialog::Settings(SettingsTab::Hosts))
                                })
                                .child(
                                    text(t!("forward.goHosts"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(resolved.ui.primary)),
                                ),
                        ),
                );
            }
            groups
        }))
}

#[derive(Clone, PartialEq, Props)]
pub struct RelayMachineProps {
    pub machine_id: String,
    pub name: String,
    pub destination: String,
}

#[component]
pub fn RelayMachine(props: RelayMachineProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let local = props.machine_id.is_empty();
    let machine_id = props.machine_id;
    let resolved = theme.resolved();
    let mut card = view().flex_col().gap(zpx(12.0));
    for share in if local { vec![true] } else { vec![false, true] } {
        let rows_store = store.clone();
        let rows_machine = machine_id.clone();
        let rows_theme = theme.clone();
        let rows = dynamic(move || {
            let resolved = rows_theme.resolved();
            let mut list = view().flex_col().gap(zpx(8.0));
            let entries: Vec<_> = rows_store
                .relays
                .get()
                .into_iter()
                .filter(|rule| {
                    rule.is_share() == share
                        && rule.machine_label == if local { "local" } else { &rows_machine }
                })
                .collect();
            if entries.is_empty() {
                list = list.child(
                    view()
                        .test_id(if share {
                            "relay-empty-shares"
                        } else {
                            "relay-empty-forwards"
                        })
                        .child(
                            text(if share {
                                t!("forward.shareEmpty")
                            } else {
                                t!("forward.empty")
                            })
                            .text_xs()
                            .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                        ),
                );
            }
            for rule in entries {
                list = list.child(
                    relay_row()
                        .key(format!("{}-{}", share, rule.id))
                        .id(rule.id)
                        .share(share),
                );
            }
            list
        });
        card = card.child(
            view()
                .flex_col()
                .gap(zpx(8.0))
                .p(zpx(12.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(resolved.ui.app))
                .child(
                    text(if share {
                        t!("forward.shareSection")
                    } else {
                        t!("forward.sshSection")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                )
                .child(rows)
                .child(
                    form::relay_form()
                        .machine_id(machine_id.clone())
                        .share(share)
                        .editing(None),
                ),
        );
    }
    view()
        .flex_col()
        .gap(zpx(8.0))
        .child(
            text(props.name)
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(resolved.ui.foreground)),
        )
        .child(
            text(props.destination)
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
        )
        .child(dynamic(move || {
            if store.relay_actions_available() {
                view().into_element()
            } else {
                view()
                    .test_id(&format!(
                        "relay-actions-unavailable-{}",
                        if local { "local" } else { &machine_id }
                    ))
                    .child(
                        text(t!("forward.unavailable"))
                            .text_xs()
                            .text_color(rgb_to_hsla(theme.resolved().ui.muted_foreground)),
                    )
                    .into_element()
            }
        }))
        .child(card)
}

#[derive(Clone, PartialEq, Props)]
pub struct RelayRowProps {
    pub id: String,
    pub share: bool,
}

#[component]
pub fn RelayRow(props: RelayRowProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let renaming = signal(false);
    let draft = signal(String::new());
    let copied = signal(false);
    let row_owner = Owner::current();
    dynamic(move || {
        let Some(rule) = store
            .relays
            .get()
            .into_iter()
            .find(|r| r.id == props.id && r.is_share() == props.share)
        else {
            return view().into_element();
        };
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let disabled =
            store.relay_toggle_pending(&rule.id, props.share) || !store.relay_actions_available();
        let connection = store.connection_generation();
        let request = store.connection_request_generation();
        let color = match rule.state {
            ForwardState::Active => ui.success,
            ForwardState::Starting => ui.warning,
            ForwardState::Error => ui.destructive,
            _ => ui.muted_foreground,
        };
        let mut main = view()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(zpx(8.0))
            .child(
                view()
                    .test_id(&format!("relay-dot-{}", rule.id))
                    .w(zpx(6.0))
                    .h(zpx(6.0))
                    .rounded_full()
                    .bg(rgb_to_hsla(color))
                    .tooltip(themed_tooltip(
                        text(state_label(rule.state)),
                        None,
                        &resolved,
                    )),
            )
            .child(
                text(if props.share {
                    t!("forward.shareSection")
                } else if rule.kind_label == "R → L" {
                    t!("forward.kind_remote")
                } else {
                    t!("forward.kind_local")
                })
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        if renaming.get() {
            let save = store.clone();
            let id = rule.id.clone();
            let share = props.share;
            let submit = move || {
                let change = if share {
                    RelayChange::Share(PublicSharePatch {
                        name: Some(draft.get().trim().into()),
                        ..Default::default()
                    })
                } else {
                    RelayChange::Forward(PortForwardPatch {
                        name: Some(draft.get().trim().into()),
                        ..Default::default()
                    })
                };
                save.change_relay(&id, change, connection, request, move || {
                    if renaming.try_get().is_some() {
                        renaming.set(false);
                    }
                });
            };
            let enter = submit.clone();
            main = main.child(
                view()
                    .disabled(disabled)
                    .flex_row()
                    .items_center()
                    .gap(zpx(4.0))
                    .child(
                        field(
                            draft,
                            format!("relay-name-input-{}", rule.id),
                            t!("forward.namePlaceholder"),
                            &resolved,
                        )
                        .on_change(move |value: &str| draft.set(value.to_string()))
                        .auto_focus(true)
                        .on_submit(move |_: &str| enter()),
                    )
                    .child(icon_button(
                        format!("relay-name-save-{}", rule.id),
                        t!("native.relays.save"),
                        Glyph::Check,
                        false,
                        &resolved,
                        submit,
                    ))
                    .child(icon_button(
                        format!("relay-name-cancel-{}", rule.id),
                        t!("native.relays.cancel"),
                        Glyph::Close,
                        false,
                        &resolved,
                        move || renaming.set(false),
                    )),
            );
        } else {
            let name = rule.name.clone();
            main = main.child(
                view()
                    .test_id(&format!("relay-name-{}", rule.id))
                    .focusable(true)
                    .disabled(disabled)
                    .on_press(move || {
                        draft.set(name.clone());
                        renaming.set(true);
                    })
                    .child(
                        text(if rule.name.is_empty() {
                            t!("forward.namePlaceholder")
                        } else {
                            rule.name.clone()
                        })
                        .text_sm()
                        .text_color(rgb_to_hsla(ui.foreground)),
                    ),
            );
        }
        let endpoint = endpoint_label(&rule.dest_host, rule.dest_port);
        let route = if props.share {
            endpoint
        } else {
            format!(
                "{} → {}",
                endpoint_label(&rule.bind_host, rule.local_port),
                endpoint
            )
        };
        main = main
            .child(text(route).text_xs().text_color(rgb_to_hsla(ui.foreground)))
            .child(
                text(state_label(rule.state))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        if rule.same_port_count > 1 {
            main = main.child(
                view()
                    .test_id(&format!("relay-same-{}", rule.id))
                    .px(zpx(6.0))
                    .py(zpx(2.0))
                    .rounded_full()
                    .bg(rgb_to_hsla(ui.tint))
                    .tooltip(themed_tooltip(
                        text(t!("forward.samePortHint", n = rule.same_port_count)),
                        None,
                        &resolved,
                    ))
                    .child(
                        text(t!("forward.samePort", n = rule.same_port_count))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.tint_foreground)),
                    ),
            );
        }
        if let Some(url) = rule.public_url.clone().filter(|url| !url.is_empty()) {
            let open = url.clone();
            let copy_owner = row_owner.clone();
            main = main
                .child(
                    view()
                        .test_id(&format!("relay-url-{}", rule.id))
                        .focusable(true)
                        .on_press(move || {
                            let _ = open_url(open.clone());
                        })
                        .child(
                            text(url.clone())
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.primary)),
                        ),
                )
                .child(icon_button(
                    format!("relay-copy-{}", rule.id),
                    if copied.get() {
                        t!("forward.shareCopied")
                    } else {
                        t!("forward.shareCopy")
                    },
                    if copied.get() {
                        Glyph::Check
                    } else {
                        Glyph::Copy
                    },
                    false,
                    &resolved,
                    move || {
                        set_clipboard(url.clone());
                        copied.set(true);
                        // Rerendering this row must not cancel the reset timer.
                        copy_owner.run(move || {
                            spawn(async move {
                                timer(Duration::from_millis(1500)).await;
                                copied.set(false);
                            });
                        });
                    },
                ));
        }
        let toggle = store.clone();
        let toggle_id = rule.id.clone();
        let edit = store.clone();
        let edit_id = rule.id.clone();
        let delete = store.clone();
        let delete_id = rule.id.clone();
        let share = props.share;
        main = main
            .child(
                view().test_id(&format!("relay-toggle-{}", rule.id)).child(
                    nsh_base::switch()
                        .w(zpx(36.0))
                        .h(zpx(20.0))
                        .p(zpx(2.0))
                        .rounded_full()
                        .bg(rgb_to_hsla(if rule.enabled {
                            ui.primary
                        } else {
                            ui.muted
                        }))
                        .disabled(disabled)
                        .thumb_style(move |style| {
                            style
                                .w(zpx(16.0))
                                .h(zpx(16.0))
                                .rounded_full()
                                .bg(rgb_to_hsla(ui.background))
                                .translate_x(zpx(if rule.enabled { 16.0 } else { 0.0 }))
                        })
                        .checked(rule.enabled)
                        .on_change(move |value: &bool| {
                            toggle.set_relay_enabled_for_context(
                                &toggle_id, share, *value, connection, request,
                            )
                        }),
                ),
            )
            .child(
                view()
                    .disabled(disabled)
                    .flex_row()
                    .child(icon_button(
                        format!("relay-edit-{}", rule.id),
                        t!("native.relays.edit"),
                        Glyph::Edit,
                        false,
                        &resolved,
                        move || {
                            edit.active_dialog.set(ActiveDialog::EditRelay {
                                id: edit_id.clone(),
                                share,
                                connection_generation: connection,
                                request_generation: request,
                            })
                        },
                    ))
                    .child(icon_button(
                        format!("relay-delete-{}", rule.id),
                        t!("native.relays.delete"),
                        Glyph::Trash,
                        false,
                        &resolved,
                        move || {
                            delete.change_relay(
                                &delete_id,
                                RelayChange::Delete { share },
                                connection,
                                request,
                                || {},
                            )
                        },
                    )),
            );
        let mut row = view()
            .test_id(&format!("relay-row-{}", rule.id))
            .flex_col()
            .gap(zpx(4.0))
            .child(main);
        if let Some(error) = rule.error {
            row = row.child(
                text(error)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            );
        }
        row.into_element()
    })
}

fn state_label(state: ForwardState) -> String {
    match state {
        ForwardState::Active => t!("forward.state_active"),
        ForwardState::Starting => t!("forward.state_starting"),
        ForwardState::Error => t!("forward.state_error"),
        _ => t!("forward.state_stopped"),
    }
}

fn field(
    value: Signal<String>,
    id: String,
    placeholder: String,
    theme: &falcon_theme::ResolvedTheme,
) -> nsh_core::element::Input {
    input()
        .test_id(&id)
        .px(zpx(8.0))
        .py(zpx(5.0))
        .rounded(zpx(8.0))
        .border_1()
        .border_color(rgba_to_hsla(theme.ui.border))
        .text_xs()
        .text_color(rgb_to_hsla(theme.ui.foreground))
        .bg(rgb_to_hsla(theme.ui.background))
        .placeholder(placeholder)
        .value(move || value.get())
}

fn parse_relay_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok().filter(|port| *port > 0)
}
