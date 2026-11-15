//! 新增 SSH 主机表单；凭据只交给服务端，不写入工作区偏好。
use crate::{
    i18n::t,
    store::use_workspace,
    theme::{rgb_to_hsla, use_theme},
    zoom::zpx,
};
use falcon_proto::{SshAuthMethod, SshHostInput};
use nsh_core::element::input;
use nsh_core::prelude::*;

#[component]
pub fn HostForm() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let expanded = signal(false);
    let name = signal(String::new());
    let address = signal(String::new());
    let username = signal(String::new());
    let port = signal("22".to_string());
    let auth = signal(SshAuthMethod::Agent);
    let key = signal(String::new());
    let secret = signal(String::new());
    dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let st = store.clone();
        let mut root = view().flex_col().gap(zpx(12.0)).child(
            view()
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.app))
                .cursor_pointer()
                .on_press(move || expanded.update(|v| *v = !*v))
                .child(
                    text(t!("host.add"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                ),
        );
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
                        .masked(true)
                        .px(zpx(8.0))
                        .py(zpx(5.0))
                        .rounded(zpx(8.0))
                        .bg(rgb_to_hsla(ui.background))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
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
        root.into_element()
    })
}
