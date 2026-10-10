//! 账户：访问密码（设置 / 修改）、钥匙串里记着的密码、退出登录。web 的 AccountPane。
//!
//! "记住的密码"一行是原生独有的：web 靠浏览器 cookie，原生把密码存进系统钥匙串，服务端重启
//! （token 只在它内存里）后客户端用它自动重登（profiles.rs）。所以这里要能看见、能忘掉；
//! 改了访问密码时钥匙串里若记着旧的，一并换成新的——否则下次自动重登拿旧密码必然失败。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, IntoElement, Render, Window, div};
use rust_i18n::t;

use super::widgets::{field, rows, section};
use crate::theme::Ui;
use crate::workspace::{AuthPhase, ToastKind, Workspace};
use crate::zoom::zpx;

pub struct AccountPane {
    ws: Entity<Workspace>,
    current: Entity<InputState>,
    next: Entity<InputState>,
    error: Option<String>,
    busy: bool,
    /// 钥匙串里有没有这台服务端的密码。读钥匙串是一次系统调用（还可能弹授权），只在打开时
    /// 与改动后读，不在每帧 render 里读
    keychain: bool,
}

impl AccountPane {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current = cx.new(|cx| InputState::new(window, cx).masked(true));
        let next = cx.new(|cx| InputState::new(window, cx).masked(true));
        for input in [&current, &next] {
            cx.subscribe_in(input, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.submit(window, cx),
                _ => {}
            })
            .detach();
        }
        let keychain = ws.read(cx).saved_password().is_some();
        Self { ws, current, next, error: None, busy: false, keychain }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.next.read(cx).value().to_string();
        if self.busy || next.chars().count() < 6 {
            return;
        }
        let current = self.current.read(cx).value().to_string();
        let client = self.ws.read(cx).client.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = client.set_password(&next, (!current.is_empty()).then_some(current.as_str())).await;
            let status = match &result {
                Ok(_) => client.auth_status().await.ok(),
                Err(_) => None,
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                this.update(cx, |this, cx| {
                    this.busy = false;
                    match result {
                        Err(err) => this.error = Some(err.to_string()),
                        Ok(_) => {
                            this.current.update(cx, |i, cx| i.set_value("", window, cx));
                            this.next.update(cx, |i, cx| i.set_value("", window, cx));
                            let ws = this.ws.clone();
                            let keychain = this.keychain;
                            // web 的 refreshAuth：第一次设密码后服务端开始要求登录，这个客户端还没有
                            // token——整页换成登录（设置对话框一起收起，否则盖在登录框上面）
                            let need_login = ws.update(cx, |w, cx| {
                                if keychain {
                                    w.store_password(Some(&next));
                                    w.client.set_relogin_password(Some(next.clone()));
                                }
                                w.toast(ToastKind::Success, t!("password.saved").to_string(), None, cx);
                                let need = status.as_ref().is_some_and(|s| s.required && !s.authenticated);
                                if let Some(s) = status {
                                    w.auth = Some(s);
                                }
                                if need {
                                    w.auth_phase = AuthPhase::NeedLogin;
                                }
                                cx.notify();
                                need
                            });
                            if need_login {
                                window.close_dialog(cx);
                            }
                        }
                    }
                    cx.notify();
                })
                .ok();
            });
        })
        .detach();
    }

    fn forget(&mut self, cx: &mut Context<Self>) {
        self.ws.update(cx, |w, cx| {
            w.store_password(None);
            w.client.set_relogin_password(None);
            w.toast(ToastKind::Info, t!("native.settings.keychainForgot").to_string(), None, cx);
        });
        self.keychain = false;
        cx.notify();
    }
}

impl Render for AccountPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let auth = self.ws.read(cx).auth.clone();
        let password_set = auth.as_ref().is_some_and(|a| a.password_set);
        let can_submit = !self.busy && self.next.read(cx).value().chars().count() >= 6;

        let status = |text: String| div().text_sm().text_color(ui.muted_foreground).child(text);
        let mut children = rows(
            vec![
                (
                    t!("settings.passwordStatus").to_string(),
                    None,
                    status(if password_set { t!("settings.passwordSet") } else { t!("settings.passwordUnset") }.to_string())
                        .into_any_element(),
                ),
                (
                    t!("native.settings.keychain").to_string(),
                    Some(t!("native.settings.keychainHint").to_string()),
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(status(
                            if self.keychain { t!("native.settings.keychainStored") } else { t!("native.settings.keychainEmpty") }
                                .to_string(),
                        ))
                        .child(
                            Button::new("keychain-forget")
                                .outline()
                                .small()
                                .disabled(!self.keychain)
                                .label(t!("native.settings.keychainForget").to_string())
                                .on_click(cx.listener(|this, _, _, cx| this.forget(cx))),
                        )
                        .into_any_element(),
                ),
            ],
            true,
            cx,
        );
        let mut form = div().flex().flex_col().gap_3().pt_4().max_w(zpx(448.));
        if password_set {
            form = form.child(field(t!("password.current").to_string(), Input::new(&self.current), cx));
        }
        form = form.child(field(t!("password.next").to_string(), Input::new(&self.next), cx));
        if let Some(err) = &self.error {
            form = form.child(div().text_size(zpx(13.)).text_color(ui.destructive).child(err.clone()));
        }
        form = form.child(
            div().flex().child(
                Button::new("password-submit")
                    .primary()
                    .small()
                    .loading(self.busy)
                    .disabled(!can_submit)
                    .label(t!("password.submit").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
            ),
        );
        children.push(form.into_any_element());

        let mut out: Vec<AnyElement> =
            vec![section(t!("settings.accountTitle").to_string(), Some(t!("password.hint").to_string()), children, cx).into_any_element()];
        if auth.as_ref().is_some_and(|a| a.required && a.authenticated) {
            let ws = self.ws.clone();
            let logout = Button::new("logout")
                .outline()
                .small()
                .label(t!("common.logout").to_string())
                .on_click(move |_, window, cx| {
                    ws.update(cx, |w, cx| w.logout(cx));
                    // 登出后整页换成登录框，设置别再盖在上面
                    window.close_dialog(cx);
                });
            out.push(
                section(
                    t!("common.logout").to_string(),
                    None,
                    rows(vec![(t!("common.logout").to_string(), Some(t!("settings.logoutHint").to_string()), logout.into_any_element())], false, cx),
                    cx,
                )
                .into_any_element(),
            );
        }
        div().flex().flex_col().children(out)
    }
}
