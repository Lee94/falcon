//! 会话通道 `/ws/sessions/:id`：照 web 的 `components/TerminalView.tsx` 与服务端
//! `ws.ts` 逐条对齐（设计文档 §3.2）。那些时序都是踩过的坑，改之前先读这两处。
//!
//! 线上是混合协议：终端字节走二进制帧（1 字节类型头 + 载荷，`decode_term_frame`），
//! 控制消息走 JSON 文本帧（`ServerMessage`）；客户端发的一律是 JSON 文本帧。
//!
//! 关掉 socket 只是 **Detach**：会话照跑，别的 Viewer 照看。Terminate 走 REST。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use falcon_proto::{
    ClientMessage, DeadReason, ServerMessage, SessionState, TermAppearance, TermFrameKind,
    WS_CLOSE_UNAUTHORIZED, WS_MAX_PAYLOAD_BYTES, decode_term_frame,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use super::{ConnectError, WsStream, close_quietly};
use crate::api::seg;
use crate::client::{AuthEvent, AuthEvents, FalconClient, Relogin};
use crate::runtime::runtime;

/// 会话 socket 推给 app 的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// 服务端接受了这条连接：收到了它的第一条消息（服务端对每条通过认证的连接都会
    /// 立刻推一条 state 或 error），紧接着就是那条消息对应的事件。开场的 appearance /
    /// resize 在此之前已经发出去了。
    ///
    /// 不在升级成功那一刻报：服务端是先接受升级、再用 4401 关掉未认证的连接，那种
    /// 连接不该让 app 以为"连上了"（web 的 onopen 就会误报一次）。
    ///
    /// `reconnected` = 这不是这个 socket 的第一次连接尝试：断开期间错过的会话状态
    /// 变化（别处 Terminate 了、改名了）不会经 WS 补发，app 应当立刻补拉一次会话
    /// 列表，不等 5s 轮询。
    Connected { reconnected: bool },
    /// 增量输出（服务端已按 16ms 合并）。直接喂给 VT 解析器——走 Zed 的
    /// `write_raw_output` 路径，别做 LF → CRLF 归一化（§3.1）。
    Output(Bytes),
    /// 整份回放（最大 4MB）：先 reset 再写，或者新建一个 Term 离锁解析后整体换上。
    /// 可能在连接中途任意时刻到达（服务端的背压重同步），不只是连上的那一刻。
    /// 回放期间终端产生的应答（DA / DSR）一律丢掉，别写回 PTY。
    Replay(Bytes),
    /// 会话状态变化。连上时服务端会先推一次当前状态（等 resize 的持久会话推
    /// unverified，已死的会话推 dead + 原因）。
    State { state: SessionState, dead_reason: Option<DeadReason> },
    /// 服务端与远端主机之间的 SSH 断线自动重连中（不是这条 WS 在重连，那个看
    /// `Disconnected`）
    Reconnecting { attempt: u32 },
    /// "会话不存在"、接回失败的原因；一句可以直接展示的话
    Error(String),
    /// 前台命令变了（`None` = 变空），换会话的自动标题
    Title(Option<String>),
    /// sudo / SSH askpass 在等密码：弹对话框，答复走 `FalconClient::answer_askpass`
    Askpass { id: String, prompt: String },
    /// 这条 WS 断了（或者没连上），`retry_in` 之后第 `retry_attempt` 次重连。
    /// 断线期间 `send_input` 的内容会被丢掉（与 web 一致：不能把断线时敲的字、
    /// 尤其是密码，攒到重连后一股脑打进一个状态已经变了的终端）。
    Disconnected { retry_attempt: u32, retry_in: Duration },
    /// 服务端说未认证（关闭码 4401），自动重登没设密码或被拒：停止重连，等 app 弹
    /// 登录框。之后任何一次登录成功（[`crate::AuthEvent::LoggedIn`]）或
    /// [`SessionSocket::reconnect_now`] 都会让它自己接着连。
    Unauthorized,
}

/// 接收会话事件的一方。
///
/// **在网络线程上被顺序调用**：同一会话的事件严格按到达顺序、一个处理完才处理下一个
/// ——app 在回调里直接解析终端字节，这是设计上的零跳转路径（§3.1）。代价是回调
/// 占着一个网络 worker：别在里面等锁等很久、别同步解析一整份 4MB 回放（那个该丢给
/// `spawn_blocking`，§3.1）。在回调里调 [`SessionSocket`] 的方法是安全的（只是入队）。
pub trait SessionSink: Send + Sync + 'static {
    fn on_event(&self, ev: SessionEvent);
}

impl<F> SessionSink for F
where
    F: Fn(SessionEvent) + Send + Sync + 'static,
{
    fn on_event(&self, ev: SessionEvent) {
        self(ev)
    }
}

/// 重连 / 心跳的节奏。缺省值与 web 一致，测试里调小。
#[derive(Debug, Clone)]
pub struct SocketOptions {
    /// 退避起点：1s 起步翻倍、15s 封顶——不打爆刚起来的后端，也不让用户干等太久
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    /// WS ping 间隔。远处的 falcon 服务端通常在反向代理后面，代理的 idle timeout
    /// 会悄悄掐掉安静的连接（服务端的 `ws` 库自动回 pong）。连着两个间隔什么都没
    /// 收到就当连接已死，主动断开重连——半开的 TCP 自己是不会报错的。
    pub ping_interval: Duration,
    /// 建连（TCP + TLS + 升级）的超时
    pub connect_timeout: Duration,
    /// 连着的时候调 [`SessionSocket::reconnect_now`]：发一个 ping 探活，这么久没
    /// 回音就当是睡眠前留下的半开连接，断开重连
    pub probe_timeout: Duration,
}

impl Default for SocketOptions {
    fn default() -> Self {
        SocketOptions {
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(15),
            ping_interval: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            probe_timeout: Duration::from_secs(5),
        }
    }
}

enum Cmd {
    Input(String),
    Resize(u16, u16),
    /// 已经序列化好的 appearance 消息
    Appearance(String),
    ReconnectNow,
    Close,
}

/// 一条会话 WebSocket：自己断线重连、退避、心跳、强发 resize、未认证时自动重登。
///
/// 方法都是同步的，只把命令推进内部队列，由网络任务发出。丢掉 = [`close`]。
///
/// [`close`]: SessionSocket::close
pub struct SessionSocket {
    session_id: String,
    cmd: UnboundedSender<Cmd>,
    closed: Arc<AtomicBool>,
}

impl SessionSocket {
    /// 打开并开始连接。连接在网络运行时上进行，这里立刻返回。
    pub fn open(client: &FalconClient, session_id: &str, sink: Arc<dyn SessionSink>) -> Self {
        Self::open_with(client, session_id, sink, SocketOptions::default())
    }

    pub fn open_with(
        client: &FalconClient,
        session_id: &str,
        sink: Arc<dyn SessionSink>,
        opts: SocketOptions,
    ) -> Self {
        let (tx, rx) = unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let driver = Driver {
            client: client.clone(),
            path: format!("/ws/sessions/{}", seg(session_id)),
            sink,
            auth_rx: client.subscribe_auth(),
            opts,
            cmd_rx: rx,
            closed: closed.clone(),
            size: None,
            appearance: None,
            retries: 0,
            connected_before: false,
            unauthorized_streak: 0,
        };
        runtime().spawn(driver.run());
        SessionSocket { session_id: session_id.to_owned(), cmd: tx, closed }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 键盘 / 粘贴 / IME 上屏的输入。超过服务端单帧上限（1MB，整帧 JSON 计）的大段
    /// 粘贴会按字符边界切成多条 input 顺序发出。没连着的时候直接丢掉（见
    /// [`SessionEvent::Disconnected`]）。
    pub fn send_input(&self, data: impl Into<String>) {
        let data = data.into();
        if !data.is_empty() {
            let _ = self.cmd.send(Cmd::Input(data));
        }
    }

    /// 终端格子数。**格子没量好之前别调**——服务端会拿它去 attach 持久会话，一个
    /// 80×24 的占位值会把 TUI 挤成 24 行（`sessions/termSize.ts`）。
    ///
    /// 记住最近一次的尺寸：每条新 socket 连上都强制补发一次（它是持久会话懒惰接回的
    /// 信号，漏发会让会话一直停在 unverified）；同一条 socket 上相同尺寸去重（服务端
    /// 每条 resize 都有落库逻辑，拖窗时逐帧发只是打扰它）。0 行 / 0 列忽略。
    pub fn resize(&self, cols: u16, rows: u16) {
        if cols > 0 && rows > 0 {
            let _ = self.cmd.send(Cmd::Resize(cols, rows));
        }
    }

    /// 当前终端配色的深浅与底 / 字色（`#rrggbb`），服务端据此代答 OSC 10/11/12
    /// （原生客户端自己**不答**颜色查询，§3.2）。主题切换时再调一次。
    ///
    /// 记住最近一次：每条新 socket 连上后**先**发它、再发 resize——服务端的
    /// OscColorGate 要在 appearance 到了之后才代答，resize 触发的接回会让 zellij
    /// 立刻发颜色查询。
    pub fn set_appearance(&self, appearance: TermAppearance, background: Option<&str>, foreground: Option<&str>) {
        let msg = ClientMessage::Appearance {
            appearance,
            background: background.map(str::to_owned),
            foreground: foreground.map(str::to_owned),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = self.cmd.send(Cmd::Appearance(json));
        }
    }

    /// 立即重连并重置退避：系统唤醒、网络恢复、用户点「立即重连」时调。
    ///
    /// 正在等退避 / 停在未认证时马上发起连接；正在连着时发一个 ping 探活（睡眠前的
    /// 连接多半已经半开，`probe_timeout` 内没回音就断开重连）。
    pub fn reconnect_now(&self) {
        let _ = self.cmd.send(Cmd::ReconnectNow);
    }

    /// 干净地关掉（Detach），不再重连。之后至多还有一个正在进行中的回调。幂等。
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.cmd.send(Cmd::Close);
        }
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

impl Drop for SessionSocket {
    fn drop(&mut self) {
        self.close();
    }
}

/// 驱动循环的下一步。
enum Next {
    Connect,
    /// 等退避（`Some`）或停在未认证（`None`）
    Wait(Option<Duration>),
    Stop,
}

/// 断线期间收到一条命令之后怎么办。
enum Offline {
    Continue,
    ConnectNow,
    Stop,
}

struct Driver {
    client: FalconClient,
    path: String,
    sink: Arc<dyn SessionSink>,
    auth_rx: AuthEvents,
    opts: SocketOptions,
    cmd_rx: UnboundedReceiver<Cmd>,
    closed: Arc<AtomicBool>,
    /// 最近一次已知尺寸（断线期间的 resize 也记着，连上后强发）
    size: Option<(u16, u16)>,
    /// 最近一次 appearance（序列化好的 JSON）
    appearance: Option<String>,
    /// 退避轮次；连上就清零（web 的 `retries`）
    retries: u32,
    connected_before: bool,
    /// 连续撞上 4401 的次数。刚重登过还是 4401 就别再登了，免得"登录—4401—登录"
    /// 死循环；收到任何正常消息清零
    unauthorized_streak: u32,
}

impl Driver {
    async fn run(mut self) {
        let mut next = Next::Connect;
        loop {
            next = match next {
                Next::Connect => self.connect_and_serve().await,
                Next::Wait(d) => self.wait(d).await,
                Next::Stop => break,
            };
        }
    }

    fn emit(&self, ev: SessionEvent) {
        if !self.closed.load(Ordering::SeqCst) {
            self.sink.on_event(ev);
        }
    }

    fn offline(&mut self, cmd: Option<Cmd>) -> Offline {
        match cmd {
            None | Some(Cmd::Close) => Offline::Stop,
            Some(Cmd::ReconnectNow) => {
                self.retries = 0;
                self.unauthorized_streak = 0;
                Offline::ConnectNow
            }
            // 断线期间的输入丢掉，理由见 SessionEvent::Disconnected
            Some(Cmd::Input(_)) => Offline::Continue,
            Some(Cmd::Resize(c, r)) => {
                self.size = Some((c, r));
                Offline::Continue
            }
            Some(Cmd::Appearance(a)) => {
                self.appearance = Some(a);
                Offline::Continue
            }
        }
    }

    async fn connect_and_serve(&mut self) -> Next {
        let token_used = self.client.token();
        // 连接 future 不能借着 self：等它的同时还要处理命令（改 self.size 之类）
        let inner = self.client.inner.clone();
        let path = self.path.clone();
        let connect = super::connect(&inner, &path, self.opts.connect_timeout);
        tokio::pin!(connect);
        let result = loop {
            tokio::select! {
                r = &mut connect => break r,
                cmd = self.cmd_rx.recv() => match self.offline(cmd) {
                    Offline::Stop => return Next::Stop,
                    // 一个挂在睡眠前的连接尝试可能要等到超时才放弃，重新来过更快
                    Offline::ConnectNow => return Next::Connect,
                    Offline::Continue => {}
                },
            }
        };
        match result {
            Ok(ws) => self.serve(ws, token_used).await,
            Err(ConnectError::Unauthorized) => self.unauthorized(token_used).await,
            Err(ConnectError::Other(msg)) => {
                log::debug!("会话 {} 连接失败：{msg}", self.path);
                self.backoff()
            }
        }
    }

    /// 断开之后排下一次重连。
    fn backoff(&mut self) -> Next {
        let delay = backoff_delay(self.opts.backoff_base, self.opts.backoff_max, self.retries);
        self.retries += 1;
        self.emit(SessionEvent::Disconnected { retry_attempt: self.retries, retry_in: delay });
        Next::Wait(Some(delay))
    }

    async fn unauthorized(&mut self, token_used: Option<String>) -> Next {
        self.unauthorized_streak += 1;
        if self.unauthorized_streak == 1 {
            match self.client.inner.relogin(token_used.clone()).await {
                Relogin::Done => return Next::Connect,
                Relogin::Failed(e) => {
                    // 登录请求本身没发出去（后端还没起来）：不算被拒，按普通断线退避
                    log::debug!("会话 {} 自动重登失败：{e}", self.path);
                    self.unauthorized_streak = 0;
                    return self.backoff();
                }
                Relogin::NoPassword | Relogin::Rejected => {}
            }
        } else {
            self.client.inner.auth.login_required(&token_used);
        }
        // 停下之前清掉积压的登录事件（它们发生在停下之前，不是叫醒信号）；但如果
        // 这期间已经有人登好了，就别停，直接用新 token 连
        while self.auth_rx.try_recv().is_ok() {}
        let now = self.client.token();
        if now.is_some() && now != token_used {
            self.unauthorized_streak = 0;
            return Next::Connect;
        }
        self.emit(SessionEvent::Unauthorized);
        Next::Wait(None)
    }

    async fn wait(&mut self, delay: Option<Duration>) -> Next {
        let parked = delay.is_none();
        // 停在未认证时没有定时器，只等命令或登录事件
        let sleep = tokio::time::sleep(delay.unwrap_or(Duration::from_secs(3600)));
        tokio::pin!(sleep);
        let mut auth_open = true;
        loop {
            tokio::select! {
                _ = &mut sleep, if !parked => return Next::Connect,
                cmd = self.cmd_rx.recv() => match self.offline(cmd) {
                    Offline::Stop => return Next::Stop,
                    Offline::ConnectNow => return Next::Connect,
                    Offline::Continue => {}
                },
                ev = self.auth_rx.next(), if parked && auth_open => match ev {
                    Some(AuthEvent::LoggedIn) => {
                        self.unauthorized_streak = 0;
                        return Next::Connect;
                    }
                    Some(_) => {}
                    None => auth_open = false,
                },
            }
        }
    }

    async fn serve(&mut self, mut ws: WsStream, token_used: Option<String>) -> Next {
        // 开场：先 appearance、再 resize（顺序与理由见 SessionSocket 的方法注释）
        let mut sent_size = None;
        if let Some(a) = self.appearance.clone()
            && ws.send(Message::text(a)).await.is_err()
        {
            return self.dropped("发送 appearance 失败");
        }
        if let Some((cols, rows)) = self.size {
            if ws.send(Message::text(resize_json(cols, rows))).await.is_err() {
                return self.dropped("发送 resize 失败");
            }
            sent_size = Some((cols, rows));
        }
        // 收到服务端第一条消息时才报 Connected（见 SessionEvent::Connected）
        let mut accepted = false;

        let interval = self.opts.ping_interval;
        let mut ping = tokio::time::interval_at(Instant::now() + interval, interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 上一个 ping 发出后还什么都没收到
        let mut awaiting = false;
        let probe = tokio::time::sleep(Duration::from_secs(3600));
        tokio::pin!(probe);
        let mut probing = false;

        loop {
            tokio::select! {
                msg = ws.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => return self.dropped(&e.to_string()),
                        None => return self.dropped("连接已关闭"),
                    };
                    // 收到任何东西（含 pong）都说明连接活着
                    awaiting = false;
                    probing = false;
                    if !accepted && matches!(msg, Message::Binary(_) | Message::Text(_)) {
                        accepted = true;
                        self.accepted();
                    }
                    match msg {
                        Message::Binary(frame) => {
                            if let Some(f) = decode_term_frame(&frame) {
                                let payload = frame.slice(1..);
                                self.emit(match f.kind {
                                    TermFrameKind::Output => SessionEvent::Output(payload),
                                    TermFrameKind::Replay => SessionEvent::Replay(payload),
                                });
                            }
                        }
                        Message::Text(text) => {
                            // 坏 JSON 与看不懂的消息都安静忽略（web 的 switch 没有 default）
                            if let Ok(m) = serde_json::from_str::<ServerMessage>(&text) {
                                self.dispatch(m);
                            }
                        }
                        Message::Close(frame) => {
                            let code = frame.as_ref().map(|f| u16::from(f.code));
                            if code == Some(WS_CLOSE_UNAUTHORIZED) {
                                // 4401 = 认证没了（后端重启丢掉了内存里的 token）。
                                // 原样重连只会无限 4401，得先重新登录
                                return self.unauthorized(token_used).await;
                            }
                            return self.dropped(&format!("服务端关闭连接（{code:?}）"));
                        }
                        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        close_quietly(&mut ws).await;
                        return Next::Stop;
                    };
                    let sent = match cmd {
                        Cmd::Close => {
                            close_quietly(&mut ws).await;
                            return Next::Stop;
                        }
                        Cmd::Input(data) => {
                            let mut ok = true;
                            for frame in input_frames(&data, WS_MAX_PAYLOAD_BYTES) {
                                if ws.send(Message::text(frame)).await.is_err() {
                                    ok = false;
                                    break;
                                }
                            }
                            ok
                        }
                        Cmd::Resize(cols, rows) => {
                            self.size = Some((cols, rows));
                            if sent_size == Some((cols, rows)) {
                                true
                            } else {
                                sent_size = Some((cols, rows));
                                ws.send(Message::text(resize_json(cols, rows))).await.is_ok()
                            }
                        }
                        Cmd::Appearance(a) => {
                            self.appearance = Some(a.clone());
                            ws.send(Message::text(a)).await.is_ok()
                        }
                        Cmd::ReconnectNow => {
                            if !probing {
                                probing = true;
                                probe.as_mut().reset(Instant::now() + self.opts.probe_timeout);
                                ws.send(Message::Ping(Bytes::new())).await.is_ok()
                            } else {
                                true
                            }
                        }
                    };
                    if !sent {
                        return self.dropped("发送失败");
                    }
                }
                _ = ping.tick() => {
                    if awaiting {
                        return self.dropped("心跳超时");
                    }
                    if ws.send(Message::Ping(Bytes::new())).await.is_err() {
                        return self.dropped("发送心跳失败");
                    }
                    awaiting = true;
                }
                _ = &mut probe, if probing => return self.dropped("探活超时"),
            }
        }
    }

    /// 服务端接受了这条连接：退避清零、4401 连击清零，报 Connected。
    fn accepted(&mut self) {
        let reconnected = self.connected_before || self.retries > 0;
        self.connected_before = true;
        self.retries = 0;
        self.unauthorized_streak = 0;
        self.emit(SessionEvent::Connected { reconnected });
    }

    fn dropped(&mut self, reason: &str) -> Next {
        log::debug!("会话 {} 断开：{reason}", self.path);
        self.backoff()
    }

    fn dispatch(&mut self, msg: ServerMessage) {
        let ev = match msg {
            ServerMessage::State { state, dead_reason } => SessionEvent::State { state, dead_reason },
            ServerMessage::Reconnecting { attempt } => SessionEvent::Reconnecting { attempt },
            ServerMessage::Error { message } => SessionEvent::Error(message),
            ServerMessage::Title { title } => SessionEvent::Title(title),
            ServerMessage::Askpass { id, prompt } => SessionEvent::Askpass { id, prompt },
            ServerMessage::Unknown => return,
        };
        self.emit(ev);
    }
}

/// 第 `retries` 次退避的等待：`base × 2^min(retries, 4)`，封顶 `max`（web 的 scheduleRetry）。
fn backoff_delay(base: Duration, max: Duration, retries: u32) -> Duration {
    base.saturating_mul(1 << retries.min(4)).min(max)
}

fn resize_json(cols: u16, rows: u16) -> String {
    serde_json::to_string(&ClientMessage::Resize { cols, rows }).expect("resize 消息总能序列化")
}

/// `{"type":"input","data":""}` 的长度：每条 input 帧在数据之外的固定开销。
const INPUT_ENVELOPE: usize = r#"{"type":"input","data":""}"#.len();

/// 一个字符在 serde_json 的字符串里占几个字节。与 serde_json 的转义表一致：
/// `"` 与 `\` 和五个有简写的控制字符是 2，其余控制字符是 `\u00XX` 的 6，DEL 不转义，
/// 非 ASCII 原样 UTF-8。
fn json_escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\u{08}' | '\u{0c}' | '\n' | '\r' | '\t' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

fn input_json(data: &str) -> String {
    // 与 ClientMessage::Input 的序列化逐字节相同（单测钉着），省一次整段拷贝
    format!(r#"{{"type":"input","data":{}}}"#, serde_json::to_string(data).expect("字符串总能序列化"))
}

/// 把一段输入编成若干条 input 帧，每帧（整帧 JSON，不是 data 的字节数）不超过 `max`。
///
/// 服务端 `maxPayload` 是 1MB，超了直接断连接。按**字符**边界切：切在一个 UTF-8
/// 字符中间，两半都不是合法字符串；切在 CRLF 或转义序列中间没关系——服务端把各条
/// input 顺序写进同一个 PTY，字节流原样拼回去。
fn input_frames(data: &str, max: usize) -> Vec<String> {
    let whole = input_json(data);
    if whole.len() <= max {
        return vec![whole];
    }
    let budget = max.saturating_sub(INPUT_ENVELOPE).max(6);
    let mut frames = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (i, c) in data.char_indices() {
        let w = json_escaped_len(c);
        if used + w > budget && i > start {
            frames.push(input_json(&data[start..i]));
            start = i;
            used = 0;
        }
        used += w;
    }
    if start < data.len() {
        frames.push(input_json(&data[start..]));
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_matches_web() {
        let s = |n| backoff_delay(Duration::from_secs(1), Duration::from_secs(15), n).as_secs();
        assert_eq!([s(0), s(1), s(2), s(3), s(4), s(5), s(30)], [1, 2, 4, 8, 15, 15, 15]);
    }

    #[test]
    fn input_json_is_client_message() {
        for data in ["ls -la\r", "\u{1b}[A\u{3}\"\\", "中文 🦅\t\u{7f}\u{0}"] {
            let ours = input_json(data);
            let theirs = serde_json::to_string(&ClientMessage::Input { data: data.to_owned() }).unwrap();
            assert_eq!(ours, theirs);
        }
        assert_eq!(INPUT_ENVELOPE, input_json("").len());
    }

    #[test]
    fn escaped_len_matches_serde_json() {
        let mut samples: Vec<char> = (0u32..0x80).filter_map(char::from_u32).collect();
        samples.extend(['é', '中', '🦅', '\u{2028}', '\u{feff}']);
        for c in samples {
            let json = serde_json::to_string(&c.to_string()).unwrap();
            assert_eq!(json_escaped_len(c), json.len() - 2, "字符 {:?}", c);
        }
    }

    #[test]
    fn big_paste_is_split_under_the_limit() {
        // 混着 CJK、emoji、会被转义膨胀的控制字符与引号，凑到 2.5MB 以上
        let unit = "粘贴\"内容\"\u{1}\u{1b}[31mred\u{1b}[0m 🦅\r\n\\";
        let data = unit.repeat(2_600_000 / unit.len() + 1);
        let frames = input_frames(&data, WS_MAX_PAYLOAD_BYTES);
        assert!(frames.len() >= 3, "{} 帧", frames.len());
        let mut back = String::new();
        for f in &frames {
            assert!(f.len() <= WS_MAX_PAYLOAD_BYTES, "帧 {} 字节超限", f.len());
            match serde_json::from_str::<ClientMessage>(f).unwrap() {
                ClientMessage::Input { data } => back.push_str(&data),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(back, data);
        // 除了最后一帧，都应当贴着上限（别切得太碎）
        assert!(frames[0].len() > WS_MAX_PAYLOAD_BYTES - 16);
    }

    #[test]
    fn small_input_is_one_frame() {
        assert_eq!(input_frames("a", 64), vec![r#"{"type":"input","data":"a"}"#.to_owned()]);
        // 恰好卡在边界
        let data = "x".repeat(64 - INPUT_ENVELOPE);
        assert_eq!(input_frames(&data, 64).len(), 1);
        let data = "x".repeat(64 - INPUT_ENVELOPE + 1);
        let frames = input_frames(&data, 64);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() <= 64));
    }
}
