//! 一扇终端窗口的交互：键盘 / IME / 鼠标上报 / 本地选区 / 滚轮 / 粘贴 / 拖放 / 光标闪烁，
//! 以及会话连接的状态。画面由 [`super::element::TerminalElement`] 按快照绘制。
//!
//! 数据流（设计见 docs/design/gpui-client.md §3.1）：socket 的回调在网络线程上直接把字节
//! 喂进 [`TermCore`]，脏位由 0 变 1 时才发一个唤醒过来；UI 线程收到唤醒只是 `notify`，
//! 渲染时取一次快照。所以不管输出多密，UI 每帧至多处理一次。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use falcon_client::{FalconClient, SessionEvent, SessionSink, SessionSocket};
use falcon_core::term_scroll::{self, MIN_THUMB_PX, ScrollState};
use falcon_term::alacritty_terminal::index::Side;
use falcon_term::alacritty_terminal::selection::SelectionType;
use falcon_term::alacritty_terminal::term::TermMode;
use falcon_term::keyroute::{KeyRoute, route_key};
use falcon_term::{
    DeltaMode, MouseAction, MouseButton as TermMouseButton, MouseReportEvent, MouseReporter,
    Snapshot, TermCore, TermEvent, TermOptions, TermSize, WheelAccumulator, keys, links, paste,
};
use futures::StreamExt;
use gpui_kit::prelude::FluentBuilder as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ScrollDelta, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement, Styled, Task, Window, canvas, div, px,
};
use web_time::Instant;

use super::element::{HoveredLink, TerminalElement, TerminalGeometry};
use crate::theme::TerminalLook;
use crate::zoom::zpx;

/// 会话在这扇窗口里的连接状态（展示用；会话本身的 active / unverified / dead 以服务端为准）。
#[derive(Clone, Debug, PartialEq)]
pub enum ConnState {
    Connecting,
    Active,
    Unverified,
    Dead(Option<String>),
    Reconnecting(u32),
    Disconnected,
    Error(String),
    Unauthorized,
}

/// 网络线程 → UI 线程的消息
enum UiEvent {
    /// 终端内容变了（脏位由 0 变 1）
    Dirty,
    Control(SessionEvent),
}

/// 挂在 socket 上的回调。在网络线程上顺序执行：终端字节就地解析，其余转给 UI。
struct Sink {
    core: Arc<TermCore>,
    tx: UnboundedSender<UiEvent>,
}

impl SessionSink for Sink {
    fn on_event(&self, ev: SessionEvent) {
        match ev {
            SessionEvent::Output(bytes) => {
                if self.core.advance(&bytes) {
                    let _ = self.tx.unbounded_send(UiEvent::Dirty);
                }
            }
            SessionEvent::Replay(bytes) => {
                // 回放最大 4MB：解析期间让 tokio 把这个 worker 上的其他任务挪走（client 的
                // 回调约定：别在回调里长时间占着网络 worker）。不能丢给别的线程——回放之后
                // 紧跟的实时输出必须在它换上之后才应用，顺序由"回调顺序执行"保证
                log::debug!("replay {} bytes", bytes.len());
                #[cfg(not(target_family = "wasm"))]
                let changed = tokio::task::block_in_place(|| self.core.replace_with_replay(&bytes));
                // 浏览器里只有主线程：就地解析，与 web 现在在主线程解析回放同一个水平（设计文档决定七）
                #[cfg(target_family = "wasm")]
                let changed = self.core.replace_with_replay(&bytes);
                if changed {
                    let _ = self.tx.unbounded_send(UiEvent::Dirty);
                }
            }
            other => {
                log::debug!("session event {other:?}");
                let _ = self.tx.unbounded_send(UiEvent::Control(other));
            }
        }
    }
}

/// 本地选区拖动中
#[derive(Clone, Copy, Debug)]
struct SelectionDrag;

/// `TerminalView::reported_held` 里每个键占的位
fn button_bit(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 4,
        _ => 0,
    }
}

/// xterm.js 的光标闪烁间隔
const BLINK_INTERVAL: Duration = Duration::from_millis(600);

/// 滚过之后滚动条亮多久（web 的 LINGER_MS）
const SCROLL_LINGER: Duration = Duration::from_millis(1200);
/// 滚动条亮着时隔多久再问一次位置：期间输出还在涨，历史长度会变
const SCROLL_REFRESH: Duration = Duration::from_millis(1000);
/// 一串滚轮之后稍等一拍再问位置（zellij 滚完才知道滚到了哪），连发时每拍至多一次
const SCROLL_PROBE_DELAY: Duration = Duration::from_millis(60);
/// 拖滑块时发 seek 的最小间隔。服务端会把在飞期间的请求合并成最后一个，这里只是少发点
const SEEK_INTERVAL: Duration = Duration::from_millis(16);

/// 拖滑块中：滑块先跟手（本地像素），不等服务端回话
#[derive(Clone, Copy, Debug)]
struct ScrollDrag {
    start_y: f32,
    start_top: f32,
    top: f32,
}

/// 滚动条（ADR 0019，web 的 TerminalScrollbar.tsx）。滚动发生在宿主机的 zellij 里，
/// 位置是问服务端才知道的：滚轮、悬停、拖动时问，平时不问。
#[derive(Default)]
struct Scrollbar {
    /// 服务端推来的位置；None = 从没收到过回话（会话不支持），整个不画
    state: Option<ScrollState>,
    /// 轨道在窗口里的位置（画的时候量）
    track: Option<Bounds<Pixels>>,
    /// 刚滚过、还亮着
    lit: bool,
    lit_epoch: usize,
    hover: bool,
    drag: Option<ScrollDrag>,
    /// 松手后到下一次回话之前，滑块停在松手处
    settle_top: Option<f32>,
    probe_pending: bool,
    last_seek: Option<Instant>,
    refresh_epoch: usize,
}

impl Scrollbar {
    fn visible(&self) -> bool {
        self.lit || self.hover || self.drag.is_some()
    }

    /// 当前状态下的轨道高度与滑块几何
    fn geometry(&self) -> Option<(ScrollState, f32, term_scroll::ThumbGeometry)> {
        let state = self.state?;
        let track = f32::from(self.track?.size.height);
        let geo = term_scroll::thumb_geometry(state, track, f32::from(zpx(MIN_THUMB_PX)))?;
        Some((state, track, geo))
    }
}

pub struct TerminalView {
    pub session_id: String,
    client: FalconClient,
    core: Arc<TermCore>,
    socket: SessionSocket,
    focus: FocusHandle,
    geometry: Option<TerminalGeometry>,
    snapshot: Arc<Snapshot>,
    marked_text: Option<String>,
    cursor_on: bool,
    blink_epoch: usize,
    /// 键盘焦点在不在这扇终端上：只有聚焦的终端才闪（xterm 同样），失焦的画空心框、不重画
    focused: bool,
    reporter: MouseReporter,
    wheel: WheelAccumulator,
    dragging: Option<SelectionDrag>,
    /// 在这扇终端里按下、已经报给程序的键（[`button_bit`] 的位）。拖动与松开只报这些键：
    /// 在标题栏 / 侧栏按下再拖过来的，程序没见过那次按下，收到带键的移动就当成在拖选
    /// （zellij 会直接起一段选区），松开就把那段选区复制走
    reported_held: u8,
    hovered_link: Option<HoveredLink>,
    scrollbar: Scrollbar,
    pub conn: ConnState,
    /// WS 断开后第几次自动重连（0 = 没断）
    ws_retry: u32,
    /// 手动接回失败的原因（提示条上显示）
    attach_error: Option<String>,
    /// 服务端推的前台命令（自动标题的第二优先级）
    pub server_title: Option<String>,
    /// 本机 falcon 服务端：拖进来的非图片文件直接粘路径（远端宿主机上没有这些文件）
    local_server: bool,
    _tasks: Vec<Task<()>>,
    /// 焦点进出、外观变化的订阅：随视图一起注销
    _subs: Vec<gpui_kit::Subscription>,
}

/// 终端视图对外的事件
pub enum TerminalViewEvent {
    TitleChanged,
    StateChanged,
    /// WS 推来的会话状态：立刻写进工作区的列表，不等 5s 轮询
    State { state: falcon_proto::SessionState, dead_reason: Option<falcon_proto::DeadReason> },
    /// 断线重连上了：补拉一次会话列表
    Reconnected,
    /// 已丢失提示条上的"新建终端"：在同一个项目里开一个新的
    NewTerminal,
    /// 已丢失提示条上的"清除记录"
    ClearRecord,
    Askpass { id: String, prompt: String },
    Unauthorized,
}

impl gpui_kit::EventEmitter<TerminalViewEvent> for TerminalView {}

impl TerminalView {
    pub fn new(
        session_id: String,
        client: FalconClient,
        local_server: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let look = TerminalLook::global(cx).clone();
        let core = Arc::new(TermCore::new(
            TermSize::new(80, 24),
            TermOptions {
                cursor_shape: look.cursor_shape,
                cursor_blinking: look.cursor_blink,
                ..TermOptions::default()
            },
        ));
        let (tx, mut rx) = unbounded();
        let sink = Arc::new(Sink {
            core: core.clone(),
            tx,
        });
        let socket = SessionSocket::open(&client, &session_id, sink);
        // 服务端的 OscColorGate 要先拿到外观才代答颜色查询；socket 连上后先发它
        set_appearance(&socket, &look.appearance);

        let pump = cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                if this.update(cx, |this, cx| this.on_ui_event(ev, cx)).is_err() {
                    break;
                }
            }
        });

        let focus = cx.focus_handle();
        let focus_in = cx.on_focus_in(&focus, window, |this, _, cx| this.on_focus_change(true, cx));
        let focus_out = cx.on_focus_out(&focus, window, |this, _, _, cx| this.on_focus_change(false, cx));
        // 主题 / 终端偏好变了：换配色、光标样式，并把新外观告诉服务端（它据此代答颜色查询、
        // 给订阅了 ?2031 的程序发亮暗通知）
        let look_sub = cx.observe_global::<TerminalLook>(|this, cx| {
            let look = TerminalLook::global(cx).clone();
            this.core.set_cursor_style(look.cursor_shape, look.cursor_blink);
            set_appearance(&this.socket, &look.appearance);
            cx.notify();
        });

        let snapshot = Arc::new(core.snapshot());
        let mut this = Self {
            session_id,
            client,
            core,
            socket,
            focus,
            geometry: None,
            snapshot,
            marked_text: None,
            cursor_on: true,
            blink_epoch: 0,
            focused: false,
            reporter: MouseReporter::new(),
            wheel: WheelAccumulator::new(),
            dragging: None,
            reported_held: 0,
            hovered_link: None,
            scrollbar: Scrollbar::default(),
            conn: ConnState::Connecting,
            ws_retry: 0,
            attach_error: None,
            server_title: None,
            local_server,
            _tasks: vec![pump],
            _subs: vec![focus_in, focus_out, look_sub],
        };
        this.restart_blink(cx);
        this
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// 系统唤醒 / 网络恢复时由工作区调用，跳过退避立即重连
    pub fn reconnect_now(&self) {
        self.socket.reconnect_now();
    }

    fn on_ui_event(&mut self, ev: UiEvent, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Dirty => {
                for event in self.core.take_events() {
                    match event {
                        TermEvent::PtyWrite(text) => self.socket.send_input(text),
                        TermEvent::ClipboardStore(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                        TermEvent::Title(_) | TermEvent::Bell => {}
                    }
                }
                cx.notify();
            }
            UiEvent::Control(ev) => self.on_control(ev, cx),
        }
    }

    fn on_control(&mut self, ev: SessionEvent, cx: &mut Context<Self>) {
        use falcon_proto::SessionState;
        match ev {
            SessionEvent::Connected { reconnected } => {
                // 新 socket：服务端会整份回放，鼠标报文的去重状态作废
                self.reporter.reset();
                self.ws_retry = 0;
                if matches!(self.conn, ConnState::Disconnected | ConnState::Connecting) {
                    self.set_conn(ConnState::Active, cx);
                }
                if reconnected {
                    // 断开期间错过的状态变化（别处 Terminate / 改名）不会经 WS 补发
                    cx.emit(TerminalViewEvent::Reconnected);
                }
            }
            SessionEvent::Disconnected { retry_attempt, .. } => {
                self.ws_retry = retry_attempt;
                if !matches!(self.conn, ConnState::Dead(_) | ConnState::Unauthorized) {
                    self.set_conn(ConnState::Disconnected, cx);
                }
                cx.notify();
            }
            SessionEvent::Unauthorized => {
                self.set_conn(ConnState::Unauthorized, cx);
                cx.emit(TerminalViewEvent::Unauthorized);
            }
            SessionEvent::State { state, dead_reason } => {
                let conn = match state {
                    SessionState::Unverified => ConnState::Unverified,
                    SessionState::Dead => ConnState::Dead(dead_reason.map(|r| r.as_str().to_string())),
                    _ => ConnState::Active,
                };
                // 接上了就先问一次滚动位置：不支持滚动条的会话不会回话，滚动条也就不出现
                if conn == ConnState::Active {
                    self.socket.scroll(None);
                }
                self.set_conn(conn, cx);
                cx.emit(TerminalViewEvent::State { state, dead_reason });
            }
            SessionEvent::Reconnecting { attempt } => self.set_conn(ConnState::Reconnecting(attempt), cx),
            SessionEvent::Error(message) => self.set_conn(ConnState::Error(message), cx),
            SessionEvent::Title(title) => {
                self.server_title = title;
                cx.emit(TerminalViewEvent::TitleChanged);
                cx.notify();
            }
            SessionEvent::Askpass { id, prompt } => cx.emit(TerminalViewEvent::Askpass { id, prompt }),
            SessionEvent::Scroll { position, length, rows } => {
                self.scrollbar.state = Some(ScrollState { position, length, rows });
                self.scrollbar.settle_top = None;
                cx.notify();
            }
            SessionEvent::Output(_) | SessionEvent::Replay(_) => {}
        }
    }

    fn set_conn(&mut self, conn: ConnState, cx: &mut Context<Self>) {
        if self.conn != conn {
            self.conn = conn;
            cx.emit(TerminalViewEvent::StateChanged);
            cx.notify();
        }
    }

    /// 元素 prepaint 时回报本帧的网格几何：格数变了就改本地终端并通知服务端。
    pub(super) fn set_geometry(&mut self, geo: TerminalGeometry, cx: &mut Context<Self>) {
        let changed = self.geometry.is_none_or(|g| g.cols != geo.cols || g.rows != geo.rows);
        self.geometry = Some(geo);
        if changed && self.core.resize(TermSize::new(geo.cols, geo.rows)) {
            self.socket.resize(geo.cols as u16, geo.rows as u16);
            cx.notify();
        } else if changed {
            // 本地尺寸没变（例如首帧恰好 80×24）也要让 socket 知道已量好的尺寸
            self.socket.resize(geo.cols as u16, geo.rows as u16);
        }
    }

    // ---------- 输入 ----------

    fn send(&self, text: impl Into<String>) {
        let text = text.into();
        log::debug!("send_input {} bytes to {}", text.len(), self.session_id);
        if !text.is_empty() {
            self.socket.send_input(text);
        }
    }

    pub(super) fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        self.core.clear_selection();
        self.core.scroll_to_bottom();
        self.send(text);
        self.bump_blink(cx);
    }

    /// 自动化脚本的输入口（等同于键入 / 输入法上屏）
    #[cfg(feature = "automation")]
    pub fn automation_input(&mut self, text: &str, cx: &mut Context<Self>) {
        self.commit_text(text, cx);
    }

    pub(super) fn set_marked_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.marked_text = Some(text);
        cx.notify();
    }

    pub(super) fn clear_marked_text(&mut self, cx: &mut Context<Self>) {
        if self.marked_text.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn marked_text_range(&self) -> Option<std::ops::Range<usize>> {
        self.marked_text.as_ref().map(|t| 0..t.encode_utf16().count())
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.bump_blink(cx);
        // AltGr 这类"修饰键参与出字"的按键交给文本输入（Zed 同一个判断）
        if event.prefer_character_input && event.keystroke.key_char.is_some() {
            return;
        }
        let ks = to_term_keystroke(&event.keystroke);
        // 全局快捷键已经被 keymap 先吃掉了，走到这里的都不是全局键
        match route_key(&ks, false, self.core.has_selection()) {
            KeyRoute::Copy => {
                self.copy(cx);
                cx.stop_propagation();
                return;
            }
            KeyRoute::Paste => {
                self.paste(cx);
                cx.stop_propagation();
                return;
            }
            KeyRoute::Global | KeyRoute::Terminal => {}
        }
        // macOS 上 ⌘ 组合一律不编码（没有对应字节），留给菜单 / 快捷键
        if ks.modifiers.platform {
            return;
        }
        let mode = self.core.mode();
        if let Some(esc) = keys::to_esc_str(&ks, mode, false) {
            self.core.clear_selection();
            self.core.scroll_to_bottom();
            self.send(esc.into_owned());
            cx.stop_propagation();
        }
        // 编不出来的（普通字符、输入法）交给平台文本输入 → InputHandler::replace_text_in_range
    }

    fn on_focus_change(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        if self.core.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.send(falcon_term::focus_report(focused));
        }
        self.bump_blink(cx);
        cx.notify();
    }

    // ---------- 光标闪烁 ----------

    fn bump_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_on = true;
        self.restart_blink(cx);
        cx.notify();
    }

    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.blink_epoch = self.blink_epoch.wrapping_add(1);
        let epoch = self.blink_epoch;
        let task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let keep = this
                    .update(cx, |this, cx| {
                        if this.blink_epoch != epoch {
                            return false;
                        }
                        // 同步输出块超时的兜底冲刷搭在闪烁节拍上
                        this.core.flush_sync_if_expired();
                        let blink = TerminalLook::global(cx).cursor_blink && this.focused;
                        if blink {
                            this.cursor_on = !this.cursor_on;
                            cx.notify();
                        } else if !this.cursor_on {
                            this.cursor_on = true;
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
        if self._tasks.len() > 1 {
            self._tasks.truncate(1);
        }
        self._tasks.push(task);
    }

    // ---------- 剪贴板 ----------

    pub fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.core.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    /// ⌘V：有文本贴文本（Excel / 网页复制常同时带位图，贴文本才对）；只有图片时上传到宿主机
    /// 再把落盘路径粘进来（CONTEXT.md「Image Paste」）。
    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let mut image = None;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::String(s) => {
                    self.paste_text(s.text(), cx);
                    return;
                }
                ClipboardEntry::Image(img) if image.is_none() => image = Some(img.clone()),
                _ => {}
            }
        }
        if let Some(img) = image {
            let mime = img.format.mime_type().to_string();
            self.upload_image(img.bytes.clone(), mime, cx);
        }
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let bracketed = self.core.mode().contains(TermMode::BRACKETED_PASTE);
        self.core.clear_selection();
        self.core.scroll_to_bottom();
        self.send(paste::encode_paste(text, bracketed));
        self.bump_blink(cx);
    }

    fn upload_image(&mut self, bytes: Vec<u8>, mime: String, cx: &mut Context<Self>) {
        if bytes.len() as u64 > falcon_proto::PASTE_IMAGE_MAX_BYTES as u64 {
            log::warn!("图片超过上限，不上传");
            return;
        }
        let fut = self.client.paste_image(&self.session_id, bytes, &mime);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| match result {
                Ok(path) => this.paste_text(&path, cx),
                Err(err) => log::warn!("图片粘贴失败：{err}"),
            })
            .ok();
        })
        .detach();
    }

    fn on_drop_paths(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let mut plain = Vec::new();
        for path in paths.paths() {
            if let Some(mime) = image_mime(path) {
                match std::fs::read(path) {
                    Ok(bytes) => self.upload_image(bytes, mime.to_string(), cx),
                    Err(err) => log::warn!("读不了拖入的图片 {}：{err}", path.display()),
                }
            } else if self.local_server {
                plain.push(shell_quote(path));
            }
        }
        if !plain.is_empty() {
            let text = plain.join(" ") + " ";
            self.paste_text(&text, cx);
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.core.clear_screen();
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.core.select_all();
        cx.notify();
    }

    // ---------- 鼠标 ----------

    fn mouse_mode(&self) -> falcon_term::MouseMode {
        self.core.mouse_mode()
    }

    /// 程序接管鼠标（且没按 Shift 绕过）时返回 true
    fn reporting(&self, modifiers: &Modifiers) -> bool {
        self.mouse_mode().protocol != 0 && !modifiers.shift
    }

    fn report(&mut self, action: MouseAction, button: TermMouseButton, pos: gpui_kit::Point<gpui_kit::Pixels>, m: &Modifiers) -> bool {
        let Some(geo) = self.geometry else {
            return false;
        };
        let (row, col, _) = geo.cell_at(pos);
        let (x, y) = geo.pixel_at(pos);
        let ev = MouseReportEvent {
            action,
            button,
            col: col as i32,
            row: row as i32,
            x,
            y,
            shift: m.shift,
            alt: m.alt,
            ctrl: m.control,
        };
        if let Some(bytes) = self.reporter.report(ev, self.mouse_mode(), (geo.cols, geo.rows)) {
            self.send(bytes);
        }
        true
    }

    fn mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let button = match e.button {
            MouseButton::Left => TermMouseButton::Left,
            MouseButton::Middle => TermMouseButton::Middle,
            MouseButton::Right => TermMouseButton::Right,
            _ => return,
        };
        if self.reporting(&e.modifiers) {
            self.report(MouseAction::Down, button, e.position, &e.modifiers);
            self.reported_held |= button_bit(e.button);
            cx.stop_propagation();
            return;
        }
        if e.button != MouseButton::Left {
            return;
        }
        // ⌘+点击打开链接
        if e.modifiers.platform
            && let Some(link) = self.link_at(e.position)
        {
            cx.open_url(&link.url);
            return;
        }
        let Some(geo) = self.geometry else {
            return;
        };
        let (row, col, right) = geo.cell_at(e.position);
        let side = if right { Side::Right } else { Side::Left };
        let ty = match e.click_count {
            2 => SelectionType::Semantic,
            n if n >= 3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        self.core.start_selection(ty, row, col, side);
        self.dragging = Some(SelectionDrag);
        cx.notify();
    }

    fn mouse_move(&mut self, e: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // 拖滑块：指针离开那一窄条后照样跟（挂在整扇终端上）
        if self.scrollbar.drag.is_some() {
            self.scrollbar_drag(e, cx);
            return;
        }
        if e.pressed_button.is_none() {
            // 空手移动：之前按着的键肯定都松了（兜住没收到的松开）
            self.reported_held = 0;
        }
        if self.reporting(&e.modifiers) {
            let button = match e.pressed_button {
                // 按下不在这里（拖窗口标题栏、拖分隔条经过这扇终端）：一条都不报
                Some(b) if self.reported_held & button_bit(b) == 0 => return,
                Some(MouseButton::Left) => TermMouseButton::Left,
                Some(MouseButton::Middle) => TermMouseButton::Middle,
                Some(MouseButton::Right) => TermMouseButton::Right,
                _ => TermMouseButton::None,
            };
            self.report(MouseAction::Move, button, e.position, &e.modifiers);
            return;
        }
        if self.dragging.is_some() && e.pressed_button == Some(MouseButton::Left) {
            if let Some(geo) = self.geometry {
                let (row, col, right) = geo.cell_at(e.position);
                self.core.update_selection(row, col, if right { Side::Right } else { Side::Left });
                cx.notify();
            }
            return;
        }
        let hovered = if e.modifiers.platform { self.link_at(e.position) } else { None };
        if hovered != self.hovered_link {
            self.hovered_link = hovered;
            cx.notify();
        }
    }

    /// 松开：落在终端上（on_mouse_up）或终端外（on_mouse_up_out）都走这里。按下时报过的键
    /// 松开也要报，松在外面也一样（位置夹到网格边上）——不然程序以为键一直按着，选区收不了尾；
    /// 没报过按下的（别处按下、拖过来松开的）一律不报
    fn mouse_up(&mut self, e: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if e.button == MouseButton::Left && self.scrollbar.drag.is_some() {
            self.scrollbar_release(cx);
            return;
        }
        let button = match e.button {
            MouseButton::Left => TermMouseButton::Left,
            MouseButton::Middle => TermMouseButton::Middle,
            MouseButton::Right => TermMouseButton::Right,
            _ => return,
        };
        let bit = button_bit(e.button);
        if self.reported_held & bit != 0 {
            self.reported_held &= !bit;
            if self.reporting(&e.modifiers) {
                self.report(MouseAction::Up, button, e.position, &e.modifiers);
                return;
            }
        } else if self.reporting(&e.modifiers) {
            return;
        }
        if self.dragging.take().is_some() {
            // 松开即复制（与 web 一致）；单击没拖出选区时清掉空选区
            if self.core.has_selection() {
                self.copy(cx);
            } else {
                self.core.clear_selection();
            }
            cx.notify();
        }
    }

    fn scroll_wheel(&mut self, e: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(geo) = self.geometry else {
            return;
        };
        // 横向为主的手势留给画布横滚（web 的 WheelAxisLock：终端只接纵向）
        let (dx, dy) = match e.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x, l.y),
        };
        if dx.abs() > dy.abs() {
            return;
        }
        let (mode, delta_y) = match e.delta {
            ScrollDelta::Pixels(p) => (DeltaMode::Pixel, -f32::from(p.y) as f64),
            ScrollDelta::Lines(l) => (DeltaMode::Line, -l.y as f64),
        };
        let cell_h = f32::from(geo.line_height) as f64;
        let term_mode = self.core.mode();
        let mouse = self.mouse_mode();
        if mouse.protocol != 0 && !e.modifiers.shift {
            // 程序接管滚轮：一个事件最多一次点击（zellij 收到每条上报自己再滚 3 行）
            let dir = self.wheel.click(mode, delta_y, cell_h, geo.rows);
            if dir != 0 {
                let (row, col, _) = geo.cell_at(e.position);
                if let Some(bytes) = wheel_report(dir > 0, row, col, &e.modifiers, mouse.encoding) {
                    self.send(bytes);
                    self.poke_scrollbar(cx);
                }
            }
            cx.stop_propagation();
        } else if term_mode.contains(TermMode::ALT_SCREEN) {
            // alt screen 没开鼠标上报：照 xterm 的 alternateScroll 发方向键
            let dir = self.wheel.click(mode, delta_y, cell_h, geo.rows);
            if dir != 0 {
                let app = term_mode.contains(TermMode::APP_CURSOR);
                let seq = match (dir > 0, app) {
                    (true, true) => "\x1bOA",
                    (true, false) => "\x1b[A",
                    (false, true) => "\x1bOB",
                    (false, false) => "\x1b[B",
                };
                self.send(seq);
            }
            cx.stop_propagation();
        } else if self.snapshot.history_size > 0 {
            let lines = self.wheel.push(mode, delta_y, cell_h, geo.rows);
            self.core.scroll_display(lines);
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn link_at(&self, pos: gpui_kit::Point<gpui_kit::Pixels>) -> Option<HoveredLink> {
        let geo = self.geometry?;
        let (row, col, _) = geo.cell_at(pos);
        let snap = &self.snapshot;
        if row >= snap.size.rows || col >= snap.size.cols {
            return None;
        }
        // OSC 8 超链接优先
        if let Some(link) = snap.cell(row, col).hyperlink() {
            let uri = link.uri().to_string();
            let same = |c: usize| snap.cell(row, c).hyperlink().is_some_and(|h| h.uri() == uri);
            let mut start = col;
            while start > 0 && same(start - 1) {
                start -= 1;
            }
            let mut end = col + 1;
            while end < snap.size.cols && same(end) {
                end += 1;
            }
            return Some(HoveredLink { row, start_col: start, end_col: end, url: uri });
        }
        let (text, cols) = snap.row_text(row);
        for (start, end, url) in links::find_urls(&text) {
            let (Some(&s), Some(&e)) = (cols.get(start), cols.get(end.saturating_sub(1))) else {
                continue;
            };
            if col >= s && col <= e {
                return Some(HoveredLink { row, start_col: s, end_col: e + 1, url });
            }
        }
        None
    }
}

// ---------- 滚动条（ADR 0019） ----------

impl TerminalView {
    /// 用户刚滚过：亮一会儿，并在这一串滚轮之后问一次位置
    fn poke_scrollbar(&mut self, cx: &mut Context<Self>) {
        let was_visible = self.scrollbar.visible();
        self.scrollbar.lit = true;
        self.scrollbar.lit_epoch = self.scrollbar.lit_epoch.wrapping_add(1);
        let epoch = self.scrollbar.lit_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SCROLL_LINGER).await;
            this.update(cx, |this, cx| {
                if this.scrollbar.lit_epoch == epoch {
                    this.scrollbar.lit = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        if !self.scrollbar.probe_pending {
            self.scrollbar.probe_pending = true;
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SCROLL_PROBE_DELAY).await;
                this.update(cx, |this, _| {
                    this.scrollbar.probe_pending = false;
                    this.socket.scroll(None);
                })
                .ok();
            })
            .detach();
        }
        if !was_visible {
            self.start_scroll_refresh(cx);
        }
        cx.notify();
    }

    /// 亮着期间隔一阵再问一次；不亮了就停。换一轮时旧的那个自己退出（epoch 对不上）
    fn start_scroll_refresh(&mut self, cx: &mut Context<Self>) {
        self.scrollbar.refresh_epoch = self.scrollbar.refresh_epoch.wrapping_add(1);
        let epoch = self.scrollbar.refresh_epoch;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(SCROLL_REFRESH).await;
                let keep = this
                    .update(cx, |this, _| {
                        if this.scrollbar.refresh_epoch != epoch || !this.scrollbar.visible() {
                            return false;
                        }
                        this.socket.scroll(None);
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }

    fn scrollbar_hover(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        let was_visible = self.scrollbar.visible();
        self.scrollbar.hover = *hovered;
        if *hovered {
            self.socket.scroll(None);
            if !was_visible {
                self.start_scroll_refresh(cx);
            }
        }
        cx.notify();
    }

    /// 按在轨道上：抓住滑块；按在滑块外则让滑块中心跳到按下处，然后照样可以接着拖
    fn scrollbar_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // 别让终端拿去起选区 / 报给程序
        cx.stop_propagation();
        let (Some(track), Some((_, track_h, geo))) = (self.scrollbar.track, self.scrollbar.geometry()) else {
            return;
        };
        let y = f32::from(e.position.y - track.origin.y);
        let grabbed = y >= geo.top && y <= geo.top + geo.height;
        let start_top = if grabbed { geo.top } else { (y - geo.height / 2.0).clamp(0.0, track_h - geo.height) };
        let was_visible = self.scrollbar.visible();
        self.scrollbar.drag = Some(ScrollDrag { start_y: f32::from(e.position.y), start_top, top: start_top });
        if !grabbed {
            self.seek_to(start_top, true);
        }
        if !was_visible {
            self.start_scroll_refresh(cx);
        }
        cx.notify();
    }

    fn scrollbar_drag(&mut self, e: &MouseMoveEvent, cx: &mut Context<Self>) {
        let (Some(drag), Some((_, track_h, geo))) = (self.scrollbar.drag, self.scrollbar.geometry()) else {
            return;
        };
        if e.pressed_button != Some(MouseButton::Left) {
            // 松开没收到（松在窗口外）：当作已经松了
            self.scrollbar_release(cx);
            return;
        }
        let next = (drag.start_top + f32::from(e.position.y) - drag.start_y).clamp(0.0, track_h - geo.height);
        if next != drag.top {
            self.scrollbar.drag = Some(ScrollDrag { top: next, ..drag });
            self.seek_to(next, false);
            cx.notify();
        }
    }

    fn scrollbar_release(&mut self, cx: &mut Context<Self>) {
        if let Some(drag) = self.scrollbar.drag.take() {
            // 节流可能吞掉了最后一下，松手时补发落点
            self.seek_to(drag.top, true);
            self.scrollbar.settle_top = Some(drag.top);
            cx.notify();
        }
    }

    /// 滑块顶边拖到 top 处：换算成行数，让 zellij 滚过去
    fn seek_to(&mut self, top: f32, force: bool) {
        let Some((state, track_h, geo)) = self.scrollbar.geometry() else {
            return;
        };
        let now = Instant::now();
        if !force && self.scrollbar.last_seek.is_some_and(|t| now - t < SEEK_INTERVAL) {
            return;
        }
        self.scrollbar.last_seek = Some(now);
        self.socket.scroll(Some(term_scroll::position_for_thumb_top(state, track_h, geo.height, top)));
    }

    /// 叠在终端右缘的那一窄条。没有可滚的历史（含 vim 这类备用屏程序）时不画，
    /// 不挡终端最右一列的点击
    fn scrollbar_overlay(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let state = self.scrollbar.state?;
        if state.length == 0 {
            return None;
        }
        let ui = crate::theme::Ui::global(cx).clone();
        let view = cx.entity();
        let measure = canvas(
            move |bounds, _, cx| {
                view.update(cx, |this, cx| {
                    if this.scrollbar.track != Some(bounds) {
                        this.scrollbar.track = Some(bounds);
                        cx.notify();
                    }
                })
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let mut strip = div()
            .id("term-scrollbar")
            .absolute()
            .top(zpx(6.))
            .bottom(zpx(6.))
            .right_0()
            .w(zpx(10.))
            .on_hover(cx.listener(Self::scrollbar_hover))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::scrollbar_down))
            .child(measure);
        if let Some((_, _, geo)) = self.scrollbar.geometry() {
            let sb = &self.scrollbar;
            let top = sb.drag.map(|d| d.top).or(sb.settle_top).unwrap_or(geo.top);
            let strong = sb.hover || sb.drag.is_some();
            strip = strip.child(
                div()
                    .absolute()
                    .right(zpx(2.))
                    .w(zpx(6.))
                    .top(px(top))
                    .h(px(geo.height))
                    .rounded_full()
                    .bg(ui.foreground.opacity(if strong { 0.45 } else { 0.25 }))
                    .when(!sb.visible(), |d| d.invisible()),
            );
        }
        Some(strip.into_any_element())
    }
}

impl TerminalView {
    /// 提示条上的"接回"：手动接回失败的原因留在提示条上
    fn manual_reattach(&mut self, cx: &mut Context<Self>) {
        self.attach_error = None;
        let fut = self.client.reattach_session(&self.session_id);
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(row) if row.state == falcon_proto::SessionState::Active => {
                        this.set_conn(ConnState::Active, cx);
                        cx.emit(TerminalViewEvent::State { state: row.state, dead_reason: row.dead_reason });
                    }
                    Ok(row) => {
                        this.attach_error = Some(rust_i18n::t!("session.attachFailed").to_string());
                        cx.emit(TerminalViewEvent::State { state: row.state, dead_reason: row.dead_reason });
                    }
                    Err(err) => this.attach_error = Some(err.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 窗口顶上的持续性提示：只用于**需要用户动作**的会话级异常（web 的 Banner）
    fn banner(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        use gpui_kit::component::button::{Button, ButtonVariants};
        use gpui_kit::component::Sizable;
        use rust_i18n::t;
        use crate::ui::Mark;
        let ui = crate::theme::Ui::global(cx).clone();
        enum Tone {
            Warn,
            Error,
        }
        let (tone, mark, title, body, actions): (Tone, Mark, String, Option<String>, Vec<gpui_kit::AnyElement>) = match &self.conn {
            ConnState::Disconnected => (
                Tone::Error,
                Mark::Dead,
                if self.ws_retry > 0 {
                    t!("session.connReconnecting", attempt = self.ws_retry).to_string()
                } else {
                    t!("session.connClosed").to_string()
                },
                Some(t!("session.connClosedBody").to_string()),
                vec![Button::new("banner-retry")
                    .outline()
                    .small()
                    .label(t!("session.retryNow").to_string())
                    .on_click(cx.listener(|this, _, _, _| this.reconnect_now()))
                    .into_any_element()],
            ),
            ConnState::Dead(reason) => {
                let reason = reason
                    .as_ref()
                    .map(|r| t!(format!("session.deadReason_{}", r.replace('-', "_"))).to_string())
                    .unwrap_or_default();
                (
                    Tone::Error,
                    Mark::Dead,
                    t!("session.deadTitle").to_string(),
                    Some(t!("session.deadBody", reason = reason).to_string()),
                    vec![
                        Button::new("banner-new")
                            .outline()
                            .small()
                            .label(t!("session.deadActionNew").to_string())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalViewEvent::NewTerminal)))
                            .into_any_element(),
                        Button::new("banner-clear")
                            .outline()
                            .small()
                            .label(t!("session.clearRecord").to_string())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TerminalViewEvent::ClearRecord)))
                            .into_any_element(),
                    ],
                )
            }
            ConnState::Unverified | ConnState::Reconnecting(_) => {
                let title = match &self.conn {
                    ConnState::Reconnecting(n) if *n > 0 => t!("session.reconnecting", attempt = *n).to_string(),
                    _ => t!("session.unverifiedTitle").to_string(),
                };
                let body = match &self.attach_error {
                    Some(err) => format!("{}: {err}", t!("session.attachFailed")),
                    None => t!("session.unverifiedBody").to_string(),
                };
                (
                    Tone::Warn,
                    Mark::Unverified,
                    title,
                    Some(body),
                    vec![Button::new("banner-reattach")
                        .warning()
                        .small()
                        .label(format!("{} {}", t!("session.reattach"), crate::ui::chord("reattach")))
                        .on_click(cx.listener(|this, _, _, cx| this.manual_reattach(cx)))
                        .into_any_element()],
                )
            }
            ConnState::Error(msg) => (
                Tone::Error,
                Mark::Dead,
                t!("session.attachFailed").to_string(),
                Some(msg.clone()),
                vec![Button::new("banner-retry2")
                    .outline()
                    .small()
                    .label(t!("session.retry").to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.manual_reattach(cx)))
                    .into_any_element()],
            ),
            _ => return None,
        };
        let (bg, border) = match tone {
            Tone::Warn => (ui.warning.opacity(0.1), ui.warning.opacity(0.4)),
            Tone::Error => (ui.destructive.opacity(0.1), ui.destructive.opacity(0.4)),
        };
        let mut bar = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px(zpx(14.))
            .py(zpx(8.))
            .bg(bg)
            .border_b_1()
            .border_color(border)
            .child(crate::ui::status_mark(mark, zpx(14.), cx))
            .child(div().flex_none().text_size(zpx(13.)).child(title));
        if let Some(body) = body {
            bar = bar.child(div().flex_1().min_w_0().truncate().text_xs().text_color(ui.muted_foreground).child(body));
        } else {
            bar = bar.child(div().flex_1());
        }
        Some(bar.child(div().flex_none().flex().gap_2().children(actions)).into_any_element())
    }

    /// 终端里的右键菜单：复制 / 粘贴 / 清屏（web 的 termMenuItems）
    fn context_items(&self, cx: &mut Context<Self>) -> Vec<crate::menus::MenuItemSpec> {
        use rust_i18n::t;
        let view = cx.entity();
        let has_selection = self.core.has_selection();
        let can_paste = self.conn == ConnState::Active;
        let (v1, v2, v3) = (view.clone(), view.clone(), view);
        vec![
            crate::menus::MenuItemSpec::new(t!("term.copy").to_string(), move |_, cx| v1.update(cx, |t, cx| t.copy(cx)))
                .disabled(!has_selection),
            crate::menus::MenuItemSpec::new(t!("term.paste").to_string(), move |_, cx| v2.update(cx, |t, cx| t.paste(cx)))
                .disabled(!can_paste),
            crate::menus::MenuItemSpec::new(t!("term.clear").to_string(), move |_, cx| v3.update(cx, |t, cx| t.clear(cx)))
                .sep(),
        ]
    }
}

fn set_appearance(socket: &SessionSocket, hint: &falcon_proto::OscColorHint) {
    if let Some(appearance) = hint.appearance {
        socket.set_appearance(appearance, hint.background.as_deref(), hint.foreground.as_deref());
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.core.take_dirty() {
            self.snapshot = Arc::new(self.core.snapshot());
        }
        let look = TerminalLook::global(cx).clone();
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let element = TerminalElement::new(
            cx.entity(),
            self.snapshot.clone(),
            look.palette.clone(),
            look.text.clone(),
            self.focus.clone(),
            focused,
            self.cursor_on || !focused,
            self.marked_text.clone(),
            self.hovered_link.clone(),
        );
        let banner = self.banner(cx);
        let scrollbar = self.scrollbar_overlay(cx);
        let items = self.context_items(cx);
        let dim = self.conn != ConnState::Active && self.conn != ConnState::Connecting;
        use gpui_kit::component::menu::ContextMenuExt;
        let body = div()
            .id(SharedString::from(format!("terminal-{}", self.session_id)))
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .bg(look.palette.background)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_scroll_wheel(cx.listener(Self::scroll_wheel))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.on_drop_paths(paths, cx)))
            .on_action(cx.listener(|this, _: &crate::actions::term::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &crate::actions::term::Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &crate::actions::term::SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &crate::actions::term::Clear, _, cx| this.clear(cx)))
            // 输入被禁用这件事要看得见（web 同样压到 0.55）
            .when(dim, |d| d.opacity(0.55))
            .child(element)
            .children(scrollbar)
            .context_menu(move |menu, _, _| crate::menus::to_popup(menu, items.clone()));
        div().size_full().flex().flex_col().children(banner).child(div().flex_1().min_h_0().child(body))
    }
}

fn to_term_keystroke(ks: &gpui_kit::Keystroke) -> falcon_term::Keystroke {
    falcon_term::Keystroke {
        modifiers: falcon_term::Modifiers {
            control: ks.modifiers.control,
            alt: ks.modifiers.alt,
            shift: ks.modifiers.shift,
            platform: ks.modifiers.platform,
            function: ks.modifiers.function,
        },
        key: ks.key.clone(),
        key_char: ks.key_char.clone(),
    }
}

/// 滚轮上报：按键号 64（上）/ 65（下）+ 修饰键位，编码照鼠标按键报文（SGR 或默认单字节）。
fn wheel_report(up: bool, row: usize, col: usize, m: &Modifiers, encoding: u16) -> Option<String> {
    let mut code = if up { 64 } else { 65 };
    if m.shift {
        code |= 4;
    }
    if m.alt {
        code |= 8;
    }
    if m.control {
        code |= 16;
    }
    if encoding == 1006 || encoding == 1016 {
        return Some(format!("\x1b[<{code};{};{}M", col + 1, row + 1));
    }
    let (b, x, y) = (code + 32, col as u32 + 1 + 32, row as u32 + 1 + 32);
    // 与按键报文同一条限制：文本输入通道发不出 0x80 以上的字节
    if b > 0x7f || x > 0x7f || y > 0x7f {
        return None;
    }
    Some(format!("\x1b[M{}{}{}", char::from_u32(b)?, char::from_u32(x)?, char::from_u32(y)?))
}

fn image_mime(path: &std::path::Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "heic" => "image/heic",
        _ => return None,
    })
}

/// 本机路径转成 POSIX shell 里安全的单引号字面量（拖文件进原生终端的同款行为）。
fn shell_quote(path: &PathBuf) -> String {
    let s = path.to_string_lossy();
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%".contains(c)) {
        return s.into_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

