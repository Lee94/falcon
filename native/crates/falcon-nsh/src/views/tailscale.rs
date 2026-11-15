//! 「从 Tailscale 添加」设备列表面板。
//!
//! 数据来自后端本机的 `tailscale status --json`，不是界面所在机器的 tailnet。这里只负责
//! 展示与选择；选中后由主机表单填写名称、地址和SSH用户名，保存前仍可修改。

use crate::{
    i18n::t,
    theme::rgb_to_hsla,
    zoom::zpx,
};
use falcon_core::tailscale::{already_added, matches_query};
use falcon_proto::{SshHost, TailscaleDevice, TailscaleDevices, TailscaleUnavailable};
use nsh_core::element::input;
use nsh_core::prelude::*;

pub fn tailscale_panel<F, R>(
    resolved: &falcon_theme::ResolvedTheme,
    result: Option<TailscaleDevices>,
    error: Option<String>,
    loading: bool,
    query: Signal<String>,
    hosts: &[SshHost],
    on_pick: F,
    on_refresh: R,
) -> impl IntoElement
where
    F: Fn(TailscaleDevice) + Clone + 'static,
    R: Fn() + Clone + 'static,
{
    let ui = &resolved.ui;
    let first_load = loading && result.is_none() && error.is_none();
    let mut panel = view()
        .label("host-tailscale-panel")
        .flex_col()
        .gap(zpx(12.0))
        .p(zpx(12.0))
        .rounded(zpx(12.0))
        .bg(rgb_to_hsla(ui.app))
        .child(
            text(t!("host.tailscaleHint"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );

    if let Some(error) = error {
        panel = panel.child(
            text(error)
                .text_xs()
                .text_color(rgb_to_hsla(ui.destructive)),
        );
    }

    if first_load {
        panel = panel.child(
            text(t!("host.tailscaleLoading"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    } else if let Some(list) = result {
        if !list.available {
            panel = panel.child(
                text(problem_text(&list))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            );
        } else if list.devices.is_empty() {
            panel = panel.child(
                text(t!("host.tailscaleEmpty"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        } else {
            panel = panel.child(
                input()
                    .w_full()
                    .px(zpx(8.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.background))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground))
                    .placeholder(t!("host.tailscaleSearch"))
                    .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                    .value(query.get())
                    .on_change(move |value: &str| query.set(value.to_string())),
            );
            let filtered: Vec<TailscaleDevice> = list
                .devices
                .iter()
                .filter(|device| matches_query(device, &query.get()))
                .cloned()
                .collect();
            if filtered.is_empty() {
                panel = panel.child(
                    text(t!("host.tailscaleNoMatch"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                );
            } else {
                let mut rows = view().flex_col().gap(zpx(8.0));
                for device in filtered {
                    let added = already_added(&device, hosts);
                    rows = rows.child(device_row(resolved, device, added, on_pick.clone()));
                }
                panel = panel.child(rows);
            }
        }
    }

    if !first_load {
        panel = panel.child(
            view()
                .label("host-tailscale-retry")
                .self_start()
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(8.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.background))
                .on_press(move || on_refresh())
                .child(
                    text(if loading {
                        t!("host.tailscaleLoading")
                    } else {
                        t!("host.tailscaleRetry")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground)),
                ),
        );
    }
    panel
}

fn device_row<F>(
    resolved: &falcon_theme::ResolvedTheme,
    device: TailscaleDevice,
    added: bool,
    on_pick: F,
) -> impl IntoElement
where
    F: Fn(TailscaleDevice) + Clone + 'static,
{
    let ui = &resolved.ui;
    let picked = device.clone();
    let mut details = view()
        .flex_row()
        .flex_wrap()
        .gap(zpx(8.0))
        .child(
            text(device.host.clone())
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .child(
            text(if device.online {
                t!("host.tailscaleOnline")
            } else {
                t!("host.tailscaleOffline")
            })
            .text_xs()
            .text_color(rgb_to_hsla(if device.online {
                ui.success
            } else {
                ui.muted_foreground
            })),
        );
    if let Some(os) = device.os.as_ref().filter(|os| !os.is_empty()) {
        details = details.child(
            text(os.clone())
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }
    if added {
        details = details.child(
            text(t!("host.tailscaleAdded"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }

    view()
        .flex_col()
        .gap(zpx(6.0))
        .px(zpx(10.0))
        .py(zpx(8.0))
        .rounded(zpx(8.0))
        .bg(rgb_to_hsla(ui.background))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(zpx(12.0))
                .child(
                    text(device.name.clone())
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    view()
                        .label(format!("host-tailscale-add-{}", device.id))
                        .flex_none()
                        .px(zpx(10.0))
                        .py(zpx(4.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint))
                        .on_press(move || on_pick(picked.clone()))
                        .child(
                            text(t!("host.tailscaleAdd"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.tint_foreground)),
                        ),
                ),
        )
        .child(details)
}

fn problem_text(list: &TailscaleDevices) -> String {
    let base = match list.reason {
        Some(TailscaleUnavailable::NotInstalled) => t!("host.tailscaleNotInstalled"),
        Some(TailscaleUnavailable::NotRunning) => t!("host.tailscaleNotRunning"),
        Some(TailscaleUnavailable::NeedsLogin) => t!("host.tailscaleNeedsLogin"),
        Some(TailscaleUnavailable::Failed) | None => t!("host.tailscaleFailed"),
    };
    match list.message.as_deref().filter(|message| !message.is_empty()) {
        Some(message)
            if matches!(
                list.reason,
                Some(TailscaleUnavailable::Failed) | None
            ) =>
        {
            format!("{base} {message}")
        }
        _ => base.to_string(),
    }
}
