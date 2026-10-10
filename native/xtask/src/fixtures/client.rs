//! fixture 生成用的最小 HTTP/1.1 与 WebSocket 客户端：只连 `http://127.0.0.1`，不要 TLS。
//!
//! 为什么不用 reqwest：xtask 是每条 `cargo xtask` 都要先编的包，为一个只连回环地址的
//! 任务拉进 hyper / tokio 整套不划算（`util::download` 走 curl 也是同一个考虑）。
//! HTTP 一律 `Connection: close`，响应按 Content-Length / chunked 读完；WS 用
//! tungstenite 的同步版，读超时就是"这段时间没有消息"。
//!
//! 与 Node 版脚本用的 fetch / WebSocket 对齐的几处：
//! - 不发 `Accept-Encoding`（fetch 会发 gzip 并自动解压；falcon 服务端不压缩，发不发都一样，
//!   不发就省得解压）；不带 cookie jar，cookie 由调用方显式放进请求头。
//! - POST / PUT 没有请求体时带 `Content-Length: 0`（fetch 规范的做法），别的方法不带。
//! - WS 握手不带 cookie（Node 的 WebSocket 没有 cookie jar）；收到的二进制帧逐帧交出去。

use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tungstenite::{Message, WebSocket};

/// 单个 HTTP 请求最长等多久。脚本里 fetch 不设超时；这里给一个宽裕的上限，免得服务端卡住
/// 时整条任务挂死（最慢的是 meegle status，要起一次 CLI）
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Response {
    pub status: u16,
    /// 头名小写
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// 同名头的所有值，按 fetch 的 `headers.get()` 用 ", " 连起来
    pub fn header(&self, name: &str) -> Option<String> {
        let values: Vec<&str> = self.headers.iter().filter(|(k, _)| k == name).map(|(_, v)| v.as_str()).collect();
        (!values.is_empty()).then(|| values.join(", "))
    }
}

/// 发一个请求，读完整个响应。非 2xx 不算错（由调用方按期望的状态码判）
pub fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&[u8]>) -> Result<Response> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).with_context(|| format!("连不上 127.0.0.1:{port}"))?;
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: */*\r\nUser-Agent: falcon-xtask\r\nConnection: close\r\n"
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    match body {
        Some(b) => head.push_str(&format!("Content-Length: {}\r\n", b.len())),
        None if method == "POST" || method == "PUT" => head.push_str("Content-Length: 0\r\n"),
        None => {}
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if let Some(b) = body {
        stream.write_all(b)?;
    }
    stream.flush()?;
    read_response(BufReader::new(stream)).with_context(|| format!("{method} {path}：读响应失败"))
}

fn read_response(mut r: impl BufRead) -> Result<Response> {
    let (status, headers) = loop {
        let line = read_line(&mut r)?;
        // "HTTP/1.1 200 OK"
        let status: u16 =
            line.split(' ').nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow!("状态行不对：{line:?}"))?;
        let mut headers = Vec::new();
        loop {
            let line = read_line(&mut r)?;
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').ok_or_else(|| anyhow!("响应头不对：{line:?}"))?;
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
        // 1xx 是中间响应（不发 Expect 就不会有 100，防御一下），后面还有真正的那个
        if !(100..200).contains(&status) {
            break (status, headers);
        }
    };
    let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
    if let Some(enc) = get("content-encoding").filter(|e| !e.eq_ignore_ascii_case("identity")) {
        bail!("响应被压缩了（{enc}）：请求里没发 Accept-Encoding，不该这样");
    }
    let body = if get("transfer-encoding").is_some_and(|te| te.to_ascii_lowercase().contains("chunked")) {
        read_chunked(&mut r)?
    } else if let Some(len) = get("content-length") {
        let len: usize = len.parse().with_context(|| format!("Content-Length 不对：{len}"))?;
        let mut body = vec![0; len];
        r.read_exact(&mut body)?;
        body
    } else {
        // 既没长度也不分块：读到对端关连接（请求带了 Connection: close）
        let mut body = Vec::new();
        r.read_to_end(&mut body)?;
        body
    };
    Ok(Response { status, headers, body })
}

fn read_chunked(r: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = read_line(r)?;
        let size = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size, 16).with_context(|| format!("分块长度不对：{line:?}"))?;
        if size == 0 {
            // 尾部的 trailer，读到空行为止
            while !read_line(r)?.is_empty() {}
            return Ok(body);
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])?;
        read_line(r)?; // 块尾的 CRLF
    }
}

/// 读一行（去掉 CRLF）。头里理论上可以有非 UTF-8 字节，宽松解码
fn read_line(r: &mut impl BufRead) -> Result<String> {
    let mut buf = Vec::new();
    if r.read_until(b'\n', &mut buf)? == 0 {
        bail!("连接提前断了");
    }
    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------- WebSocket ----------------

pub enum Frame {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pumped {
    /// 到点了
    Deadline,
    /// 回调说可以停了
    Stopped,
    /// 连接关了（关闭握手，或者对端直接断开——浏览器的 WebSocket 两种都只是一个 close 事件）
    Closed,
}

pub struct Ws {
    sock: WebSocket<TcpStream>,
    closed: bool,
}

impl Ws {
    pub fn connect(port: u16, path: &str) -> Result<Ws> {
        let stream = TcpStream::connect(("127.0.0.1", port)).with_context(|| format!("连不上 127.0.0.1:{port}"))?;
        // 握手也有个上限；握手完每次读都按调用方的截止时间重设
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let url = format!("ws://127.0.0.1:{port}{path}");
        let (sock, _) = tungstenite::client(url.as_str(), stream).map_err(|e| anyhow!("{path} WS 握手失败：{e}"))?;
        Ok(Ws { sock, closed: false })
    }

    /// 发一条文本帧。连接已经关了就丢掉（浏览器的 WebSocket 在 CLOSED 状态下 send 也是静默丢弃）
    pub fn send(&mut self, text: &str) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        match self.sock.send(Message::text(text)) {
            Ok(()) => Ok(()),
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                self.closed = true;
                Ok(())
            }
            Err(e) => Err(anyhow!("WS 发送失败：{e}")),
        }
    }

    /// 收消息直到 `until`；`on` 返回 true 就提前停
    pub fn pump(&mut self, until: Instant, on: &mut dyn FnMut(Frame) -> Result<bool>) -> Result<Pumped> {
        loop {
            if self.closed {
                return Ok(Pumped::Closed);
            }
            let now = Instant::now();
            if now >= until {
                return Ok(Pumped::Deadline);
            }
            self.sock.get_ref().set_read_timeout(Some(until - now))?;
            let frame = match self.sock.read() {
                Ok(Message::Text(t)) => Frame::Text(t.as_str().to_string()),
                Ok(Message::Binary(b)) => Frame::Binary(b.to_vec()),
                Ok(Message::Close(_)) => {
                    // tungstenite 已经排好了回给对端的 close 帧，刷出去就算握手完成
                    let _ = self.sock.flush();
                    self.closed = true;
                    continue;
                }
                // ping 的 pong 由 tungstenite 自动回
                Ok(_) => continue,
                // 读超时：tungstenite 的读缓冲保留半截帧，下一轮接着读是安全的
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(_) => {
                    self.closed = true;
                    continue;
                }
            };
            if on(frame)? {
                return Ok(Pumped::Stopped);
            }
        }
    }

    /// 照脚本里的 `await sleep(ms)`：这段时间里来的消息照收；连接断了也睡满
    pub fn hold(&mut self, dur: Duration, on: &mut dyn FnMut(Frame) -> Result<bool>) -> Result<()> {
        let until = Instant::now() + dur;
        loop {
            match self.pump(until, on)? {
                Pumped::Deadline => return Ok(()),
                Pumped::Stopped => continue,
                Pumped::Closed => {
                    std::thread::sleep(until.saturating_duration_since(Instant::now()));
                    return Ok(());
                }
            }
        }
    }

    /// `ws.close()`：发出 close 帧，不等回应。之后到的消息一律不要（浏览器的 WebSocket 在
    /// CLOSING 状态下不再派发 message）
    pub fn close(&mut self) {
        if !self.closed {
            let _ = self.sock.close(None);
            let _ = self.sock.flush();
        }
    }

    /// `close()` 之后等对端回 close（或断开），最多等到 `until`
    pub fn wait_closed(&mut self, until: Instant) {
        let _ = self.pump(until, &mut |_| Ok(false));
    }
}
