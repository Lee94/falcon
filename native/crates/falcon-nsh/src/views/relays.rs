//! 中转设置：按宿主机分组；表单与开关直接使用现有服务端接口。
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
    let share_port = signal(String::new());
    let share_name = signal(String::new());
    let bind_port = signal(String::new());
    let dest_port = signal(String::new());
    let dest_host = signal(String::new());
    let forward_name = signal(String::new());
    let remote = signal(false);
    dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let local = props.machine_id.is_empty();
        let rules: Vec<_> = store
            .relays
            .get()
            .into_iter()
            .filter(|r| {
                r.machine_label == props.machine_id
                    || r.machine_label == props.name
                    || (local && r.machine_label == "local")
            })
            .collect();
        let mut sections = view().flex_col().gap(zpx(12.0)).child(
            view()
                .flex_row()
                .gap(zpx(8.0))
                .items_center()
                .child(
                    text(props.name.clone())
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(props.destination.clone())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                ),
        );
        for share in if local { vec![true] } else { vec![false, true] } {
            let mut section = view()
                .flex_col()
                .gap(zpx(12.0))
                .p(zpx(12.0))
                .rounded(zpx(12.0))
                .bg(rgb_to_hsla(ui.app))
                .child(
                    text(if share {
                        t!("forward.shareSection")
                    } else {
                        t!("forward.sshSection")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                );
            let entries: Vec<_> = rules
                .iter()
                .filter(|r| (r.kind_label == "Quick Tunnel") == share)
                .collect();
            if entries.is_empty() {
                section = section.child(
                    text(if share {
                        t!("forward.shareEmpty")
                    } else {
                        t!("forward.empty")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                );
            }
            for rule in entries {
                let st = store.clone();
                let id = rule.id.clone();
                let enabled = rule.enabled;
                section = section.child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.0))
                        .child(
                            text(format!(":{} → {}", rule.local_port, rule.target))
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            nsh_base::switch()
                                .w(zpx(36.0))
                                .h(zpx(20.0))
                                .rounded_full()
                                .bg(rgb_to_hsla(if enabled { ui.primary } else { ui.muted }))
                                .thumb_class(
                                    "size-4 rounded-full bg-background checked:translate-x-4",
                                )
                                .checked(enabled)
                                .on_change(move |enabled: &bool| {
                                    st.set_relay_enabled(&id, share, *enabled)
                                }),
                        ),
                );
            }
            if share {
                let st = store.clone();
                let host = props.machine_id.clone();
                section = section
                    .child(
                        view()
                            .flex_row()
                            .gap(zpx(8.0))
                            .child(
                                field(
                                    share_port,
                                    if local {
                                        t!("forward.localPort")
                                    } else {
                                        t!("forward.remotePort")
                                    },
                                    &resolved,
                                )
                                .w(zpx(110.0)),
                            )
                            .child(
                                field(share_name, t!("forward.namePlaceholder"), &resolved)
                                    .flex_1()
                                    .min_w_0(),
                            )
                            .child(
                                view()
                                    .px(zpx(10.0))
                                    .py(zpx(5.0))
                                    .rounded(zpx(8.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.muted))
                                    .on_press(move || {
                                        if let Some(port) =
                                            share_port.get().parse::<u16>().ok().filter(|p| *p > 0)
                                        {
                                            st.create_public_share(&host, port, &share_name.get());
                                        } else {
                                            st.show_toast(t!("forward.portInvalid"));
                                        }
                                    })
                                    .child(
                                        text(t!("forward.shareAddAction"))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.foreground)),
                                    ),
                            ),
                    )
                    .child(
                        text(if local {
                            t!("forward.shareHint_local")
                        } else {
                            t!("forward.shareHint_remote")
                        })
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                    );
            } else {
                let st = store.clone();
                let host = props.machine_id.clone();
                section = section.child(
                    view()
                        .flex_row()
                        .gap(zpx(6.0))
                        .flex_wrap()
                        .child(
                            view()
                                .px(zpx(8.0))
                                .py(zpx(5.0))
                                .rounded(zpx(8.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.muted))
                                .on_press(move || remote.update(|v| *v = !*v))
                                .child(
                                    text(if remote.get() {
                                        t!("forward.kind_remote")
                                    } else {
                                        t!("forward.kind_local")
                                    })
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.foreground)),
                                ),
                        )
                        .child(field(bind_port, t!("forward.localPort"), &resolved).w(zpx(100.0)))
                        .child(
                            text("→")
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                        .child(
                            field(dest_host, t!("forward.destHostPlaceholder"), &resolved)
                                .w(zpx(150.0)),
                        )
                        .child(field(dest_port, t!("forward.remotePort"), &resolved).w(zpx(100.0)))
                        .child(
                            field(forward_name, t!("forward.namePlaceholder"), &resolved)
                                .flex_1()
                                .min_w_0(),
                        )
                        .child(
                            view()
                                .px(zpx(10.0))
                                .py(zpx(5.0))
                                .rounded(zpx(8.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.muted))
                                .on_press(move || {
                                    match (
                                        bind_port.get().parse::<u16>(),
                                        dest_port.get().parse::<u16>(),
                                    ) {
                                        (Ok(bind), Ok(dest)) if bind > 0 && dest > 0 => st
                                            .create_port_forward(
                                                &host,
                                                remote.get(),
                                                bind,
                                                &dest_host.get(),
                                                dest,
                                                &forward_name.get(),
                                            ),
                                        _ => st.show_toast(t!("forward.portInvalid")),
                                    }
                                })
                                .child(
                                    text(t!("forward.addAction"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                ),
                        ),
                );
            }
            sections = sections.child(section);
        }
        sections.into_element()
    })
}

fn field(
    value: Signal<String>,
    placeholder: String,
    theme: &falcon_theme::ResolvedTheme,
) -> nsh_core::element::Input {
    input()
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
        .on_change(move |v: &str| value.set(v.to_string()))
}
