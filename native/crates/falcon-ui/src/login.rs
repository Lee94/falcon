//! 登录（web 的 `components/Login.tsx`）：服务端设了访问密码、或绑在非回环地址时需要。
//!
//! 默认"记住密码"：存进钥匙串，服务端重启（token 只在它内存里）后客户端自动重登，
//! 不用每次重新输入。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Render, Window, div};
use rust_i18n::t;

use crate::theme::{Ui, radius};
use crate::workspace::Workspace;
use crate::zoom::zpx;

pub struct LoginView {
    ws: Entity<Workspace>,
    input: Entity<InputState>,
    remember: bool,
    error: Option<String>,
    busy: bool,
}

impl LoginView {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("login.password").to_string())
        });
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.submit(cx);
            }
        })
        .detach();
        Self {
            ws,
            input,
            remember: true,
            error: None,
            busy: false,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let password = self.input.read(cx).value().to_string();
        if password.is_empty() {
            return;
        }
        self.busy = true;
        self.error = None;
        let remember = self.remember;
        let task = self.ws.update(cx, |w, cx| w.login(password, remember, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                if result.is_err() {
                    this.error = Some(t!("login.failed").to_string());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for LoginView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let server = crate::window::display_name(&self.ws.read(cx).profile, cx);
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                crate::ui::island(cx)
                    .w(zpx(340.))
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .rounded(radius::XL)
                    .child(div().text_base().font_weight(gpui_kit::FontWeight::SEMIBOLD).child(t!("login.title").to_string()))
                    .child(div().text_xs().text_color(ui.muted_foreground).child(server))
                    .child(Input::new(&self.input))
                    .child(
                        Checkbox::new("login-remember")
                            .checked(self.remember)
                            .label(t!("native.login.remember").to_string())
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.remember = *checked;
                                cx.notify();
                            })),
                    )
                    .when_some(self.error.clone(), |d, err| {
                        d.child(div().text_xs().text_color(ui.destructive).child(err))
                    })
                    .child(
                        Button::new("login-submit")
                            .primary()
                            .loading(self.busy)
                            .label(t!("login.submit").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
    }
}
