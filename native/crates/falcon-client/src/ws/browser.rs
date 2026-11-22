//! Agent 浏览器通道 `/ws/browser/:sessionId`（ADR 0029）。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use falcon_proto::{
    BrowserClientMessage, BrowserFrameHeader, BrowserServerMessage, KeyKind, PointerButton, PointerKind,
    WS_CLOSE_UNAUTHORIZED, decode_browser_frame,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{ConnectError, WsMsg};
use crate::api::{Query, seg};
use crate::client::{AuthEvent, AuthEvents, FalconClient, Relogin};
use crate::runtime;
use crate::ws::session::SocketOptions;

/// 浏览器 socket 推给 UI 的事件。
#[derive(Debug, Clone)]
pub enum BrowserSocketEvent {
    Connected { reconnected: bool },
    Frame { header: BrowserFrameHeader, jpeg: Bytes },
    Message(BrowserServerMessage),
    Disconnected { retry_attempt: u32, retry_in: Duration },
    Unauthorized,
}

pub trait BrowserSink: Send + Sync + 'static {
    fn on_event(&self, ev: BrowserSocketEvent);
}

impl<F> BrowserSink for F
where
    F: Fn(BrowserSocketEvent) + Send + Sync + 'static,
{
    fn on_event(&self, ev: BrowserSocketEvent) {
        self(ev)
    }
}

enum Cmd {
    Send(String),
    Visibility(bool),
    Close,
}

/// 一条浏览器 WebSocket：自动断线重连、心跳、携带可见性。丢掉自动关闭。
pub struct BrowserSocket {
    cmd: UnboundedSender<Cmd>,
    closed: Arc<AtomicBool>,
}

impl BrowserSocket {
    pub fn open(
        client: &FalconClient,
        session_id: impl Into<String>,
        initial_visible: bool,
        sink: impl BrowserSink,
    ) -> Self {
        Self::open_with(client, session_id, initial_visible, SocketOptions::default(), sink)
    }

    pub fn open_with(
        client: &FalconClient,
        session_id: impl Into<String>,
        initial_visible: bool,
        opts: SocketOptions,
        sink: impl BrowserSink,
    ) -> Self {
        Self::open_path(client, browser_path(&session_id.into(), None), initial_visible, opts, sink)
    }

    pub fn open_instance(
        client: &FalconClient,
        session_id: &str,
        browser_session: Option<&str>,
        initial_visible: bool,
        sink: impl BrowserSink,
    ) -> Self {
        let path = browser_path(session_id, browser_session);
        Self::open_path(client, path, initial_visible, SocketOptions::default(), sink)
    }

    fn open_path(
        client: &FalconClient,
        path: String,
        initial_visible: bool,
        opts: SocketOptions,
        sink: impl BrowserSink,
    ) -> Self {
        let (tx, rx) = unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let inner = client.inner.clone();
        let auth = client.subscribe_auth();
        let closed_task = closed.clone();
        runtime::spawn(async move {
            drive(inner, auth, path, initial_visible, opts, rx, sink, closed_task).await;
        });
        Self { cmd: tx, closed }
    }

    fn send_msg(&self, msg: &BrowserClientMessage) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        if let Ok(text) = serde_json::to_string(msg) {
            let _ = self.cmd.send(Cmd::Send(text));
        }
    }

    pub fn ack(&self, seq: u64) {
        self.send_msg(&BrowserClientMessage::Ack { seq });
    }

    pub fn set_visible(&self, visible: bool) {
        if !self.closed.load(Ordering::Relaxed) {
            let _ = self.cmd.send(Cmd::Visibility(visible));
        }
    }

    pub fn takeover(&self, device: Option<String>) {
        self.send_msg(&BrowserClientMessage::Takeover { device });
    }

    pub fn release(&self) {
        self.send_msg(&BrowserClientMessage::Release);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn pointer(
        &self,
        kind: PointerKind,
        x: f64,
        y: f64,
        button: PointerButton,
        clicks: u8,
        modifiers: u8,
        seq: u64,
    ) {
        self.send_msg(&BrowserClientMessage::Pointer {
            kind,
            x,
            y,
            button,
            clicks,
            modifiers,
            seq,
        });
    }

    pub fn wheel(&self, x: f64, y: f64, dx: f64, dy: f64, seq: u64) {
        self.send_msg(&BrowserClientMessage::Wheel { x, y, dx, dy, seq });
    }

    pub fn key(&self, kind: KeyKind, key: String, code: String, modifiers: u8) {
        self.send_msg(&BrowserClientMessage::Key {
            kind,
            key,
            code,
            modifiers,
        });
    }

    pub fn text(&self, text: String) {
        if !text.is_empty() {
            self.send_msg(&BrowserClientMessage::Text { text });
        }
    }

    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.cmd.send(Cmd::Close);
        }
    }
}

impl Drop for BrowserSocket {
    fn drop(&mut self) {
        self.close();
    }
}

fn browser_path(session_id: &str, browser_session: Option<&str>) -> String {
    let query = Query::new().opt("browserSession", browser_session).finish();
    format!("/ws/browser/{}{query}", seg(session_id))
}

async fn drive(
    inner: Arc<crate::client::Inner>,
    mut auth: AuthEvents,
    path: String,
    mut visible: bool,
    opts: SocketOptions,
    mut rx: UnboundedReceiver<Cmd>,
    sink: impl BrowserSink,
    closed: Arc<AtomicBool>,
) {
    let mut attempt: u32 = 0;
    let mut ever_connected = false;

    loop {
        if closed.load(Ordering::Relaxed) {
            return;
        }
        match super::connect(&inner, &path, opts.connect_timeout).await {
            Ok(mut ws) => {
                let vis_text = serde_json::to_string(&BrowserClientMessage::Visibility { visible }).unwrap_or_default();
                if ws.send_text(vis_text).await.is_err() {
                    attempt = attempt.saturating_add(1);
                } else {
                    let mut accepted = false;
                    let mut unauth = false;
                    let mut ping_wait = false;
                    let mut ping = tokio::time::interval(opts.ping_interval);
                    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    ping.tick().await;

                    loop {
                        tokio::select! {
                            msg = ws.recv() => match msg {
                                Some(Ok(WsMsg::Text(t))) => {
                                    ping_wait = false;
                                    if !accepted {
                                        accepted = true;
                                        attempt = 0;
                                        sink.on_event(BrowserSocketEvent::Connected { reconnected: ever_connected });
                                        ever_connected = true;
                                    }
                                    if let Ok(parsed) = serde_json::from_str::<BrowserServerMessage>(&t) {
                                        sink.on_event(BrowserSocketEvent::Message(parsed));
                                    }
                                }
                                Some(Ok(WsMsg::Binary(b))) => {
                                    ping_wait = false;
                                    if !accepted {
                                        accepted = true;
                                        attempt = 0;
                                        sink.on_event(BrowserSocketEvent::Connected { reconnected: ever_connected });
                                        ever_connected = true;
                                    }
                                    if let Some((header, body)) = decode_browser_frame(&b) {
                                        let jpeg = b.slice(b.len() - body.len()..);
                                        sink.on_event(BrowserSocketEvent::Frame { header, jpeg });
                                    }
                                }
                                Some(Ok(WsMsg::Alive)) => {
                                    ping_wait = false;
                                }
                                Some(Ok(WsMsg::Close(code))) => {
                                    if code == Some(WS_CLOSE_UNAUTHORIZED) {
                                        unauth = true;
                                    }
                                    break;
                                }
                                Some(Err(_)) | None => break,
                            },
                            cmd = rx.recv() => match cmd {
                                Some(Cmd::Send(t)) => {
                                    if ws.send_text(t).await.is_err() {
                                        break;
                                    }
                                }
                                Some(Cmd::Visibility(v)) => {
                                    visible = v;
                                    let t = serde_json::to_string(&BrowserClientMessage::Visibility { visible: v }).unwrap_or_default();
                                    if ws.send_text(t).await.is_err() {
                                        break;
                                    }
                                }
                                Some(Cmd::Close) | None => {
                                    ws.close().await;
                                    return;
                                }
                            },
                            _ = ping.tick() => {
                                if ping_wait || ws.ping().await.is_err() {
                                    break;
                                }
                                ping_wait = true;
                            }
                        }
                    }

                    if closed.load(Ordering::Relaxed) {
                        return;
                    }
                    if unauth {
                        let stale = inner.auth.token();
                        match inner.relogin(stale).await {
                            Relogin::Done => continue,
                            Relogin::NoPassword | Relogin::Rejected => {
                                sink.on_event(BrowserSocketEvent::Unauthorized);
                                if !wait_for_login(&mut auth, &mut rx, &mut visible, &closed).await {
                                    return;
                                }
                                attempt = 0;
                                continue;
                            }
                            Relogin::Failed(_) => {}
                        }
                    }
                    attempt = attempt.saturating_add(1);
                }
            }
            Err(ConnectError::Unauthorized) => {
                let stale = inner.auth.token();
                match inner.relogin(stale).await {
                    Relogin::Done => continue,
                    Relogin::NoPassword | Relogin::Rejected => {
                        sink.on_event(BrowserSocketEvent::Unauthorized);
                        if !wait_for_login(&mut auth, &mut rx, &mut visible, &closed).await {
                            return;
                        }
                        attempt = 0;
                        continue;
                    }
                    Relogin::Failed(_) => {
                        attempt = attempt.saturating_add(1);
                    }
                }
            }
            Err(ConnectError::Other(_)) => {
                attempt = attempt.saturating_add(1);
            }
        }

        let shift = attempt.saturating_sub(1).min(10);
        let delay = opts.backoff_base.saturating_mul(1u32 << shift).min(opts.backoff_max);
        sink.on_event(BrowserSocketEvent::Disconnected {
            retry_attempt: attempt,
            retry_in: delay,
        });
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => break,
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Visibility(v)) => visible = v,
                    Some(Cmd::Close) | None => return,
                    Some(Cmd::Send(_)) => {}
                }
            }
        }
    }
}

async fn wait_for_login(
    auth: &mut AuthEvents,
    rx: &mut UnboundedReceiver<Cmd>,
    visible: &mut bool,
    closed: &Arc<AtomicBool>,
) -> bool {
    loop {
        if closed.load(Ordering::Relaxed) {
            return false;
        }
        tokio::select! {
            ev = auth.recv() => {
                if matches!(ev, Ok(AuthEvent::LoggedIn)) {
                    return true;
                }
            }
            cmd = rx.recv() => match cmd {
                Some(Cmd::Visibility(v)) => *visible = v,
                Some(Cmd::Close) | None => return false,
                Some(Cmd::Send(_)) => {}
            }
        }
    }
}
