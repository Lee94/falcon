//! 设置 →「中转」（旧 React 版 SettingsModal 的 RelaysPane，ADR 0016）：端口转发与公网
//! 发布按**机器**管理，不跟项目走。
//!
//! 按机器分块：「本机」只有公网发布（端口转发两头都要落在一条 SSH 链路上）；每台已保存的
//! SSH Host 有端口转发 + 公网发布。隧道走该主机自己的那条 SshLink，与项目终端的链路无关。
//!
//! 同端口可以存多条通道，同时只有一条生效：启用一条时服务端顺手停掉同端口的其它通道，
//! 所以任何写操作之后都重拉整张列表（`GET /api/relays`），不只替换这一行。同端口的规则
//! 挂一枚徽标（槽位口径在 falcon_core::relay，与服务端 sessions/relay_spec.rs 一致）。
//!
//! 规则的 `state` / `error` / 公网 URL 是此刻的事实：页面开着时每 3s 轮询。设置对话框里
//! 切到别的页就把这一页整个丢掉（settings.rs），轮询随之停——沿用 React 版切 tab 即卸载的做法。

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Duration;

use falcon_client::{ApiResult, FalconClient};
use falcon_core::host_color::ssh_conn;
use falcon_core::relay::same_port_counts;
use falcon_proto::{
    ForwardKind, ForwardState, PortForward, PortForwardInput, PortForwardPatch, PublicShare, PublicShareInput,
    PublicSharePatch, RelayList, SshHost,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    Animation, AnimationExt, AnyElement, App, ClipboardItem, Context, Div, ElementId, Entity, EventEmitter, FontWeight,
    IntoElement, Render, SharedString, Subscription, Task, Window, div, pulsating_between, relative,
};
use rust_i18n::t;

use super::widgets::section;
use crate::panels::meegle::widgets::{hint, hint_detail, icon_button, segmented, tiny_button};
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::workspace::{ToastKind, Workspace};
use crate::zoom::zpx;

const POLL: Duration = Duration::from_secs(3);

/// 表单表里本机那一块的键（主机用自己的 id，UUID 撞不上它）
const LOCAL: &str = "local";

/// 「去添加远端主机」：settings.rs 订阅它切到 Hosts 页
pub enum RelaysEvent {
    GoHosts,
}

/// 添加转发的表单。只填两个端口：监听地址与目标地址一律走后端默认的 127.0.0.1——绑 0.0.0.0 /
/// 具体网卡是少数派需求，真要改可以直接调 API
struct ForwardForm {
    kind: ForwardKind,
    bind: Entity<InputState>,
    dest: Entity<InputState>,
    name: Entity<InputState>,
    /// 上一次的监听端口：目标端口没被单独改过（还等于它）就跟着监听端口走
    last_bind: String,
    busy: bool,
    error: Option<String>,
    clear: bool,
}

struct ShareForm {
    port: Entity<InputState>,
    name: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    clear: bool,
}

/// 一台机器的两张添加表单。本机没有转发表单
struct MachineForms {
    fwd: Option<ForwardForm>,
    share: ShareForm,
    _subs: Vec<Subscription>,
}

pub struct RelaysPane {
    ws: Entity<Workspace>,
    client: FalconClient,
    list: Option<RelayList>,
    error: Option<String>,
    /// 上一轮没回来不叠加
    in_flight: bool,
    /// 正在改的规则（开关 / 删除），期间按钮禁用
    busy: HashSet<String>,
    copied: Option<String>,
    copied_reset: Option<Task<()>>,
    /// 键：本机是 [`LOCAL`]，主机是 host id。输入框要 window 才能建，所以在 render 里按需补
    forms: HashMap<String, MachineForms>,
    _poll: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<RelaysEvent> for RelaysPane {}

/// 端口输入：只收数字、最多 5 位
fn port_input(placeholder: String, window: &mut Window, cx: &mut Context<RelaysPane>) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .validate(|s, _| s.len() <= 5 && s.chars().all(|c| c.is_ascii_digit()))
    })
}

fn name_input(window: &mut Window, cx: &mut Context<RelaysPane>) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(t!("forward.namePlaceholder").to_string()))
}

fn parse_port(s: &str) -> Option<u16> {
    s.parse::<u16>().ok().filter(|p| *p >= 1)
}

/// 回环地址是默认值，显示出来只是噪音；非默认的（直接调 API 建的）才带上
fn endpoint_label(host: &str, port: u16) -> String {
    if matches!(host, "127.0.0.1" | "localhost" | "::1") { port.to_string() } else { format!("{host}:{port}") }
}

fn route_label(row: &PortForward) -> String {
    format!("{} → {}", endpoint_label(&row.bind_host, row.bind_port), endpoint_label(&row.dest_host, row.dest_port))
}

fn state_label(state: ForwardState) -> String {
    match state {
        ForwardState::Stopped => t!("forward.state_stopped"),
        ForwardState::Starting => t!("forward.state_starting"),
        ForwardState::Active => t!("forward.state_active"),
        ForwardState::Error => t!("forward.state_error"),
        _ => t!("forward.state_stopped"),
    }
    .to_string()
}

fn kind_labels(kind: ForwardKind) -> (String, String) {
    if kind == ForwardKind::Local {
        (t!("forward.localPort").to_string(), t!("forward.remotePort").to_string())
    } else {
        (t!("forward.remotePort").to_string(), t!("forward.localPort").to_string())
    }
}

impl RelaysPane {
    pub fn new(ws: Entity<Workspace>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let client = ws.read(cx).client.clone();
        // 主机增删改都在 ws.hosts 上，跟着重画（新主机的表单在 render 里补）
        let subs = vec![cx.observe(&ws, |_, _, cx| cx.notify())];
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                if this.update(cx, |this, cx| this.load(cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            ws,
            client,
            list: None,
            error: None,
            in_flight: false,
            busy: HashSet::new(),
            copied: None,
            copied_reset: None,
            forms: HashMap::new(),
            _poll: poll,
            _subs: subs,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.in_flight {
            return;
        }
        self.in_flight = true;
        let fut = self.client.list_relays();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.in_flight = false;
                match res {
                    Ok(list) => {
                        this.list = Some(list);
                        this.error = None;
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
    }

    /// 改完规则立刻重拉，不等下一轮轮询。在飞的那一轮可能是写之前发的，不能拿它当结果
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.in_flight = false;
        self.load(cx);
    }

    /// 行上的开关 / 删除：同一条规则一次只跑一个请求
    fn run<T: 'static>(&mut self, id: String, fut: impl Future<Output = ApiResult<T>> + 'static, cx: &mut Context<Self>) {
        if !self.busy.insert(id.clone()) {
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.busy.remove(&id);
                match res {
                    Ok(_) => this.refresh(cx),
                    Err(err) => this.ws.update(cx, |w, cx| {
                        w.handle_error(&err, cx);
                        w.toast(ToastKind::Danger, t!("toast.failed").to_string(), Some(err.message.clone()), cx);
                    }),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn copy_url(&mut self, id: String, url: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
        self.copied = Some(id);
        self.copied_reset = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1500)).await;
            this.update(cx, |this, cx| {
                this.copied = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---------------- 表单 ----------------

    /// 给每台机器补上表单、丢掉已删主机的表单，并处理提交成功后的清空
    fn sync_forms(&mut self, hosts: &[SshHost], window: &mut Window, cx: &mut Context<Self>) {
        let keep: HashSet<&str> = std::iter::once(LOCAL).chain(hosts.iter().map(|h| h.id.as_str())).collect();
        self.forms.retain(|k, _| keep.contains(k.as_str()));
        for key in keep {
            if !self.forms.contains_key(key) {
                let forms = self.new_forms(key.to_string(), key != LOCAL, window, cx);
                self.forms.insert(key.to_string(), forms);
            }
        }
        for forms in self.forms.values_mut() {
            if let Some(fwd) = forms.fwd.as_mut().filter(|f| f.clear) {
                fwd.clear = false;
                for input in [&fwd.bind, &fwd.dest, &fwd.name] {
                    input.update(cx, |s, cx| s.set_value("", window, cx));
                }
                fwd.last_bind.clear();
            }
            if std::mem::take(&mut forms.share.clear) {
                for input in [&forms.share.port, &forms.share.name] {
                    input.update(cx, |s, cx| s.set_value("", window, cx));
                }
            }
        }
    }

    fn new_forms(&mut self, key: String, with_forward: bool, window: &mut Window, cx: &mut Context<Self>) -> MachineForms {
        let mut subs = Vec::new();
        let fwd = with_forward.then(|| {
            let (a, b) = kind_labels(ForwardKind::Local);
            let form = ForwardForm {
                kind: ForwardKind::Local,
                bind: port_input(a, window, cx),
                dest: port_input(b, window, cx),
                name: name_input(window, cx),
                last_bind: String::new(),
                busy: false,
                error: None,
                clear: false,
            };
            // 回车 = 提交；输入变了要重画"添加"按钮的禁用态
            for input in [&form.dest, &form.name] {
                let k = key.clone();
                subs.push(cx.subscribe(input, move |this, _, ev: &InputEvent, cx| match ev {
                    InputEvent::PressEnter { .. } => this.submit_forward(&k, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                }));
            }
            let k = key.clone();
            subs.push(cx.subscribe_in(&form.bind, window, move |this, state, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.submit_forward(&k, cx),
                InputEvent::Change => {
                    // 目标端口没被单独改过就跟着监听端口走，两边同号是常态
                    let value = state.read(cx).value().to_string();
                    let Some(fwd) = this.forms.get_mut(&k).and_then(|f| f.fwd.as_mut()) else { return };
                    let dest = fwd.dest.read(cx).value().to_string();
                    if dest.is_empty() || dest == fwd.last_bind {
                        let v = value.clone();
                        fwd.dest.update(cx, |d, cx| d.set_value(v, window, cx));
                    }
                    fwd.last_bind = value;
                    cx.notify();
                }
                _ => {}
            }));
            form
        });
        let share = ShareForm {
            port: port_input(
                if with_forward { t!("forward.remotePort") } else { t!("forward.localPort") }.to_string(),
                window,
                cx,
            ),
            name: name_input(window, cx),
            busy: false,
            error: None,
            clear: false,
        };
        for input in [&share.port, &share.name] {
            let k = key.clone();
            subs.push(cx.subscribe(input, move |this, _, ev: &InputEvent, cx| match ev {
                InputEvent::PressEnter { .. } => this.submit_share(&k, cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }));
        }
        MachineForms { fwd, share, _subs: subs }
    }

    fn submit_forward(&mut self, key: &str, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let Some(fwd) = self.forms.get_mut(key).and_then(|f| f.fwd.as_mut()) else { return };
        if fwd.busy {
            return;
        }
        let bind = fwd.bind.read(cx).value().to_string();
        let dest = fwd.dest.read(cx).value().to_string();
        if bind.is_empty() {
            return;
        }
        let listen = parse_port(&bind);
        let target = if dest.is_empty() { listen } else { parse_port(&dest) };
        let (Some(bind_port), Some(dest_port)) = (listen, target) else {
            fwd.error = Some(t!("forward.portInvalid").to_string());
            cx.notify();
            return;
        };
        let name = fwd.name.read(cx).value().trim().to_string();
        let input = PortForwardInput {
            host_id: key.to_string(),
            name: Some(name).filter(|n| !n.is_empty()),
            kind: fwd.kind,
            bind_host: None,
            bind_port,
            dest_host: None,
            dest_port,
            enabled: None,
        };
        fwd.busy = true;
        fwd.error = None;
        cx.notify();
        let fut = client.create_forward(&input);
        let key = key.to_string();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if let Err(err) = &res {
                    this.ws.update(cx, |w, cx| w.handle_error(err, cx));
                }
                if let Some(fwd) = this.forms.get_mut(&key).and_then(|f| f.fwd.as_mut()) {
                    fwd.busy = false;
                    match &res {
                        Ok(_) => fwd.clear = true,
                        Err(err) => fwd.error = Some(err.message.clone()),
                    }
                }
                if res.is_ok() {
                    this.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn submit_share(&mut self, key: &str, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let Some(forms) = self.forms.get_mut(key) else { return };
        let share = &mut forms.share;
        if share.busy {
            return;
        }
        let raw = share.port.read(cx).value().to_string();
        if raw.is_empty() {
            return;
        }
        let Some(dest_port) = parse_port(&raw) else {
            share.error = Some(t!("forward.portInvalid").to_string());
            cx.notify();
            return;
        };
        let name = share.name.read(cx).value().trim().to_string();
        // enabled 不带：服务端默认启用，建完立刻是 starting，第一次要在后台下载 cloudflared
        let input = PublicShareInput {
            host_id: (key != LOCAL).then(|| key.to_string()),
            name: Some(name).filter(|n| !n.is_empty()),
            dest_host: None,
            dest_port,
            enabled: None,
        };
        share.busy = true;
        share.error = None;
        cx.notify();
        let fut = client.create_share(&input);
        let key = key.to_string();
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                if let Err(err) = &res {
                    this.ws.update(cx, |w, cx| w.handle_error(err, cx));
                }
                if let Some(forms) = this.forms.get_mut(&key) {
                    forms.share.busy = false;
                    match &res {
                        Ok(_) => forms.share.clear = true,
                        Err(err) => forms.share.error = Some(err.message.clone()),
                    }
                }
                if res.is_ok() {
                    this.refresh(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_kind(&mut self, key: &str, kind: ForwardKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fwd) = self.forms.get_mut(key).and_then(|f| f.fwd.as_mut()) else { return };
        fwd.kind = kind;
        let (a, b) = kind_labels(kind);
        fwd.bind.update(cx, |s, cx| s.set_placeholder(a, window, cx));
        fwd.dest.update(cx, |s, cx| s.set_placeholder(b, window, cx));
        cx.notify();
    }

    // ---------------- 画 ----------------

    fn state_dot(&self, id: &str, state: ForwardState, cx: &App) -> AnyElement {
        let ui = Ui::global(cx);
        let color = match state {
            ForwardState::Active => ui.success,
            ForwardState::Starting => ui.warning,
            ForwardState::Error => ui.destructive,
            _ => ui.muted_foreground.opacity(0.4),
        };
        let label: SharedString = state_label(state).into();
        let dot = div()
            .id(ElementId::Name(format!("relay-dot-{id}").into()))
            .flex_none()
            .size(zpx(6.))
            .rounded_full()
            .bg(color)
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx));
        if state == ForwardState::Starting {
            // React 版的 animate-breathe：正在建立的那颗点慢慢呼吸
            dot.with_animation(
                ElementId::Name(format!("relay-breathe-{id}").into()),
                Animation::new(Duration::from_millis(1600)).repeat().with_easing(pulsating_between(0.35, 1.0)),
                |d, delta| d.opacity(delta),
            )
            .into_any_element()
        } else {
            dot.into_any_element()
        }
    }

    /// 同端口徽标：这个端口上不止一条通道时才挂
    fn same_port_badge(id: &str, n: usize, cx: &App) -> Option<AnyElement> {
        if n < 2 {
            return None;
        }
        let ui = Ui::global(cx);
        let tip: SharedString = t!("forward.samePortHint", n = n).to_string().into();
        Some(
            div()
                .id(ElementId::Name(format!("relay-same-{id}").into()))
                .flex_none()
                .h(zpx(18.))
                .px(zpx(6.))
                .flex()
                .items_center()
                .rounded_full()
                .bg(ui.tint)
                .text_color(ui.tint_foreground)
                .text_size(zpx(11.))
                .whitespace_nowrap()
                .child(t!("forward.samePort", n = n).to_string())
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .into_any_element(),
        )
    }

    /// 一行规则的外壳：主行（点 + 内容 + 操作）+ 可选的错误行
    fn row_shell(&self, main: Div, error: Option<String>, last: bool, cx: &App) -> AnyElement {
        let ui = Ui::global(cx);
        div()
            .px_3()
            .py(zpx(10.))
            .when(!last, |d| d.border_b_1().border_color(ui.border.opacity(0.5)))
            .child(main.flex().items_center().gap_2())
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .mt_1()
                        .pl(zpx(14.))
                        .text_size(zpx(11.))
                        .font_family(crate::fonts::BERKELEY)
                        .text_color(ui.destructive)
                        .child(e),
                )
            })
            .into_any_element()
    }

    fn name_cell(name: Option<String>, cx: &App) -> Div {
        let ui = Ui::global(cx);
        match name.filter(|n| !n.is_empty()) {
            Some(n) => div().flex_1().min_w_0().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(n),
            None => div().flex_1().min_w_0().text_sm().text_color(ui.muted_foreground.opacity(0.6)).child("—"),
        }
    }

    fn forward_row(&self, row: &PortForward, same: usize, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let busy = self.busy.contains(&row.id);
        let kind = if row.kind == ForwardKind::Local { t!("forward.kind_local") } else { t!("forward.kind_remote") }.to_string();
        let (toggle_id, enabled) = (row.id.clone(), row.enabled);
        let del_id = row.id.clone();
        let main = div()
            .child(self.state_dot(&row.id, row.state, cx))
            .child(div().w(zpx(36.)).flex_none().text_xs().text_color(ui.muted_foreground).child(kind))
            .child(Self::name_cell(row.name.clone(), cx))
            .child(
                div()
                    .w(zpx(180.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(zpx(6.))
                    .child(div().min_w_0().truncate().font_family(crate::fonts::BERKELEY).text_xs().child(route_label(row)))
                    .children(Self::same_port_badge(&row.id, same, cx)),
            )
            .child(div().w(zpx(72.)).flex_none().truncate().text_xs().text_color(ui.muted_foreground).child(state_label(row.state)))
            .child(
                Switch::new(ElementId::Name(format!("fwd-on-{}", row.id).into()))
                    .xsmall()
                    .checked(row.enabled)
                    .disabled(busy)
                    .tooltip(t!("forward.enabled").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let patch = PortForwardPatch { enabled: Some(!enabled), ..Default::default() };
                        let fut = this.client.update_forward(&toggle_id, &patch);
                        this.run(toggle_id.clone(), fut, cx);
                    })),
            )
            .child(icon_button(
                ElementId::Name(format!("fwd-del-{}", row.id).into()),
                icon(IconName::Trash).size(zpx(14.)),
                t!("forward.delete").to_string(),
                24.,
                busy,
                cx,
                cx.listener(move |this, _, _, cx| {
                    let fut = this.client.delete_forward(&del_id);
                    this.run(del_id.clone(), fut, cx);
                }),
            ));
        self.row_shell(main, row.error.clone(), last, cx)
    }

    fn share_row(&self, row: &PublicShare, same: usize, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let busy = self.busy.contains(&row.id);
        let url_cell = match row.public_url.clone() {
            Some(url) => {
                let shown = url.strip_prefix("https://").unwrap_or(&url).to_string();
                div()
                    .id(ElementId::Name(format!("share-url-{}", row.id).into()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::fonts::BERKELEY)
                    .text_xs()
                    .text_color(ui.primary)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .child(shown)
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(state_label(row.state))
                .into_any_element(),
        };
        let mut main = div()
            .child(self.state_dot(&row.id, row.state, cx))
            .child(Self::name_cell(row.name.clone(), cx))
            .child(
                div()
                    .w(zpx(120.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(zpx(6.))
                    .child(div().font_family(crate::fonts::BERKELEY).text_xs().child(endpoint_label(&row.dest_host, row.dest_port)))
                    .children(Self::same_port_badge(&row.id, same, cx)),
            )
            .child(div().flex_1().min_w_0().flex().child(url_cell));
        if let Some(url) = row.public_url.clone() {
            let copied = self.copied.as_deref() == Some(row.id.as_str());
            let (copy_id, copy_url) = (row.id.clone(), url.clone());
            main = main
                .child(tiny_button(
                    ElementId::Name(format!("share-copy-{}", row.id).into()),
                    if copied { IconName::Check } else { IconName::Copy },
                    if copied { t!("forward.shareCopied") } else { t!("forward.shareCopy") }.to_string(),
                    24.,
                    cx,
                    cx.listener(move |this, _, _, cx| this.copy_url(copy_id.clone(), &copy_url, cx)),
                ))
                .child(tiny_button(
                    ElementId::Name(format!("share-open-{}", row.id).into()),
                    IconName::ExternalLink,
                    t!("forward.shareOpen").to_string(),
                    24.,
                    cx,
                    move |_, _, cx| cx.open_url(&url),
                ));
        }
        let (toggle_id, enabled) = (row.id.clone(), row.enabled);
        let del_id = row.id.clone();
        main = main
            .child(
                Switch::new(ElementId::Name(format!("share-on-{}", row.id).into()))
                    .xsmall()
                    .checked(row.enabled)
                    .disabled(busy)
                    .tooltip(t!("forward.enabled").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let patch = PublicSharePatch { enabled: Some(!enabled), ..Default::default() };
                        let fut = this.client.update_share(&toggle_id, &patch);
                        this.run(toggle_id.clone(), fut, cx);
                    })),
            )
            .child(icon_button(
                ElementId::Name(format!("share-del-{}", row.id).into()),
                icon(IconName::Trash).size(zpx(14.)),
                t!("forward.shareDelete").to_string(),
                24.,
                busy,
                cx,
                cx.listener(move |this, _, _, cx| {
                    let fut = this.client.delete_share(&del_id);
                    this.run(del_id.clone(), fut, cx);
                }),
            ));
        self.row_shell(main, row.error.clone(), last, cx)
    }

    fn form_hint(text: String, cx: &App) -> Div {
        let ui = Ui::global(cx);
        div().text_size(zpx(11.)).line_height(relative(1.6)).text_color(ui.muted_foreground).child(text)
    }

    fn forward_form(&self, key: &str, cx: &mut Context<Self>) -> Option<Div> {
        let fwd = self.forms.get(key)?.fwd.as_ref()?;
        let ui = Ui::global(cx).clone();
        let this = cx.weak_entity();
        let hint_text = if fwd.kind == ForwardKind::Local { t!("forward.hint_local") } else { t!("forward.hint_remote") }.to_string();
        let bind_empty = fwd.bind.read(cx).value().is_empty();
        let (k_kind, k_add) = (key.to_string(), key.to_string());
        Some(
            div()
                .flex()
                .flex_col()
                .gap(zpx(6.))
                .px_3()
                .py(zpx(10.))
                .border_t_1()
                .border_color(ui.border.opacity(0.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(zpx(120.)).flex_none().child(segmented(
                            &format!("fwd-kind-{key}"),
                            fwd.kind,
                            vec![
                                (ForwardKind::Local, t!("forward.kind_local").to_string()),
                                (ForwardKind::Remote, t!("forward.kind_remote").to_string()),
                            ],
                            move |k, window, cx| {
                                let _ = this.update(cx, |p, cx| p.set_kind(&k_kind, k, window, cx));
                            },
                            cx,
                        )))
                        .child(div().w(zpx(110.)).flex_none().child(Input::new(&fwd.bind).small()))
                        .child(icon(IconName::ArrowRight).size(zpx(12.)).text_color(ui.muted_foreground))
                        .child(div().w(zpx(110.)).flex_none().child(Input::new(&fwd.dest).small()))
                        .child(div().flex_1().min_w_0().child(Input::new(&fwd.name).small()))
                        .child(
                            Button::new(SharedString::from(format!("fwd-add-{key}")))
                                .primary()
                                .small()
                                .icon(IconName::Plus)
                                .label(t!("forward.addAction").to_string())
                                .loading(fwd.busy)
                                .disabled(fwd.busy || bind_empty)
                                .on_click(cx.listener(move |this, _, _, cx| this.submit_forward(&k_add, cx))),
                        ),
                )
                .child(Self::form_hint(hint_text, cx))
                .when_some(fwd.error.clone(), |d, e| d.child(div().text_size(zpx(11.)).text_color(ui.destructive).child(e))),
        )
    }

    fn share_form(&self, key: &str, cx: &mut Context<Self>) -> Option<Div> {
        let share = &self.forms.get(key)?.share;
        let ui = Ui::global(cx).clone();
        let hint_text = if key == LOCAL { t!("forward.shareHint_local") } else { t!("forward.shareHint_remote") }.to_string();
        let port_empty = share.port.read(cx).value().is_empty();
        let k_add = key.to_string();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(zpx(6.))
                .px_3()
                .py(zpx(10.))
                .border_t_1()
                .border_color(ui.border.opacity(0.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(zpx(110.)).flex_none().child(Input::new(&share.port).small()))
                        .child(div().flex_1().min_w_0().child(Input::new(&share.name).small()))
                        .child(
                            Button::new(SharedString::from(format!("share-add-{key}")))
                                .primary()
                                .small()
                                .icon(IconName::Globe)
                                .label(t!("forward.shareAddAction").to_string())
                                .loading(share.busy)
                                .disabled(share.busy || port_empty)
                                .on_click(cx.listener(move |this, _, _, cx| this.submit_share(&k_add, cx))),
                        ),
                )
                .child(Self::form_hint(hint_text, cx))
                .when_some(share.error.clone(), |d, e| d.child(div().text_size(zpx(11.)).text_color(ui.destructive).child(e))),
        )
    }

    fn sub_title(text: String, cx: &App) -> Div {
        let ui = Ui::global(cx);
        div().px_3().pt(zpx(10.)).pb_1().text_size(zpx(11.)).text_color(ui.muted_foreground).child(text)
    }

    /// 一台机器：标题行（图标 + 名字 + 连接串）+ 一块 sunken 卡片装它的规则与表单
    fn machine(
        &self,
        key: &str,
        title: String,
        conn: String,
        list: &RelayList,
        counts: &HashMap<String, usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let local = key == LOCAL;
        let head = div()
            .flex()
            .items_center()
            .gap_2()
            .mb_2()
            .child(icon(if local { IconName::Monitor } else { IconName::Server }).size(zpx(14.)).text_color(ui.muted_foreground))
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().min_w_0().truncate().font_family(crate::fonts::BERKELEY).text_xs().text_color(ui.muted_foreground).child(conn));

        let mut card = crate::ui::sunken(cx).flex().flex_col().pb_1();
        if !local {
            let rows: Vec<PortForward> = list.forwards.iter().filter(|f| f.host_id == key).cloned().collect();
            card = card.child(Self::sub_title(t!("forward.sshSection").to_string(), cx));
            if rows.is_empty() {
                card = card.child(hint(t!("forward.empty").to_string(), cx).py_2());
            }
            let n = rows.len();
            for (i, row) in rows.iter().enumerate() {
                let same = counts.get(&row.id).copied().unwrap_or(1);
                card = card.child(self.forward_row(row, same, i + 1 == n, cx));
            }
            if let Some(form) = self.forward_form(key, cx) {
                card = card.child(form);
            }
        }
        let shares: Vec<PublicShare> =
            list.shares.iter().filter(|s| s.host_id.as_deref() == (!local).then_some(key)).cloned().collect();
        card = card.child(
            Self::sub_title(t!("forward.shareSection").to_string(), cx).when(!local, |d| d.mt_1().border_t_1().border_color(ui.border)),
        );
        if shares.is_empty() {
            card = card.child(hint(t!("forward.shareEmpty").to_string(), cx).py_2());
        }
        let n = shares.len();
        for (i, row) in shares.iter().enumerate() {
            let same = counts.get(&row.id).copied().unwrap_or(1);
            card = card.child(self.share_row(row, same, i + 1 == n, cx));
        }
        if let Some(form) = self.share_form(key, cx) {
            card = card.child(form);
        }
        div().mb_6().flex().flex_col().child(head).child(card.rounded(radius::LG)).into_any_element()
    }
}

impl Render for RelaysPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hosts = self.ws.read(cx).hosts.clone();
        self.sync_forms(&hosts, window, cx);
        let ui = Ui::global(cx).clone();

        let children: Vec<AnyElement> = match (&self.list, &self.error) {
            (None, Some(err)) => vec![hint_detail(t!("forward.loadFailed").to_string(), err.clone(), cx).px_0().into_any_element()],
            (None, None) => vec![hint(t!("forward.loading").to_string(), cx).px_0().into_any_element()],
            (Some(list), _) => {
                let list = list.clone();
                let counts = same_port_counts(&list);
                let mut out = vec![self.machine(
                    LOCAL,
                    t!("forward.local").to_string(),
                    t!("forward.localConn").to_string(),
                    &list,
                    &counts,
                    cx,
                )];
                for host in &hosts {
                    out.push(self.machine(&host.id, host.name.clone(), ssh_conn(host), &list, &counts, cx));
                }
                if hosts.is_empty() {
                    out.push(
                        crate::ui::sunken(cx)
                            .px_4()
                            .py_6()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("forward.noHosts").to_string()))
                            .child(
                                Button::new("relays-go-hosts")
                                    .outline()
                                    .small()
                                    .icon(IconName::Server)
                                    .label(t!("forward.goHosts").to_string())
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(RelaysEvent::GoHosts))),
                            )
                            .into_any_element(),
                    );
                }
                out
            }
        };
        section(t!("forward.title").to_string(), Some(t!("forward.hint").to_string()), children, cx)
    }
}
