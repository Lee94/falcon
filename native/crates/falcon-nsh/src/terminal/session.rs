//! 会话终端状态与 `/ws/sessions/:id` 传输绑定。
//!
//! 严格落实 Falcon 会话链路的三条时序铁律：
//! 1. **外观先于尺寸（ADR 0015）**：`connect` 与换主题时先 `set_appearance` 再 `resize`，避免懒惰接回那一刻
//!    程序查询 OSC 10/11 拿到错底色；未测出合法尺寸（`cols < 2 || rows < 1`）前绝不发占位 `resize`；
//! 2. **持久会话禁发 `PtyWrite`（ADR 0020 决定五）**：持久会话的 DA/DSR 由宿主机 VT 进程唯一代答，客户端收到
//!    `TermEvent::PtyWrite` 一律丢弃，仅非持久会话回写；
//! 3. **多客户端生效尺寸同步建网（ADR 0022）**：网络线程收到 `SessionEvent::Size { cols, rows, owner: false }`
//!    时当场同步 `core.resize`，保证紧随其后的 `Replay` / `Output` 在正确列宽上解析。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::{FalconClient, SessionEvent, SessionSink, SessionSocket, SocketOptions};
#[cfg(not(target_arch = "wasm32"))]
use falcon_proto::TermAppearance;
use falcon_proto::{DeadReason, ProgramStatusRecord, SessionState};
use falcon_term::{
    DeltaMode, MouseMode, MouseReportEvent, MouseReporter, Snapshot, TermCore, TermEvent,
    TermOptions, TermSize, WheelAccumulator, WheelRoute, arrow_for_wheel, route_wheel,
};
use falcon_theme::ResolvedTheme;
use parking_lot::Mutex;

/// 会话非定尺寸生效网格记录（ADR 0022）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EffectiveGrid {
    pub cols: u16,
    pub rows: u16,
    pub owner: bool,
}

impl Default for EffectiveGrid {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            owner: true,
        }
    }
}

/// 单条会话连接在客户端侧持有的实时元数据。
#[derive(Clone, Debug)]
pub struct SessionRuntimeMeta {
    pub state: SessionState,
    pub dead_reason: Option<DeadReason>,
    pub title: Option<String>,
    pub program_status: Vec<ProgramStatusRecord>,
    pub reconnecting_attempt: Option<u32>,
    pub last_error: Option<String>,
    pub askpass: Option<(String, String)>,
    pub clipboard_pending: Option<String>,
}

impl Default for SessionRuntimeMeta {
    fn default() -> Self {
        Self {
            state: SessionState::Active,
            dead_reason: None,
            title: None,
            program_status: Vec::new(),
            reconnecting_attempt: None,
            last_error: None,
            askpass: None,
            clipboard_pending: None,
        }
    }
}

/// 一个终端会话的核心模型（线程安全，可同时被网络任务与多个组件闭包共享）。
pub struct TerminalSession {
    session_id: String,
    durable: AtomicBool,
    core: Arc<TermCore>,
    #[cfg(not(target_arch = "wasm32"))]
    socket: Mutex<Option<SessionSocket>>,
    effective_grid: Mutex<EffectiveGrid>,
    measured_grid: Mutex<Option<(u16, u16)>>,
    meta: Mutex<SessionRuntimeMeta>,
    mouse: Mutex<MouseReporter>,
    wheel: Mutex<WheelAccumulator>,
    frame_seq: AtomicU64,
}

impl TerminalSession {
    /// 创建离线/演示/压测终端实例（不发网络请求，可直接 `feed_output` / `feed_replay`）。
    pub fn new_offline(session_id: impl Into<String>, cols: u16, rows: u16, durable: bool) -> Arc<Self> {
        let size = TermSize::new(cols.max(2) as usize, rows.max(1) as usize);
        Arc::new(Self {
            session_id: session_id.into(),
            durable: AtomicBool::new(durable),
            core: Arc::new(TermCore::new(size, TermOptions::default())),
            #[cfg(not(target_arch = "wasm32"))]
            socket: Mutex::new(None),
            effective_grid: Mutex::new(EffectiveGrid {
                cols: size.cols as u16,
                rows: size.rows as u16,
                owner: true,
            }),
            measured_grid: Mutex::new(None),
            meta: Mutex::new(SessionRuntimeMeta::default()),
            mouse: Mutex::new(MouseReporter::new()),
            wheel: Mutex::new(WheelAccumulator::new()),
            frame_seq: AtomicU64::new(1),
        })
    }

    /// 绑定在线 `FalconClient`，按 ADR 0015 先置入 `appearance` 再建连。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn connect(
        self: &Arc<Self>,
        client: &FalconClient,
        resolved: &ResolvedTheme,
        terminal_backed_agent_view: bool,
    ) {
        let mut opts = SocketOptions::default();
        opts.terminal_backed_agent_view = terminal_backed_agent_view;
        let sink: Arc<dyn SessionSink> = self.clone();
        let sock = SessionSocket::open_with(client, &self.session_id, sink, opts);

        // ADR 0015：appearance 严格先于 resize 入队
        let app = if resolved.is_dark {
            TermAppearance::Dark
        } else {
            TermAppearance::Light
        };
        let bg = resolved.terminal.background.to_hex();
        let fg = resolved.terminal.foreground.to_hex();
        sock.set_appearance(app, Some(&bg), Some(&fg));

        if let Some((cols, rows)) = *self.measured_grid.lock() {
            if cols >= 2 && rows >= 1 {
                sock.resize(cols, rows);
            }
        }
        *self.socket.lock() = Some(sock);
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn is_durable(&self) -> bool {
        self.durable.load(Ordering::Relaxed)
    }

    pub fn set_durable(&self, durable: bool) {
        self.durable.store(durable, Ordering::Relaxed);
    }

    pub fn core(&self) -> &Arc<TermCore> {
        &self.core
    }

    pub fn snapshot(&self) -> Snapshot {
        self.core.snapshot()
    }

    pub fn frame_seq(&self) -> u64 {
        self.frame_seq.load(Ordering::Acquire)
    }

    pub fn effective_grid(&self) -> EffectiveGrid {
        *self.effective_grid.lock()
    }

    pub fn meta(&self) -> SessionRuntimeMeta {
        self.meta.lock().clone()
    }

    pub fn take_clipboard_pending(&self) -> Option<String> {
        self.meta.lock().clipboard_pending.take()
    }

    /// 注入一段终端实时字节流（网络线程与本地压测共用）。
    pub fn feed_output(&self, bytes: &[u8]) {
        self.core.advance(bytes);
        self.drain_term_events();
        self.bump_frame();
    }

    /// 注入整份回放快照（离锁换 Term 并保留本地历史阅读偏移）。
    pub fn feed_replay(&self, bytes: &[u8]) {
        self.core.replace_with_replay(bytes);
        let _ = self.core.take_events();
        self.bump_frame();
    }

    /// 同步当前主题深浅线索给服务端（ADR 0015）。
    pub fn sync_appearance(&self, resolved: &ResolvedTheme) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(sock) = self.socket.lock().as_ref() {
            let app = if resolved.is_dark {
                TermAppearance::Dark
            } else {
                TermAppearance::Light
            };
            let bg = resolved.terminal.background.to_hex();
            let fg = resolved.terminal.foreground.to_hex();
            sock.set_appearance(app, Some(&bg), Some(&fg));
        }
        #[cfg(target_arch = "wasm32")]
        let _ = resolved;
    }

    /// 视图量出真实像素并折算成 `(cols, rows)` 后调用：非法极小值直接忽略，绝不把 0×0 或 1×0 发给 PTY。
    pub fn report_viewport_grid(&self, cols: usize, rows: usize) {
        if cols < 2 || rows < 1 {
            return;
        }
        let cols_u16 = cols.min(u16::MAX as usize) as u16;
        let rows_u16 = rows.min(u16::MAX as usize) as u16;
        *self.measured_grid.lock() = Some((cols_u16, rows_u16));

        let is_owner = self.effective_grid.lock().owner;
        if is_owner {
            if self.core.resize(TermSize::new(cols, rows)) {
                let mut eff = self.effective_grid.lock();
                eff.cols = cols_u16;
                eff.rows = rows_u16;
                self.bump_frame();
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(sock) = self.socket.lock().as_ref() {
            sock.resize(cols_u16, rows_u16);
        }
    }

    /// 报告该终端窗口是否处于当前可见画布上（ADR 0022）。
    pub fn set_visible(&self, visible: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(sock) = self.socket.lock().as_ref() {
            sock.set_visible(visible);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = visible;
    }

    /// 键盘 / IME 上屏 / 粘贴向会话输入字符串。
    pub fn send_input(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(sock) = self.socket.lock().as_ref() {
            sock.send_input(text);
            return;
        }
        // 离线演示与纯静态预览模式下回显至本地网格，便于无服务端交互验证与 TUI 直通回归测试
        let echoed = text.replace('\r', "\r\n");
        self.feed_output(echoed.as_bytes());
    }

    /// 粘贴文本：若当前终端开启了 Bracketed Paste（`?2004h`），自动包裹转义序列并清洗嵌套结束符。
    pub fn send_paste(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bracketed = self
            .core
            .mode()
            .contains(falcon_term::alacritty_terminal::term::TermMode::BRACKETED_PASTE);
        let payload = falcon_term::paste::encode_paste(text, bracketed);
        self.send_input(&payload);
    }

    /// 鼠标事件路由：若程序开启了鼠标上报则编码 SGR/X10 报文发往会话。
    pub fn handle_mouse(&self, ev: MouseReportEvent) -> bool {
        let mode: MouseMode = self.core.mouse_mode();
        if mode.protocol == 0 || ev.shift {
            return false;
        }
        let size = self.core.size();
        if let Some(seq) = self.mouse.lock().report(ev, mode, (size.cols, size.rows)) {
            self.send_input(&seq);
            return true;
        }
        false
    }

    /// 滚轮事件路由：鼠标上报 → 备用屏方向键 → 本地历史滚动。
    pub fn handle_wheel(
        &self,
        delta_mode: DeltaMode,
        delta_y: f64,
        cell_height: f64,
        col: i32,
        row: i32,
        shift: bool,
    ) {
        let term_mode = self.core.mode();
        let mouse_mode = self.core.mouse_mode();
        let size = self.core.size();
        match route_wheel(term_mode, mouse_mode.protocol, shift) {
            WheelRoute::Report => {
                let step = self
                    .wheel
                    .lock()
                    .click(delta_mode, delta_y, cell_height, size.rows);
                if step != 0 {
                    // SGR 滚轮键码：64 向上（朝历史），65 向下
                    let btn = if step > 0 { 64 } else { 65 };
                    let seq = format!("\x1b[<{btn};{};{}M", col.max(0) + 1, row.max(0) + 1);
                    self.send_input(&seq);
                }
            }
            WheelRoute::Arrows { app_cursor } => {
                let step = self
                    .wheel
                    .lock()
                    .click(delta_mode, delta_y, cell_height, size.rows);
                if step != 0 {
                    self.send_input(arrow_for_wheel(step > 0, app_cursor));
                }
            }
            WheelRoute::Scrollback => {
                let lines = self
                    .wheel
                    .lock()
                    .push(delta_mode, delta_y, cell_height, size.rows);
                if lines != 0 {
                    self.scroll_lines(lines);
                }
            }
        }
    }

    pub fn scroll_lines(&self, delta_lines: i32) {
        self.core.scroll_display(delta_lines);
        self.bump_frame();
    }

    /// 关闭 WebSocket 仅代表 **Detach**（收起窗口），后台会话继续运行（CONTEXT.md）。
    pub fn detach_socket(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            *self.socket.lock() = None;
        }
    }

    fn drain_term_events(&self) {
        let durable = self.is_durable();
        for ev in self.core.take_events() {
            match ev {
                TermEvent::PtyWrite(reply) => {
                    // ADR 0020 决定五：持久会话绝不从客户端重复回答终端查询
                    #[cfg(not(target_arch = "wasm32"))]
                    if !durable {
                        if let Some(sock) = self.socket.lock().as_ref() {
                            sock.send_input(reply);
                        }
                    }
                    #[cfg(target_arch = "wasm32")]
                    let _ = (durable, reply);
                }
                TermEvent::ClipboardStore(text) => {
                    self.meta.lock().clipboard_pending = Some(text);
                }
                TermEvent::Title(title) => {
                    let mut meta = self.meta.lock();
                    if meta.title.is_none() {
                        meta.title = title;
                    }
                }
                TermEvent::Bell => {}
            }
        }
    }

    fn bump_frame(&self) {
        self.frame_seq.fetch_add(1, Ordering::Release);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl SessionSink for TerminalSession {
    fn on_event(&self, ev: SessionEvent) {
        match ev {
            SessionEvent::Connected { .. } => {
                let mut meta = self.meta.lock();
                meta.reconnecting_attempt = None;
                meta.last_error = None;
                drop(meta);
                self.bump_frame();
            }
            SessionEvent::Output(bytes) => {
                self.feed_output(&bytes);
            }
            SessionEvent::Replay(bytes) => {
                self.feed_replay(&bytes);
            }
            SessionEvent::Size { cols, rows, owner } => {
                // ADR 0022：在网络线程上按顺序立即调整网格，确保紧随其后的 Output/Replay 不错列
                let c = cols.max(2) as usize;
                let r = rows.max(1) as usize;
                {
                    let mut eff = self.effective_grid.lock();
                    eff.cols = c as u16;
                    eff.rows = r as u16;
                    eff.owner = owner;
                }
                if !owner {
                    self.core.resize(TermSize::new(c, r));
                } else if let Some((mc, mr)) = *self.measured_grid.lock() {
                    self.core.resize(TermSize::new(mc as usize, mr as usize));
                }
                self.bump_frame();
            }
            SessionEvent::State { state, dead_reason } => {
                let mut meta = self.meta.lock();
                meta.state = state;
                meta.dead_reason = dead_reason;
                drop(meta);
                self.bump_frame();
            }
            SessionEvent::Reconnecting { attempt } => {
                self.meta.lock().reconnecting_attempt = Some(attempt);
                self.bump_frame();
            }
            SessionEvent::Error(msg) => {
                self.meta.lock().last_error = Some(msg);
                self.bump_frame();
            }
            SessionEvent::Title(title) => {
                self.meta.lock().title = title;
                self.bump_frame();
            }
            SessionEvent::Askpass { id, prompt } => {
                self.meta.lock().askpass = Some((id, prompt));
                self.bump_frame();
            }
            SessionEvent::ProgramStatus(records) => {
                self.meta.lock().program_status = records;
                self.bump_frame();
            }
            SessionEvent::Disconnected { retry_attempt, .. } => {
                self.meta.lock().reconnecting_attempt = Some(retry_attempt);
                self.bump_frame();
            }
            SessionEvent::AgentSnapshot(_)
            | SessionEvent::AgentEntry(_)
            | SessionEvent::AgentDelta { .. }
            | SessionEvent::AgentMeta { .. }
            | SessionEvent::Unauthorized => {}
        }
    }
}
