//! WebSocket 公共部分：建连（TCP → 可选 TLS → 升级），升级请求带登录 cookie。
//!
//! TLS 自己接（tokio-rustls + 与 REST 同一份平台校验器配置），不用 tokio-tungstenite
//! 的 TLS feature：那些挂在双下划线的私有 feature 上，还会顺带拉 webpki-roots。
//! 自己接也方便把 TCP_NODELAY 钉死——按键是一个字节一个字节发的，Nagle 会把
//! 它们攒到下一个 ACK 才发，打字延迟平白多出几十毫秒。

pub(crate) mod install;
pub(crate) mod session;

use std::net::IpAddr;
use std::time::Duration;

use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::{COOKIE, USER_AGENT};
use tokio_tungstenite::tungstenite::{self, Error as WsError};
use url::{Host, Url};

use crate::client::Inner;
use crate::error::error_chain;
use crate::tls;

pub(crate) trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub(crate) type WsStream = WebSocketStream<Box<dyn Io>>;

#[derive(Debug)]
pub(crate) enum ConnectError {
    /// 升级请求被 HTTP 401 挡回来（反代自己做了鉴权时会这样；falcon 服务端本身是
    /// 先接受升级、再用 4401 关掉，那种在读循环里才看得到）
    Unauthorized,
    Other(String),
}

pub(crate) async fn connect(inner: &Inner, path: &str, timeout: Duration) -> Result<WsStream, ConnectError> {
    match tokio::time::timeout(timeout, connect_inner(inner, path)).await {
        Ok(r) => r,
        Err(_) => Err(ConnectError::Other(format!("连接超时（{}s）", timeout.as_secs()))),
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

/// 干净地关掉：发 Close 帧，等对方回 Close（最多一秒，别让关窗口卡住）。
pub(crate) async fn close_quietly(ws: &mut WsStream) {
    use tungstenite::protocol::CloseFrame;
    use tungstenite::protocol::frame::coding::CloseCode;
    let frame = CloseFrame { code: CloseCode::Normal, reason: "".into() };
    let _ = tokio::time::timeout(Duration::from_secs(1), ws.close(Some(frame))).await;
}
