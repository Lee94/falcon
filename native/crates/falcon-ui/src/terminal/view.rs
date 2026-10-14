//! 一扇终端窗口的交互：键盘 / IME / 鼠标上报 / 本地选区 / 滚轮 / 粘贴 / 拖放 / 光标闪烁，
//! 以及会话连接的状态。画面由 [`super::element::TerminalElement`] 按快照绘制。
//!
//! 数据流（设计见 docs/design/gpui-client.md §3.1）：socket 的回调在网络线程上直接把字节
//! 喂进 [`TermCore`]，脏位由 0 变 1 时才发一个唤醒过来；UI 线程收到唤醒只是 `notify`，
//! 渲染时取一次快照。所以不管输出多密，UI 每帧至多处理一次。终端产生的事件（回复、剪贴板）
//! 不跟着脏位走：每段输出解析完都取一次，有就单独送过来（[`UiEvent::Term`]）。
//!
//! 宿主机上不再有 Zellij（ADR 0020）：程序的字节原样透传，滚动、滚动条、鼠标与滚轮的模式
//! 全按本地 alacritty 的状态来；持久会话的终端查询由宿主机的 VT 进程回答，这里只答非持久会话的。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use falcon_client::{FalconClient, SessionEvent, SessionSink, SessionSocket};
use falcon_core::term_scroll::{self, MIN_THUMB_PX, ScrollState};
use falcon_proto::ProgramStatusRecord;
use falcon_term::alacritty_terminal::index::Side;
use falcon_term::alacritty_terminal::selection::SelectionType;
use falcon_term::alacritty_terminal::term::TermMode;
use falcon_term::keyroute::{KeyRoute, route_key};
use falcon_term::links::LinkSpan;
use falcon_term::{
    DeltaMode, MouseAction, MouseButton as TermMouseButton, MouseReportEvent, MouseReporter,
    Snapshot, TermCore, TermEvent, TermOptions, TermSize, WheelAccumulator, WheelRoute, keys,
    paste,
};
use futures::StreamExt;
use gpui_kit::prelude::FluentBuilder as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, Modifiers, ModifiersChangedEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, Task, Window, canvas, div, px,
};

use super::element::{TerminalElement, TerminalGeometry};
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
    /// 终端产生的事件（回复、剪贴板），每段输出解析完就取走送来
    Term(Vec<TermEvent>),
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
                // 不管脏位翻没翻都取：脏位已经是 1（UI 还没画）时产生的回复，以前要等下一段输出
                // 把脏位重新拨起来才被取走，程序就卡在等 DA / DSR 回复上（ADR 0020「客户端」）
                self.drain_events();
            }
            SessionEvent::Replay(bytes) => {
                // 回放最大 4MB：原生上解析期间让 tokio 把这个 worker 上的其他任务挪走（client 的
                // 回调约定：别在回调里长时间占着网络 worker）；浏览器里只有主线程，就地解析，与
                // 旧 React 版在主线程解析回放同一个水平（设计文档决定七）。不能丢给别的线程——回放之后
                // 紧跟的实时输出必须在它换上之后才应用，顺序由"回调顺序执行"保证
                log::debug!("replay {} bytes", bytes.len());
                let changed = falcon_client::block_in_place(|| self.core.replace_with_replay(&bytes));
                if changed {
                    let _ = self.tx.unbounded_send(UiEvent::Dirty);
                }
                // 回放期间静音，照理没有事件；取一次是为了不留例外
                self.drain_events();
            }
            other => {
                log::debug!("session event {other:?}");
                let _ = self.tx.unbounded_send(UiEvent::Control(other));
            }
        }
    }
}

impl Sink {
    fn drain_events(&self) {
        let events = self.core.take_events();
        if !events.is_empty() {
            let _ = self.tx.unbounded_send(UiEvent::Term(events));
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

/// 点开链接要按的修饰键：mac 键位是 ⌘，其余是 Ctrl（Windows Terminal、VS Code 的习惯）。
/// GPUI 的 `platform` 在 Windows 上是 Win 键，不能直接拿来用
fn link_modifier(m: &Modifiers) -> bool {
    if crate::ui::mac_keys() { m.platform } else { m.control }
}

/// xterm.js 的光标闪烁间隔
const BLINK_INTERVAL: Duration = Duration::from_millis(600);

/// 滚过之后滚动条亮多久（React 版的 LINGER_MS）
const SCROLL_LINGER: Duration = Duration::from_millis(1200);

/// 持久性还不知道时（会话列表还没到）最多替程序攒多少条回复。只是兜一下启动那一小会儿，
/// 程序一般问一两条就停下来等了
const PENDING_REPLIES_MAX: usize = 64;

/// 拖滑块中：滑块跟手（本地像素），终端按换算出的行数同步滚过去
#[derive(Clone, Copy, Debug)]
struct ScrollDrag {
    start_y: f32,
    start_top: f32,
    top: f32,
}

/// 滚动条（React 版的 TerminalScrollbar.tsx）。位置一律读本地 Grid（ADR 0020：宿主机上不再有
/// Zellij，字节原样透传，历史就在本地 alacritty 里；ADR 0019 那套问宿主机插件的探测、刷新
/// 定时器与 WS `scroll` 消息全部退役）。每帧从快照推一次，滚轮、拖动都直接改本地终端。
#[derive(Default)]
struct Scrollbar {
    /// 这一帧快照里的位置（render 时写）；还没画过时为 None
    state: Option<ScrollState>,
    /// 轨道在窗口里的位置（画的时候量）
    track: Option<Bounds<Pixels>>,
    /// 刚滚过、还亮着
    lit: bool,
    lit_epoch: usize,
    hover: bool,
    drag: Option<ScrollDrag>,
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
    /// （开着鼠标的 tmux / vim 会直接起一段选区），松开就把那段选区复制走
    reported_held: u8,
    /// 按着链接修饰键（[`link_modifier`]）时指针下的链接：画下划线、换手形指针
    hovered_link: Option<LinkSpan>,
    scrollbar: Scrollbar,
    pub conn: ConnState,
    /// WS 断开后第几次自动重连（0 = 没断）
    ws_retry: u32,
    /// 手动接回失败的原因（提示条上显示）
    attach_error: Option<String>,
    /// 服务端推的前台命令（自动标题的第二优先级）
    pub server_title: Option<String>,
    /// 程序自己设的终端标题（OSC 0 / 2），只给标题栏胶囊的悬停提示当「在干什么」用
    /// （`program_status::activity_text`）。改了不 notify：Claude Code 跑的时候标题前缀每秒换
    /// 一次，不值得为它重画终端，提示弹出时再来取
    osc_title: Option<String>,
    /// 会话持不持久（`Session.durable`），决定终端回复发不发：持久会话的查询由宿主机的 VT 进程
    /// 一处回答，客户端不答（ADR 0020 决定五）。None = 会话列表还没到（启动时恢复的窗口比列表
    /// 先建出来），这期间的回复先攒在 `pending_replies`，知道了再决定发还是丢
    durable: Option<bool>,
    pending_replies: Vec<String>,
    /// WS 推来的程序状态（OSC 7501）全集；只有持久会话有。None = 这条连接上还没推过（没连上、
    /// 断开了、非持久会话），这时工作区以会话列表里的摘要为准
    program_status: Option<Vec<ProgramStatusRecord>>,
    /// 这一份全集里的 done / error 已经发过 status_seen：服务端清完会推新的全集，到那时为止
    /// 别每敲一个键发一次
    seen_sent: bool,
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
    /// 要用户看见的提示（图片粘贴超限 / 上传失败），由工作区转成通知
    Toast { kind: crate::workspace::ToastKind, title: String, body: Option<String> },
    /// WS 推来的程序状态（OSC 7501）全集：工作区据此改列表里的摘要、决定要不要提醒
    ProgramStatus(Vec<ProgramStatusRecord>),
}

impl gpui_kit::EventEmitter<TerminalViewEvent> for TerminalView {}

impl TerminalView {
    /// `durable` 取自会话列表；列表还没到时给 None，之后由工作区 [`Self::set_durable`] 补上
    pub fn new(
        session_id: String,
        client: FalconClient,
        local_server: bool,
        durable: Option<bool>,
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
        // 颜色查询由宿主机的 VT 进程（持久）/ 服务端的 OscColorGate（非持久）按外观回答，
        // 要先拿到外观；socket 连上后先发它
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
        // 主题 / 终端偏好变了：换配色、光标样式，并把新外观告诉服务端（据此回答颜色查询、
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
            osc_title: None,
            durable,
            pending_replies: Vec::new(),
            program_status: None,
            seen_sent: false,
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

    /// 键盘焦点在不在这扇终端上（工作区据此判断 blocked 提醒该不该弹：人就在这扇窗口里时不弹）
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// 这条 WS 上推来的程序状态全集；还没推过（或连接断了）为 None。工作区拿它盖掉轮询来的
    /// 摘要：推送是实时的，轮询的响应可能比推送晚到、带着过时的状态
    pub fn program_records(&self) -> Option<&[ProgramStatusRecord]> {
        self.program_status.as_deref()
    }

    /// 程序设的终端标题（OSC 0 / 2）；没设过或被复位为 None
    pub fn osc_title(&self) -> Option<&str> {
        self.osc_title.as_deref()
    }

    /// 会话列表到了：补上持久性，攒着的回复按它发掉或丢掉。持久性在会话的一生里不变，
    /// 已经知道了就不再改
    pub fn set_durable(&mut self, durable: bool) {
        if self.durable.is_some() {
            return;
        }
        self.durable = Some(durable);
        let pending = std::mem::take(&mut self.pending_replies);
        if !durable {
            for text in pending {
                self.socket.send_input(text);
            }
        }
    }

    /// 终端给程序的回复（DA、DSR、DECRQM……）：只有非持久会话由客户端答。持久会话的查询由
    /// 宿主机的 VT 进程一处回答——没有 Viewer 时也有人答，多个 Viewer 时也只答一遍；客户端再答，
    /// 程序就会多收到一份，可能当成按键显示出来（ADR 0020 决定五）
    fn reply(&mut self, text: String) {
        match self.durable {
            Some(false) => self.socket.send_input(text),
            Some(true) => {}
            None if self.pending_replies.len() < PENDING_REPLIES_MAX => self.pending_replies.push(text),
            None => {}
        }
    }

    fn on_ui_event(&mut self, ev: UiEvent, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Dirty => cx.notify(),
            UiEvent::Term(events) => {
                for event in events {
                    match event {
                        TermEvent::PtyWrite(text) => self.reply(text),
                        TermEvent::ClipboardStore(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                        TermEvent::Title(title) => self.osc_title = title,
                        TermEvent::Bell => {}
                    }
                }
            }
            UiEvent::Control(ev) => self.on_control(ev, cx),
        }
    }

    /// 用户看过了这个会话（窗口获得焦点、在里面敲键 / 点鼠标 / 粘贴）：让宿主机清掉程序状态里的
    /// done / error（ADR 0020 决定六）。没有可清的就不发；发过一次后等服务端推来新的全集再说
    fn mark_seen(&mut self) {
        let finished = self.program_status.as_deref().is_some_and(falcon_core::program_status::has_finished);
        if self.seen_sent || !finished {
            return;
        }
        self.seen_sent = true;
        self.socket.status_seen(Vec::new());
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
                // 断开期间的变化收不到推送，交还给会话列表的轮询；连回来服务端会再推一次全集
                self.program_status = None;
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
            SessionEvent::ProgramStatus(records) => {
                self.program_status = Some(records.clone());
                self.seen_sent = false;
                // 人正在这扇窗口里（重连后服务端补推全集时常见）：done / error 当场算看过
                if self.focused {
                    self.mark_seen();
                }
                cx.emit(TerminalViewEvent::ProgramStatus(records));
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
        self.mark_seen();
        self.bump_blink(cx);
    }

    /// 自动化脚本的输入口（等同于键入 / 输入法上屏）
    #[cfg(feature = "automation")]
    pub fn automation_input(&mut self, text: &str, cx: &mut Context<Self>) {
        self.commit_text(text, cx);
    }

    /// 自动化脚本查窗口坐标处的链接（不打开）。用最新的画面，不等下一帧
    #[cfg(feature = "automation")]
    pub fn automation_link_at(&mut self, pos: gpui_kit::Point<gpui_kit::Pixels>) -> Option<String> {
        if self.core.take_dirty() {
            self.snapshot = Arc::new(self.core.snapshot());
        }
        self.link_at(pos).map(|l| l.url)
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
            self.mark_seen();
            cx.stop_propagation();
        }
        // 编不出来的（普通字符、输入法）交给平台文本输入 → InputHandler::replace_text_in_range
    }

    fn on_focus_change(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        if self.core.mode().contains(TermMode::FOCUS_IN_OUT) {
            self.send(falcon_term::focus_report(focused));
        }
        if focused {
            self.mark_seen();
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
            self.upload_images(vec![(img.bytes.clone(), mime)], cx);
        }
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let bracketed = self.core.mode().contains(TermMode::BRACKETED_PASTE);
        self.core.clear_selection();
        self.core.scroll_to_bottom();
        self.send(paste::encode_paste(text, bracketed));
        self.mark_seen();
        self.bump_blink(cx);
    }

    /// 图片逐张传到会话宿主机，落盘路径按原顺序用空格连起来**一次**粘进终端（旧 React 版
    /// `uploadImages` 的口径；逐张各贴一次的话，拖进两张图两个路径会首尾相接）。含空白的
    /// 路径包上双引号（Windows 用户名带空格很常见，Claude Code 按这个约定剥引号）。超过上限的、
    /// 传失败的各弹一条提示后跳过，其余照贴。
    fn upload_images(&mut self, images: Vec<(Vec<u8>, String)>, cx: &mut Context<Self>) {
        use crate::workspace::ToastKind;
        use rust_i18n::t;
        let max = falcon_proto::PASTE_IMAGE_MAX_BYTES;
        let mut uploads = Vec::new();
        for (bytes, mime) in images {
            if bytes.len() as u64 > max {
                let title = t!("term.pasteImageTooLarge", max = max / 1024 / 1024).to_string();
                cx.emit(TerminalViewEvent::Toast { kind: ToastKind::Warning, title, body: None });
                continue;
            }
            uploads.push(self.client.paste_image(&self.session_id, bytes, &mime));
        }
        if uploads.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let mut paths = Vec::new();
            for upload in uploads {
                match upload.await {
                    Ok(path) => paths.push(falcon_core::paste_image::quote_for_prompt(&path)),
                    Err(err) => {
                        let title = t!("term.pasteImageFailed").to_string();
                        let ev = TerminalViewEvent::Toast { kind: ToastKind::Warning, title, body: Some(err.to_string()) };
                        this.update(cx, |_, cx| cx.emit(ev)).ok();
                    }
                }
            }
            if !paths.is_empty() {
                this.update(cx, |this, cx| this.paste_text(&paths.join(" "), cx)).ok();
            }
        })
        .detach();
    }

    fn on_drop_paths(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let mut plain = Vec::new();
        let mut images = Vec::new();
        for path in paths.paths() {
            if let Some(mime) = image_mime(path) {
                match std::fs::read(path) {
                    Ok(bytes) => images.push((bytes, mime.to_string())),
                    Err(err) => cx.emit(TerminalViewEvent::Toast {
                        kind: crate::workspace::ToastKind::Warning,
                        title: rust_i18n::t!("term.pasteImageFailed").to_string(),
                        body: Some(format!("{}：{err}", path.display())),
                    }),
                }
            } else if self.local_server {
                plain.push(shell_quote(path));
            }
        }
        if !plain.is_empty() {
            let text = plain.join(" ") + " ";
            self.paste_text(&text, cx);
        }
        if !images.is_empty() {
            self.upload_images(images, cx);
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
        self.mark_seen();
        let button = match e.button {
            MouseButton::Left => TermMouseButton::Left,
            MouseButton::Middle => TermMouseButton::Middle,
            MouseButton::Right => TermMouseButton::Right,
            _ => return,
        };
        // ⌘+点击（Windows / Linux 上 Ctrl+点击）打开链接。排在鼠标上报前面：程序接管了鼠标
        // （Claude Code 的全屏界面、vim 的 mouse=a、开着鼠标的 tmux）时照样能开——iTerm2 /
        // Ghostty 也是这样，SGR 上报里本来就没有 ⌘ 这一位。这次按下不报，松开也就不报
        if e.button == MouseButton::Left
            && link_modifier(&e.modifiers)
            && let Some(link) = self.link_at(e.position)
        {
            cx.open_url(&link.url);
            cx.stop_propagation();
            return;
        }
        if self.reporting(&e.modifiers) {
            self.report(MouseAction::Down, button, e.position, &e.modifiers);
            self.reported_held |= button_bit(e.button);
            cx.stop_propagation();
            return;
        }
        if e.button != MouseButton::Left {
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
            // 悬停链接在上报之前算：程序接管鼠标时也要看得出哪里能点
            if self.refresh_hovered_link(e.position, &e.modifiers) {
                cx.notify();
            }
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
    }

    /// 光按下 / 松开修饰键、指针不动：链接也要马上亮起 / 熄灭（只有聚焦的终端收得到这个事件）
    fn modifiers_changed(&mut self, e: &ModifiersChangedEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.refresh_hovered_link(window.mouse_position(), &e.modifiers) {
            cx.notify();
        }
    }

    /// 按指针位置与修饰键重算悬停的链接，变了返回 true
    fn refresh_hovered_link(&mut self, pos: gpui_kit::Point<gpui_kit::Pixels>, modifiers: &Modifiers) -> bool {
        let hovered = if link_modifier(modifiers) { self.link_at(pos) } else { None };
        if hovered == self.hovered_link {
            return false;
        }
        self.hovered_link = hovered;
        true
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
            // 松开即复制（沿用 React 版）；单击没拖出选区时清掉空选区
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
        // 横向为主的手势留给画布横滚（React 版的 WheelAxisLock：终端只接纵向）
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
        let mouse = self.mouse_mode();
        // 归属按 xterm 语义判（falcon-term 的 route_wheel）：鼠标上报 > 备用屏 + ?1007 转方向键 >
        // 本地 scrollback。字节是程序原样输出的，它关掉 ?1007 时备用屏里的滚轮就不再变成方向键
        match falcon_term::route_wheel(self.core.mode(), mouse.protocol, e.modifiers.shift) {
            WheelRoute::Report => {
                // 程序接管滚轮：一个事件最多一次点击（程序收到每条上报自己会滚好几行）
                let dir = self.wheel.click(mode, delta_y, cell_h, geo.rows);
                if dir != 0 {
                    let (row, col, _) = geo.cell_at(e.position);
                    if let Some(bytes) = wheel_report(dir > 0, row, col, &e.modifiers, mouse.encoding) {
                        self.send(bytes);
                    }
                }
                cx.stop_propagation();
            }
            WheelRoute::Arrows { app_cursor } => {
                let dir = self.wheel.click(mode, delta_y, cell_h, geo.rows);
                if dir != 0 {
                    self.send(falcon_term::arrow_for_wheel(dir > 0, app_cursor));
                }
                cx.stop_propagation();
            }
            // 备用屏里没有历史（含程序关掉了 ?1007 的情形）：滚轮放行给画布
            WheelRoute::Scrollback if self.snapshot.history_size > 0 => {
                let lines = self.wheel.push(mode, delta_y, cell_h, geo.rows);
                self.core.scroll_display(lines);
                self.poke_scrollbar(cx);
                cx.stop_propagation();
            }
            WheelRoute::Scrollback => {}
        }
    }

    /// 指针下的链接（网格外——内边距、滚动条那一条——不算）
    fn link_at(&self, pos: gpui_kit::Point<gpui_kit::Pixels>) -> Option<LinkSpan> {
        let geo = self.geometry?;
        if !geo.contains(pos) {
            return None;
        }
        let (row, col, _) = geo.cell_at(pos);
        self.snapshot.link_at(row, col)
    }
}

// ---------- 滚动条（位置读本地 Grid，ADR 0020） ----------

impl TerminalView {
    /// 用户刚滚过：亮一会儿
    fn poke_scrollbar(&mut self, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    fn scrollbar_hover(&mut self, hovered: &bool, _: &mut Window, cx: &mut Context<Self>) {
        self.scrollbar.hover = *hovered;
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
        self.scrollbar.drag = Some(ScrollDrag { start_y: f32::from(e.position.y), start_top, top: start_top });
        if !grabbed {
            self.seek_to(start_top);
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
            self.seek_to(next);
            cx.notify();
        }
    }

    fn scrollbar_release(&mut self, cx: &mut Context<Self>) {
        if self.scrollbar.drag.take().is_some() {
            // 松手后滑块回到按真实位置算出的地方（取整到行，与跟手的像素差不到一行）
            cx.notify();
        }
    }

    /// 滑块顶边拖到 top 处：换算成「视口下方的行数」，本地终端直接滚过去。本地操作，
    /// 不用像 ADR 0019 那样节流、等回话
    fn seek_to(&mut self, top: f32) {
        let Some((state, track_h, geo)) = self.scrollbar.geometry() else {
            return;
        };
        let position = term_scroll::position_for_thumb_top(state, track_h, geo.height, top);
        self.core.scroll_to(position as usize);
    }

    /// 叠在终端右缘的那一窄条。没有可滚的历史（含 vim 这类备用屏程序：备用屏没有历史）时
    /// 不画，不挡终端最右一列的点击
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
            let top = sb.drag.map(|d| d.top).unwrap_or(geo.top);
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

    /// 窗口顶上的持续性提示：只用于**需要用户动作**的会话级异常（React 版的 Banner）
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

    /// 终端里的右键菜单：复制 / 粘贴 / 清屏（React 版的 termMenuItems）
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
            // 链接底下的内容变了（滚动、输出把它顶走）：按现在的画面重认
            if self.hovered_link.is_some() {
                self.refresh_hovered_link(window.mouse_position(), &window.modifiers());
            }
        }
        // 滚动条的位置每帧从快照推：输出在涨、回放换了整个终端、滚轮与拖动都会改它，
        // 一处读就不会漏
        let snap = &self.snapshot;
        self.scrollbar.state = Some(ScrollState::from_grid(snap.display_offset, snap.history_size, snap.size.rows));
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
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
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
            // 输入被禁用这件事要看得见（照 React 版压到 0.55）
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

