//! 远端主机表单（web 的 `components/HostForm.tsx` + `SshFields.tsx`）。
//!
//! 名称 / 主机 / 端口 / 用户 / 认证方式（key / password / agent）/ 私钥路径 / 密码；
//! 「检测连接」试连的是表单里**还没保存**的凭据（`POST /api/hosts/test`，编辑时带上 hostId，
//! 留空的 secret / keyPath 沿用已保存的）。存完回调 `on_saved`——从新建项目表单里顺手加一台
//! 时，把新主机直接选进那张表单，不用用户再点一次下拉框。
//!
//! [`SshFields`] 是主机表单和存量项目（没绑定已保存主机的 SSH 项目）手写 SSH 共用的连接字段。

use std::rc::Rc;

use falcon_proto::{SshAuthMethod, SshHost, SshHostInput, SshProbeResult};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent};
use gpui_kit::component::{Disableable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Entity, IntoElement, Render, Window, div};
use rust_i18n::t;

use crate::dialogs::project_form::kit::{self, OptState, Options, Opt, Tone, error_line, field};
use crate::theme::Ui;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

pub type OnSaved = Rc<dyn Fn(&SshHost, &mut Window, &mut App)>;

// ---------------- 共用的 SSH 连接字段 ----------------

/// 表单里的初值（主机表单取自 SshHost，存量项目取自 project.ssh）
#[derive(Clone, Debug)]
pub struct SshInit {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    pub key_path: String,
}

impl Default for SshInit {
    fn default() -> Self {
        Self { host: String::new(), port: 22, username: String::new(), auth_method: SshAuthMethod::Key, key_path: String::new() }
    }
}

/// 表单当前填的值
#[derive(Clone, Debug)]
pub struct SshValues {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    pub key_path: String,
    pub secret: String,
}

pub struct SshFields {
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub username: Entity<InputState>,
    pub auth: Entity<OptState>,
    pub key_path: Entity<InputState>,
    pub secret: Entity<InputState>,
    /// 已保存过密码 / passphrase：提示"留空则保持不变"
    pub has_secret: bool,
}

impl SshFields {
    pub fn new<V: 'static>(init: SshInit, has_secret: bool, window: &mut Window, cx: &mut Context<V>) -> Self {
        let input = |value: String, placeholder: Option<&str>, window: &mut Window, cx: &mut Context<V>| {
            cx.new(|cx| {
                let s = InputState::new(window, cx).default_value(value);
                match placeholder {
                    Some(p) => s.placeholder(p.to_string()),
                    None => s,
                }
            })
        };
        let host = input(init.host, None, window, cx);
        let port = input(init.port.to_string(), None, window, cx);
        let username = input(init.username, None, window, cx);
        let key_path = input(init.key_path, Some("~/.ssh/id_ed25519"), window, cx);
        let secret = cx.new(|cx| InputState::new(window, cx).masked(true));
        let options = Options::flat(vec![
            Opt::new("key", t!("project.authKey").to_string()),
            Opt::new("password", t!("project.authPassword").to_string()),
            Opt::new("agent", t!("project.authAgent").to_string()),
        ]);
        let auth = cx.new(|cx| kit::new_select(options, init.auth_method.as_str(), window, cx));
        Self { host, port, username, auth, key_path, secret, has_secret }
    }

    /// 任何一个字段变了就回调（主机表单据此清掉上一次的检测结果；也让认证方式切换时重画）
    pub fn on_change<V: 'static>(&self, window: &mut Window, cx: &mut Context<V>, f: impl Fn(&mut V, &mut Context<V>) + Clone + 'static) {
        for input in [&self.host, &self.port, &self.username, &self.key_path, &self.secret] {
            let f = f.clone();
            cx.subscribe_in(input, window, move |this, _, ev: &InputEvent, _, cx| {
                if matches!(ev, InputEvent::Change) {
                    f(this, cx);
                }
            })
            .detach();
        }
        cx.subscribe_in(&self.auth, window, move |this, _, _: &SelectEvent<Options>, _, cx| f(this, cx))
            .detach();
    }

    pub fn auth_method(&self, cx: &App) -> SshAuthMethod {
        kit::selected(&self.auth, cx)
            .and_then(|v| SshAuthMethod::from_wire(&v))
            .unwrap_or(SshAuthMethod::Key)
    }

    pub fn values(&self, cx: &App) -> SshValues {
        let v = |s: &Entity<InputState>| s.read(cx).value().to_string();
        SshValues {
            host: v(&self.host),
            // web 是 Number(value)：空串 / 非数字交给服务端（主机端点按 `port || 22` 兜底）
            port: v(&self.port).trim().parse().unwrap_or(0),
            username: v(&self.username),
            auth_method: self.auth_method(cx),
            key_path: v(&self.key_path),
            secret: v(&self.secret),
        }
    }

    pub fn render(&self, cx: &App) -> Vec<AnyElement> {
        let method = self.auth_method(cx);
        let mut out = vec![
            div()
                .flex()
                .gap(zpx(10.))
                .child(div().flex_1().min_w_0().child(field(Some(t!("project.host").to_string()), Input::new(&self.host), None, cx)))
                .child(div().w(zpx(88.)).child(field(Some(t!("project.port").to_string()), Input::new(&self.port), None, cx)))
                .into_any_element(),
            field(Some(t!("project.username").to_string()), Input::new(&self.username), None, cx).into_any_element(),
            field(Some(t!("project.authMethod").to_string()), Select::new(&self.auth).w_full(), None, cx).into_any_element(),
        ];
        if method == SshAuthMethod::Key {
            out.push(field(Some(t!("project.keyPath").to_string()), Input::new(&self.key_path), None, cx).into_any_element());
        }
        if method != SshAuthMethod::Agent {
            let label = if method == SshAuthMethod::Key { t!("project.passphrase") } else { t!("project.sshPassword") };
            let hint = self.has_secret.then(|| (t!("project.secretKept").to_string(), Tone::Muted));
            out.push(field(Some(label.to_string()), Input::new(&self.secret).mask_toggle(), hint, cx).into_any_element());
        }
        out
    }
}

// ---------------- 主机表单 ----------------

pub struct HostForm {
    ws: Entity<Workspace>,
    existing: Option<SshHost>,
    on_saved: Option<OnSaved>,
    name: Entity<InputState>,
    ssh: SshFields,
    error: Option<String>,
    busy: bool,
    /// 上一次检测连接的结果（ok, 文案）；改了任何连接字段就清掉
    probe: Option<(bool, String)>,
    probing: bool,
}

impl HostForm {
    fn new(ws: Entity<Workspace>, existing: Option<SshHost>, on_saved: Option<OnSaved>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.namePlaceholder").to_string())
                .default_value(existing.as_ref().map(|h| h.name.clone()).unwrap_or_default())
        });
        cx.subscribe_in(&name, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let init = existing
            .as_ref()
            .map(|h| SshInit {
                host: h.host.clone(),
                port: h.port,
                username: h.username.clone(),
                auth_method: h.auth_method,
                key_path: h.key_path.clone().unwrap_or_default(),
            })
            .unwrap_or_default();
        let ssh = SshFields::new(init, existing.as_ref().is_some_and(|h| h.has_secret), window, cx);
        ssh.on_change(window, cx, |this: &mut Self, cx| {
            this.probe = None;
            cx.notify();
        });
        Self { ws, existing, on_saved, name, ssh, error: None, busy: false, probe: None, probing: false }
    }

    fn input(&self, cx: &App) -> SshHostInput {
        let v = self.ssh.values(cx);
        SshHostInput {
            name: self.name.read(cx).value().to_string(),
            host: v.host,
            port: v.port,
            username: v.username,
            auth_method: v.auth_method,
            key_path: (v.auth_method == SshAuthMethod::Key).then_some(v.key_path),
            secret: (!v.secret.is_empty()).then_some(v.secret),
        }
    }

    fn test_conn(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut input = self.input(cx);
        if input.host.trim().is_empty() || input.username.trim().is_empty() || self.probing {
            return;
        }
        if input.name.trim().is_empty() {
            input.name = "test".into();
        }
        self.probing = true;
        self.probe = None;
        self.error = None;
        let client = self.ws.read(cx).client.clone();
        let host_id = self.existing.as_ref().map(|h| h.id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.test_host_draft(&input, host_id.as_deref()).await;
            this.update(cx, |this, cx| {
                this.probing = false;
                this.probe = Some(match result {
                    Ok(SshProbeResult::Reachable { kind, home }) => {
                        let kind_key = format!("host.testKind_{}", kind.as_str());
                        (true, format!("{} · {} · {home}", t!("host.testOk"), t!(&kind_key)))
                    }
                    Ok(SshProbeResult::Failed { error }) => (false, error),
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        (false, err.message.clone())
                    }
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input(cx);
        if self.busy || input.name.trim().is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        let client = self.ws.read(cx).client.clone();
        let edit_id = self.existing.as_ref().map(|h| h.id.clone());
        cx.spawn_in(window, async move |this, cx| {
            let result = match &edit_id {
                Some(id) => client.update_host(id, &input).await,
                None => client.create_host(&input).await,
            };
            this.update_in(cx, |this, window, cx| {
                this.busy = false;
                match result {
                    Ok(saved) => {
                        let editing = edit_id.is_some();
                        let saved2 = saved.clone();
                        this.ws.update(cx, |w, cx| {
                            // 先把结果放进列表：项目表单的下拉马上就要选它，等不及那一轮刷新
                            match w.hosts.iter_mut().find(|h| h.id == saved2.id) {
                                Some(h) => *h = saved2.clone(),
                                None => w.hosts.push(saved2.clone()),
                            }
                            w.refresh_hosts(cx);
                            // 连接配置会刷回引用它的项目
                            if editing {
                                w.refresh_projects(cx);
                            }
                            let title = if editing {
                                t!("host.saved", name = saved2.name.clone())
                            } else {
                                t!("host.created", name = saved2.name.clone())
                            };
                            w.toast(ToastKind::Success, title.to_string(), None, cx);
                            cx.notify();
                        });
                        window.close_dialog(cx);
                        if let Some(f) = this.on_saved.clone() {
                            f(&saved, window, cx);
                        }
                    }
                    Err(err) => {
                        this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for HostForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let v = self.ssh.values(cx);
        let can_test = !self.probing && !v.host.trim().is_empty() && !v.username.trim().is_empty();
        let can_save = !self.busy && !self.name.read(cx).value().trim().is_empty();
        let save_label = if self.existing.is_some() { t!("host.save") } else { t!("host.create") };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(field(Some(t!("host.name").to_string()), Input::new(&self.name), None, cx))
            .children(self.ssh.render(cx))
            .when_some(self.error.clone(), |d, e| d.child(error_line(e, cx)))
            .when_some(self.probe.clone(), |d, (ok, text)| {
                d.child(div().text_size(zpx(13.)).text_color(if ok { ui.success } else { ui.destructive }).child(text))
            })
            .child(
                div()
                    .mt_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("host-test")
                            .outline()
                            .label(if self.probing { t!("host.testing") } else { t!("host.test") }.to_string())
                            .disabled(!can_test)
                            .on_click(cx.listener(|this, _, window, cx| this.test_conn(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("host-cancel")
                            .outline()
                            .label(t!("common.cancel").to_string())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("host-save")
                            .primary()
                            .label(save_label.to_string())
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

pub fn open(ws: &Entity<Workspace>, edit: Option<SshHost>, on_saved: Option<OnSaved>, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| HostForm::new(ws.clone(), edit, on_saved, window, cx));
    let f = form.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let title = if f.read(cx).existing.is_some() { t!("host.editTitle") } else { t!("host.createTitle") };
        let submit = f.clone();
        // lockOverlay：填了一半的 SSH 配置手滑点到遮罩就全丢，不可接受
        dialog
            .title(title.to_string())
            .overlay_closable(false)
            .close_button(false)
            .child(f.clone())
            .on_ok(move |_, window, cx| {
                // 回车 = 提交；成功后由提交流程自己关对话框
                submit.update(cx, |this, cx| this.submit(window, cx));
                false
            })
    });
    let name = form.read(cx).name.clone();
    name.update(cx, |i, cx| i.focus(window, cx));
}
