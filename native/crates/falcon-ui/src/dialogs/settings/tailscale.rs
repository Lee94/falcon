//! 设置 → Tailscale：客户端内置 Tailscale 节点开关、状态、登录与高级配置（ADR 0030）。
//!
//! 这是客户端本机的设置，不存服务端。

use falcon_tailnet::State;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, FontWeight, IntoElement, Render, Window, div,
};
use rust_i18n::t;

use super::widgets;
use crate::tailnet::{LibraryState, TailnetController};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

pub struct TailscalePane {
    ws: Entity<Workspace>,
    tailnet: Entity<TailnetController>,
    hostname: Entity<InputState>,
    control_url: Entity<InputState>,
    auth_key: Entity<InputState>,
    advanced_open: bool,
}

impl TailscalePane {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let tailnet = crate::tailnet::entity(cx);
        cx.observe(&tailnet, |_, _, cx| cx.notify()).detach();
        let (cfg_hostname, cfg_control, default_host) = {
            let tn = tailnet.read(cx);
            (
                tn.config().hostname.clone(),
                tn.config().control_url.clone(),
                tn.default_hostname().to_owned(),
            )
        };
        let hostname = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder(default_host);
            s.set_value(cfg_hostname, window, cx);
            s
        });
        let control_url = cx.new(|cx| {
            let mut s = InputState::new(window, cx)
                .placeholder(t!("tailnet.controlUrlPlaceholder").to_string());
            s.set_value(cfg_control, window, cx);
            s
        });
        let auth_key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(t!("tailnet.authKeyPlaceholder").to_string())
        });
        Self { ws, tailnet, hostname, control_url, auth_key, advanced_open: false }
    }

    fn save_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hostname = self.hostname.read(cx).value().trim().to_owned();
        let control_url = self.control_url.read(cx).value().trim().to_owned();
        let raw_key = self.auth_key.read(cx).value().trim().to_owned();
        let auth_key = (!raw_key.is_empty()).then_some(raw_key);
        // auth key 只写不读：提交后立即清掉输入框
        if auth_key.is_some() {
            self.auth_key.update(cx, |s, cx| s.set_value(String::new(), window, cx));
        }
        self.tailnet.update(cx, |tn, cx| tn.update_settings(hostname, control_url, auth_key, cx));
        self.ws.update(cx, |w, cx| {
            w.toast(ToastKind::Success, t!("tailnet.saved").to_string(), None, cx);
        });
    }

    fn confirm_logout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tailnet = self.tailnet.clone();
        crate::dialogs::confirm(
            crate::dialogs::ConfirmOpts {
                title: t!("tailnet.logoutConfirmTitle").to_string(),
                body: t!("tailnet.logoutConfirmBody").to_string(),
                confirm_label: t!("tailnet.logout").to_string(),
                danger: true,
                ..Default::default()
            },
            move |_, cx| {
                tailnet.update(cx, |tn, cx| tn.logout(cx));
            },
            window,
            cx,
        );
    }

    fn render_status_card(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let tn = self.tailnet.read(cx);
        let library = tn.library().clone();
        let status = tn.status().clone();
        let busy = tn.busy();
        let last_error = tn.last_error().map(str::to_owned);
        let effective_name = tn.effective_hostname();

        if library == LibraryState::Missing {
            return crate::ui::sunken(cx)
                .px_4()
                .py_3()
                .flex()
                .items_start()
                .gap_2()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(icon(IconName::Info).size(zpx(14.)).text_color(ui.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().text_color(ui.foreground).child(t!("tailnet.notIncluded").to_string()))
                        .child(div().line_height(zpx(18.)).child(t!("tailnet.notIncludedHint").to_string())),
                )
                .into_any_element();
        }

        if let LibraryState::LoadFailed(err) = library {
            return crate::ui::sunken(cx)
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().text_color(ui.destructive).child(t!("tailnet.loadFailed").to_string()))
                .child(div().text_xs().text_color(ui.muted_foreground).child(err))
                .into_any_element();
        }

        match status.state {
            State::Stopped => {
                let mut box_el = crate::ui::sunken(cx)
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.stateStopped").to_string()));
                if let Some(err) = last_error {
                    box_el = box_el.child(div().text_xs().text_color(ui.destructive).child(err));
                }
                box_el.into_any_element()
            }
            State::Starting => crate::ui::sunken(cx)
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(div().w(zpx(8.)).h(zpx(8.)).rounded_full().bg(ui.warning))
                .child(t!("tailnet.stateStarting").to_string())
                .into_any_element(),
            State::NeedsLogin => {
                let login_url = status.login_url.clone();
                let ws_copy = self.ws.clone();
                let mut actions = div().flex().items_center().gap_2().mt_2();
                if let Some(url) = login_url.clone() {
                    let open_url = url.clone();
                    let copy_url = url;
                    actions = actions
                        .child(
                            Button::new("tailnet-login")
                                .primary()
                                .small()
                                .icon(IconName::ExternalLink)
                                .label(t!("tailnet.login").to_string())
                                .on_click(move |_, _, cx| cx.open_url(&open_url)),
                        )
                        .child(
                            Button::new("tailnet-copy-login")
                                .outline()
                                .small()
                                .icon(IconName::Copy)
                                .label(t!("tailnet.copyLoginUrl").to_string())
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copy_url.clone()));
                                    ws_copy.update(cx, |w, cx| {
                                        w.toast(ToastKind::Success, t!("plugin.copied").to_string(), None, cx);
                                    });
                                }),
                        );
                } else {
                    actions = actions.child(
                        div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.waitingLoginUrl").to_string()),
                    );
                }
                crate::ui::sunken(cx)
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(zpx(8.)).h(zpx(8.)).rounded_full().bg(ui.warning))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(t!("tailnet.stateNeedsLogin").to_string())),
                    )
                    .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.loginHint").to_string()))
                    .child(actions)
                    .into_any_element()
            }
            State::NeedsApproval => crate::ui::sunken(cx)
                .px_4()
                .py_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(zpx(8.)).h(zpx(8.)).rounded_full().bg(ui.warning))
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(t!("tailnet.stateNeedsApproval").to_string())),
                )
                .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.approvalHint").to_string()))
                .into_any_element(),
            State::Running => {
                let self_node = status.self_node.clone();
                let node_name = self_node
                    .as_ref()
                    .map(|n| n.name.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(effective_name);
                let ipv4 = self_node.as_ref().and_then(|n| n.ipv4.clone()).unwrap_or_else(|| "—".to_owned());
                let tailnet_name = self_node
                    .as_ref()
                    .and_then(|n| n.tailnet.clone().or_else(|| n.magic_dns_suffix.clone()))
                    .unwrap_or_else(|| "—".to_owned());
                let peer_count = status.peers.len();
                let online_count = status.peers.iter().filter(|p| p.online).count();
                let tailnet = self.tailnet.clone();

                let kv = |label: String, value: String| {
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .py_1()
                        .child(div().text_xs().text_color(ui.muted_foreground).child(label))
                        .child(
                            div()
                                .font_family(crate::fonts::BERKELEY)
                                .text_xs()
                                .text_color(ui.foreground)
                                .child(value),
                        )
                };

                crate::ui::sunken(cx)
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(div().w(zpx(8.)).h(zpx(8.)).rounded_full().bg(ui.success))
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(t!("tailnet.stateRunning").to_string())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(ui.muted_foreground)
                                    .child(t!("tailnet.peerSummary", online = online_count, total = peer_count).to_string()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .border_t_1()
                            .border_color(ui.border.opacity(0.6))
                            .pt_2()
                            .child(kv(t!("tailnet.deviceName").to_string(), node_name))
                            .child(kv(t!("tailnet.ip").to_string(), ipv4))
                            .child(kv(t!("tailnet.tailnetName").to_string(), tailnet_name)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .pt_2()
                            .border_t_1()
                            .border_color(ui.border.opacity(0.6))
                            .child(
                                Button::new("tailnet-disconnect")
                                    .outline()
                                    .small()
                                    .disabled(busy)
                                    .label(t!("tailnet.disconnect").to_string())
                                    .on_click(move |_, _, cx| {
                                        tailnet.update(cx, |tn, cx| tn.set_enabled(false, cx));
                                    }),
                            )
                            .child(
                                Button::new("tailnet-logout")
                                    .ghost()
                                    .small()
                                    .disabled(busy)
                                    .label(t!("tailnet.logout").to_string())
                                    .on_click(cx.listener(|this, _, window, cx| this.confirm_logout(window, cx))),
                            ),
                    )
                    .into_any_element()
            }
            State::Error => {
                let err_text = status
                    .error
                    .or(last_error)
                    .unwrap_or_else(|| t!("common.error").to_string());
                let tailnet = self.tailnet.clone();
                crate::ui::sunken(cx)
                    .px_4()
                    .py_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(zpx(8.)).h(zpx(8.)).rounded_full().bg(ui.destructive))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(ui.destructive).child(t!("tailnet.stateError").to_string())),
                    )
                    .child(div().text_xs().text_color(ui.muted_foreground).child(err_text))
                    .child(
                        div().flex().gap_2().child(
                            Button::new("tailnet-retry")
                                .outline()
                                .small()
                                .disabled(busy)
                                .label(t!("tailnet.retry").to_string())
                                .on_click(move |_, _, cx| tailnet.update(cx, |tn, cx| tn.retry(cx))),
                        ),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_advanced(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let busy = self.tailnet.read(cx).busy();
        let missing = *self.tailnet.read(cx).library() == LibraryState::Missing;
        let open = self.advanced_open;
        let chevron = if open { IconName::ChevronDown } else { IconName::ChevronRight };

        let toggle = div()
            .id("tailnet-advanced-toggle")
            .flex()
            .items_center()
            .gap_1()
            .py_2()
            .cursor_pointer()
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(ui.muted_foreground)
            .hover(|s| s.text_color(ui.foreground))
            .on_click(cx.listener(|this, _, _, cx| {
                this.advanced_open = !this.advanced_open;
                cx.notify();
            }))
            .child(icon(chevron).size(zpx(13.)))
            .child(t!("tailnet.advanced").to_string());

        let mut container = div().mt_4().flex().flex_col().child(toggle);
        if open {
            container = container.child(
                crate::ui::sunken(cx)
                    .mt_1()
                    .p_4()
                    .rounded(radius::MD)
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(ui.foreground).child(t!("tailnet.deviceName").to_string()))
                            .child(Input::new(&self.hostname).small())
                            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.deviceNameHint").to_string())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(ui.foreground).child(t!("tailnet.controlUrl").to_string()))
                            .child(Input::new(&self.control_url).small())
                            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.controlUrlHint").to_string())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_xs().text_color(ui.foreground).child(t!("tailnet.authKey").to_string()))
                            .child(Input::new(&self.auth_key).small())
                            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.authKeyHint").to_string())),
                    )
                    .child(
                        div().flex().justify_end().child(
                            Button::new("tailnet-save-advanced")
                                .outline()
                                .small()
                                .disabled(busy || missing)
                                .label(t!("tailnet.saveAndApply").to_string())
                                .on_click(cx.listener(|this, _, window, cx| this.save_config(window, cx))),
                        ),
                    ),
            );
        }
        container.into_any_element()
    }
}

impl Render for TailscalePane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (enabled, busy, missing) = {
            let tn = self.tailnet.read(cx);
            (tn.config().enabled, tn.busy(), *tn.library() == LibraryState::Missing)
        };
        let tailnet = self.tailnet.clone();
        let toggle = Switch::new("tailnet-enable-switch")
            .checked(enabled)
            .disabled(busy || missing)
            .on_click(move |_, _, cx| {
                tailnet.update(cx, |tn, cx| tn.set_enabled(!enabled, cx));
            });

        let mut children = widgets::rows(
            vec![(
                t!("tailnet.enableLabel").to_string(),
                Some(t!("tailnet.enableHint").to_string()),
                toggle.into_any_element(),
            )],
            true,
            cx,
        );
        children.push(div().mt_3().child(self.render_status_card(cx)).into_any_element());
        children.push(self.render_advanced(cx));

        widgets::section(
            t!("tailnet.title").to_string(),
            Some(t!("tailnet.intro").to_string()),
            children,
            cx,
        )
    }
}
