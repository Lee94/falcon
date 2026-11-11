//! 从 falcon 后端这台机器的 Tailscale 里挑一台，预填远端主机表单。
//!
//! 列表来自 `GET /api/tailscale/devices`（后端本机的 `tailscale status`）。点一台打开
//! [`host_form`]：地址用 MagicDNS 名或 Tailscale IP，用户名先填后端的登录名，认证方式先用
//! SSH agent。保存前都能改。表单叠在这个对话框上面，存完还能继续加下一台。

use falcon_proto::{SshAuthMethod, SshHost, TailscaleDevice, TailscaleDevices, TailscaleUnavailable};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FontWeight, IntoElement, Render, SharedString, Window, div};
use rust_i18n::t;

use crate::dialogs::host_form::{self, HostDraft, OnSaved, SshInit};
use crate::dialogs::project_form::kit::{error_line, note};
use crate::theme::Ui;
use crate::workspace::Workspace;
use crate::zoom::zpx;

pub fn open(ws: &Entity<Workspace>, on_saved: Option<OnSaved>, window: &mut Window, cx: &mut App) {
    let picker = cx.new(|cx| TailscalePicker::new(ws.clone(), on_saved, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        // 搜索框里的回车会冒泡成对话框的「确定」。这里没有确定，别让它把列表关掉
        dialog
            .title(t!("host.tailscaleTitle").to_string())
            .w(zpx(560.))
            .entrance(gpui_kit::component::dialog::DialogEntrance::FadeSlide)
            .on_ok(|_, _, _| false)
            .child(picker.clone())
    });
}

struct TailscalePicker {
    ws: Entity<Workspace>,
    on_saved: Option<OnSaved>,
    search: Entity<InputState>,
    loading: bool,
    result: Option<TailscaleDevices>,
    error: Option<String>,
}

impl TailscalePicker {
    fn new(ws: Entity<Workspace>, on_saved: Option<OnSaved>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(t!("host.tailscaleSearch").to_string()));
        cx.subscribe_in(&search, window, |_, _, ev: &InputEvent, _, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        cx.observe(&ws, |_, _, cx| cx.notify()).detach();
        let mut this = Self { ws, on_saved, search, loading: false, result: None, error: None };
        this.fetch(cx);
        this
    }

    fn fetch(&mut self, cx: &mut Context<Self>) {
        self.loading = true;
        self.error = None;
        cx.notify();
        let client = self.ws.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.tailscale_devices().await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(list) => this.result = Some(list),
                    Err(err) => this.error = Some(err.message),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pick(&mut self, device: &TailscaleDevice, window: &mut Window, cx: &mut Context<Self>) {
        let username = self.result.as_ref().map(|r| r.suggested_username.clone()).unwrap_or_default();
        let hosts = self.ws.read(cx).hosts.clone();
        let draft = HostDraft {
            name: fresh_name(&device.name, &hosts),
            ssh: SshInit {
                host: device.host.clone(),
                port: 22,
                username,
                // Tailscale 网络里的机器多半已经能用本机 SSH agent 的密钥登录。
                // 要密钥文件或密码的，在表单里改过再保存
                auth_method: SshAuthMethod::Agent,
                key_path: String::new(),
            },
        };
        host_form::open_draft(&self.ws, draft, self.on_saved.clone(), window, cx);
    }
}

impl Render for TailscalePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let search = self.search.clone();
        let loading = self.loading;
        let error = self.error.clone();
        let result = self.result.clone();
        let query = search.read(cx).value().trim().to_lowercase();
        let hosts = self.ws.read(cx).hosts.clone();
        let first_load = loading && result.is_none() && error.is_none();

        let mut col = div().flex().flex_col().gap_3().w_full().child(note(t!("host.tailscaleHint").to_string(), cx));
        if let Some(err) = error.clone() {
            col = col.child(error_line(err, cx));
        }
        if first_load {
            col = col.child(note(t!("host.tailscaleLoading").to_string(), cx));
        } else if let Some(list) = result.as_ref() {
            if !list.available {
                col = col.child(error_line(problem_text(list), cx));
            } else if list.devices.is_empty() {
                col = col.child(note(t!("host.tailscaleEmpty").to_string(), cx));
            } else {
                let filtered: Vec<TailscaleDevice> = list.devices.iter().filter(|d| matches_query(d, &query)).cloned().collect();
                col = col.child(Input::new(&search));
                if filtered.is_empty() {
                    col = col.child(note(t!("host.tailscaleNoMatch").to_string(), cx));
                } else {
                    col = col.child(device_list(&filtered, &hosts, cx));
                }
            }
        }
        if !first_load {
            col = col.child(
                div().flex().child(
                    Button::new("tailscale-retry")
                        .outline()
                        .small()
                        .disabled(loading)
                        .label(if loading { t!("host.tailscaleLoading") } else { t!("host.tailscaleRetry") }.to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.fetch(cx))),
                ),
            );
        }
        col
    }
}

fn device_list(devices: &[TailscaleDevice], hosts: &[SshHost], cx: &mut Context<TailscalePicker>) -> impl IntoElement {
    let ui = Ui::global(cx).clone();
    let n = devices.len();
    let mut table = crate::ui::sunken(cx).id("tailscale-devices").max_h(zpx(360.)).overflow_y_scroll().flex().flex_col().px_2();
    for (i, device) in devices.iter().enumerate() {
        let added = already_added(device, hosts);
        let click = device.clone();
        table = table.child(
            div()
                .id(SharedString::from(format!("ts-row-{i}")))
                .min_h(zpx(52.))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .when(i + 1 < n, |d| d.border_b_1().border_color(ui.border.opacity(0.5)))
                .hover(|s| s.bg(ui.background.opacity(0.7)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(zpx(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(device.name.clone()),
                                )
                                .when(added, |d| {
                                    d.child(div().flex_none().text_xs().text_color(ui.muted_foreground).child(t!("host.tailscaleAdded").to_string()))
                                }),
                        )
                        .child(
                            div()
                                .truncate()
                                .font_family(crate::fonts::BERKELEY)
                                .text_xs()
                                .text_color(ui.muted_foreground)
                                .child(address_line(device)),
                        )
                        .child(div().truncate().text_xs().text_color(ui.muted_foreground).child(meta_line(device))),
                )
                .child(
                    Button::new(SharedString::from(format!("ts-add-{i}")))
                        .ghost()
                        .small()
                        .label(t!("host.tailscaleAdd").to_string())
                        .on_click(cx.listener(move |this, _, window, cx| this.pick(&click, window, cx))),
                ),
        );
    }
    table
}

fn problem_text(list: &TailscaleDevices) -> String {
    let base = match list.reason {
        Some(TailscaleUnavailable::NotInstalled) => t!("host.tailscaleNotInstalled"),
        Some(TailscaleUnavailable::NotRunning) => t!("host.tailscaleNotRunning"),
        Some(TailscaleUnavailable::NeedsLogin) => t!("host.tailscaleNeedsLogin"),
        Some(TailscaleUnavailable::Failed) | None => t!("host.tailscaleFailed"),
    };
    match list.message.as_deref().filter(|m| !m.is_empty()) {
        Some(message) if matches!(list.reason, Some(TailscaleUnavailable::Failed) | None) => format!("{base} {message}"),
        _ => base.to_string(),
    }
}

fn address_line(device: &TailscaleDevice) -> String {
    match &device.ipv4 {
        Some(ip) if ip != &device.host => format!("{} · {ip}", device.host),
        _ => device.host.clone(),
    }
}

fn meta_line(device: &TailscaleDevice) -> String {
    let mut parts = Vec::new();
    if let Some(os) = device.os.as_deref().filter(|s| !s.is_empty()) {
        parts.push(os.to_string());
    }
    parts.push(if device.online { t!("host.tailscaleOnline") } else { t!("host.tailscaleOffline") }.to_string());
    if let Some(owner) = device.owner.as_deref().filter(|s| !s.is_empty()) {
        parts.push(owner.to_string());
    }
    parts.join(" · ")
}

fn matches_query(device: &TailscaleDevice, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut blob = format!("{} {} {}", device.name, device.host, device.os.as_deref().unwrap_or(""));
    for extra in [device.dns_name.as_deref(), device.ipv4.as_deref(), device.owner.as_deref()] {
        if let Some(extra) = extra {
            blob.push(' ');
            blob.push_str(extra);
        }
    }
    blob.to_lowercase().contains(query)
}

fn already_added(device: &TailscaleDevice, hosts: &[SshHost]) -> bool {
    hosts.iter().any(|host| {
        same_addr(&host.host, &device.host)
            || device.dns_name.as_deref().is_some_and(|name| same_addr(&host.host, name))
            || device.ipv4.as_deref().is_some_and(|ip| same_addr(&host.host, ip))
    })
}

fn same_addr(saved: &str, candidate: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('.').to_ascii_lowercase();
    !saved.trim().is_empty() && norm(saved) == norm(candidate)
}

/// 显示名和已保存的主机撞了就加序号，免得一保存就 409
fn fresh_name(base: &str, hosts: &[SshHost]) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "tailscale" } else { base };
    let taken = |name: &str| hosts.iter().any(|host| host.name.eq_ignore_ascii_case(name));
    if !taken(base) {
        return base.to_string();
    }
    for i in 2..1000 {
        let candidate = format!("{base} ({i})");
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{base} ({})", hosts.len() + 2)
}
