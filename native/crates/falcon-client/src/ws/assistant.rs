//! AI 助手的对话通道 `/ws/assistant/threads/:id`（docs/design/ai-assistant.md 第 8 节）。
//!
//! 比会话通道简单得多：全是 JSON 文本帧，没有尺寸 / 外观 / 回放那一套。连上服务端先推一份
//! `snapshot`、再推增量，所以断线重连之后什么都不用补——新的快照就是全集。
//!
//! 与会话通道的一处不同：**断线期间发的消息排队，连上再发**。终端输入断线时丢掉是对的（不能把
//! 密码攒到重连后打进一个状态已经变了的终端），这里发的是用户想清楚了的一句话、一次批准，丢了
//! 反而让人以为发出去了。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use falcon_proto::WS_CLOSE_UNAUTHORIZED;
use falcon_proto::assistant::{AssistantClientMessage, AssistantServerMessage};
use futures::StreamExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use super::{ConnectError, WsConn, WsMsg};
use crate::api::seg;
use crate::client::{AuthEvent, AuthEvents, FalconClient, Relogin};
use crate::runtime;

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantEvent {
    /// 收到了服务端的第一条消息（之后紧跟着就是快照）
    Connected,
    Message(AssistantServerMessage),
    /// 断了，过一会儿自己重连
    Disconnected,
    /// 4401 且自动重登不成：等 app 弹登录框，登好了自己接着连
    Unauthorized,
}

/// 在网络线程上被顺序调用（浏览器里是主线程），同 [`crate::SessionSink`]
pub trait AssistantSink: Send + Sync + 'static {
    fn on_event(&self, ev: AssistantEvent);
}

impl<F> AssistantSink for F
where
    F: Fn(AssistantEvent) + Send + Sync + 'static,
{
    fn on_event(&self, ev: AssistantEvent) {
        self(ev)
    }
}

enum Cmd {
    Send(String),
    Close,
}

/// 一条对话的 WebSocket：自己重连、退避、心跳。方法都只是入队。丢掉 = 关掉
pub struct AssistantSocket {
    cmd: UnboundedSender<Cmd>,
    closed: Arc<AtomicBool>,
}

const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(15);
const PING_INTERVAL: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

impl AssistantSocket {
    pub fn open(client: &FalconClient, thread_id: &str, sink: Arc<dyn AssistantSink>) -> Self {
        let (tx, rx) = unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let driver = Driver {
            client: client.clone(),
            path: format!("/ws/assistant/threads/{}", seg(thread_id)),
            sink,
            auth_rx: client.subscribe_auth(),
            cmd_rx: rx,
            closed: closed.clone(),
            queue: VecDeque::new(),
            retries: 0,
            relogged: false,
        };
        runtime::spawn(driver.run());
        AssistantSocket { cmd: tx, closed }
    }

    pub fn send(&self, msg: &AssistantClientMessage) {
        if let Ok(json) = serde_json::to_string(msg) {
            let _ = self.cmd.send(Cmd::Send(json));
        }
    }

    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.cmd.send(Cmd::Close);
        }
    }
}

impl Drop for AssistantSocket {
    fn drop(&mut self) {
        self.close();
    }
}

enum Next {
    Connect,
    Wait(Option<Duration>),
    Stop,
}

struct Driver {
    client: FalconClient,
    path: String,
    sink: Arc<dyn AssistantSink>,
    auth_rx: AuthEvents,
    cmd_rx: UnboundedReceiver<Cmd>,
    closed: Arc<AtomicBool>,
    /// 没发出去的消息（断线期间入的队）
    queue: VecDeque<String>,
    retries: u32,
    /// 这一串 4401 里已经自动重登过一次
    relogged: bool,
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

    fn emit(&self, ev: AssistantEvent) {
        if !self.closed.load(Ordering::SeqCst) {
            self.sink.on_event(ev);
        }
    }

    /// 没连着时收到的命令：入队或停下
    fn offline(&mut self, cmd: Option<Cmd>) -> bool {
        match cmd {
            None | Some(Cmd::Close) => false,
            Some(Cmd::Send(m)) => {
                self.queue.push_back(m);
                true
            }
        }
    }

    async fn connect_and_serve(&mut self) -> Next {
        let token_used = self.client.token();
        let inner = self.client.inner.clone();
        let path = self.path.clone();
        let connect = super::connect(&inner, &path, CONNECT_TIMEOUT);
        tokio::pin!(connect);
        let result = loop {
            tokio::select! {
                r = &mut connect => break r,
                cmd = self.cmd_rx.recv() => if !self.offline(cmd) { return Next::Stop },
            }
        };
        match result {
            Ok(ws) => self.serve(ws, token_used).await,
            Err(ConnectError::Unauthorized) => self.unauthorized(token_used).await,
            Err(ConnectError::Other(msg)) => {
                log::debug!("对话 {} 连接失败：{msg}", self.path);
                self.backoff()
            }
        }
    }

    fn backoff(&mut self) -> Next {
        let delay = BACKOFF_BASE.saturating_mul(1 << self.retries.min(4)).min(BACKOFF_MAX);
        self.retries += 1;
        self.emit(AssistantEvent::Disconnected);
        Next::Wait(Some(delay))
    }

    async fn unauthorized(&mut self, token_used: Option<String>) -> Next {
        if !self.relogged {
            self.relogged = true;
            match self.client.inner.relogin(token_used.clone()).await {
                Relogin::Done => return Next::Connect,
                Relogin::Failed(_) => {
                    self.relogged = false;
                    return self.backoff();
                }
                Relogin::NoPassword | Relogin::Rejected => {}
            }
        } else {
            self.client.inner.auth.login_required(&token_used);
        }
        while self.auth_rx.try_recv().is_ok() {}
        let now = self.client.token();
        if now.is_some() && now != token_used {
            self.relogged = false;
            return Next::Connect;
        }
        self.emit(AssistantEvent::Unauthorized);
        Next::Wait(None)
    }

    async fn wait(&mut self, delay: Option<Duration>) -> Next {
        let parked = delay.is_none();
        let sleep = runtime::sleep(delay.unwrap_or(Duration::from_secs(3600)));
        tokio::pin!(sleep);
        let mut auth_open = true;
        loop {
            tokio::select! {
                _ = &mut sleep, if !parked => return Next::Connect,
                cmd = self.cmd_rx.recv() => if !self.offline(cmd) { return Next::Stop },
                ev = self.auth_rx.next(), if parked && auth_open => match ev {
                    Some(AuthEvent::LoggedIn) => {
                        self.relogged = false;
                        return Next::Connect;
                    }
                    Some(_) => {}
                    None => auth_open = false,
                },
            }
        }
    }

    async fn serve(&mut self, mut ws: WsConn, token_used: Option<String>) -> Next {
        let mut accepted = false;
        let mut ping = Box::pin(runtime::sleep(PING_INTERVAL));
        let mut awaiting = false;
        loop {
            tokio::select! {
                msg = ws.recv() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => return self.dropped(&e),
                        None => return self.dropped("连接已关闭"),
                    };
                    awaiting = false;
                    match msg {
                        WsMsg::Text(text) => {
                            if !accepted {
                                accepted = true;
                                self.retries = 0;
                                self.relogged = false;
                                self.emit(AssistantEvent::Connected);
                                // 断线期间排的队：连上就发
                                while let Some(m) = self.queue.pop_front() {
                                    if ws.send_text(m.clone()).await.is_err() {
                                        self.queue.push_front(m);
                                        return self.dropped("发送失败");
                                    }
                                }
                            }
                            if let Ok(m) = serde_json::from_str::<AssistantServerMessage>(&text) {
                                self.emit(AssistantEvent::Message(m));
                            }
                        }
                        WsMsg::Close(code) => {
                            if code == Some(WS_CLOSE_UNAUTHORIZED) {
                                return self.unauthorized(token_used).await;
                            }
                            return self.dropped(&format!("服务端关闭连接（{code:?}）"));
                        }
                        WsMsg::Binary(_) | WsMsg::Alive => {}
                    }
                }
                cmd = self.cmd_rx.recv() => match cmd {
                    None | Some(Cmd::Close) => {
                        ws.close().await;
                        return Next::Stop;
                    }
                    Some(Cmd::Send(m)) => {
                        if !accepted {
                            self.queue.push_back(m);
                        } else if ws.send_text(m.clone()).await.is_err() {
                            self.queue.push_back(m);
                            return self.dropped("发送失败");
                        }
                    }
                },
                _ = &mut ping => {
                    if awaiting {
                        return self.dropped("心跳超时");
                    }
                    if ws.ping().await.is_err() {
                        return self.dropped("发送心跳失败");
                    }
                    awaiting = true;
                    ping = Box::pin(runtime::sleep(PING_INTERVAL));
                }
            }
        }
    }

    fn dropped(&mut self, reason: &str) -> Next {
        log::debug!("对话 {} 断开：{reason}", self.path);
        self.backoff()
    }
}
