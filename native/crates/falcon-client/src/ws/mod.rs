//! WebSocket 公共部分：会话通道与安装通道共用的一层薄接口 [`WsConn`]。
//!
//! 两个 target 各一份实现，对上只露同一组方法：
//! - 原生（`native.rs`）：自己接 TCP / TLS，tungstenite 升级，升级请求带登录 cookie；
//! - 浏览器（`web.rs`）：`web_sys::WebSocket`，cookie 由浏览器带，`binaryType = arraybuffer`。
//!   浏览器发不了 ping 帧，[`WsConn::ping`] 改发应用层的 `{"type":"ping"}`（服务端回 pong），
//!   所以心跳与唤醒后的探活两边是同一套逻辑（`session.rs`）。

pub(crate) mod install;
pub(crate) mod session;

#[cfg(not(target_family = "wasm"))]
mod native;
#[cfg(target_family = "wasm")]
mod web;

use std::time::Duration;

use bytes::Bytes;

#[cfg(not(target_family = "wasm"))]
pub(crate) use native::WsConn;
#[cfg(target_family = "wasm")]
pub(crate) use web::WsConn;

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
    /// 先接受升级、再用 4401 关掉，那种在读循环里才看得到）。浏览器分不出这种情况，
    /// 只会报一个 1006，wasm 上永远走 `Other`
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    Unauthorized,
    Other(String),
}

pub(crate) async fn connect(inner: &Inner, path: &str, timeout: Duration) -> Result<WsConn, ConnectError> {
    match crate::runtime::timeout(timeout, WsConn::connect(inner, path)).await {
        Some(r) => r,
        None => Err(ConnectError::Other(format!("连接超时（{}s）", timeout.as_secs()))),
    }
}
