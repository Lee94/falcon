//! 集成测试用的假服务端：一个端口上同时应答 HTTP 与 WebSocket 升级，行为由测试脚本化。
//!
//! 跑在 `falcon_client::runtime()` 上（测试线程本身不在任何 tokio 上下文里，正好顺带
//! 验证客户端与执行器无关）。HTTP 一律 `Connection: close`，省掉 keep-alive 的解析。

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use falcon_client::{FalconClient, runtime};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use url::Url;

pub const WAIT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct HttpReq {
    pub method: String,
    /// 路径 + query，原样
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpReq {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn cookie(&self) -> Option<&str> {
        self.header("cookie")
    }

    pub fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or("")
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

#[derive(Debug, Clone)]
pub struct HttpResp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResp {
    pub fn json(status: u16, v: serde_json::Value) -> Self {
        HttpResp {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&v).unwrap(),
        }
    }

    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        HttpResp { status, headers: vec![("Content-Type".into(), content_type.into())], body }
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

/// 服务端收到的 WS 消息
#[derive(Debug, Clone, PartialEq)]
pub enum WsIn {
    Text(String),
    Ping,
    Closed,
}

enum WsOut {
    Text(String),
    Binary(Vec<u8>),
    Close(u16),
    /// 不打招呼直接掐断 TCP
    Kill,
    /// 不再读也不再回任何东西（模拟半开连接：对方的 ping 石沉大海）
    Freeze,
}

/// 服务端这一侧的一条 WS 连接。
pub struct WsConn {
    pub path: String,
    pub cookie: Option<String>,
    incoming: std_mpsc::Receiver<WsIn>,
    outgoing: tokio_mpsc::UnboundedSender<WsOut>,
}

impl WsConn {
    /// 下一条文本消息（跳过 ping）
    pub fn recv_text(&self) -> String {
        loop {
            match self.incoming.recv_timeout(WAIT).expect("等客户端消息超时") {
                WsIn::Text(t) => return t,
                WsIn::Ping => continue,
                WsIn::Closed => panic!("连接已关闭"),
            }
        }
    }

    pub fn recv_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.recv_text()).unwrap()
    }

    /// `within` 之内没有新的文本消息
    pub fn assert_silent(&self, within: Duration) {
        let deadline = std::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.incoming.recv_timeout(left) {
                Ok(WsIn::Text(t)) => panic!("不该再收到消息：{t}"),
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    }

    pub fn recv_raw(&self, timeout: Duration) -> Option<WsIn> {
        self.incoming.recv_timeout(timeout).ok()
    }

    /// 服务端对每条通过认证的连接都会先推一条 state：客户端收到它才报 Connected。
    pub fn hello(&self) {
        self.send_text(r#"{"type":"state","state":"active"}"#);
    }

    pub fn send_text(&self, s: &str) {
        let _ = self.outgoing.send(WsOut::Text(s.to_owned()));
    }

    pub fn send_binary(&self, b: Vec<u8>) {
        let _ = self.outgoing.send(WsOut::Binary(b));
    }

    pub fn close(&self, code: u16) {
        let _ = self.outgoing.send(WsOut::Close(code));
    }

    pub fn kill(&self) {
        let _ = self.outgoing.send(WsOut::Kill);
    }

    pub fn freeze(&self) {
        let _ = self.outgoing.send(WsOut::Freeze);
    }
}

type Handler = dyn Fn(&HttpReq) -> HttpResp + Send + Sync;

pub struct Mock {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<HttpReq>>>,
    ws: std_mpsc::Receiver<WsConn>,
}

impl Mock {
    pub fn start(handler: impl Fn(&HttpReq) -> HttpResp + Send + Sync + 'static) -> Mock {
        let handler: Arc<Handler> = Arc::new(handler);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (ws_tx, ws_rx) = std_mpsc::channel();
        let listener = runtime()
            .block_on(TcpListener::bind("127.0.0.1:0"))
            .expect("绑端口失败");
        let addr = listener.local_addr().unwrap();
        let log = requests.clone();
        runtime().spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                let handler = handler.clone();
                let log = log.clone();
                let ws_tx = ws_tx.clone();
                tokio::spawn(async move {
                    if is_ws(&stream).await {
                        serve_ws(stream, ws_tx).await;
                    } else {
                        serve_http(stream, &*handler, &log).await;
                    }
                });
            }
        });
        Mock { addr, requests, ws: ws_rx }
    }

    pub fn url(&self) -> Url {
        Url::parse(&format!("http://{}", self.addr)).unwrap()
    }

    pub fn client(&self) -> FalconClient {
        FalconClient::new(self.url())
    }

    pub fn requests(&self) -> Vec<HttpReq> {
        self.requests.lock().unwrap().clone()
    }

    pub fn next_ws(&self) -> WsConn {
        self.ws.recv_timeout(WAIT).expect("等 WS 连接超时")
    }

    pub fn next_ws_within(&self, timeout: Duration) -> Option<WsConn> {
        self.ws.recv_timeout(timeout).ok()
    }
}

async fn is_ws(stream: &TcpStream) -> bool {
    let mut buf = [0u8; 8];
    for _ in 0..50 {
        match stream.peek(&mut buf).await {
            Ok(n) if n >= 8 => return &buf == b"GET /ws/",
            Ok(0) | Err(_) => return false,
            Ok(_) => tokio::time::sleep(Duration::from_millis(2)).await,
        }
    }
    false
}

async fn serve_http(mut stream: TcpStream, handler: &Handler, log: &Mutex<Vec<HttpReq>>) {
    let mut buf = Vec::new();
    let head_end = loop {
        let mut chunk = [0u8; 4096];
        let n = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut start = lines.next().unwrap_or("").split(' ');
    let method = start.next().unwrap_or("").to_owned();
    let path = start.next().unwrap_or("").to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < len {
        let mut chunk = vec![0u8; 64 * 1024];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
        }
    }
    let req = HttpReq { method, path, headers, body };
    let resp = handler(&req);
    log.lock().unwrap().push(req);
    let reason = match resp.status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Whatever",
    };
    let mut out = format!("HTTP/1.1 {} {reason}\r\n", resp.status);
    for (k, v) in &resp.headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", resp.body.len()));
    let _ = stream.write_all(out.as_bytes()).await;
    let _ = stream.write_all(&resp.body).await;
    let _ = stream.shutdown().await;
}

async fn serve_ws(stream: TcpStream, conns: std_mpsc::Sender<WsConn>) {
    let (info_tx, info_rx) = std::sync::mpsc::channel();
    // 签名由 tungstenite 的 Callback 定死（Err 是整个 HTTP 响应），不归我们管
    #[allow(clippy::result_large_err)]
    let callback = move |req: &Request, resp: Response| {
        let cookie = req.headers().get("cookie").and_then(|v| v.to_str().ok()).map(str::to_owned);
        let _ = info_tx.send((req.uri().to_string(), cookie));
        Ok(resp)
    };
    let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, callback).await else { return };
    let (path, cookie) = info_rx.recv().unwrap();
    let (in_tx, in_rx) = std_mpsc::channel();
    let (out_tx, mut out_rx) = tokio_mpsc::unbounded_channel();
    let _ = conns.send(WsConn { path, cookie, incoming: in_rx, outgoing: out_tx });
    loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Text(t))) => { let _ = in_tx.send(WsIn::Text(t.to_string())); }
                Some(Ok(Message::Ping(_))) => { let _ = in_tx.send(WsIn::Ping); }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                    let _ = in_tx.send(WsIn::Closed);
                    return;
                }
                Some(Ok(_)) => {}
            },
            out = out_rx.recv() => match out {
                Some(WsOut::Text(t)) => { let _ = ws.send(Message::text(t)).await; }
                Some(WsOut::Binary(b)) => { let _ = ws.send(Message::binary(b)).await; }
                Some(WsOut::Close(code)) => {
                    let frame = CloseFrame { code: CloseCode::from(code), reason: "".into() };
                    let _ = ws.close(Some(frame)).await;
                    // 等对方回 Close 或断开
                    while let Some(Ok(_)) = ws.next().await {}
                    let _ = in_tx.send(WsIn::Closed);
                    return;
                }
                Some(WsOut::Kill) | None => return,
                Some(WsOut::Freeze) => {
                    // 攥着连接不读不写，直到测试把 WsConn 丢掉
                    while out_rx.recv().await.is_some() {}
                    return;
                }
            },
        }
    }
}

/// 收事件的 sink：推进一个 std channel，测试线程按超时取。
pub struct Events {
    rx: std_mpsc::Receiver<falcon_client::SessionEvent>,
}

impl Events {
    pub fn new() -> (Arc<dyn falcon_client::SessionSink>, Events) {
        let (tx, rx) = std_mpsc::channel();
        let tx = Mutex::new(tx);
        let sink = move |ev| {
            let _ = tx.lock().unwrap().send(ev);
        };
        (Arc::new(sink), Events { rx })
    }

    pub fn next(&self) -> falcon_client::SessionEvent {
        self.rx.recv_timeout(WAIT).expect("等会话事件超时")
    }

    /// 一直取到满足条件的那个事件，途中的其它事件丢掉
    pub fn until(&self, mut f: impl FnMut(&falcon_client::SessionEvent) -> bool) -> falcon_client::SessionEvent {
        loop {
            let ev = self.next();
            if f(&ev) {
                return ev;
            }
        }
    }

    pub fn try_next(&self, within: Duration) -> Option<falcon_client::SessionEvent> {
        self.rx.recv_timeout(within).ok()
    }
}
