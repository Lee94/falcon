//! 新增 SSH 主机表单；凭据只交给服务端，不写入工作区偏好。
//! 「从 Tailscale 添加」选中设备后只预填这张表单，认证仍是普通 SSH。
use crate::{
    i18n::t,
    store::use_workspace,
    theme::{rgb_to_hsla, use_theme},
    zoom::zpx,
};
use falcon_core::tailscale::fresh_name;
use falcon_proto::{SshAuthMethod, SshHost, SshHostInput, TailscaleDevice, TailscaleDevices};
use nsh_core::element::input;
use nsh_core::prelude::*;

#[component]
pub fn HostForm() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let expanded = signal(false);
    let tailscale_open = signal(false);
    let tailscale_loading = signal(false);
    let tailscale_result = signal(None::<TailscaleDevices>);
    let tailscale_error = signal(None::<String>);
    let tailscale_query = signal(String::new());
    let tailscale_request = signal(0_u64);
    let name = signal(String::new());
    let address = signal(String::new());
    let username = signal(String::new());
    let port = signal("22".to_string());
    let auth = signal(SshAuthMethod::Agent);
    let key = signal(String::new());
    let secret = signal(String::new());

    // 任务挂在表单的 effect 上，而不是按钮所在的那次 dynamic 渲染上：打开面板会立刻
    // 重绘 dynamic，直接在它的事件闭包里 spawn 会在第一次 poll 前随旧渲染一起被取消。
    let fetch_store = store.clone();
    effect(move || {
        let request = tailscale_request.get();
        if request == 0 {
            return;
        }
        tailscale_loading.set(true);
        tailscale_error.set(None);
        let store = fetch_store.clone();
        spawn(async move {
            let fetched = store.tailscale_devices().await;
            if tailscale_request.get() != request {
                return;
            }
            tailscale_loading.set(false);
            match fetched {
                Ok(list) => tailscale_result.set(Some(list)),
                Err(error) => tailscale_error.set(Some(error)),
            }
        });
    });
    let refresh_tailscale = move || {
        if !tailscale_loading.get() {
            tailscale_request.update(|request| *request += 1);
        }
    };

    dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let st = store.clone();
        let refresh_on_open = refresh_tailscale.clone();
        let header = view()
            .flex_row()
            .flex_wrap()
            .gap(zpx(8.0))
            .child(
                view()
                    .label("host-add-manual")
                    .px(zpx(10.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.app))
                    .cursor_pointer()
                    .on_press(move || {
                        let open = !expanded.get();
                        expanded.set(open);
                        if open {
                            tailscale_open.set(false);
                        }
                    })
                    .child(
                        text(t!("host.add"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    ),
            )
            .child(
                view()
                    .label("host-add-tailscale")
                    .px(zpx(10.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.app))
                    .cursor_pointer()
                    .on_press(move || {
                        let open = !tailscale_open.get();
                        tailscale_open.set(open);
                        if open {
                            expanded.set(false);
                            if tailscale_result.get().is_none()
                                && tailscale_error.get().is_none()
                            {
                                refresh_on_open();
                            }
                        }
                    })
                    .child(
                        text(t!("host.fromTailscale"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    ),
            );
        let mut root = view().flex_col().gap(zpx(12.0)).child(header);
        if expanded.get() {
            let mut fields = view()
                .flex_col()
                .gap(zpx(10.0))
                .p(zpx(12.0))
                .rounded(zpx(12.0))
                .bg(rgb_to_hsla(ui.app));
            for (label, value) in [
                (t!("host.name"), name),
                (t!("project.host"), address),
                (t!("project.port"), port),
                (t!("project.username"), username),
            ] {
                fields = fields.child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(12.0))
                        .child(
                            text(label)
                                .w(zpx(96.0))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            input()
                                .flex_1()
                                .px(zpx(8.0))
                                .py(zpx(5.0))
                                .rounded(zpx(8.0))
                                .bg(rgb_to_hsla(ui.background))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground))
                                .value(value.get())
                                .on_change(move |v: &str| value.set(v.to_string())),
                        ),
                );
            }
            fields = fields.child(
                view().flex_row().gap(zpx(8.0)).children(
                    [
                        (SshAuthMethod::Agent, "project.authAgent"),
                        (SshAuthMethod::Key, "project.authKey"),
                        (SshAuthMethod::Password, "project.authPassword"),
                    ]
                    .into_iter()
                    .map(|(method, label)| {
                        view()
                            .px(zpx(8.0))
                            .py(zpx(5.0))
                            .rounded(zpx(8.0))
                            .cursor_pointer()
                            .when(auth.get() == method, |v| v.bg(rgb_to_hsla(ui.tint)))
                            .on_press(move || auth.set(method))
                            .child(
                                text(t!(label))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            )
                    }),
                ),
            );
            if auth.get() == SshAuthMethod::Key {
                fields = fields.child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(5.0))
                        .rounded(zpx(8.0))
                        .bg(rgb_to_hsla(ui.background))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder(t!("project.keyPath"))
                        .value(key.get())
                        .on_change(move |v: &str| key.set(v.to_string())),
                );
            }
            if auth.get() != SshAuthMethod::Agent {
                fields = fields.child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(5.0))
                        .rounded(zpx(8.0))
                        .bg(rgb_to_hsla(ui.background))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .masked(true)
                        .placeholder(t!("project.sshPassword"))
                        .value(secret.get())
                        .on_change(move |v: &str| secret.set(v.to_string())),
                );
            }
            fields = fields.child(
                view()
                    .px(zpx(10.0))
                    .py(zpx(6.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.tint))
                    .cursor_pointer()
                    .on_press(move || {
                        if name.get().trim().is_empty()
                            || address.get().trim().is_empty()
                            || username.get().trim().is_empty()
                        {
                            return;
                        }
                        let Some(port) = port.get().parse::<u16>().ok().filter(|p| *p > 0) else {
                            return;
                        };
                        st.create_ssh_host(SshHostInput {
                            name: name.get(),
                            host: address.get(),
                            port,
                            username: username.get(),
                            auth_method: auth.get(),
                            key_path: (!key.get().is_empty()).then(|| key.get()),
                            secret: (!secret.get().is_empty()).then(|| secret.get()),
                        });
                        secret.set(String::new());
                        expanded.set(false);
                    })
                    .child(
                        text(t!("host.create"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.tint_foreground)),
                    ),
            );
            root = root.child(fields);
        }
        if tailscale_open.get() {
            let pick_store = store.clone();
            let on_pick = move |device: TailscaleDevice| {
                let hosts: Vec<SshHost> = pick_store
                    .hosts
                    .get()
                    .into_iter()
                    .filter_map(|host| host.source)
                    .collect();
                name.set(fresh_name(&device.name, &hosts));
                address.set(device.host);
                port.set("22".into());
                username.set(
                    tailscale_result
                        .get()
                        .map(|result| result.suggested_username)
                        .unwrap_or_default(),
                );
                // Tailscale 只解决地址发现，登录仍优先尝试本机 SSH agent。
                auth.set(SshAuthMethod::Agent);
                key.set(String::new());
                secret.set(String::new());
                tailscale_open.set(false);
                expanded.set(true);
            };
            let saved_hosts: Vec<SshHost> = store
                .hosts
                .get()
                .into_iter()
                .filter_map(|host| host.source)
                .collect();
            root = root.child(super::tailscale::tailscale_panel(
                &resolved,
                tailscale_result.get(),
                tailscale_error.get(),
                tailscale_loading.get(),
                tailscale_query,
                &saved_hosts,
                on_pick,
                refresh_tailscale.clone(),
            ));
        }
        root.into_element()
    })
}
