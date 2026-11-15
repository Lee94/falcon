use super::*;

#[derive(Clone, PartialEq, Props)]
pub struct RelayEditorProps {
    pub id: String,
    pub share: bool,
    pub connection_generation: u64,
    pub request_generation: u64,
}

#[component]
pub fn RelayEditor(props: RelayEditorProps) -> impl IntoElement {
    let store = use_workspace();
    let record = store
        .relays
        .get_untracked()
        .into_iter()
        .find(|r| r.id == props.id && r.is_share() == props.share);
    let resolved = use_theme().resolved();
    let close = store.clone();
    let mut body = view().flex_col().p(zpx(16.0)).gap(zpx(12.0)).child(
        view()
            .flex_row()
            .items_center()
            .justify_between()
            .child(
                text(t!("native.relays.edit"))
                    .text_sm()
                    .text_color(rgb_to_hsla(resolved.ui.foreground)),
            )
            .child(icon_button(
                "relay-editor-close",
                t!("native.relays.cancel"),
                Glyph::Close,
                false,
                &resolved,
                move || {
                    close
                        .active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Relays))
                },
            )),
    );
    if let Some(record) = record {
        body = body.child(
            relay_form()
                .machine_id(if record.machine_label == "local" {
                    String::new()
                } else {
                    record.machine_label.clone()
                })
                .share(props.share)
                .editing(Some(record.id))
                .connection_generation(Some(props.connection_generation))
                .request_generation(Some(props.request_generation)),
        );
    }
    body
}

#[derive(Clone, PartialEq, Props)]
pub struct RelayFormProps {
    pub machine_id: String,
    pub share: bool,
    pub editing: Option<String>,
    #[prop(default)]
    pub connection_generation: Option<u64>,
    #[prop(default)]
    pub request_generation: Option<u64>,
}

#[component]
pub fn RelayForm(props: RelayFormProps) -> impl IntoElement {
    let store = use_workspace();
    let resolved = use_theme().resolved();
    let editing = props.editing.as_ref().and_then(|id| {
        store
            .relays
            .get_untracked()
            .into_iter()
            .find(|r| &r.id == id && r.is_share() == props.share)
    });
    let share = props.share;
    let local = props.machine_id.is_empty();
    let key = if editing.is_some() {
        "edit"
    } else if local {
        "local"
    } else {
        &props.machine_id
    }
    .to_string();
    let port = signal(
        editing
            .as_ref()
            .map(|r| r.local_port.to_string())
            .unwrap_or_default(),
    );
    let dest = signal(
        editing
            .as_ref()
            .map(|r| r.dest_port.to_string())
            .unwrap_or_default(),
    );
    let host = signal(
        editing
            .as_ref()
            .map(|r| r.dest_host.clone())
            .unwrap_or_default(),
    );
    let name = signal(editing.as_ref().map(|r| r.name.clone()).unwrap_or_default());
    let remote = signal(editing.as_ref().is_some_and(|r| r.kind_label == "R → L"));
    let error = signal(None::<String>);
    let connection = props
        .connection_generation
        .unwrap_or_else(|| store.connection_generation());
    let request = props
        .request_generation
        .unwrap_or_else(|| store.connection_request_generation());
    let submit_store = store.clone();
    let submit_machine = props.machine_id.clone();
    let form_owner = Owner::current();
    let record_to_edit = editing.clone();
    let submit = move || {
        let Some(bind) = parse_relay_port(&port.get()) else {
            error.set(Some(t!("forward.portInvalid")));
            return;
        };
        let Ok((destination, pasted_port)) = parse_forward_dest(&host.get()) else {
            error.set(Some(t!("forward.hostInvalid")));
            return;
        };
        let target = if let Some(pasted_port) = pasted_port {
            Some(pasted_port)
        } else if share || dest.get().trim().is_empty() {
            Some(bind)
        } else {
            parse_relay_port(&dest.get())
        };
        let Some(target) = target else {
            error.set(Some(t!("forward.portInvalid")));
            return;
        };
        error.set(None);
        let owner = form_owner.clone();
        let clear = move || {
            if owner.is_live() {
                for value in [port, dest, host, name] {
                    value.set(String::new());
                }
            }
        };
        if let Some(record) = &record_to_edit {
            let change = if share {
                RelayChange::Share(PublicSharePatch {
                    name: Some(name.get().trim().into()),
                    dest_host: Some(destination.unwrap_or_else(|| "127.0.0.1".into())),
                    dest_port: Some(target),
                    ..Default::default()
                })
            } else {
                RelayChange::Forward(PortForwardPatch {
                    name: Some(name.get().trim().into()),
                    kind: Some(if remote.get() {
                        falcon_proto::ForwardKind::Remote
                    } else {
                        falcon_proto::ForwardKind::Local
                    }),
                    bind_port: Some(bind),
                    dest_host: Some(destination.unwrap_or_else(|| "127.0.0.1".into())),
                    dest_port: Some(target),
                    ..Default::default()
                })
            };
            let close = submit_store.clone();
            let dialog = close.active_dialog.get_untracked();
            let editor_owner = form_owner.clone();
            submit_store.change_relay(&record.id, change, connection, request, move || {
                if editor_owner.is_live() && close.active_dialog.get_untracked() == dialog {
                    close
                        .active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Relays));
                }
            });
        } else if share {
            submit_store.create_public_share_then(&submit_machine, bind, name.get().trim(), clear);
        } else {
            submit_store.create_port_forward_then(
                &submit_machine,
                remote.get(),
                bind,
                destination.as_deref().unwrap_or_default(),
                target,
                name.get().trim(),
                clear,
            );
        }
    };
    let enter = submit.clone();
    let busy_store = store.clone();
    let busy_editing = editing;
    let busy_host = props.machine_id.clone();
    let disabled = move || {
        !busy_store.relay_actions_available()
            || if let Some(rule) = &busy_editing {
                busy_store.relay_toggle_pending(&rule.id, share)
            } else {
                busy_store.relay_create_pending(&busy_host, share)
            }
    };
    let mut form = view().flex_col().gap(zpx(8.0));
    if !share {
        let foreground = rgb_to_hsla(resolved.ui.foreground);
        form = form.child(
            view()
                .test_id(&format!("relay-direction-{key}"))
                .focusable(true)
                .on_press(move || remote.update(|v| *v = !*v))
                .child(dynamic(move || {
                    text(if remote.get() {
                        t!("forward.kind_remote")
                    } else {
                        t!("forward.kind_local")
                    })
                    .text_xs()
                    .text_color(foreground)
                })),
        );
    }
    let port_id = if share {
        format!("relay-share-port-{key}")
    } else {
        format!("relay-bind-port-{key}")
    };
    let enter_port = enter.clone();
    form = form.child(
        field(
            port,
            port_id,
            if share {
                t!("forward.port")
            } else {
                t!("forward.bindPort")
            },
            &resolved,
        )
        .auto_focus(props.editing.is_some())
        .on_change(move |v: &str| {
            // Follow the listening port until the target was independently changed.
            if !share && (dest.get().is_empty() || dest.get() == port.get()) {
                dest.set(v.to_string());
            }
            port.set(v.to_string());
        })
        .on_submit(move |_: &str| enter_port()),
    );
    if !share || props.editing.is_some() {
        let enter_host = enter.clone();
        form = form.child(
            field(
                host,
                format!("relay-dest-host-{key}"),
                t!("forward.destHostPlaceholder"),
                &resolved,
            )
            .on_change(move |v: &str| {
                // An incomplete IPv6 address can look like host:port. Preserve
                // the draft until the user leaves the field or submits it.
                host.set(v.to_string());
            })
            .on_blur(move || {
                // Transfer a complete endpoint's port before the user edits the
                // dedicated port field, so that later port edits take priority.
                if let Ok((Some(address), Some(target))) = parse_forward_dest(&host.get()) {
                    host.set(address);
                    if share {
                        port.set(target.to_string());
                    } else {
                        dest.set(target.to_string());
                    }
                }
            })
            .on_submit(move |_: &str| enter_host()),
        );
    }
    if !share {
        let enter_dest = enter.clone();
        form = form.child(
            field(
                dest,
                format!("relay-dest-port-{key}"),
                t!("forward.destPort"),
                &resolved,
            )
            .on_change(move |value: &str| dest.set(value.to_string()))
            .on_submit(move |_: &str| enter_dest()),
        );
    }
    let name_id = if share {
        format!("relay-share-name-{key}")
    } else {
        format!("relay-forward-name-{key}")
    };
    let error_id = format!(
        "relay-{}-error-{key}",
        if share { "share" } else { "forward" }
    );
    let error_color = rgb_to_hsla(resolved.ui.destructive);
    let submit_id = if props.editing.is_some() {
        "relay-edit-save".into()
    } else {
        format!(
            "relay-create-{}-{key}",
            if share { "share" } else { "forward" }
        )
    };
    form.child(
        field(name, name_id, t!("forward.namePlaceholder"), &resolved)
            .on_change(move |value: &str| name.set(value.to_string()))
            .on_submit(move |_: &str| enter()),
    )
    .child(dynamic(move || {
        error.get().map_or_else(
            || view().into_element(),
            |e| {
                view()
                    .test_id(&error_id)
                    .child(text(e).text_xs().text_color(error_color))
                    .into_element()
            },
        )
    }))
    .child(
        view()
            .test_id(&submit_id)
            .focusable(true)
            .disabled(disabled)
            .on_press(submit)
            .px(zpx(10.0))
            .py(zpx(6.0))
            .rounded(zpx(8.0))
            .bg(rgb_to_hsla(resolved.ui.primary))
            .child(
                text(if props.editing.is_some() {
                    t!("native.relays.save")
                } else {
                    t!("forward.addAction")
                })
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.primary_foreground)),
            ),
    )
    .child(
        text(if share {
            if local {
                t!("forward.shareHint_local")
            } else {
                t!("forward.shareHint_remote")
            }
        } else {
            t!("native.relays.defaults")
        })
        .text_xs()
        .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
    )
}
