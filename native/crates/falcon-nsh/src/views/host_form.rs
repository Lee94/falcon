//! SSH 主机表单；凭据只交给服务端，不写入工作区偏好。
//! 「从 Tailscale 添加」选中设备后只预填这张表单，认证仍是普通 SSH。
use std::{cell::RefCell, rc::Rc};

use crate::{
    i18n::t,
    store::{ActiveDialog, SettingsTab, use_workspace},
    theme::{rgb_to_hsla, use_theme},
    zoom::zpx,
};
use falcon_core::tailscale::fresh_name;
use falcon_proto::{SshAuthMethod, SshHost, SshHostInput, TailscaleDevice, TailscaleDevices};
use nsh_core::element::input;
use nsh_core::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct HostFormProps {
    pub editing_id: Option<String>,
}

#[derive(Clone)]
struct HostDraft {
    name: String,
    address: String,
    username: String,
    port: String,
    auth: SshAuthMethod,
    key: String,
    secret: String,
}

impl From<Option<&SshHost>> for HostDraft {
    fn from(host: Option<&SshHost>) -> Self {
        Self {
            name: host.map_or_else(String::new, |host| host.name.clone()),
            address: host.map_or_else(String::new, |host| host.host.clone()),
            username: host.map_or_else(String::new, |host| host.username.clone()),
            port: host.map_or_else(|| "22".to_string(), |host| host.port.to_string()),
            auth: host.map_or(SshAuthMethod::Agent, |host| host.auth_method.clone()),
            key: host
                .and_then(|host| host.key_path.clone())
                .unwrap_or_default(),
            secret: String::new(),
        }
    }
}

#[component]
pub fn HostForm(props: HostFormProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let editing_id = props.editing_id;
    let editing_generation = editing_id
        .as_ref()
        .map(|_| store.editing_host_generation.get());
    let editing_host = editing_id.as_ref().and_then(|id| {
        store
            .editing_host
            .get()
            .filter(|host| &host.id == id)
    });
    let form_owner = Owner::current();
    let draft = Rc::new(RefCell::new(HostDraft::from(editing_host.as_ref())));
    let expanded = signal(editing_host.is_some());
    let tailscale_open = signal(false);
    let tailscale_loading = signal(false);
    let tailscale_result = signal(None::<TailscaleDevices>);
    let tailscale_error = signal(None::<String>);
    let tailscale_query = signal(String::new());
    let tailscale_request = signal(0_u64);
    let form_revision = signal(0_u64);
    let busy = signal(false);
    let form_error = signal(None::<String>);
    // 「检测连接」对表单草稿的试连结果（ok, 文案）；改连接字段即作废——同 GPUI
    // `SshFields::on_change` 清 `probe` 的语义。
    let probe = signal(None::<(bool, String)>);
    // Web 端不探测 SSH 主机，试连按钮只在原生端渲染。
    #[cfg(not(target_arch = "wasm32"))]
    let probing = signal(false);
    // 地址与用户名都非空才可试连（GPUI 的 `can_test`）。草稿存在 RefCell 里、
    // 文本输入不 bump revision，这里单独用 signal 跟踪可连性让按钮实时禁用。
    let testable = signal({
        let draft = draft.borrow();
        !draft.address.trim().is_empty() && !draft.username.trim().is_empty()
    });

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

    let resolved = theme.resolved();
    let ui = &resolved.ui;
    let header = if editing_host.is_some() {
        view()
            .child(
                text(t!("host.editTitle"))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)),
            )
            .into_element()
    } else {
        let toggle_form = expanded.clone();
        let toggle_tailscale = tailscale_open.clone();
        let refresh_on_open = refresh_tailscale.clone();
        let manual_busy = busy.clone();
        let manual_action_busy = busy.clone();
        let tailscale_busy = busy.clone();
        let tailscale_action_busy = busy.clone();
        view()
            .flex_row()
            .flex_wrap()
            .gap(zpx(8.0))
            .child(
                view()
                    .test_id("host-add-manual")
                    .px(zpx(10.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.app))
                    .disabled(move || manual_busy.get())
                    .cursor_pointer()
                    .on_press(move || {
                        if manual_action_busy.get() {
                            return;
                        }
                        let open = !toggle_form.get();
                        toggle_form.set(open);
                        if open {
                            toggle_tailscale.set(false);
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
                    .test_id("host-add-tailscale")
                    .px(zpx(10.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.app))
                    .disabled(move || tailscale_busy.get())
                    .cursor_pointer()
                    .on_press(move || {
                        if tailscale_action_busy.get() {
                            return;
                        }
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
            )
            .into_element()
    };

    let fields_draft = draft.clone();
    let fields_editing_id = editing_id.clone();
    let fields_theme = theme.clone();
    let fields_expanded = expanded.clone();
    let fields_revision = form_revision.clone();
    let fields_busy = busy.clone();
    let fields_probe = probe.clone();
    let fields_testable = testable.clone();
    let fields = dynamic(move || {
        let _revision = fields_revision.get();
        let is_busy = fields_busy.get();
        if !fields_expanded.get() {
            return view().into_element();
        }
        let resolved = fields_theme.resolved();
        let ui = &resolved.ui;
        let values = fields_draft.borrow().clone();
        let mut fields = view()
            .test_id("host-form-fields")
            .flex_col()
            .gap(zpx(10.0))
            .p(zpx(12.0))
            .rounded(zpx(12.0))
            .bg(rgb_to_hsla(ui.app));

        for (label, id, value, field) in [
            (t!("host.name"), "host-form-name", values.name, 0_u8),
            (
                t!("project.host"),
                "host-form-address",
                values.address,
                1_u8,
            ),
            (
                t!("project.port"),
                "host-form-port",
                values.port,
                2_u8,
            ),
            (
                t!("project.username"),
                "host-form-username",
                values.username,
                3_u8,
            ),
        ] {
            let field_draft = fields_draft.clone();
            let field_busy = fields_busy.clone();
            let field_probe = fields_probe.clone();
            let field_testable = fields_testable.clone();
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
                            .test_id(id)
                            .flex_1()
                            .px(zpx(8.0))
                            .py(zpx(5.0))
                            .rounded(zpx(8.0))
                            .bg(rgb_to_hsla(ui.background))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground))
                            .disabled(is_busy)
                            .value(value)
                            .on_change(move |value: &str| {
                                if field_busy.get() {
                                    return;
                                }
                                let mut draft = field_draft.borrow_mut();
                                match field {
                                    0 => draft.name = value.to_string(),
                                    1 => draft.address = value.to_string(),
                                    2 => draft.port = value.to_string(),
                                    _ => draft.username = value.to_string(),
                                }
                                if field != 0 {
                                    field_probe.set(None);
                                    field_testable.set(
                                        !draft.address.trim().is_empty()
                                            && !draft.username.trim().is_empty(),
                                    );
                                }
                            }),
                    ),
            );
        }

        fields = fields.child(view().flex_row().gap(zpx(8.0)).children(
            [
                (SshAuthMethod::Agent, "project.authAgent"),
                (SshAuthMethod::Key, "project.authKey"),
                (SshAuthMethod::Password, "project.authPassword"),
            ]
            .into_iter()
            .map(|(method, label)| {
                let selected = values.auth == method;
                let field_draft = fields_draft.clone();
                let revision = fields_revision.clone();
                let field_busy = fields_busy.clone();
                let field_probe = fields_probe.clone();
                view()
                    .label(format!("host-auth-{}", method.as_str()))
                    .px(zpx(8.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .disabled(is_busy)
                    .cursor_pointer()
                    .when(selected, |v| v.bg(rgb_to_hsla(ui.tint)))
                    .on_press(move || {
                        if field_busy.get() {
                            return;
                        }
                        field_draft.borrow_mut().auth = method.clone();
                        field_probe.set(None);
                        revision.update(|revision| *revision = revision.wrapping_add(1));
                    })
                    .child(
                        text(t!(label))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
            }),
        ));

        if values.auth == SshAuthMethod::Key {
            let field_draft = fields_draft.clone();
            let field_busy = fields_busy.clone();
            let field_probe = fields_probe.clone();
            fields = fields.child(
                input()
                    .test_id("host-form-key-path")
                    .w_full()
                    .px(zpx(8.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.background))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground))
                    .disabled(is_busy)
                    .placeholder(t!("project.keyPath"))
                    .value(values.key)
                    .on_change(move |value: &str| {
                        if !field_busy.get() {
                            field_draft.borrow_mut().key = value.to_string();
                            field_probe.set(None);
                        }
                    }),
            );
        }
        if values.auth != SshAuthMethod::Agent {
            let field_draft = fields_draft.clone();
            let field_busy = fields_busy.clone();
            let field_probe = fields_probe.clone();
            let secret_placeholder = if fields_editing_id.is_some() {
                t!("host.secretPreserve")
            } else {
                t!("project.sshPassword")
            };
            fields = fields.child(
                input()
                    .test_id("host-form-secret")
                    .w_full()
                    .px(zpx(8.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.background))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground))
                    .masked(true)
                    .disabled(is_busy)
                    .placeholder(secret_placeholder)
                    .value(values.secret)
                    .on_change(move |value: &str| {
                        if !field_busy.get() {
                            field_draft.borrow_mut().secret = value.to_string();
                            field_probe.set(None);
                        }
                    }),
            );
        }
        fields.into_element()
    });

    let action_store = store.clone();
    let action_theme = theme.clone();
    let action_draft = draft.clone();
    let action_busy = busy.clone();
    let action_error = form_error.clone();
    let action_expanded = expanded.clone();
    let action_editing_id = editing_id.clone();
    let action_probe = probe.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let action_probing = probing.clone();
    #[cfg(not(target_arch = "wasm32"))]
    let action_testable = testable.clone();
    let actions = dynamic(move || {
        if !action_expanded.get() {
            return view().into_element();
        }
        let resolved = action_theme.resolved();
        let ui = &resolved.ui;
        let is_busy = action_busy.get();
        let error = action_error.get();
        let st_submit = action_store.clone();
        let submit_draft = action_draft.clone();
        let submit_busy = action_busy.clone();
        let submit_error = action_error.clone();
        let submit_editing_id = action_editing_id.clone();
        let submit_expanded = action_expanded.clone();
        let submit_owner = form_owner.clone();
        let submit = move || {
            if submit_busy.get() {
                return;
            }
            let draft = submit_draft.borrow().clone();
            if draft.name.trim().is_empty()
                || draft.address.trim().is_empty()
                || draft.username.trim().is_empty()
            {
                submit_error.set(Some(t!("host.validationRequired")));
                return;
            }
            let Some(port) = draft.port.parse::<u16>().ok().filter(|port| *port > 0) else {
                submit_error.set(Some(t!("host.validationPort")));
                return;
            };
            let key_path = (draft.auth == SshAuthMethod::Key && !draft.key.is_empty())
                .then_some(draft.key);
            let secret = (draft.auth != SshAuthMethod::Agent && !draft.secret.is_empty())
                .then_some(draft.secret);
            let input = SshHostInput {
                name: draft.name.trim().to_string(),
                host: draft.address.trim().to_string(),
                port,
                username: draft.username.trim().to_string(),
                auth_method: draft.auth,
                key_path,
                secret,
            };
            submit_busy.set(true);
            submit_error.set(None);
            let callback_busy = submit_busy.clone();
            let callback_error = submit_error.clone();
            let callback_draft = submit_draft.clone();
            let callback_expanded = submit_expanded.clone();
            let callback_store = st_submit.clone();
            let editing = submit_editing_id.clone();
            let callback_owner = submit_owner.clone();
            let callback_connection_generation = st_submit.connection_generation();
            let callback_editing_generation = editing_generation;
            let started = st_submit.save_ssh_host(
                submit_editing_id.clone(),
                editing_generation,
                input,
                move |result| {
                    if !callback_owner.is_live()
                        || callback_store.connection_generation() != callback_connection_generation
                        || callback_editing_generation.is_some_and(|generation| {
                            callback_store.editing_host_generation.get() != generation
                        })
                    {
                        return;
                    }
                    callback_busy.set(false);
                    match result {
                        Ok(()) => {
                            callback_draft.borrow_mut().secret.clear();
                            if editing.is_none()
                                && callback_store.active_dialog.get()
                                    == ActiveDialog::Settings(SettingsTab::Hosts)
                            {
                                callback_expanded.set(false);
                            }
                        }
                        Err(error) => callback_error.set(Some(error)),
                    }
                },
            );
            if !started && submit_busy.get() {
                submit_busy.set(false);
                submit_error.set(Some(t!("host.operationBusy")));
            }
        };
        let mut save_button = view()
            .test_id("host-form-submit")
            .px(zpx(10.0))
            .py(zpx(6.0))
            .rounded(zpx(8.0))
            .bg(rgb_to_hsla(ui.tint));
        if !is_busy {
            save_button = save_button.cursor_pointer().on_press(submit);
        }
        save_button = save_button.child(
            text(if is_busy {
                t!("host.saving")
            } else if action_editing_id.is_some() {
                t!("host.saveEdits")
            } else {
                t!("host.create")
            })
            .text_xs()
            .text_color(rgb_to_hsla(ui.tint_foreground)),
        );
        let mut footer = view().flex_row().items_center().gap(zpx(8.0));
        // Web 端不在浏览器里探测 SSH 主机（同 `WorkspaceStore::test_host_connection`）。
        #[cfg(not(target_arch = "wasm32"))]
        {
            let can_test = !is_busy && !action_probing.get() && action_testable.get();
            let test_store = action_store.clone();
            let test_draft = action_draft.clone();
            let test_busy = action_busy.clone();
            let test_error = action_error.clone();
            let test_probe = action_probe.clone();
            let test_probing = action_probing.clone();
            let test_testable = action_testable.clone();
            let test_editing_id = action_editing_id.clone();
            let test_owner = form_owner.clone();
            let mut test_button = view()
                .test_id("host-form-test")
                .px(zpx(10.0))
                .py(zpx(6.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.secondary));
            if can_test {
                test_button = test_button.cursor_pointer().on_press(move || {
                    if test_busy.get() || test_probing.get() || !test_testable.get() {
                        return;
                    }
                    let Some(client) = test_store.client() else {
                        return;
                    };
                    // 试的是表单里还没保存的凭据；编辑已有主机时带 host_id，
                    // 留空的 secret / keyPath 服务端沿用已保存的（GPUI `input()` 语义）。
                    let draft = test_draft.borrow().clone();
                    let input = SshHostInput {
                        name: if draft.name.trim().is_empty() {
                            "test".to_string()
                        } else {
                            draft.name.trim().to_string()
                        },
                        host: draft.address.trim().to_string(),
                        port: draft.port.trim().parse().unwrap_or(0),
                        username: draft.username.trim().to_string(),
                        auth_method: draft.auth,
                        key_path: (draft.auth == SshAuthMethod::Key).then_some(draft.key),
                        secret: (!draft.secret.is_empty()).then_some(draft.secret),
                    };
                    test_probing.set(true);
                    test_probe.set(None);
                    test_error.set(None);
                    let generation = test_store.connection_generation();
                    let host_id = test_editing_id.clone();
                    let callback_store = test_store.clone();
                    let callback_owner = test_owner.clone();
                    // 挂到 store 的根 owner：本区域会随 test_probing 翻转重建，
                    // 区域 owner 一收摊任务就被取消、界面永远停在 Connecting。
                    test_store.spawn_root(async move {
                        let result = client
                            .test_host_draft(&input, host_id.as_deref())
                            .await;
                        if !callback_owner.is_live()
                            || callback_store.connection_generation() != generation
                        {
                            return;
                        }
                        test_probing.set(false);
                        match result {
                            Ok(falcon_proto::SshProbeResult::Reachable { kind, home }) => {
                                let kind = match kind {
                                    falcon_proto::HostKind::Windows => {
                                        t!("host.testKind_windows")
                                    }
                                    _ => t!("host.testKind_posix"),
                                };
                                test_probe.set(Some((
                                    true,
                                    format!("{} · {kind} · {home}", t!("host.testOk")),
                                )));
                            }
                            Ok(falcon_proto::SshProbeResult::Failed { error }) => {
                                test_probe.set(Some((false, error)));
                            }
                            Err(error) => {
                                test_probe.set(Some((false, error.to_string())));
                            }
                        }
                    });
                });
            }
            footer = footer.child(test_button.child(
                text(if action_probing.get() {
                    t!("host.testing")
                } else {
                    t!("host.test")
                })
                .text_xs()
                .text_color(rgb_to_hsla(ui.secondary_foreground)),
            ));
        }
        if let Some(error) = error {
            footer = footer.child(
                text(error)
                    .test_id("host-form-error")
                    .flex_1()
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            );
        }
        footer = footer.child(save_button);
        if action_editing_id.is_some() {
            let cancel_store = action_store.clone();
            let cancel_draft = action_draft.clone();
            let cancel_error = action_error.clone();
            let cancel_id = action_editing_id.clone();
            let cancel_generation = editing_generation;
            let mut cancel = view()
                .test_id("host-form-cancel")
                .px(zpx(10.0))
                .py(zpx(6.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.secondary));
            if !is_busy {
                cancel = cancel.cursor_pointer().on_press(move || {
                    cancel_draft.borrow_mut().secret.clear();
                    cancel_error.set(None);
                    if let (Some(id), Some(generation)) =
                        (cancel_id.as_deref(), cancel_generation)
                    {
                        cancel_store.clear_host_edit_if_current(id, generation);
                    }
                    cancel_store
                        .active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::Hosts));
                });
            }
            footer = footer.child(cancel.child(
                text(t!("common.cancel"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.secondary_foreground)),
            ));
        }
        view()
            .flex_col()
            .gap(zpx(8.0))
            .when_some(action_probe.get(), |column, (ok, message)| {
                column.child(
                    text(message)
                        .test_id("host-form-probe")
                        .text_xs()
                        .text_color(rgb_to_hsla(if ok { ui.success } else { ui.destructive })),
                )
            })
            .child(footer)
            .into_element()
    });

    let pick_store = store.clone();
    let pick_draft = draft.clone();
    let pick_revision = form_revision.clone();
    let pick_expanded = expanded.clone();
    let pick_tailscale_open = tailscale_open.clone();
    let pick_result = tailscale_result.clone();
    let pick_probe = probe.clone();
    let pick_testable = testable.clone();
    let on_pick = move |device: TailscaleDevice| {
        let hosts: Vec<SshHost> = pick_store
            .hosts
            .get()
            .into_iter()
            .filter_map(|host| host.source)
            .collect();
        let username = pick_result
            .get()
            .map(|result| result.suggested_username)
            .unwrap_or_default();
        let mut draft = pick_draft.borrow_mut();
        draft.name = fresh_name(&device.name, &hosts);
        draft.address = device.host;
        draft.port = "22".into();
        draft.username = username;
        draft.auth = SshAuthMethod::Agent;
        draft.key.clear();
        draft.secret.clear();
        let testable =
            !draft.address.trim().is_empty() && !draft.username.trim().is_empty();
        drop(draft);
        pick_probe.set(None);
        pick_testable.set(testable);
        pick_revision.update(|revision| *revision = revision.wrapping_add(1));
        pick_tailscale_open.set(false);
        pick_expanded.set(true);
    };

    let tailscale_store = store.clone();
    let tailscale_theme = theme.clone();
    let tailscale_open_state = tailscale_open.clone();
    let tailscale_result_state = tailscale_result.clone();
    let tailscale_error_state = tailscale_error.clone();
    let tailscale_loading_state = tailscale_loading.clone();
    let tailscale_query_state = tailscale_query.clone();
    let tailscale_refresh = refresh_tailscale.clone();
    let tailscale_panel = dynamic(move || {
        if !tailscale_open_state.get() {
            return view().into_element();
        }
        let resolved = tailscale_theme.resolved();
        let saved_hosts: Vec<SshHost> = tailscale_store
            .hosts
            .get()
            .into_iter()
            .filter_map(|host| host.source)
            .collect();
        super::tailscale::tailscale_panel(
            &resolved,
            tailscale_result_state.get(),
            tailscale_error_state.get(),
            tailscale_loading_state.get(),
            tailscale_query_state,
            &saved_hosts,
            on_pick.clone(),
            tailscale_refresh.clone(),
        )
        .into_element()
    });

    view()
        .test_id("host-form")
        .flex_col()
        .gap(zpx(12.0))
        .child(header)
        .child(fields)
        .child(actions)
        .child(tailscale_panel)
}
