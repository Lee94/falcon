//! 中转运行时（端口转发 `forward.rs` + 公网发布 `share.rs`，ADR 0016）共用的部分：链路接口、
//! 错误、本机监听与桥接。
//!
//! TS 里这几样在 `forward.ts` / `share.ts` 各抄一份（`pipeSockets`、`listenLocal` / `listenBridge`、
//! `openLocal` / `openBridge`），这里收成一份；TS 没有对应文件。
//!
//! 执行模型：两个管理器都跑在会话核心的单线程 LocalSet 上（与 Node 的事件循环同一个语义），
//! 链路是 `Rc`。监听循环与「经链路打通一条隧道」在 LocalSet 上（要碰 `Rc` 的链路）；隧道打通
//! 之后两头都是 `Send` 的流，搬字节交给 tokio 的工作线程，不占会话核心。

use std::rc::Rc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::ssh::SshLink;
use crate::exec::LocalBoxFuture;

/// 主机 id → 这台主机的那条链路（SessionManager.getHostLink）。主机不存在时 None
pub type LinkForOf<L> = Rc<dyn Fn(&str) -> Option<Rc<L>>>;
/// 生产上的 [`LinkForOf`]：链路就是 [`SshLink`]
pub type LinkFor = LinkForOf<SshLink>;
/// 连不上主机时请 SessionManager 退避重连；连上后它的 "up" 会回调 `on_link_up`
pub type WantReconnect = Rc<dyn Fn(&str)>;

/// 中转的错误。路由按 [`RelayError::status`] 回码，与 `routes.ts` 的 `/api/forwards*`、
/// `/api/shares*` 一致：不存在 404，其余（入参校验）400，响应体 `{ error: <消息> }`。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
    /// 规则或主机不存在（TS 的 `ForwardNotFoundError` / `ShareNotFoundError`）。路由据此回 404
    #[error("{0}")]
    NotFound(String),
    /// 入参校验失败（TS 的 `throw new Error(parsed.error)`）。路由回 400
    #[error("{0}")]
    Invalid(String),
    /// 启动失败：链路连不上、端口占用、cloudflared 起不来。只从 `start()` 出来——
    /// 建 / 改规则时启动丢在后台，结果写进规则的 state / error，路由碰不到它
    #[error("{0}")]
    Failed(String),
}

impl RelayError {
    pub fn status(&self) -> http::StatusCode {
        match self {
            RelayError::NotFound(_) => http::StatusCode::NOT_FOUND,
            RelayError::Invalid(_) | RelayError::Failed(_) => http::StatusCode::BAD_REQUEST,
        }
    }
}

/// 中转要用到的那几样链路能力。生产上就是 [`SshLink`]；抽成 trait 是为了单测里换一条
/// 假链路，不连真 SSH 也能把监听、桥接、让位、代数这一套跑一遍。
pub trait RelayLink: 'static {
    /// 一条打通了的隧道（russh 的 `ChannelStream`）
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;
    /// 远端转发进来的一条连接（russh 的 `Channel`），用 [`RelayLink::incoming_stream`] 转成流
    type Incoming: Send + 'static;

    /// 把链路连上（TS 的 `getClient()`）。错误是给人看的消息
    fn ensure_connected(&self) -> LocalBoxFuture<'_, Result<(), String>>;
    /// direct-tcpip：经远端连它那边的 `dest_host:dest_port`
    fn open_tunnel<'a>(&'a self, dest_host: &'a str, dest_port: u16) -> LocalBoxFuture<'a, anyhow::Result<Self::Stream>>;
    /// tcpip-forward：远端监听，进来的连接从返回的接收端里出来。错误是给人看的消息
    fn listen_remote<'a>(
        &'a self,
        bind_host: &'a str,
        bind_port: u16,
    ) -> LocalBoxFuture<'a, Result<mpsc::UnboundedReceiver<Self::Incoming>, String>>;
    fn unlisten_remote<'a>(&'a self, bind_host: &'a str, bind_port: u16) -> LocalBoxFuture<'a, ()>;
    fn incoming_stream(incoming: Self::Incoming) -> Self::Stream;
}

impl RelayLink for SshLink {
    type Stream = russh::ChannelStream<russh::client::Msg>;
    type Incoming = russh::Channel<russh::client::Msg>;

    fn ensure_connected(&self) -> LocalBoxFuture<'_, Result<(), String>> {
        Box::pin(async move { self.get_client().await.map(|_| ()).map_err(|e| e.message) })
    }

    fn open_tunnel<'a>(&'a self, dest_host: &'a str, dest_port: u16) -> LocalBoxFuture<'a, anyhow::Result<Self::Stream>> {
        Box::pin(async move { Ok(self.forward_out(dest_host, dest_port).await?.into_stream()) })
    }

    fn listen_remote<'a>(
        &'a self,
        bind_host: &'a str,
        bind_port: u16,
    ) -> LocalBoxFuture<'a, Result<mpsc::UnboundedReceiver<Self::Incoming>, String>> {
        Box::pin(async move {
            self.add_remote_forward(bind_host, bind_port).await.map_err(|e| {
                // 服务端拒了 tcpip-forward（端口占用 / 没权限 / sshd 禁了转发）：照 ssh2 的原话，
                // 规则上显示的错误与 Node 版一致
                if matches!(e.downcast_ref::<russh::Error>(), Some(russh::Error::RequestDenied)) {
                    format!("Unable to bind to {bind_host}:{bind_port}")
                } else {
                    format!("{e:#}")
                }
            })
        })
    }

    fn unlisten_remote<'a>(&'a self, bind_host: &'a str, bind_port: u16) -> LocalBoxFuture<'a, ()> {
        Box::pin(self.remove_remote_forward(bind_host, bind_port))
    }

    fn incoming_stream(incoming: Self::Incoming) -> Self::Stream {
        incoming.into_stream()
    }
}

/// 在后端本机监听 `host:port`。失败的消息照 Node `server.listen` 的样子写
/// （`listen EADDRINUSE: address already in use 127.0.0.1:5432`）：这是规则上给人看的错误，
/// 与 Node 版一致，比 Rust 的 `Address already in use (os error 48)` 也更好认。
///
/// tokio 在 Unix 上给监听套接字设 SO_REUSEADDR（libuv 也设），同端口切换时旧连接的
/// TIME_WAIT 不会挡住新监听。
pub(crate) async fn bind_local(host: &str, port: u16) -> Result<TcpListener, String> {
    TcpListener::bind((host, port)).await.map_err(|e| listen_error(&e, host, port))
}

fn listen_error(e: &std::io::Error, host: &str, port: u16) -> String {
    use std::io::ErrorKind;
    let code = match e.kind() {
        ErrorKind::AddrInUse => Some(("EADDRINUSE", "address already in use")),
        ErrorKind::PermissionDenied => Some(("EACCES", "permission denied")),
        ErrorKind::AddrNotAvailable => Some(("EADDRNOTAVAIL", "address not available")),
        _ => None,
    };
    match code {
        Some((code, what)) => format!("listen {code}: {what} {host}:{port}"),
        None => format!("listen {host}:{port}：{e}"),
    }
}

/// accept 的瞬时错误：对端在握手队列里就断了之类，libuv 也是跳过（`UV_ECONNABORTED` 直接 continue）。
/// 其余错误（fd 耗尽等）Node 会在 server 上发 'error'，TS 据此拆掉这条规则。
fn transient_accept_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        e.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::Interrupted | ErrorKind::WouldBlock
    )
}

/// 本机监听循环（TS 的 `net.createServer` + `openLocal` / `openBridge`）：每条进来的连接经
/// `link.open_tunnel` 打到 `dest`，打不通就关掉这条连接（TS 的 `socket.destroy()`）。
///
/// 监听器归这个任务所有：abort 它（并 await 它的 JoinHandle）就放掉端口。已经建立的连接
/// 不跟着断——同 Node 的 `server.close()`「不再接新连接、保留已有连接」。
///
/// accept 本身出错时调 `on_error` 并退出循环（TS 在 listen 成功后挂的那个 'error' 处理器）。
pub(crate) fn spawn_tunnel_listener<L: RelayLink>(
    listener: TcpListener,
    link: Rc<L>,
    dest_host: String,
    dest_port: u16,
    on_error: Box<dyn FnOnce(String)>,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let link = link.clone();
                    let host = dest_host.clone();
                    tokio::task::spawn_local(async move {
                        // 打不通：socket 随任务 drop，等于 destroy
                        let Ok(stream) = link.open_tunnel(&host, dest_port).await else { return };
                        tokio::spawn(pipe(socket, stream));
                    });
                }
                Err(e) if transient_accept_error(&e) => continue,
                Err(e) => {
                    on_error(e.to_string());
                    return;
                }
            }
        }
    })
}

/// 远端转发的入站分发（TS `listenRemote` 里交给 `addRemoteForward` 的回调）：每条进来的
/// 连接在后端本机连 `dest`，连不上就关掉通道（TS 的 `stream.destroy()`）。
///
/// 接收端在链路那头的 acceptor 被摘掉（`unlisten_remote` / 链路 dispose）时关闭，循环随之结束。
pub(crate) fn spawn_remote_acceptor<L: RelayLink>(
    mut incoming: mpsc::UnboundedReceiver<L::Incoming>,
    dest_host: String,
    dest_port: u16,
) -> JoinHandle<()> {
    tokio::task::spawn_local(async move {
        while let Some(conn) = incoming.recv().await {
            let host = dest_host.clone();
            tokio::spawn(async move {
                let stream = L::incoming_stream(conn);
                let Ok(socket) = TcpStream::connect((host.as_str(), dest_port)).await else { return };
                pipe(socket, stream).await;
            });
        }
    })
}

/// 两头对接（TS 的 `pipeSockets`）。任一头出错两头一起关；一头读到 EOF 时把 EOF 转给
/// 另一头、等反方向也读完再关——Node 版是任一头 'close' 就两头 destroy，半关闭时反方向
/// 还在路上的数据会丢，这里不丢。
pub(crate) async fn pipe<A, B>(mut a: A, mut b: B)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
}

/// TS 的 `req.body ?? {}`：路由把缺省的请求体当空对象传进来，这里替它兜一次
pub(crate) fn or_empty(input: &serde_json::Value) -> std::borrow::Cow<'_, serde_json::Value> {
    if input.is_null() {
        std::borrow::Cow::Owned(serde_json::Value::Object(Default::default()))
    } else {
        std::borrow::Cow::Borrowed(input)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! 单测用的假链路：不连 SSH。`open_tunnel` 在本机连一个回显服务（假装那是远端的目标），
    //! `listen_remote` 把发送端留给测试，测试往里塞 duplex 流假装远端有连接进来。

    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    pub struct FakeLink {
        /// Some = 连不上，消息是这个
        pub fail_connect: RefCell<Option<String>>,
        /// 连接要挂多久才返回（模拟握手慢 / 超时）
        pub connect_delay_ms: Cell<u64>,
        pub connects: Cell<u32>,
        /// open_tunnel 实际连到的本机端口：dest_port → 这里的端口（没登记就原样连 dest）
        pub tunnel_to: RefCell<HashMap<u16, u16>>,
        pub tunnels: RefCell<Vec<(String, u16)>>,
        pub remote: RefCell<HashMap<u16, mpsc::UnboundedSender<tokio::io::DuplexStream>>>,
        pub unlistened: RefCell<Vec<(String, u16)>>,
        /// Some = listen_remote 失败，消息是这个
        pub fail_listen_remote: RefCell<Option<String>>,
    }

    impl RelayLink for FakeLink {
        type Stream = tokio::io::DuplexStream;
        type Incoming = tokio::io::DuplexStream;

        fn ensure_connected(&self) -> LocalBoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                self.connects.set(self.connects.get() + 1);
                let delay = self.connect_delay_ms.get();
                if delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                match self.fail_connect.borrow().clone() {
                    Some(msg) => Err(msg),
                    None => Ok(()),
                }
            })
        }

        fn open_tunnel<'a>(
            &'a self,
            dest_host: &'a str,
            dest_port: u16,
        ) -> LocalBoxFuture<'a, anyhow::Result<Self::Stream>> {
            Box::pin(async move {
                self.tunnels.borrow_mut().push((dest_host.to_string(), dest_port));
                let port = self.tunnel_to.borrow().get(&dest_port).copied().unwrap_or(dest_port);
                let socket = TcpStream::connect(("127.0.0.1", port)).await?;
                let (ours, theirs) = tokio::io::duplex(64 * 1024);
                tokio::spawn(pipe(socket, theirs));
                Ok(ours)
            })
        }

        fn listen_remote<'a>(
            &'a self,
            _bind_host: &'a str,
            bind_port: u16,
        ) -> LocalBoxFuture<'a, Result<mpsc::UnboundedReceiver<Self::Incoming>, String>> {
            Box::pin(async move {
                if let Some(msg) = self.fail_listen_remote.borrow().clone() {
                    return Err(msg);
                }
                let (tx, rx) = mpsc::unbounded_channel();
                self.remote.borrow_mut().insert(bind_port, tx);
                Ok(rx)
            })
        }

        fn unlisten_remote<'a>(&'a self, bind_host: &'a str, bind_port: u16) -> LocalBoxFuture<'a, ()> {
            Box::pin(async move {
                self.remote.borrow_mut().remove(&bind_port);
                self.unlistened.borrow_mut().push((bind_host.to_string(), bind_port));
            })
        }

        fn incoming_stream(incoming: Self::Incoming) -> Self::Stream {
            incoming
        }
    }

    /// 本机回显服务（假装是远端 / 本机的目标服务），返回端口
    pub async fn echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        port
    }

    /// 往 `stream` 写一句、读回同样长度
    pub async fn roundtrip<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, msg: &[u8]) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream.write_all(msg).await.unwrap();
        let mut buf = vec![0u8; msg.len()];
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_exact(&mut buf))
            .await
            .expect("回显超时")
            .unwrap();
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_matches_routes_ts() {
        assert_eq!(RelayError::NotFound("主机不存在".into()).status(), http::StatusCode::NOT_FOUND);
        assert_eq!(RelayError::Invalid("目标端口无效".into()).status(), http::StatusCode::BAD_REQUEST);
        assert_eq!(RelayError::Failed("x".into()).to_string(), "x");
    }

    #[tokio::test]
    async fn listen_errors_read_like_node() {
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = taken.local_addr().unwrap().port();
        let err = bind_local("127.0.0.1", port).await.unwrap_err();
        assert_eq!(err, format!("listen EADDRINUSE: address already in use 127.0.0.1:{port}"));
    }

    #[test]
    fn or_empty_treats_null_body_as_empty_object() {
        assert_eq!(*or_empty(&serde_json::Value::Null), serde_json::json!({}));
        assert_eq!(*or_empty(&serde_json::json!("x")), serde_json::json!("x"));
    }
}
