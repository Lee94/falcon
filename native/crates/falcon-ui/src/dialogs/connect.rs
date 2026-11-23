//! "连接到 falcon 服务端"：管理服务端配置、打开对应的窗口（设计 §4.7）。
//!
//! 独立的小窗口而不是某个服务端窗口里的对话框：它管的是"开哪些窗口"，不属于任何一台服务端。
//! 一台服务端一个窗口，已经开着的就把它拉到前面，不重复开。

use std::collections::HashMap;

use falcon_client::{ServerRoute, TailnetProbeHit};
use falcon_tailnet::State;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Disableable, Root, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, FontWeight, IntoElement, Render, SharedString, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, point, px, size,
};
use rust_i18n::t;

use crate::profiles::{Profiles, ServerProfile, new_profile_id};
use crate::tailnet::{LibraryState, TailnetController};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

const DEFAULT_FALCON_PORT: u16 = 4923;

pub fn open_connect_window(cx: &mut App) {
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(t!("native.menu.connect").to_string().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(580.), px(580.)), cx))),
        window_background: crate::window_background::appearance(cx),
        ..Default::default()
    };
    let _ = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ConnectView::new(window, cx));
        cx.new(|cx| crate::window_background::mount_root(view, window, cx))
    });
}

/// 窗口是否已经为这台服务端开着；开着就激活它
fn activate_existing(profile_id: &str, cx: &mut App) -> bool {
    for handle in cx.windows() {
        let Some(root) = handle.downcast::<Root>() else {
            continue;
        };
        let matches = root
            .read(cx)
            .ok()
            .and_then(|r| r.view().clone().downcast::<crate::window::ServerWindow>().ok())
            .is_some_and(|v| v.read(cx).ws.read(cx).profile.id == profile_id);
        if matches {
            let _ = root.update(cx, |_, window, _| window.activate_window());
            return true;
        }
    }
    false
}

/// 已打开的服务端窗口同步切换网络路由。
fn sync_open_window_route(profile_id: &str, route: ServerRoute, cx: &mut App) {
    for handle in cx.windows() {
        let Some(root) = handle.downcast::<Root>() else {
            continue;
        };
        let ws = root
            .read(cx)
            .ok()
            .and_then(|r| r.view().clone().downcast::<crate::window::ServerWindow>().ok())
            .and_then(|v| {
                let ws = v.read(cx).ws.clone();
                (ws.read(cx).profile.id == profile_id).then_some(ws)
            });
        if let Some(ws) = ws {
            ws.update(cx, |w, cx| w.set_route(route, cx));
        }
    }
}

pub fn open_profile(profile: ServerProfile, cx: &mut App) {
    if !activate_existing(&profile.id, cx) {
        crate::window::open_server_window(profile, cx);
    }
}

fn route_label(route: ServerRoute) -> String {
    match route {
        ServerRoute::Auto => t!("tailnet.routeAuto").to_string(),
        ServerRoute::Direct => t!("tailnet.routeDirect").to_string(),
        ServerRoute::Tailnet => t!("tailnet.routeTailnet").to_string(),
    }
}

fn next_route(route: ServerRoute) -> ServerRoute {
    match route {
        ServerRoute::Auto => ServerRoute::Direct,
        ServerRoute::Direct => ServerRoute::Tailnet,
        ServerRoute::Tailnet => ServerRoute::Auto,
    }
}

pub struct ConnectView {
    name: Entity<InputState>,
    url: Entity<InputState>,
    route: ServerRoute,
    error: Option<String>,
    tailnet: Entity<TailnetController>,
    tailscale_open: bool,
    probing: bool,
    probe_hits: HashMap<String, TailnetProbeHit>,
    probe_gen: u64,
}

impl ConnectView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder(t!("native.connect.namePlaceholder").to_string()));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://falcon.example.com"));
        let tailnet = crate::tailnet::entity(cx);
        cx.observe(&tailnet, |this, tn, cx| {
            let ready = tn.read(cx).status().state == State::Running && tn.read(cx).proxy().is_some();
            if this.tailscale_open && ready && !this.probing && this.probe_hits.is_empty() {
                this.probe_tailscale(cx);
            }
            cx.notify();
        })
        .detach();
        Self {
            name,
            url,
            route: ServerRoute::Auto,
            error: None,
            tailnet,
            tailscale_open: false,
            probing: false,
            probe_hits: HashMap::new(),
            probe_gen: 0,
        }
    }

    fn toggle_tailscale_picker(&mut self, cx: &mut Context<Self>) {
        self.tailscale_open = !self.tailscale_open;
        if self.tailscale_open {
            self.probe_tailscale(cx);
        }
        cx.notify();
    }

    fn probe_tailscale(&mut self, cx: &mut Context<Self>) {
        let (proxy, peers) = {
            let tn = self.tailnet.read(cx);
            if tn.status().state != State::Running {
                return;
            }
            let Some(proxy) = tn.proxy().cloned() else {
                return;
            };
            (proxy, tn.status().peers.clone())
        };
        self.probe_gen += 1;
        let generation = self.probe_gen;
        self.probing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let hits = falcon_client::probe_tailnet_servers(proxy, peers, DEFAULT_FALCON_PORT).await;
            let _ = this.update(cx, |this, cx| {
                if this.probe_gen != generation {
                    return;
                }
                this.probing = false;
                this.probe_hits = hits.into_iter().map(|h| (h.device_id.clone(), h)).collect();
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_tailscale_peer(
        &mut self,
        name: String,
        host: String,
        port: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = format!("http://{host}:{port}");
        self.name.update(cx, |s, cx| s.set_value(name, window, cx));
        self.url.update(cx, |s, cx| s.set_value(target, window, cx));
        self.route = ServerRoute::Auto;
        self.error = None;
        self.tailscale_open = false;
        cx.notify();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().trim().trim_end_matches('/').to_string();
        let parsed = url::Url::parse(&url);
        let ok = parsed.as_ref().is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some());
        if !ok {
            self.error = Some(t!("native.connect.badUrl").to_string());
            cx.notify();
            return;
        }
        let mut profile = ServerProfile {
            id: new_profile_id(),
            name: self.name.read(cx).value().trim().to_string(),
            url,
            plaintext_ok: false,
            route: self.route,
        };
        self.error = None;
        cx.notify();
        if !profile.is_insecure() {
            Self::save_and_open(profile, cx);
            return;
        }
        // 非回环的 http://：密码与终端内容裸奔，问过一次、记在配置上（设计 §4.7）
        profile.plaintext_ok = true;
        let body = t!("native.connect.plaintextBody", url = profile.url.as_str()).to_string();
        crate::dialogs::confirm(
            crate::dialogs::ConfirmOpts {
                title: t!("native.connect.plaintextTitle").to_string(),
                body,
                confirm_label: t!("native.connect.plaintextConfirm").to_string(),
                danger: true,
                ..Default::default()
            },
            move |_, cx| Self::save_and_open(profile.clone(), cx),
            window,
            cx,
        );
    }

    fn save_and_open(profile: ServerProfile, cx: &mut App) {
        cx.global_mut::<Profiles>().upsert(profile.clone());
        // 确认框的回调拿不到本视图，改全局后让列表重画
        cx.refresh_windows();
        open_profile(profile, cx);
    }

    fn render_tailscale_picker(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let (library, status, busy) = {
            let tn = self.tailnet.read(cx);
            (tn.library().clone(), tn.status().clone(), tn.busy())
        };
        let mut box_el = crate::ui::sunken(cx)
            .p_3()
            .rounded(radius::MD)
            .flex()
            .flex_col()
            .gap_2();

        match library {
            LibraryState::Missing => {
                return box_el.child(
                    div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.notIncluded").to_string()),
                );
            }
            LibraryState::LoadFailed(err) => {
                return box_el
                    .child(div().text_xs().text_color(ui.destructive).child(t!("tailnet.loadFailed").to_string()))
                    .child(div().text_xs().text_color(ui.muted_foreground).child(err));
            }
            LibraryState::Available => {}
        }

        match status.state {
            State::Stopped => {
                let tailnet = self.tailnet.clone();
                box_el = box_el
                    .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.pickerStopped").to_string()))
                    .child(
                        div().flex().child(
                            Button::new("connect-tailnet-enable")
                                .outline()
                                .small()
                                .disabled(busy)
                                .label(t!("tailnet.enableNow").to_string())
                                .on_click(move |_, _, cx| {
                                    tailnet.update(cx, |tn, cx| tn.set_enabled(true, cx));
                                }),
                        ),
                    );
            }
            State::Starting => {
                box_el = box_el.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(ui.muted_foreground)
                        .child(div().w(zpx(8.)).h(zpx(8.)).rounded_full().bg(ui.warning))
                        .child(t!("tailnet.stateStarting").to_string()),
                );
            }
            State::NeedsLogin => {
                box_el = box_el.child(
                    div().text_xs().text_color(ui.warning).child(t!("tailnet.stateNeedsLogin").to_string()),
                );
                if let Some(login_url) = status.login_url.clone() {
                    box_el = box_el.child(
                        div().flex().child(
                            Button::new("connect-tailnet-login")
                                .primary()
                                .small()
                                .icon(IconName::ExternalLink)
                                .label(t!("tailnet.login").to_string())
                                .on_click(move |_, _, cx| cx.open_url(&login_url)),
                        ),
                    );
                } else {
                    box_el = box_el.child(
                        div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.waitingLoginUrl").to_string()),
                    );
                }
            }
            State::NeedsApproval => {
                box_el = box_el.child(
                    div().text_xs().text_color(ui.warning).child(t!("tailnet.approvalHint").to_string()),
                );
            }
            State::Error => {
                let tailnet = self.tailnet.clone();
                box_el = box_el
                    .child(
                        div()
                            .text_xs()
                            .text_color(ui.destructive)
                            .child(status.error.unwrap_or_else(|| t!("tailnet.stateError").to_string())),
                    )
                    .child(
                        div().flex().child(
                            Button::new("connect-tailnet-retry")
                                .outline()
                                .small()
                                .disabled(busy)
                                .label(t!("tailnet.retry").to_string())
                                .on_click(move |_, _, cx| {
                                    tailnet.update(cx, |tn, cx| tn.retry(cx));
                                }),
                        ),
                    );
            }
            State::Running => {
                let mut peers = status.peers;
                let hits = &self.probe_hits;
                peers.sort_by(|a, b| {
                    let a_hit = hits.contains_key(&a.id);
                    let b_hit = hits.contains_key(&b.id);
                    b_hit
                        .cmp(&a_hit)
                        .then_with(|| b.online.cmp(&a.online))
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });

                let header = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(ui.muted_foreground)
                            .child(if self.probing {
                                t!("tailnet.probing").to_string()
                            } else {
                                t!("tailnet.pickerTitle").to_string()
                            }),
                    )
                    .child(
                        Button::new("connect-tailnet-reprobe")
                            .ghost()
                            .xsmall()
                            .disabled(self.probing)
                            .label(t!("tailnet.reprobe").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.probe_tailscale(cx))),
                    );
                box_el = box_el.child(header);

                if peers.is_empty() {
                    box_el = box_el.child(
                        div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.noPeers").to_string()),
                    );
                } else {
                    let mut list = div().flex().flex_col().gap_1();
                    for peer in peers {
                        let hit = self.probe_hits.get(&peer.id).cloned();
                        let target_host = peer.connection_host().to_owned();
                        if target_host.is_empty() {
                            continue;
                        }
                        let pick_name = peer.name.clone();
                        let pick_host = target_host.clone();
                        let pick_port = hit.as_ref().map(|h| h.port).unwrap_or(DEFAULT_FALCON_PORT);
                        let meta = match peer.os.as_deref().filter(|s| !s.is_empty()) {
                            Some(os) => format!("{target_host} · {os}"),
                            None => target_host,
                        };
                        let dot_color = if peer.online { ui.success } else { ui.muted_foreground.opacity(0.4) };

                        list = list.child(
                            div()
                                .id(SharedString::from(format!("ts-peer-{}", peer.id)))
                                .px_2()
                                .py(zpx(6.))
                                .rounded(zpx(6.))
                                .cursor_pointer()
                                .hover(|s| s.bg(ui.muted))
                                .flex()
                                .items_center()
                                .gap_2()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.pick_tailscale_peer(
                                        pick_name.clone(),
                                        pick_host.clone(),
                                        pick_port,
                                        window,
                                        cx,
                                    );
                                }))
                                .child(div().flex_none().w(zpx(7.)).h(zpx(7.)).rounded_full().bg(dot_color))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(div().truncate().text_xs().text_color(ui.foreground).child(peer.name))
                                                .when_some(hit, |d, h| {
                                                    let label = if h.system.version.is_empty() {
                                                        t!("tailnet.falconRunning").to_string()
                                                    } else {
                                                        format!("Falcon v{}", h.system.version)
                                                    };
                                                    d.child(
                                                        div()
                                                            .flex_none()
                                                            .px(zpx(6.))
                                                            .py(zpx(1.))
                                                            .rounded_full()
                                                            .bg(ui.success.opacity(0.15))
                                                            .text_size(zpx(10.))
                                                            .text_color(ui.success)
                                                            .child(label),
                                                    )
                                                }),
                                        )
                                        .child(
                                            div()
                                                .truncate()
                                                .text_size(zpx(11.))
                                                .text_color(ui.muted_foreground)
                                                .child(meta),
                                        ),
                                ),
                        );
                    }
                    box_el = box_el.child(list);
                }
            }
        }
        box_el
    }
}

impl Render for ConnectView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let profiles: Vec<ServerProfile> = Profiles::global(cx).all().to_vec();
        let mut list = div().flex().flex_col().gap_1();
        for p in profiles {
            let name = crate::window::display_name(&p, cx);
            let pid = p.id.clone();
            let p_open = p.clone();
            let via_tailnet = !p.is_local() && p.is_routed_via_tailnet();
            let mut title_line = div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(div().truncate().child(name));
            if via_tailnet {
                title_line = title_line.child(
                    div()
                        .flex_none()
                        .px(zpx(6.))
                        .py(zpx(1.))
                        .rounded_full()
                        .bg(ui.primary.opacity(0.14))
                        .text_size(zpx(10.))
                        .text_color(ui.primary)
                        .child(t!("tailnet.badgeVia").to_string()),
                );
            }
            let mut row = div()
                .id(SharedString::from(format!("prof-{}", p.id)))
                .min_h(zpx(40.))
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .rounded(zpx(8.))
                .hover(|s| s.bg(ui.muted))
                .child(
                    icon(if p.is_local() { IconName::Monitor } else { IconName::Server })
                        .size(zpx(14.))
                        .text_color(ui.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(title_line)
                        .child(div().truncate().text_size(zpx(11.)).text_color(ui.muted_foreground).child(p.url.clone())),
                );
            if !p.is_local() {
                let p_cycle = p.clone();
                row = row.child(
                    Button::new(SharedString::from(format!("prof-route-{}", p.id)))
                        .xsmall()
                        .ghost()
                        .label(route_label(p.route))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            let mut updated = p_cycle.clone();
                            let next = next_route(updated.route);
                            updated.route = next;
                            cx.global_mut::<Profiles>().upsert(updated.clone());
                            sync_open_window_route(&updated.id, next, cx);
                            cx.refresh_windows();
                        })),
                );
            }
            row = row.child(
                Button::new(SharedString::from(format!("prof-open-{}", p.id)))
                    .small()
                    .outline()
                    .label(t!("native.connect.open").to_string())
                    .on_click(move |_, _, cx| open_profile(p_open.clone(), cx)),
            );
            if !p.is_local() {
                row = row.child(
                    Button::new(SharedString::from(format!("prof-del-{}", p.id)))
                        .small()
                        .ghost()
                        .icon(IconName::Trash)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.global_mut::<Profiles>().remove(&pid);
                            cx.notify();
                        })),
                );
            }
            list = list.child(row);
        }

        let mut route_picker = div().flex().items_center().gap_1();
        for candidate in [ServerRoute::Auto, ServerRoute::Direct, ServerRoute::Tailnet] {
            let active = self.route == candidate;
            let btn = Button::new(SharedString::from(format!("connect-route-{candidate:?}")))
                .xsmall()
                .label(route_label(candidate))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.route = candidate;
                    cx.notify();
                }));
            let btn = if active { btn.primary() } else { btn.outline() };
            route_picker = route_picker.child(btn);
        }

        let mut add_card = crate::ui::island(cx)
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().font_weight(FontWeight::MEDIUM).child(t!("native.connect.add").to_string()))
                    .child(
                        Button::new("prof-from-tailscale")
                            .xsmall()
                            .outline()
                            .icon(IconName::Globe)
                            .label(t!("tailnet.addFromTailscale").to_string())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_tailscale_picker(cx))),
                    ),
            );
        if self.tailscale_open {
            add_card = add_card.child(self.render_tailscale_picker(cx));
        }
        add_card = add_card
            .child(Input::new(&self.name))
            .child(Input::new(&self.url))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(div().text_xs().text_color(ui.muted_foreground).child(t!("tailnet.routeLabel").to_string()))
                    .child(route_picker),
            )
            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("native.connect.hint").to_string()))
            .when_some(self.error.clone(), |d, e| d.child(div().text_xs().text_color(ui.destructive).child(e)))
            .child(
                Button::new("prof-add")
                    .primary()
                    .label(t!("native.connect.connect").to_string())
                    .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
            );

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(crate::window_background::fill(cx))
            .text_color(ui.foreground)
            .text_size(zpx(13.))
            .pt(zpx(40.))
            .child(
                div()
                    .id("connect-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_4()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(crate::ui::island(cx).p_3().child(list))
                    .child(add_card),
            )
            // 顶上 40 的留白在 macOS 是给红绿灯的；Windows 上在这条里铺拖动区 + 三颗窗口按钮
            .when(falcon_platform::get(cx).info().custom_window_controls(), |d| {
                let strip = zpx(40.);
                d.relative().child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(strip)
                        .flex()
                        .child(crate::window_controls::drag_area(div().id("connect-drag").flex_1().h_full(), cx))
                        .children(crate::window_controls::controls(strip, window, cx)),
                )
            })
    }
}
