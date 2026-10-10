//! 浏览器的 WebSocket：`web_sys::WebSocket`，与 web 的 `TerminalView.tsx` 同一套用法。
//!
//! - 登录 cookie 由浏览器自己带（同源，httpOnly），这里设不了也不该设；
//! - `binaryType = arraybuffer`：终端字节走二进制帧，拿到的是 ArrayBuffer，拷进 `Bytes`；
//! - 浏览器发不了 ping 帧，[`WsConn::ping`] 是空操作；
//! - 升级被 HTTP 401 挡回来时浏览器只报 1006，分不出来，一律当普通连接失败。falcon 服务端
//!   自己是先接受升级再用 4401 关，那条路照常看得到。
//!
//! 事件回调把消息推进一条无界通道，[`WsConn::recv`] 从通道里取。回调闭包存在 `WsConn`
//! 里，`WsConn` 丢掉时先摘掉回调再关连接，闭包随之释放。

use bytes::Bytes;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use super::{ConnectError, WsMsg};
use crate::client::Inner;

enum Incoming {
    Open,
    Msg(WsMsg),
}

pub(crate) struct WsConn {
    ws: WebSocket,
    rx: UnboundedReceiver<Incoming>,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl WsConn {
    pub(crate) async fn connect(inner: &Inner, path: &str) -> Result<WsConn, ConnectError> {
        let url = inner.ws_url(path);
        let ws = WebSocket::new(&url).map_err(|e| ConnectError::Other(format!("建 WebSocket 失败 {url}：{e:?}")))?;
        ws.set_binary_type(BinaryType::Arraybuffer);

        let (tx, rx) = unbounded();
        let open_tx = tx.clone();
        let on_open = Closure::<dyn FnMut(Event)>::new(move |_| {
            let _ = open_tx.unbounded_send(Incoming::Open);
        });
        let msg_tx = tx.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
            let data = ev.data();
            let msg = if let Some(buf) = data.dyn_ref::<js_sys::ArrayBuffer>() {
                WsMsg::Binary(Bytes::from(js_sys::Uint8Array::new(buf).to_vec()))
            } else if let Some(text) = data.as_string() {
                WsMsg::Text(text)
            } else {
                // binaryType 已经钉成 arraybuffer，不会有 Blob；真来了也只说明连接活着
                WsMsg::Alive
            };
            let _ = msg_tx.unbounded_send(Incoming::Msg(msg));
        });
        // error 事件之后浏览器一定会再派一个 close，只听 close 就够了
        let on_close = Closure::<dyn FnMut(CloseEvent)>::new(move |ev: CloseEvent| {
            let _ = tx.unbounded_send(Incoming::Msg(WsMsg::Close(Some(ev.code()))));
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        let mut conn = WsConn { ws, rx, _on_open: on_open, _on_message: on_message, _on_close: on_close };
        match conn.rx.next().await {
            Some(Incoming::Open) => Ok(conn),
            Some(Incoming::Msg(WsMsg::Close(code))) => {
                Err(ConnectError::Other(format!("连不上 {url}（关闭码 {}）", code.unwrap_or(0))))
            }
            _ => Err(ConnectError::Other(format!("连不上 {url}"))),
        }
    }

    pub(crate) async fn recv(&mut self) -> Option<Result<WsMsg, String>> {
        loop {
            match self.rx.next().await? {
                Incoming::Msg(m) => return Some(Ok(m)),
                Incoming::Open => {}
            }
        }
    }

    pub(crate) async fn send_text(&mut self, text: String) -> Result<(), String> {
        self.ws.send_with_str(&text).map_err(|e| format!("{e:?}"))
    }

    pub(crate) async fn ping(&mut self) -> Result<(), String> {
        Ok(())
    }

    pub(crate) async fn close(&mut self) {
        self.detach();
    }

    fn detach(&self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        // CONNECTING / OPEN 时才需要关；1000 = 正常关闭（Detach）
        if self.ws.ready_state() <= WebSocket::OPEN {
            let _ = self.ws.close_with_code(1000);
        }
    }
}

impl Drop for WsConn {
    fn drop(&mut self) {
        self.detach();
    }
}
