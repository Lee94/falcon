//! Agent 浏览器窗口（ADR 0029）：实时画面、一键接管 / 交还、输入转发与可展开操作记录。

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use falcon_client::{BrowserSocket, BrowserSocketEvent, FalconClient};
use falcon_core::pane_key::BrowserTarget;
use falcon_proto::{
    BrowserControl, BrowserControlHolder, BrowserEvent, BrowserEventKind, BrowserFrameHeader,
    BrowserServerMessage, BrowserTab, KeyKind, PointerButton, PointerKind,
};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, CursorStyle, Element, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, GlobalElementId, Image, ImageFormat, ImageSource, InputHandler, IntoElement,
    KeyDownEvent, KeyUpEvent, Keystroke, LayoutId, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, RenderImage, ScrollEvent, SharedString,
    Style, Task, UTF16Selection, Window, div, img, px, relative, size,
};
use rust_i18n::t;
use web_time::Instant;

use crate::prefs::Prefs;
use crate::theme::{Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

const AUTO_OPEN_PREF_KEY: &str = "falcon.browser.autoOpen";
const MOVE_THROTTLE: Duration = Duration::from_millis(16);
const ELAPSED_TICK: Duration = Duration::from_secs(15);
const LOG_PAGE_SIZE: usize = 200;

/// 设置开关：「agent 打开浏览器时自动弹出窗口」，默认开。
pub fn auto_open_enabled(cx: &App) -> bool {
    Prefs::global(cx).get(AUTO_OPEN_PREF_KEY) != Some("off")
}

pub fn set_auto_open_enabled(on: bool, cx: &mut App) {
    cx.global_mut::<Prefs>()
        .set(AUTO_OPEN_PREF_KEY, if on { "on" } else { "off" }.to_string());
}

/// 从 URL 提取展示用域名（终端标题栏标记用）。
pub fn url_domain(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.trim();
    (!host.is_empty()).then(|| host.to_string())
}

/// 向工作区 / 画布发出的轻量通知（控制权切换时让窗口描边与标题栏标记立刻重画）。
pub enum BrowserViewEvent {
    StateChanged,
}

impl EventEmitter<BrowserViewEvent> for BrowserView {}

enum ThumbState {
    Loading,
    Ready(Arc<RenderImage>),
    Failed,
}

pub struct BrowserView {
    pub session_id: String,
    browser_session: Option<String>,
    /// 同一终端的多个浏览器实例用各自 key 隔离 GPUI 元素 ID。
    view_key: String,
    client: FalconClient,
    socket: BrowserSocket,
    focus: FocusHandle,
    visible: bool,
    connected: bool,
    gone: bool,
    control: BrowserControl,
    url: String,
    tabs: Vec<BrowserTab>,

    /// 当前已解码的画面与对应帧头（用于坐标换算与 ack）
    frame: Option<(BrowserFrameHeader, Arc<RenderImage>)>,
    /// 画面解码只保留最新一帧，避免慢机器积压
    decoding_frame: bool,
    pending_frame: Option<(BrowserFrameHeader, Bytes)>,
    /// 视口中实际绘制出的画面矩形（窗口坐标，prepaint 更新）
    image_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// 最近一次 mouseMove 发送时刻（60Hz 节流）
    last_move: Option<Instant>,
    /// 输入法组字状态
    marked_text: Option<String>,

    /// 底部「记录」抽屉
    log_open: bool,
    log_loading: bool,
    log_has_more: bool,
    events: Vec<BrowserEvent>,
    /// 已点开明文显示的敏感输入事件 id
    revealed_events: HashSet<u64>,
    /// 已展开缩略图预览的事件 id → 缩略图加载状态
    thumbs: HashMap<u64, ThumbState>,
    expanded_thumb: Option<u64>,

    _tasks: Vec<Task<()>>,
}

impl Focusable for BrowserView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserView {
    pub fn new(
        target: BrowserTarget,
        client: FalconClient,
        initial_visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let view_key = target.key();
        let browser_session = (!target.is_default()).then_some(target.browser_session);
        let session_id = target.session_id;
        let focus = cx.focus_handle();
        let (tx, mut rx) = unbounded::<BrowserSocketEvent>();
        let socket = BrowserSocket::open_instance(
            &client,
            &session_id,
            browser_session.as_deref(),
            initial_visible,
            move |ev| {
                let _ = tx.unbounded_send(ev);
            },
        );

        let mut tasks = Vec::new();
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Some(ev) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_socket_event(ev, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));

        // 接管期间每 15 秒刷新一次「你在操作 · N 分钟」文案
        tasks.push(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(ELAPSED_TICK).await;
            let keep = this
                .update(cx, |this, cx| {
                    if this.control.holder == BrowserControlHolder::Human && !this.gone {
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        }));

        let mut view = Self {
            session_id,
            browser_session,
            view_key,
            client,
            socket,
            focus,
            visible: initial_visible,
            connected: false,
            gone: false,
            control: BrowserControl::default(),
            url: String::new(),
            tabs: Vec::new(),
            frame: None,
            decoding_frame: false,
            pending_frame: None,
            image_bounds: Rc::new(Cell::new(Bounds::default())),
            last_move: None,
            marked_text: None,
            log_open: false,
            log_loading: false,
            log_has_more: false,
            events: Vec::new(),
            revealed_events: HashSet::new(),
            thumbs: HashMap::new(),
            expanded_thumb: None,
            _tasks: tasks,
        };
        view.load_initial_log(cx);
        view
    }

    pub fn is_human_control(&self) -> bool {
        !self.gone && self.control.holder == BrowserControlHolder::Human
    }

    pub fn current_url(&self) -> &str {
        &self.url
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            self.socket.set_visible(visible);
        }
    }

    fn on_socket_event(&mut self, ev: BrowserSocketEvent, cx: &mut Context<Self>) {
        match ev {
            BrowserSocketEvent::Connected { reconnected } => {
                self.connected = true;
                if reconnected {
                    self.load_initial_log(cx);
                }
                cx.notify();
            }
            BrowserSocketEvent::Disconnected { .. } | BrowserSocketEvent::Unauthorized => {
                self.connected = false;
                cx.notify();
            }
            BrowserSocketEvent::Frame { header, jpeg } => {
                if self.decoding_frame {
                    self.pending_frame = Some((header, jpeg));
                } else {
                    self.decode_frame(header, jpeg, cx);
                }
            }
            BrowserSocketEvent::Message(msg) => match msg {
                BrowserServerMessage::Hello {
                    control,
                    url,
                    tabs,
                    gone,
                    ..
                } => {
                    self.control = control;
                    self.url = url;
                    self.tabs = tabs;
                    self.gone = gone;
                    cx.emit(BrowserViewEvent::StateChanged);
                    cx.notify();
                }
                BrowserServerMessage::Event { event } => {
                    if !self.events.iter().any(|e| e.id == event.id) {
                        self.events.push(event);
                    }
                    cx.notify();
                }
                BrowserServerMessage::Thumb { event_id, thumb_id } => {
                    if let Some(ev) = self.events.iter_mut().rev().find(|e| e.id == event_id) {
                        ev.thumb_id = Some(thumb_id);
                    }
                    cx.notify();
                }
                BrowserServerMessage::Control {
                    holder,
                    since,
                    policy_enforced,
                } => {
                    self.control = BrowserControl {
                        holder,
                        since,
                        policy_enforced,
                    };
                    cx.emit(BrowserViewEvent::StateChanged);
                    cx.notify();
                }
                BrowserServerMessage::Status {
                    url,
                    tabs,
                    ..
                } => {
                    if let Some(u) = url {
                        self.url = u;
                    }
                    if let Some(tabs) = tabs {
                        self.tabs = tabs;
                    }
                    self.gone = false;
                    cx.emit(BrowserViewEvent::StateChanged);
                    cx.notify();
                }
                BrowserServerMessage::Gone => {
                    self.gone = true;
                    self.control.holder = BrowserControlHolder::Agent;
                    self.control.since = None;
                    cx.emit(BrowserViewEvent::StateChanged);
                    cx.notify();
                }
                BrowserServerMessage::Unknown => {}
            },
        }
    }

    fn decode_frame(&mut self, header: BrowserFrameHeader, jpeg: Bytes, cx: &mut Context<Self>) {
        self.decoding_frame = true;
        let svg = cx.svg_renderer();
        cx.spawn(async move |this, cx| {
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    Image::from_bytes(ImageFormat::Jpeg, jpeg.to_vec())
                        .to_image_data(svg)
                        .ok()
                })
                .await;
            this.update(cx, |this, cx| {
                this.decoding_frame = false;
                if let Some(image) = decoded {
                    this.frame = Some((header, image));
                    this.socket.ack(header.seq);
                    cx.notify();
                } else {
                    // 解码失败也要 ack，避免上游卡死在这一帧
                    this.socket.ack(header.seq);
                }
                if let Some((next_header, next_jpeg)) = this.pending_frame.take() {
                    this.decode_frame(next_header, next_jpeg, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_initial_log(&mut self, cx: &mut Context<Self>) {
        if self.log_loading {
            return;
        }
        self.log_loading = true;
        let fut = self.client.browser_instance_log(
            &self.session_id,
            self.browser_session.as_deref(),
            None,
            Some(LOG_PAGE_SIZE),
        );
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.log_loading = false;
                if let Ok(page) = res {
                    let mut by_id: HashMap<u64, BrowserEvent> =
                        this.events.drain(..).map(|e| (e.id, e)).collect();
                    for ev in page.events {
                        by_id.entry(ev.id).or_insert(ev);
                    }
                    let mut merged: Vec<BrowserEvent> = by_id.into_values().collect();
                    merged.sort_by_key(|e| e.id);
                    this.events = merged;
                    this.log_has_more = page.has_more;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_older_log(&mut self, cx: &mut Context<Self>) {
        if self.log_loading || !self.log_has_more {
            return;
        }
        let Some(before) = self.events.first().map(|e| e.id) else { return };
        self.log_loading = true;
        let fut = self.client.browser_instance_log(
            &self.session_id,
            self.browser_session.as_deref(),
            Some(before),
            Some(LOG_PAGE_SIZE),
        );
        cx.spawn(async move |this, cx| {
            let res = fut.await;
            this.update(cx, |this, cx| {
                this.log_loading = false;
                if let Ok(page) = res {
                    let existing: HashSet<u64> = this.events.iter().map(|e| e.id).collect();
                    let mut prefix: Vec<BrowserEvent> = page
                        .events
                        .into_iter()
                        .filter(|e| !existing.contains(&e.id))
                        .collect();
                    prefix.append(&mut this.events);
                    this.events = prefix;
                    this.log_has_more = page.has_more;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn toggle_thumb(&mut self, event_id: u64, thumb_id: String, cx: &mut Context<Self>) {
        if self.expanded_thumb == Some(event_id) {
            self.expanded_thumb = None;
            cx.notify();
            return;
        }
        self.expanded_thumb = Some(event_id);
        if matches!(self.thumbs.get(&event_id), Some(ThumbState::Ready(_) | ThumbState::Loading)) {
            cx.notify();
            return;
        }
        self.thumbs.insert(event_id, ThumbState::Loading);
        let url = FalconClient::browser_instance_thumb_path(
            &self.session_id,
            self.browser_session.as_deref(),
            &thumb_id,
        );
        let fut = self.client.raw_bytes(&url);
        let svg = cx.svg_renderer();
        cx.spawn(async move |this, cx| {
            let decoded = match fut.await {
                Ok(raw) => {
                    let bytes = raw.bytes.to_vec();
                    cx.background_executor()
                        .spawn(async move {
                            Image::from_bytes(ImageFormat::Jpeg, bytes).to_image_data(svg).ok()
                        })
                        .await
                }
                Err(_) => None,
            };
            this.update(cx, |this, cx| {
                this.thumbs.insert(
                    event_id,
                    match decoded {
                        Some(img) => ThumbState::Ready(img),
                        None => ThumbState::Failed,
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn takeover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.gone {
            return;
        }
        window.focus(&self.focus, cx);
        self.socket.takeover(Some(local_device_label()));
    }

    fn release(&mut self) {
        self.socket.release();
    }

    fn current_seq(&self) -> u64 {
        self.frame.as_ref().map_or(0, |(h, _)| h.seq)
    }

    fn normalized_point(&self, pos: Point<Pixels>) -> Option<(f64, f64)> {
        let rect = self.image_bounds.get();
        let w = f32::from(rect.size.width);
        let h = f32::from(rect.size.height);
        if w <= 1.0 || h <= 1.0 {
            return None;
        }
        let lx = f32::from(pos.x - rect.origin.x);
        let ly = f32::from(pos.y - rect.origin.y);
        if lx < 0.0 || ly < 0.0 || lx > w || ly > h {
            return None;
        }
        Some(((lx / w) as f64, (ly / h) as f64))
    }

    fn on_viewport_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.gone {
            return;
        }
        window.focus(&self.focus, cx);
        // Agent 操作期间：第一下点击只用来接管，不送进页面，避免误点（ADR 0029）
        if self.control.holder != BrowserControlHolder::Human {
            if e.button == MouseButton::Left {
                self.takeover(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        let Some((x, y)) = self.normalized_point(e.position) else { return };
        let button = map_mouse_button(e.button);
        let clicks = e.click_count.clamp(1, 3) as u8;
        self.socket.pointer(
            PointerKind::Down,
            x,
            y,
            button,
            clicks,
            encode_modifiers(&e.modifiers),
            self.current_seq(),
        );
        cx.stop_propagation();
    }

    fn on_viewport_mouse_up(&mut self, e: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_human_control() {
            return;
        }
        let Some((x, y)) = self.normalized_point(e.position) else { return };
        self.socket.pointer(
            PointerKind::Up,
            x,
            y,
            map_mouse_button(e.button),
            e.click_count.clamp(1, 3) as u8,
            encode_modifiers(&e.modifiers),
            self.current_seq(),
        );
        cx.stop_propagation();
    }

    fn on_viewport_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, _cx: &mut Context<Self>) {
        if !self.is_human_control() {
            return;
        }
        let now = Instant::now();
        if self.last_move.is_some_and(|t| now.duration_since(t) < MOVE_THROTTLE) {
            return;
        }
        let Some((x, y)) = self.normalized_point(e.position) else { return };
        self.last_move = Some(now);
        let button = e.pressed_button.map_or(PointerButton::None, map_mouse_button);
        self.socket.pointer(
            PointerKind::Move,
            x,
            y,
            button,
            0,
            encode_modifiers(&e.modifiers),
            self.current_seq(),
        );
    }

    fn on_viewport_wheel(&mut self, e: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_human_control() {
            return;
        }
        let Some((x, y)) = self.normalized_point(e.position) else { return };
        let delta = e.delta.pixel_delta(px(100.));
        self.socket.wheel(
            x,
            y,
            -f32::from(delta.x) as f64,
            -f32::from(delta.y) as f64,
            self.current_seq(),
        );
        cx.stop_propagation();
    }

    fn on_key_down(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.is_human_control() {
            return;
        }
        let ks = &e.keystroke;
        // 粘贴：Cmd+V（macOS）或 Ctrl+V（非 macOS）把本机剪贴板文字按 char 送进远端浏览器
        let is_paste = ks.key.eq_ignore_ascii_case("v")
            && !ks.modifiers.alt
            && !ks.modifiers.shift
            && ((cfg!(target_os = "macos") && ks.modifiers.platform && !ks.modifiers.control)
                || (!cfg!(target_os = "macos") && ks.modifiers.control && !ks.modifiers.platform));
        if is_paste {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
                && !text.is_empty()
            {
                self.socket.text(text);
            }
            cx.stop_propagation();
            return;
        }
        // macOS 上 Cmd 组合留给 falcon 的窗口管理快捷键
        if cfg!(target_os = "macos") && ks.modifiers.platform {
            return;
        }
        let (dom_key, dom_code) = map_dom_key(ks);
        let mods = encode_modifiers(&ks.modifiers);
        self.socket.key(KeyKind::Down, dom_key, dom_code, mods);
        // 特殊按键或带 Ctrl/Alt 修饰的按键不走 InputHandler；普通可打印字符交给 InputHandler::replace_text_in_range
        let has_cmd_mod = ks.modifiers.control || ks.modifiers.alt || ks.modifiers.platform;
        if has_cmd_mod || !is_printable_key(&ks.key) {
            cx.stop_propagation();
        }
    }

    fn on_key_up(&mut self, e: &KeyUpEvent, _: &mut Window, _cx: &mut Context<Self>) {
        if !self.is_human_control() {
            return;
        }
        let ks = &e.keystroke;
        if cfg!(target_os = "macos") && ks.modifiers.platform {
            return;
        }
        let (dom_key, dom_code) = map_dom_key(ks);
        self.socket
            .key(KeyKind::Up, dom_key, dom_code, encode_modifiers(&ks.modifiers));
    }

    pub(crate) fn commit_text(&mut self, text: &str) {
        if !self.is_human_control() || text.is_empty() {
            return;
        }
        self.socket.text(text.to_string());
    }

    pub(crate) fn set_marked_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.marked_text = Some(text);
        cx.notify();
    }

    pub(crate) fn clear_marked_text(&mut self, cx: &mut Context<Self>) {
        if self.marked_text.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn marked_text_range(&self) -> Option<std::ops::Range<usize>> {
        self.marked_text.as_ref().map(|t| 0..t.encode_utf16().count())
    }

    fn render_top_bar(&self, ui: &Ui, cx: &mut Context<Self>) -> impl IntoElement {
        let addr_text = if self.url.is_empty() {
            t!("browser.blankUrl").to_string()
        } else {
            self.url.clone()
        };
        let full_url: SharedString = self.url.clone().into();

        let status_label = if self.gone {
            t!("browser.closed").to_string()
        } else if self.control.holder == BrowserControlHolder::Human {
            let mins = self
                .control
                .since
                .map(|since| ((now_ms() - since).max(0) / 60_000) as u64)
                .unwrap_or(0);
            if mins == 0 {
                t!("browser.humanNow").to_string()
            } else {
                t!("browser.humanMinutes", n = mins).to_string()
            }
        } else {
            t!("browser.agentOperating").to_string()
        };

        let status_color = if self.gone {
            ui.muted_foreground
        } else if self.control.holder == BrowserControlHolder::Human {
            ui.tint_strong
        } else {
            ui.muted_foreground
        };

        let mut bar = div()
            .h(zpx(34.))
            .flex_none()
            .flex()
            .items_center()
            .gap(zpx(8.))
            .px_2()
            .bg(ui.background)
            .border_b_1()
            .border_color(ui.border)
            .text_xs();

        if let Some(browser_session) = &self.browser_session {
            bar = bar.child(
                div()
                    .flex_none()
                    .max_w(zpx(160.))
                    .truncate()
                    .text_color(ui.muted_foreground)
                    .child(browser_session.clone()),
            );
        }

        // 地址
        bar = bar.child(
            div()
                .id(SharedString::from(format!("browser-url-{}", self.view_key)))
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(zpx(5.))
                .px_2()
                .h(zpx(22.))
                .rounded(radius::SM)
                .bg(ui.app.opacity(0.6))
                .text_color(if self.url.is_empty() { ui.muted_foreground } else { ui.foreground })
                .when(!self.url.is_empty(), |d| {
                    let tip = full_url.clone();
                    d.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                })
                .child(icon(IconName::Globe).size(zpx(12.)).text_color(ui.muted_foreground).flex_none())
                .child(div().flex_1().min_w_0().truncate().child(addr_text)),
        );

        // 标签页数
        if !self.tabs.is_empty() {
            let tab_count = self.tabs.len();
            let tip: SharedString = self
                .tabs
                .iter()
                .map(|tab| {
                    let mark = if tab.active { "• " } else { "  " };
                    let name = if tab.title.is_empty() { &tab.url } else { &tab.title };
                    format!("{mark}{name}")
                })
                .collect::<Vec<_>>()
                .join("\n")
                .into();
            bar = bar.child(
                div()
                    .id(SharedString::from(format!("browser-tabs-{}", self.view_key)))
                    .flex_none()
                    .px(zpx(6.))
                    .h(zpx(20.))
                    .flex()
                    .items_center()
                    .rounded(radius::SM)
                    .bg(ui.muted)
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(t!("browser.tabsCount", n = tab_count).to_string()),
            );
        }

        // 状态
        bar = bar.child(
            div()
                .flex_none()
                .text_size(zpx(11.))
                .text_color(status_color)
                .when(self.is_human_control(), |d| d.font_weight(gpui_kit::FontWeight::MEDIUM))
                .child(status_label),
        );

        // policy 锁失效提示（决定五最后一条）
        if !self.gone && !self.control.policy_enforced {
            bar = bar.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(zpx(3.))
                    .text_size(zpx(11.))
                    .text_color(ui.warning)
                    .child(icon(IconName::TriangleAlert).size(zpx(12.)).text_color(ui.warning))
                    .child(t!("browser.policyWarn").to_string()),
            );
        }

        // 唯一的一个按钮：接管 / 交还（浏览器已关闭时不显示）
        if !self.gone {
            if self.control.holder == BrowserControlHolder::Human {
                bar = bar.child(
                    Button::new(SharedString::from(format!("browser-release-{}", self.view_key)))
                        .outline()
                        .xsmall()
                        .label(t!("browser.release").to_string())
                        .on_click(cx.listener(|this, _, _, _| this.release())),
                );
            } else {
                bar = bar.child(
                    Button::new(SharedString::from(format!("browser-takeover-{}", self.view_key)))
                        .primary()
                        .xsmall()
                        .label(t!("browser.takeover").to_string())
                        .on_click(cx.listener(|this, _, window, cx| this.takeover(window, cx))),
                );
            }
        }

        bar
    }

    fn render_viewport(&self, ui: &Ui, cx: &mut Context<Self>) -> impl IntoElement {
        let frame = self.frame.clone();
        let aspect = frame.as_ref().map(|(h, img)| {
            let sz = img.size(0);
            let w = if sz.width.0 > 0 { sz.width.0 as f32 } else { h.device_width.max(1) as f32 };
            let h_px = if sz.height.0 > 0 { sz.height.0 as f32 } else { h.device_height.max(1) as f32 };
            (w, h_px)
        });
        let cursor = if self.gone {
            CursorStyle::Arrow
        } else {
            CursorStyle::PointingHand
        };

        div()
            .id(SharedString::from(format!("browser-viewport-{}", self.view_key)))
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            .overflow_hidden()
            .bg(ui.app)
            .track_focus(&self.focus)
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_viewport_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_viewport_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_viewport_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_viewport_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_viewport_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_viewport_mouse_up))
            .on_mouse_move(cx.listener(Self::on_viewport_mouse_move))
            .on_scroll(cx.listener(Self::on_viewport_wheel))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_key_up(cx.listener(Self::on_key_up))
            .child(BrowserInputElement {
                view: cx.entity().clone(),
                focus: self.focus.clone(),
                image_bounds: self.image_bounds.clone(),
                aspect,
            })
            .child(match frame {
                Some((_, image)) => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        img(ImageSource::Render(image))
                            .size_full()
                            .object_fit(gpui_kit::ObjectFit::Contain),
                    )
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_xs()
                    .text_color(ui.muted_foreground)
                    .child(icon(IconName::Globe).size(zpx(24.)).text_color(ui.muted_foreground.opacity(0.6)))
                    .child(if self.gone {
                        t!("browser.closed").to_string()
                    } else if self.connected {
                        t!("browser.waitingFrame").to_string()
                    } else {
                        t!("browser.connecting").to_string()
                    })
                    .into_any_element(),
            })
    }

    fn render_log_drawer(&self, ui: &Ui, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.events.len();
        let toggle = div()
            .id(SharedString::from(format!("browser-log-toggle-{}", self.view_key)))
            .h(zpx(26.))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px_2()
            .bg(ui.background)
            .border_t_1()
            .border_color(ui.border)
            .cursor_pointer()
            .text_size(zpx(11.))
            .text_color(ui.muted_foreground)
            .hover(|s| s.text_color(ui.foreground))
            .on_click(cx.listener(|this, _, _, cx| {
                this.log_open = !this.log_open;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(zpx(5.))
                    .child(
                        icon(if self.log_open { IconName::ChevronDown } else { IconName::ChevronUp })
                            .size(zpx(12.)),
                    )
                    .child(t!("browser.logTitle", n = count).to_string()),
            );

        let mut drawer = div().flex_none().flex().flex_col().bg(ui.background).child(toggle);
        if !self.log_open {
            return drawer;
        }

        let mut list = div()
            .id(SharedString::from(format!("browser-log-list-{}", self.view_key)))
            .h(zpx(210.))
            .w_full()
            .overflow_y_scroll()
            .border_t_1()
            .border_color(ui.border)
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .gap(zpx(3.))
            .text_size(zpx(11.));

        if self.log_has_more {
            let loading = self.log_loading;
            list = list.child(
                div()
                    .id(SharedString::from(format!("browser-log-more-{}", self.view_key)))
                    .py_1()
                    .text_center()
                    .cursor_pointer()
                    .text_color(ui.tint_strong)
                    .on_click(cx.listener(|this, _, _, cx| this.load_older_log(cx)))
                    .child(if loading {
                        t!("browser.loading").to_string()
                    } else {
                        t!("browser.loadOlder").to_string()
                    }),
            );
        }

        if self.events.is_empty() {
            list = list.child(
                div()
                    .py_4()
                    .text_center()
                    .text_color(ui.muted_foreground)
                    .child(t!("browser.logEmpty").to_string()),
            );
        } else {
            for ev in &self.events {
                list = list.child(self.render_log_row(ev, ui, cx));
            }
        }

        drawer = drawer.child(list);
        drawer
    }

    fn render_log_row(&self, ev: &BrowserEvent, ui: &Ui, cx: &mut Context<Self>) -> impl IntoElement {
        let ev_id = ev.id;
        let time_str = format_clock_hms(ev.ts);
        let mut row = div().flex().flex_col().gap(zpx(4.)).py(zpx(2.));
        let mut line = div().flex().items_center().gap(zpx(6.)).min_w_0();
        line = line.child(
            div()
                .flex_none()
                .text_color(ui.muted_foreground.opacity(0.75))
                .child(time_str),
        );

        match &ev.kind {
            BrowserEventKind::Command {
                action,
                summary,
                sensitive_text,
                ..
            } => {
                line = line.child(
                    div()
                        .flex_none()
                        .px(zpx(5.))
                        .rounded(radius::SM)
                        .bg(ui.muted)
                        .text_color(ui.foreground)
                        .child(action.clone()),
                );
                if let Some(s) = summary {
                    line = line.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.muted_foreground)
                            .child(s.clone()),
                    );
                }
                if let Some(secret) = sensitive_text {
                    let revealed = self.revealed_events.contains(&ev_id);
                    let display = if revealed { secret.clone() } else { "••••••".to_string() };
                    line = line.child(
                        div()
                            .id(SharedString::from(format!(
                                "browser-secret-{}-{ev_id}",
                                self.view_key
                            )))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(zpx(3.))
                            .px(zpx(5.))
                            .rounded(radius::SM)
                            .bg(ui.muted)
                            .cursor_pointer()
                            .text_color(ui.foreground)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if !this.revealed_events.remove(&ev_id) {
                                    this.revealed_events.insert(ev_id);
                                }
                                cx.notify();
                            }))
                            .child(
                                icon(if revealed { IconName::EyeOff } else { IconName::Eye })
                                    .size(zpx(11.))
                                    .text_color(ui.muted_foreground),
                            )
                            .child(display),
                    );
                }
            }
            BrowserEventKind::Result {
                action,
                ok,
                duration_ms,
                error,
                ..
            } => {
                line = line.child(
                    icon(if *ok { IconName::CircleCheck } else { IconName::CircleX })
                        .size(zpx(12.))
                        .text_color(if *ok { ui.success } else { ui.destructive })
                        .flex_none(),
                );
                let mut desc = action.clone().unwrap_or_else(|| t!("browser.resultLabel").to_string());
                if let Some(ms) = duration_ms {
                    desc.push_str(&format!(" · {ms}ms"));
                }
                if let Some(err) = error {
                    desc.push_str(&format!(" · {err}"));
                }
                line = line.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if *ok { ui.muted_foreground } else { ui.destructive })
                        .child(desc),
                );
                if let Some(tid) = &ev.thumb_id {
                    let tid_click = tid.clone();
                    let active = self.expanded_thumb == Some(ev_id);
                    line = line.child(
                        div()
                            .id(SharedString::from(format!(
                                "browser-thumb-btn-{}-{ev_id}",
                                self.view_key
                            )))
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap(zpx(3.))
                            .px(zpx(5.))
                            .rounded(radius::SM)
                            .bg(if active { ui.tint } else { ui.muted })
                            .text_color(if active { ui.tint_foreground } else { ui.muted_foreground })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_thumb(ev_id, tid_click.clone(), cx);
                            }))
                            .child(icon(IconName::FileImage).size(zpx(11.)))
                            .child(t!("browser.thumb").to_string()),
                    );
                }
            }
            BrowserEventKind::Console { level, text } => {
                let is_err = level.eq_ignore_ascii_case("error");
                let color = if is_err { ui.destructive } else { ui.warning };
                line = line
                    .child(
                        div()
                            .flex_none()
                            .text_color(color)
                            .child(format!("console.{level}")),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(color).child(text.clone()));
            }
            BrowserEventKind::PageError { message } => {
                line = line
                    .child(icon(IconName::CircleAlert).size(zpx(12.)).text_color(ui.destructive).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.destructive)
                            .child(message.clone()),
                    );
            }
            BrowserEventKind::Url { url, .. } => {
                line = line
                    .child(icon(IconName::Globe).size(zpx(11.)).text_color(ui.muted_foreground).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(ui.muted_foreground)
                            .child(url.clone()),
                    );
            }
            BrowserEventKind::Control { holder, device } => {
                let is_human = *holder == BrowserControlHolder::Human;
                let text = match (is_human, device.as_deref()) {
                    (true, Some(dev)) if !dev.is_empty() => t!("browser.logTakeoverDevice", device = dev).to_string(),
                    (true, _) => t!("browser.logTakeover").to_string(),
                    (false, _) => t!("browser.logRelease").to_string(),
                };
                let color = if is_human { ui.tint_strong } else { ui.muted_foreground };
                line = line
                    .child(
                        icon(if is_human { IconName::Hand } else { IconName::Bot })
                            .size(zpx(12.))
                            .text_color(color)
                            .flex_none(),
                    )
                    .child(div().flex_1().min_w_0().truncate().text_color(color).child(text));
            }
            BrowserEventKind::Unknown => {}
        }

        row = row.child(line);

        if self.expanded_thumb == Some(ev_id) {
            let preview = match self.thumbs.get(&ev_id) {
                Some(ThumbState::Ready(image)) => div()
                    .pl(zpx(56.))
                    .child(
                        img(ImageSource::Render(image.clone()))
                            .max_w(zpx(280.))
                            .max_h(zpx(160.))
                            .rounded(radius::SM)
                            .border_1()
                            .border_color(ui.border),
                    )
                    .into_any_element(),
                Some(ThumbState::Failed) => div()
                    .pl(zpx(56.))
                    .text_color(ui.muted_foreground)
                    .child(t!("browser.thumbFailed").to_string())
                    .into_any_element(),
                _ => div()
                    .pl(zpx(56.))
                    .text_color(ui.muted_foreground)
                    .child(t!("browser.loading").to_string())
                    .into_any_element(),
            };
            row = row.child(preview);
        }

        row
    }
}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.app)
            .child(self.render_top_bar(&ui, cx))
            .child(self.render_viewport(&ui, cx))
            .child(self.render_log_drawer(&ui, cx))
    }
}

/// 挂在浏览器视口上的透明辅助元素：
/// 1) 在 prepaint 时算出画面按 `contain` 居中后的精确矩形（供鼠标归一化坐标用）；
/// 2) 挂上 GPUI 的 `InputHandler`，接管普通文本和中文输入法上屏。
struct BrowserInputElement {
    view: Entity<BrowserView>,
    focus: FocusHandle,
    image_bounds: Rc<Cell<Bounds<Pixels>>>,
    aspect: Option<(f32, f32)>,
}

impl IntoElement for BrowserInputElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for BrowserInputElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.position = gpui_kit::Position::Absolute;
        style.inset = gpui_kit::Edges::all(px(0.).into());
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
        let rect = match self.aspect {
            Some((nat_w, nat_h)) if nat_w > 0. && nat_h > 0. => contain_bounds(bounds, nat_w, nat_h),
            _ => bounds,
        };
        self.image_bounds.set(rect);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui_kit::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.view.read(cx).is_human_control() {
            window.handle_input(
                &self.focus,
                BrowserInputHandler {
                    view: self.view.clone(),
                    bounds,
                },
                cx,
            );
        }
    }
}

struct BrowserInputHandler {
    view: Entity<BrowserView>,
    bounds: Bounds<Pixels>,
}

impl InputHandler for BrowserInputHandler {
    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut App) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&mut self, _: &mut Window, cx: &mut App) -> Option<std::ops::Range<usize>> {
        self.view.read(cx).marked_text_range()
    }

    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<String> {
        None
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view.update(cx, |view, cx| {
            view.clear_marked_text(cx);
            view.commit_text(text);
        });
        window.invalidate_character_coordinates();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        new_text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.set_marked_text(new_text.to_string(), cx));
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut App) {
        self.view.update(cx, |view, cx| view.clear_marked_text(cx));
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Window,
        _: &mut App,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds {
            origin: self.bounds.origin,
            size: size(px(1.), px(16.)),
        })
    }

    fn apple_press_and_hold_enabled(&mut self) -> bool {
        false
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut App) -> Option<usize> {
        None
    }
}

fn contain_bounds(container: Bounds<Pixels>, nat_w: f32, nat_h: f32) -> Bounds<Pixels> {
    let cw = f32::from(container.size.width);
    let ch = f32::from(container.size.height);
    if cw <= 0. || ch <= 0. || nat_w <= 0. || nat_h <= 0. {
        return container;
    }
    let scale = (cw / nat_w).min(ch / nat_h);
    let rw = nat_w * scale;
    let rh = nat_h * scale;
    Bounds {
        origin: gpui_kit::point(
            container.origin.x + px((cw - rw) * 0.5),
            container.origin.y + px((ch - rh) * 0.5),
        ),
        size: size(px(rw), px(rh)),
    }
}

fn map_mouse_button(button: MouseButton) -> PointerButton {
    match button {
        MouseButton::Left => PointerButton::Left,
        MouseButton::Right => PointerButton::Right,
        MouseButton::Middle => PointerButton::Middle,
        _ => PointerButton::None,
    }
}

/// 修饰键位掩码（ADR 0029 决定六）：1 Alt、2 Ctrl、4 Meta、8 Shift。
fn encode_modifiers(m: &Modifiers) -> u8 {
    let mut bits = 0u8;
    if m.alt {
        bits |= 1;
    }
    if m.control {
        bits |= 2;
    }
    if m.platform {
        bits |= 4;
    }
    if m.shift {
        bits |= 8;
    }
    bits
}

fn is_printable_key(key: &str) -> bool {
    key.chars().count() == 1 || key == "space"
}

fn map_dom_key(ks: &Keystroke) -> (String, String) {
    match ks.key.as_str() {
        "enter" => ("Enter".into(), "Enter".into()),
        "backspace" => ("Backspace".into(), "Backspace".into()),
        "tab" => ("Tab".into(), "Tab".into()),
        "escape" => ("Escape".into(), "Escape".into()),
        "space" => (" ".into(), "Space".into()),
        "up" => ("ArrowUp".into(), "ArrowUp".into()),
        "down" => ("ArrowDown".into(), "ArrowDown".into()),
        "left" => ("ArrowLeft".into(), "ArrowLeft".into()),
        "right" => ("ArrowRight".into(), "ArrowRight".into()),
        "delete" => ("Delete".into(), "Delete".into()),
        "home" => ("Home".into(), "Home".into()),
        "end" => ("End".into(), "End".into()),
        "pageup" => ("PageUp".into(), "PageUp".into()),
        "pagedown" => ("PageDown".into(), "PageDown".into()),
        k if k.len() >= 2 && k.starts_with('f') && k[1..].bytes().all(|b| b.is_ascii_digit()) => {
            let f = k.to_ascii_uppercase();
            (f.clone(), f)
        }
        k if k.len() == 1 => {
            let ch = k.chars().next().unwrap();
            let key = ks
                .key_char
                .clone()
                .unwrap_or_else(|| if ks.modifiers.shift { ch.to_ascii_uppercase().to_string() } else { k.to_string() });
            let code = if ch.is_ascii_alphabetic() {
                format!("Key{}", ch.to_ascii_uppercase())
            } else if ch.is_ascii_digit() {
                format!("Digit{ch}")
            } else {
                key.clone()
            };
            (key, code)
        }
        other => (other.to_string(), other.to_string()),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn format_clock_hms(ts_ms: i64) -> String {
    let sec = ts_ms / 1000;
    let local_sec = sec + crate::local_time::utc_offset_at(sec);
    let secs = local_sec.rem_euclid(86_400);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

fn local_device_label() -> String {
    if let Ok(name) = std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")) {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    match std::env::consts::OS {
        "macos" => "macOS".into(),
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        other => other.into(),
    }
}
