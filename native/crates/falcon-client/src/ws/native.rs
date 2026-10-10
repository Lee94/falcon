//! 原生的 WebSocket：TCP → 可选 TLS → 升级，升级请求带登录 cookie。
//!
//! TLS 自己接（tokio-rustls + 与 REST 同一份平台校验器配置），不用 tokio-tungstenite
//! 的 TLS feature：那些挂在双下划线的私有 feature 上，还会顺带拉 webpki-roots。
//! 自己接也方便把 TCP_NODELAY 钉死——按键是一个字节一个字节发的，Nagle 会把
//! 它们攒到下一个 ACK 才发，打字延迟平白多出几十毫秒。

use std::net::IpAddr;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::{COOKIE, USER_AGENT};
use tokio_tungstenite::tungstenite::{self, Error as WsError, Message};
use url::{Host, Url};

use super::{ConnectError, WsMsg};
use crate::client::Inner;
use crate::error::error_chain;
use crate::tls;

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type WsStream = WebSocketStream<Box<dyn Io>>;

/// 一条建好的连接。
pub(crate) struct WsConn {
    ws: WsStream,
}

impl WsConn {
    pub(crate) async fn connect(inner: &Inner, path: &str) -> Result<WsConn, ConnectError> {
        connect_inner(inner, path).await.map(|ws| WsConn { ws })
    }

    /// 下一帧；`None` = 流结束，`Some(Err)` = 连接出错（原因给日志用）。
    pub(crate) async fn recv(&mut self) -> Option<Result<WsMsg, String>> {
        Some(match self.ws.next().await? {
            Ok(Message::Text(t)) => Ok(WsMsg::Text(t.as_str().to_owned())),
            Ok(Message::Binary(b)) => Ok(WsMsg::Binary(b)),
            Ok(Message::Close(frame)) => Ok(WsMsg::Close(frame.as_ref().map(|f| u16::from(f.code)))),
            Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Ok(WsMsg::Alive),
            Err(e) => Err(e.to_string()),
        })
    }

    pub(crate) async fn send_text(&mut self, text: String) -> Result<(), String> {
        self.ws.send(Message::text(text)).await.map_err(|e| e.to_string())
    }

    pub(crate) async fn ping(&mut self) -> Result<(), String> {
        self.ws.send(Message::Ping(Default::default())).await.map_err(|e| e.to_string())
    }

    /// 干净地关掉：发 Close 帧，等对方回 Close（最多一秒，别让关窗口卡住）。
    pub(crate) async fn close(&mut self) {
        use tungstenite::protocol::CloseFrame;
        use tungstenite::protocol::frame::coding::CloseCode;
        let frame = CloseFrame { code: CloseCode::Normal, reason: "".into() };
        let _ = tokio::time::timeout(Duration::from_secs(1), self.ws.close(Some(frame))).await;
    }
}

async fn connect_inner(inner: &Inner, path: &str) -> Result<WsStream, ConnectError> {
    let other = |m: String| ConnectError::Other(m);
    let url_str = inner.ws_url(path);
    let url = Url::parse(&url_str).map_err(|e| other(format!("地址不合法 {url_str}：{e}")))?;
    let secure = match url.scheme() {
        "ws" => false,
        "wss" => true,
        s => return Err(other(format!("不支持的协议 {s}://"))),
    };
    let port = url.port_or_known_default().ok_or_else(|| other("地址缺端口".to_owned()))?;
    let host = url.host().ok_or_else(|| other("地址缺主机名".to_owned()))?;

    let tcp = match host {
        Host::Domain(d) => TcpStream::connect((d, port)).await,
        Host::Ipv4(ip) => TcpStream::connect((IpAddr::V4(ip), port)).await,
        Host::Ipv6(ip) => TcpStream::connect((IpAddr::V6(ip), port)).await,
    }
    .map_err(|e| other(format!("连不上 {host}:{port}：{e}")))?;
    let _ = tcp.set_nodelay(true);

    let io: Box<dyn Io> = if secure {
        let config = tls::client_config().map_err(other)?;
        let name = match host {
            Host::Domain(d) => ServerName::try_from(d.to_owned())
                .map_err(|e| other(format!("主机名不合法 {d}：{e}")))?,
            Host::Ipv4(ip) => ServerName::IpAddress(IpAddr::V4(ip).into()),
            Host::Ipv6(ip) => ServerName::IpAddress(IpAddr::V6(ip).into()),
        };
        let stream = TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(|e| other(format!("TLS 握手失败：{}", error_chain(&e))))?;
        Box::new(stream)
    } else {
        Box::new(tcp)
    };

    let mut request = url_str
        .as_str()
        .into_client_request()
        .map_err(|e| other(error_chain(&e)))?;
    let headers = request.headers_mut();
    // 服务端不查 Origin（ws.ts），只认 cookie
    if let Some(cookie) = inner.auth.cookie_header()
        && let Ok(v) = HeaderValue::from_str(&cookie)
    {
        headers.insert(COOKIE, v);
    }
    headers.insert(USER_AGENT, HeaderValue::from_static(concat!("falcon-native/", env!("CARGO_PKG_VERSION"))));

    match tokio_tungstenite::client_async_with_config(request, io, None).await {
        Ok((ws, _)) => Ok(ws),
        Err(WsError::Http(resp)) if resp.status() == 401 => Err(ConnectError::Unauthorized),
        Err(e) => Err(other(error_chain(&e))),
    }
}
