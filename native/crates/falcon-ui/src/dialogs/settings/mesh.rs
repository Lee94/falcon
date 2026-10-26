//! 设置「网络」页：本机的 Tailscale（tsnet）与 NetBird。
//!
//! 两边都是后端本机上的 sidecar，不是这台客户端上的。入网之后把报出来的地址填进一条
//! SSH Host，另一台 Falcon 就能连过来——mesh 不替代 SSH，也不替代登录。
//! setup key 在这页上填。它只写不读：服务端加密存好，界面只看得到「已设置」，
//! 输入框留空保存 = 不改。环境变量 `FALCON_NB_SETUP_KEY` 仍然优先。

use falcon_proto::{MeshProviderStatus, MeshState, MeshStatus};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Context, Entity, IntoElement, Render, Window, div};
use rust_i18n::t;

use super::widgets;
use crate::theme::Ui;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

pub struct MeshPane {
    ws: Entity<Workspace>,
    status: Option<MeshStatus>,
    /// NetBird 管理地址。空 = 官方的。回车保存。
    netbird_url: Entity<InputState>,
    /// setup key。只写不读，留空保存 = 不改。
    setup_key: Entity<InputState>,
}

impl MeshPane {
    pub fn new(ws: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let netbird_url = cx.new(|cx| InputState::new(window, cx).placeholder("https://netbird.example.com"));
        let setup_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX"));
        for (input, save_key) in [(&netbird_url, false), (&setup_key, true)] {
            let save_key = save_key;
            cx.subscribe_in(input, window, move |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    if save_key { this.save_setup_key(window, cx) } else { this.save_netbird_url(window, cx) }
                }
            })
            .detach();
        }
        let fut = ws.read(cx).client.mesh_status();
        cx.spawn_in(window, async move |this, cx| {
            let res = fut.await;
            this.update_in(cx, |this, window, cx| {
                if let Ok(st) = res {
                    this.apply(st, window, cx);
                }
            })
            .ok();
        })
        .detach();
        Self { ws, status: None, netbird_url, setup_key }
    }

    fn apply(&mut self, st: MeshStatus, window: &mut Window, cx: &mut Context<Self>) {
        let url = st.netbird_management_url.clone();
        self.netbird_url.update(cx, |i, cx| i.set_value(url, window, cx));
        // setup key 只写不读：每次刷新都把输入框清空，避免把上一回没保存的明文留在屏幕上
        self.setup_key.update(cx, |i, cx| i.set_value("", window, cx));
        self.status = Some(st);
        cx.notify();
    }

    fn save_netbird_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.netbird_url.read(cx).value().trim().to_string();
        let fut = self.ws.read(cx).client.set_netbird_url(&url);
        cx.spawn_in(window, async move |this, cx| {
            let res = fut.await;
            this.update_in(cx, |this, window, cx| {
                match res {
                    Ok(st) => {
                        let restarted = st.restarted;
                        this.apply(st, window, cx);
                        if restarted {
                            this.ws.update(cx, |w, cx| {
                                w.toast(ToastKind::Success, t!("mesh.restarted").to_string(), None, cx)
                            });
                        }
                    }
                    Err(e) => this.ws.update(cx, |w, cx| w.toast(ToastKind::Danger, e.to_string(), None, cx)),
                }
            })
            .ok();
        })
        .detach();
    }

    fn save_setup_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.setup_key.read(cx).value().trim().to_string();
        // 留空 = 不改。清掉走旁边的「清除」。
        if key.is_empty() {
            return;
        }
        let fut = self.ws.read(cx).client.set_netbird_setup_key(&key);
        cx.spawn_in(window, async move |this, cx| {
            let res = fut.await;
            this.update_in(cx, |this, window, cx| {
                match res {
                    Ok(st) => {
                        let restarted = st.restarted;
                        this.apply(st, window, cx);
                        let msg = if restarted { t!("mesh.restarted") } else { t!("mesh.keySaved") };
                        this.ws.update(cx, |w, cx| w.toast(ToastKind::Success, msg.to_string(), None, cx));
                    }
                    Err(e) => this.ws.update(cx, |w, cx| w.toast(ToastKind::Danger, e.to_string(), None, cx)),
                }
            })
            .ok();
        })
        .detach();
    }

    fn clear_setup_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fut = self.ws.read(cx).client.set_netbird_setup_key("");
        cx.spawn_in(window, async move |this, cx| {
            let res = fut.await;
            this.update_in(cx, |this, window, cx| {
                match res {
                    Ok(st) => this.apply(st, window, cx),
                    Err(e) => this.ws.update(cx, |w, cx| w.toast(ToastKind::Danger, e.to_string(), None, cx)),
                }
            })
            .ok();
        })
        .detach();
    }

    fn toggle(&mut self, provider: &'static str, enabled: bool, cx: &mut Context<Self>) {
        let fut = self.ws.read(cx).client.set_mesh(provider, enabled);
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(st) => this.status = Some(st),
                    Err(e) => this.ws.update(cx, |w, cx| w.toast(ToastKind::Danger, e.to_string(), None, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

fn state_label(s: &MeshState) -> String {
    let key = match s {
        MeshState::Off => "mesh.stateOff",
        MeshState::Starting => "mesh.stateStarting",
        MeshState::Online => "mesh.stateOnline",
        MeshState::NeedsLogin => "mesh.stateNeedsLogin",
        MeshState::Error => "mesh.stateError",
        MeshState::Unavailable => "mesh.stateUnavailable",
        MeshState::Unknown => "mesh.stateUnknown",
    };
    t!(key).to_string()
}

fn address_of(st: &MeshProviderStatus) -> Option<String> {
    st.dns_name.clone().or_else(|| st.ipv4.clone()).or_else(|| st.ipv6.clone())
}

fn provider_block(
    pane: &Entity<MeshPane>,
    netbird_url: &Entity<InputState>,
    setup_key: &Entity<InputState>,
    title: &str,
    hint: &str,
    provider: &'static str,
    st: &MeshProviderStatus,
    cx: &App,
) -> gpui_kit::AnyElement {
    let ui = Ui::global(cx);
    let on = !matches!(st.state, MeshState::Off | MeshState::Unavailable);
    let pane_sw = pane.clone();
    let toggle = Switch::new(gpui_kit::ElementId::Name(format!("mesh-{provider}").into()))
        .checked(on)
        .disabled(matches!(st.state, MeshState::Unavailable))
        .on_click(move |_, _, cx| pane_sw.update(cx, |p, cx| p.toggle(provider, !on, cx)));
    let mut lines = vec![widgets::row(title.to_string(), Some(hint.to_string()), toggle, true, cx)];
    if provider == "netbird" {
        // 不能在这里 pane.read：provider_block 是从 MeshPane::render 里调的，
        // 自己还借着，再读一次就是 "already being updated"，直接 panic。
        let input = Input::new(netbird_url).small();
        let pane_save = pane.clone();
        let save = Button::new("mesh-netbird-url-save")
            .outline()
            .small()
            .label(t!("mesh.save").to_string())
            .on_click(move |_, window, cx| pane_save.update(cx, |p, cx| p.save_netbird_url(window, cx)));
        let key_input = Input::new(setup_key).small();
        let pane_key = pane.clone();
        let save_key = Button::new("mesh-netbird-key-save")
            .outline()
            .small()
            .label(t!("mesh.save").to_string())
            .on_click(move |_, window, cx| pane_key.update(cx, |p, cx| p.save_setup_key(window, cx)));
        let pane_clear = pane.clone();
        let clear_key = Button::new("mesh-netbird-key-clear")
            .outline()
            .small()
            .disabled(!st.configured)
            .label(t!("mesh.clear").to_string())
            .on_click(move |_, window, cx| pane_clear.update(cx, |p, cx| p.clear_setup_key(window, cx)));
        let key_hint = if st.configured { t!("mesh.keySet").to_string() } else { t!("mesh.keyUnset").to_string() };
        lines.push(widgets::row(
            t!("mesh.setupKey").to_string(),
            Some(key_hint),
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(zpx(240.)).child(key_input))
                .child(save_key)
                .child(clear_key),
            true,
            cx,
        ));
        lines.push(widgets::row(
            t!("mesh.netbirdUrl").to_string(),
            Some(t!("mesh.netbirdUrlHint").to_string()),
            // 右侧控件没有弹性宽度可分：flex_1 在这里会让布局算出负的可用宽度，直接 panic。
            // 输入框给一个确定的宽度。
            div().flex().items_center().gap_2().child(div().w(zpx(240.)).child(input)).child(save),
            true,
            cx,
        ));
    }
    lines.push(widgets::row(
        t!("mesh.state").to_string(),
        st.error.clone(),
        div().text_sm().text_color(ui.muted_foreground).child(state_label(&st.state)),
        address_of(st).is_some(),
        cx,
    ));
    if let Some(addr) = address_of(st) {
        let copy = addr.clone();
        let ws = pane.read(cx).ws.clone();
        let button = Button::new(gpui_kit::ElementId::Name(format!("mesh-copy-{provider}").into()))
            .outline()
            .small()
            .icon(IconName::Copy)
            .label(addr)
            .on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                ws.update(cx, |w, cx| w.toast(ToastKind::Success, t!("mesh.copied").to_string(), None, cx));
            });
        lines.push(widgets::row(t!("mesh.address").to_string(), Some(t!("mesh.addressHint").to_string()), button, false, cx));
    }
    if !st.configured {
        lines.push(
            div()
                .pt_2()
                .text_xs()
                .line_height(zpx(18.))
                .text_color(ui.muted_foreground)
                .child(t!("mesh.notConfigured").to_string())
                .into_any_element(),
        );
    }
    div().flex().flex_col().children(lines).into_any_element()
}

impl Render for MeshPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx);
        let Some(st) = &self.status else {
            return div().text_xs().text_color(ui.muted_foreground).child(t!("mesh.loading").to_string()).into_any_element();
        };
        let pane = cx.entity();
        div()
            .child(widgets::section(
                t!("mesh.title").to_string(),
                Some(t!("mesh.hint").to_string()),
                // Tailscale 先不露出来：sidecar 还在，设置里暂时只配 NetBird。
                vec![provider_block(
                    &pane,
                    &self.netbird_url,
                    &self.setup_key,
                    &t!("mesh.netbird").to_string(),
                    &t!("mesh.netbirdHint").to_string(),
                    "netbird",
                    &st.netbird,
                    cx,
                )],
                cx,
            ))
            .into_any_element()
    }
}
