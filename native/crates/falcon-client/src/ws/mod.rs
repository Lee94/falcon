//! WebSocket 公共部分：会话通道与安装通道共用的一层薄接口 [`WsConn`]。

pub(crate) mod install;
pub(crate) mod session;

mod native;

use std::time::Duration;

use bytes::Bytes;

pub(crate) use native::WsConn;

use crate::client::Inner;

/// 收到的一帧。
#[derive(Debug)]
pub(crate) enum WsMsg {
    Text(String),
    Binary(Bytes),
    /// 对方关了连接；带关闭码（4401 = 未认证）
    Close(Option<u16>),
    /// ping / pong 之类没有内容的控制帧：只说明连接还活着
    Alive,
}

#[derive(Debug)]
pub(crate) enum ConnectError {
    /// 升级请求被 HTTP 401 挡回来（反代自己做了鉴权时会这样；falcon 服务端本身是
    /// 先接受升级、再用 4401 关掉，那种在读循环里才看得到）。
    Unauthorized,
    Other(String),
}

pub(crate) async fn connect(inner: &Inner, path: &str, timeout: Duration) -> Result<WsConn, ConnectError> {
    match crate::runtime::timeout(timeout, WsConn::connect(inner, path)).await {
        Some(r) => r,
        None => Err(ConnectError::Other(format!("连接超时（{}s）", timeout.as_secs()))),
    }
}
