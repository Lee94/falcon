use crate::i18n::t;
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;
use nsh_core::element::input;
use nsh_core::prelude::*;

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
    let share_port = signal(String::new());
    let share_name = signal(String::new());
    let bind_port = signal(String::new());
    let dest_port = signal(String::new());
    let dest_host = signal(String::new());
    let forward_name = signal(String::new());
    let remote = signal(false);
    let share_error = signal(None::<String>);
    let forward_error = signal(None::<String>);
    let machine_id = props.machine_id.clone();
    let machine_name = props.name.clone();
    let destination = props.destination.clone();

    let rows_store = store.clone();
    let rows_machine_id = machine_id.clone();
    let rows_machine_name = machine_name.clone();
    let rows_theme = theme.clone();
    let rows = dynamic(move || {
        let resolved = rows_theme.resolved();
        let ui = &resolved.ui;
        let rules: Vec<_> = rows_store.relays.get().into_iter().filter(|rule| {
            rule.machine_label == rows_machine_id
                || rule.machine_label == rows_machine_name
                || (local && rule.machine_label == "local")
        }).collect();
        let sections: &[bool] = if local { &[true] } else { &[false, true] };
        let mut content = view().flex_col().gap(zpx(16.0)).child(
            view().flex_col().gap(zpx(3.0))
                .child(text(if local { t!("forward.local") } else { rows_machine_name.clone() }).text_sm().font_weight(FontWeight::SEMIBOLD).text_color(rgb_to_hsla(ui.foreground)))
                .child(text(if local { t!("forward.localConn") } else { format!("{} · {}", t!("forward.machine"), destination) }).text_xs().text_color(rgb_to_hsla(ui.muted_foreground))),
        );
        if !rows_store.relay_actions_available() {
            let unavailable_id = format!("relay-actions-unavailable-{}", if local { "local" } else { &rows_machine_id });
            content = content.child(
                view()
                    .test_id(&unavailable_id)
                    .child(text(t!("forward.unavailable")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground))),
            );
        }
        for is_share in sections {
            let entries: Vec<_> = rules.iter().filter(|rule| rule.kind_label == if *is_share { "Quick Tunnel" } else { "L → R" } || (!*is_share && rule.kind_label == "R → L")).cloned().collect();
            let title = if *is_share { t!("forward.shareTitle") } else { t!("forward.forwardTitle") };
            let mut section = view().flex_col().gap(zpx(7.0)).child(text(title).text_xs().font_weight(FontWeight::SEMIBOLD).text_color(rgb_to_hsla(ui.muted_foreground)));
            if entries.is_empty() {
                section = section.child(
                    view()
                        .test_id(if *is_share { "relay-empty-shares" } else { "relay-empty-forwards" })
                        .child(text(t!("forward.empty")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground))),
                );
            }
            for rule in entries {
                let share_kind = *is_share;
                let pending = rows_store.relay_toggle_pending(&rule.id, share_kind);
                let enabled = rule.enabled;
                let id = rule.id.clone();
                let callback = rows_store.clone();
                let connection_generation = rows_store.connection_generation();
                let request_generation = rows_store.connection_request_generation();
                let mut row = view().flex_row().items_center().justify_between().gap(zpx(8.0))
                    .child(view().flex_col().gap(zpx(2.0))
                        .child(text(format!("{} · {}", rule.local_port, rule.target)).text_xs().text_color(rgb_to_hsla(ui.foreground)))
                        .child(if let Some(url) = rule.public_url.clone() { text(url).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)).into_element() } else { text(rule.kind_label.clone()).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)).into_element() }))
                    .child(view().test_id(&format!("relay-toggle-{}", rule.id))
                        .child(nsh_base::switch().w(zpx(36.0)).h(zpx(20.0)).rounded_full().bg(rgb_to_hsla(if enabled { ui.primary } else { ui.muted }))
                            .disabled(pending || !rows_store.relay_actions_available())
                            .thumb_class("size-4 rounded-full bg-background checked:translate-x-4")
                            .checked(enabled)
                            .on_change(move |value: &bool| callback.set_relay_enabled_for_context(&id, share_kind, *value, connection_generation, request_generation))));
                if rule.same_port_conflict {
                    row = row.child(text(t!("forward.conflict")).text_xs().text_color(rgb_to_hsla(ui.destructive_foreground)));
                }
                section = section.child(row);
            }
            content = content.child(section);
        }
        content
    });

    let resolved = theme.resolved();
    let share_host = machine_id.clone();
    let share_store = store.clone();
    let share_store_busy = store.clone();
    let share_host_busy = share_host.clone();
    let share_validation = share_error;
    let share_error_color = rgb_to_hsla(resolved.ui.destructive_foreground);
    let share_error_id = format!("relay-share-error-{}", if local { "local" } else { &machine_id });
    let share_submit_id = format!("relay-create-share-{}", if local { "local" } else { &machine_id });
    let share_form = view().flex_col().gap(zpx(8.0))
        .child(text(t!("forward.addShare")).text_xs().text_color(rgb_to_hsla(resolved.ui.muted_foreground)))
        .child(field(share_port, format!("relay-share-port-{}", if local { "local" } else { &machine_id }), t!("forward.port"), &resolved))
        .child(field(share_name, format!("relay-share-name-{}", if local { "local" } else { &machine_id }), t!("forward.name"), &resolved))
        .child(dynamic(move || {
            share_validation.get().map_or_else(
                || view().into_element(),
                |error| view().test_id(&share_error_id).child(text(error).text_xs().text_color(share_error_color)).into_element(),
            )
        }))
        .child(view().test_id(&share_submit_id).disabled(move || !share_store_busy.relay_actions_available() || share_store_busy.relay_create_pending(&share_host_busy, true))
            .on_press(move || {
                let Some(port) = parse_relay_port(&share_port.get()) else {
                    share_error.set(Some(t!("forward.portInvalid")));
                    return;
                };
                share_error.set(None);
                share_store.create_public_share(&share_host, port, share_name.get().trim());
            })
            .px(zpx(10.0)).py(zpx(6.0)).rounded(zpx(8.0)).bg(rgb_to_hsla(resolved.ui.primary)).child(text(t!("forward.addAction")).text_xs().text_color(rgb_to_hsla(resolved.ui.primary_foreground))));

    let forward_form = if local {
        None
    } else {
        let forward_host = machine_id.clone();
        let forward_store = store.clone();
        let forward_busy_store = store.clone();
        let forward_host_busy = forward_host.clone();
        let forward_validation = forward_error;
        let forward_error_color = rgb_to_hsla(resolved.ui.destructive_foreground);
        let forward_error_id = format!("relay-forward-error-{}", machine_id);
        let submit_id = format!("relay-create-forward-{}", machine_id);
        let label_color = rgb_to_hsla(resolved.ui.foreground);
        Some(view().flex_col().gap(zpx(8.0))
            .child(text(t!("forward.addForward")).text_xs().text_color(rgb_to_hsla(resolved.ui.muted_foreground)))
            .child(field(bind_port, format!("relay-bind-port-{}", machine_id), t!("forward.bindPort"), &resolved))
            .child(field(dest_host, format!("relay-dest-host-{}", machine_id), t!("forward.destHost"), &resolved))
            .child(field(dest_port, format!("relay-dest-port-{}", machine_id), t!("forward.destPort"), &resolved))
            .child(field(forward_name, format!("relay-forward-name-{}", machine_id), t!("forward.name"), &resolved))
            .child(view().flex_row().items_center().gap(zpx(6.0)).test_id(&format!("relay-direction-{}", machine_id)).on_press(move || remote.update(|value| *value = !*value))
                .child(dynamic(move || text(if remote.get() { t!("forward.remote") } else { t!("forward.local") }).text_xs().text_color(label_color))))
            .child(dynamic(move || {
                forward_validation.get().map_or_else(
                    || view().into_element(),
                    |error| view().test_id(&forward_error_id).child(text(error).text_xs().text_color(forward_error_color)).into_element(),
                )
            }))
            .child(view().test_id(&submit_id).disabled(move || !forward_busy_store.relay_actions_available() || forward_busy_store.relay_create_pending(&forward_host_busy, false))
                .on_press(move || {
                    let (Some(bind), Some(port)) = (parse_relay_port(&bind_port.get()), parse_relay_port(&dest_port.get())) else {
                        forward_error.set(Some(t!("forward.portInvalid")));
                        return;
                    };
                    forward_error.set(None);
                    forward_store.create_port_forward(&forward_host, remote.get(), bind, dest_host.get().trim(), port, forward_name.get().trim());
                }).px(zpx(10.0)).py(zpx(6.0)).rounded(zpx(8.0)).bg(rgb_to_hsla(resolved.ui.primary)).child(text(t!("forward.addAction")).text_xs().text_color(rgb_to_hsla(resolved.ui.primary_foreground)))))
    };

    let mut body = view().flex_col().gap(zpx(14.0)).child(rows);
    if let Some(forward_form) = forward_form { body = body.child(forward_form); }
    body.child(share_form)
}

fn field(value: Signal<String>, id: String, placeholder: String, theme: &falcon_theme::ResolvedTheme) -> nsh_core::element::Input {
    input().test_id(&id).px(zpx(8.0)).py(zpx(5.0)).rounded(zpx(8.0)).border_1()
        .border_color(rgba_to_hsla(theme.ui.border)).text_xs().text_color(rgb_to_hsla(theme.ui.foreground))
        .bg(rgb_to_hsla(theme.ui.background)).placeholder(placeholder)
        .value(move || value.get()).on_change(move |v: &str| value.set(v.to_string()))
}

fn parse_relay_port(value: &str) -> Option<u16> {
    value.trim().parse::<u16>().ok().filter(|port| *port > 0)
}
