//! 没装 CLI 的提示与 device-code 登录卡片（web `MeeglePanel.tsx` 的 `InstallHint` / `LoginCard`）。
//!
//! 登录：点「登录」让后端起 `meegle auth login --device-code`，拿到授权链接后这里给一个按钮，
//! 用系统浏览器打开；用户授权完成，面板每 2s 的状态轮询会把卡片切成正文（ADR 0010 决定三：
//! 授权链接开在**用户**这台机器的浏览器里，不是后端那台）。

use falcon_proto::{MEEGLE_HOSTS, MeegleStatus};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableListItem;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_kit::component::{Disableable, IndexPath, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Div, Entity, IntoElement, Render, SharedString, Subscription, Window, div, relative};
use rust_i18n::t;

use super::common::Ctx;
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

/// 没找到 CLI（内置的那份跑不起来，PATH 里也没有）
pub(super) fn install_hint(bin: Option<String>, checking: bool, cx: &App, on_recheck: impl Fn(&mut gpui_kit::Window, &mut App) + 'static) -> Div {
    let ui = Ui::global(cx);
    div()
        .px_3()
        .py_4()
        .flex()
        .flex_col()
        .text_size(zpx(12.))
        .line_height(relative(1.6))
        .text_color(ui.muted_foreground)
        .child(div().child(t!("meegle.notInstalled").to_string()))
        .when_some(bin, |d, bin| d.child(div().mt_1().text_size(zpx(11.)).child(t!("native.meegle.labelled", label = t!("meegle.binTried"), value = bin).to_string())))
        .child(
            div()
                .mt_2()
                .px_2()
                .py(zpx(6.))
                .rounded(radius::SM)
                .bg(ui.app)
                .text_size(zpx(11.))
                .text_color(ui.foreground)
                .child("npm install -g @lark-project/meegle"),
        )
        .child(div().mt_2().child(t!("meegle.installHint").to_string()))
        .child(
            div().mt_3().flex().child(
                Button::new("meegle-recheck")
                    .outline()
                    .small()
                    .icon(IconName::RefreshCw)
                    .loading(checking)
                    .label(t!("meegle.recheck").to_string())
                    .on_click(move |_, window, cx| on_recheck(window, cx)),
            ),
        )
}

/// 站点下拉的一项：两个已知站点 + 自定义
#[derive(Clone)]
struct HostOption {
    value: SharedString,
    label: SharedString,
    host: Option<SharedString>,
}

impl SearchableListItem for HostOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    /// 触发器上也带域名（web 的 SelectValue 就是选中项的整行内容）
    fn display_title(&self) -> Option<gpui_kit::AnyElement> {
        let host = self.host.clone()?;
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.label.clone())
                .child(div().text_size(zpx(11.)).opacity(0.6).child(host))
                .into_any_element(),
        )
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let ui = Ui::global(cx);
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.label.clone())
            .when_some(self.host.clone(), |d, h| d.child(div().text_size(zpx(12.)).text_color(ui.muted_foreground).child(h)))
    }

    fn value(&self) -> &SharedString {
        &self.value
    }
}

const HOST_CUSTOM: &str = "__custom";

pub struct LoginCard {
    ctx: Ctx,
    select: Entity<SelectState<SearchableVec<HostOption>>>,
    choice: String,
    custom: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    _subs: Vec<Subscription>,
}

impl LoginCard {
    pub(super) fn new(ctx: Ctx, status: &MeegleStatus, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let options = vec![
            HostOption { value: MEEGLE_HOSTS[0].into(), label: t!("meegle.hostFeishu").to_string().into(), host: Some(MEEGLE_HOSTS[0].into()) },
            HostOption { value: MEEGLE_HOSTS[1].into(), label: t!("meegle.hostMeegle").to_string().into(), host: Some(MEEGLE_HOSTS[1].into()) },
            HostOption { value: HOST_CUSTOM.into(), label: t!("meegle.hostCustom").to_string().into(), host: None },
        ];
        // 上次登录过的站点优先；不是两个已知站点之一就当自定义域名
        let known = status.host.as_deref().is_some_and(|h| MEEGLE_HOSTS.contains(&h));
        let choice = match &status.host {
            Some(h) if known => h.clone(),
            Some(_) => HOST_CUSTOM.to_string(),
            None => MEEGLE_HOSTS[0].to_string(),
        };
        let ix = options.iter().position(|o| o.value.as_ref() == choice).unwrap_or(0);
        let select = cx.new(|cx| SelectState::new(SearchableVec::new(options), Some(IndexPath::new(ix)), window, cx));
        let custom_default = status.host.clone().filter(|_| !known).unwrap_or_default();
        let custom = cx.new(|cx| InputState::new(window, cx).placeholder(t!("meegle.hostPlaceholder").to_string()).default_value(custom_default));
        let subs = vec![
            cx.subscribe(&select, |this, _, ev: &SelectEvent<SearchableVec<HostOption>>, cx| {
                let SelectEvent::Confirm(Some(v)) = ev else { return };
                this.choice = v.to_string();
                cx.notify();
            }),
            cx.subscribe(&custom, |this, _, ev: &InputEvent, cx| match ev {
                InputEvent::PressEnter { .. } => this.submit(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
        ];
        Self { ctx, select, choice, custom, busy: false, error: None, _subs: subs }
    }

    fn host(&self, cx: &App) -> String {
        if self.choice == HOST_CUSTOM { self.custom.read(cx).value().trim().to_string() } else { self.choice.clone() }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let host = self.host(cx);
        if host.is_empty() {
            self.error = Some(t!("meegle.hostInvalid").to_string());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let fut = self.ctx.client.meegle_login(&host);
        let ctx = self.ctx.clone();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.busy = false;
                match res {
                    Ok(_) => {
                        let _ = ctx.panel.update(cx, |p, cx| p.login_changed(cx));
                    }
                    Err(err) => {
                        ctx.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                        this.error = Some(err.message.clone());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let fut = self.ctx.client.meegle_cancel_login();
        let ctx = self.ctx.clone();
        cx.spawn(async move |_, cx| {
            let _ = fut.await;
            cx.update(|cx| {
                let _ = ctx.panel.update(cx, |p, cx| p.login_changed(cx));
            });
        })
        .detach();
    }
}

impl Render for LoginCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let login = self.ctx.panel.upgrade().and_then(|p| p.read(cx).status.as_ref().and_then(|s| s.login.clone()));
        let base = div().px_3().py_4().flex().flex_col().text_size(zpx(12.)).line_height(relative(1.6)).text_color(ui.muted_foreground);

        if let Some(login) = login {
            let url = login.url.clone();
            return base
                .child(div().child(t!("meegle.loginPending").to_string()))
                .child(
                    div().mt_3().flex().child(
                        Button::new("meegle-open-auth")
                            .primary()
                            .w_full()
                            .icon(IconName::ExternalLink)
                            .label(t!("meegle.openAuth").to_string())
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                )
                .when(!login.code.is_empty(), |d| {
                    d.child(
                        div()
                            .mt_2()
                            .flex()
                            .gap_1()
                            .child(t!("native.meegle.labelPrefix", label = t!("meegle.authCode")).to_string())
                            .child(div().text_color(ui.foreground).child(login.code.clone())),
                    )
                })
                .child(div().mt_1().truncate().text_size(zpx(11.)).child(login.host.clone()))
                .child(
                    div().mt_3().flex().child(
                        Button::new("meegle-cancel-login")
                            .outline()
                            .small()
                            .label(t!("meegle.cancelLogin").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ),
                )
                .into_any_element();
        }

        let host = self.host(cx);
        base.child(div().child(t!("meegle.notLoggedIn").to_string()))
            .child(div().mt_3().text_size(zpx(11.)).child(t!("meegle.host").to_string()))
            .child(div().mt_1().child(Select::new(&self.select).w_full()))
            .when(self.choice == HOST_CUSTOM, |d| d.child(div().mt(zpx(6.)).child(Input::new(&self.custom))))
            .child(
                div().mt_3().flex().child(
                    Button::new("meegle-login")
                        .primary()
                        .icon(icon(IconName::LogIn))
                        .loading(self.busy)
                        .disabled(host.is_empty())
                        .label(if self.busy { t!("meegle.loginStarting") } else { t!("meegle.login") }.to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                ),
            )
            .when_some(self.error.clone(), |d, err| d.child(div().mt_2().text_size(zpx(11.)).text_color(ui.destructive).child(err)))
            .into_any_element()
    }
}
