//! 守护进程会话的 [`Backend`]：经一条字节流与宿主机上的 holder 说 falcon-proto 的帧协议
//! （ADR 0020 决定四）。字节流有两种：
//! - 本机：直接连 holder 的 Unix socket / Windows 命名管道；
//! - SSH：exec 一条 `falcon-sessiond bridge`，stdin / stdout 就是连接（bridge 只搬字节）。
//!
//! 一条连接上可能有多个 Viewer：服务端自己的 ViewerArbiter 仍按原样取尺寸与深浅，作为一个
//! 连接提交给 holder。接回 / 新 Viewer 要回放时发 `Sync`：holder 保证这条连接上 `Synced`
//! 之前的输出都已含在快照里、之后的都在快照之后，所以这里只要**按到达顺序**把快照交给
//! 会话核心（`on_event(Synced)`），会话核心在同一个同步段里先把攒着的输出发给已经在看的
//! Viewer、再把快照发给等回放的 Viewer（manager 的 on_backend_event）。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use falcon_proto::sessiond::{
    Appearance, BRIDGE_EXIT_EXITED, BRIDGE_EXIT_GONE, BRIDGE_EXIT_UNAVAILABLE, BRIDGE_PREAMBLE, ClientMsg, Event, FALCON_VERSION,
    FrameDecoder, Hello, HolderMsg, PROTO_MAX, PROTO_MIN, Resize, Role, Scheme, Welcome,
};
use falcon_proto::{OscColorHint, TermAppearance};
use russh::ChannelMsg;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

use crate::exec::{LocalBoxFuture, Utf8Decoder};
use crate::sessions::backend::{Backend, BackendCallbacks, BackendEvent};
use crate::sessions::ssh::SshLink;

/// 等 holder 的 Welcome 最多多久。SSH 上带 `--create` 时这段时间里还要起 holder（spawn 自己
/// 等就绪最多 10 秒）
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(25);
/// 问前台命令最多等多久：它是关窗口前的一道保险，不能把关窗口拦住
const FOREGROUND_TIMEOUT: Duration = Duration::from_secs(2);
/// bridge 的 stderr 留多少，握手失败时拿它当错误详情
const STDERR_KEEP: usize = 4096;

/// 连到哪
pub enum Transport {
    /// 本机：holder 的 socket 路径 / 命名管道名
    Local(String),
    /// SSH：在宿主机上 exec 这条命令（bridge）
    Ssh { link: Rc<SshLink>, command: String },
}

/// 连不上 / 握手失败的原因
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// 会话在宿主机上不在了
    Gone,
    /// 会话在断线期间正常结束了
    Exited,
    /// 会话还在，但暂时连不上（刚起来 / 卡住），稍后重试
    Unavailable(String),
    /// 链路或别的故障
    Failed(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Gone => f.write_str("会话在宿主机上已经不存在"),
            ConnectError::Exited => f.write_str("会话已经结束"),
            ConnectError::Unavailable(m) => write!(f, "会话暂时连不上：{m}"),
            ConnectError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ConnectError {}

enum Cmd {
    Frame(Vec<u8>),
    Close,
}

/// 字节流那一头的事
enum Wire {
    Bytes(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(u32),
    Closed,
}

pub struct SessiondBackend {
    tx: mpsc::UnboundedSender<Cmd>,
    next_req: Cell<u32>,
    foreground: Rc<RefCell<HashMap<u32, oneshot::Sender<Option<String>>>>>,
}

impl SessiondBackend {
    fn send(&self, msg: ClientMsg) {
        let _ = self.tx.send(Cmd::Frame(msg.encode()));
    }

    fn req(&self) -> u32 {
        let r = self.next_req.get();
        self.next_req.set(r.wrapping_add(1).max(1));
        r
    }
}

impl Backend for SessiondBackend {
    fn write(&self, data: &str) {
        self.send(ClientMsg::Input(data.as_bytes().to_vec()));
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.send(ClientMsg::Resize(Resize { cols, rows }));
    }

    fn destroy(&self) {
        // 只断开这条连接：会话在宿主机上照常运行（Detach）
        let _ = self.tx.send(Cmd::Close);
    }

    fn request_sync(&self) -> bool {
        let req = self.req();
        self.send(ClientMsg::Sync { req });
        true
    }

    fn foreground(&self) -> Option<LocalBoxFuture<'static, Option<String>>> {
        let req = self.req();
        let (tx, rx) = oneshot::channel();
        self.foreground.borrow_mut().insert(req, tx);
        self.send(ClientMsg::Foreground { req });
        let waiters = self.foreground.clone();
        Some(Box::pin(async move {
            let res = tokio::time::timeout(FOREGROUND_TIMEOUT, rx).await;
            waiters.borrow_mut().remove(&req);
            res.ok().and_then(Result::ok).flatten()
        }))
    }

    fn set_appearance(&self, hint: &OscColorHint) {
        let scheme = hint.appearance.map(|a| match a {
            TermAppearance::Light => Scheme::Light,
            _ => Scheme::Dark,
        });
        self.send(ClientMsg::Appearance(Appearance {
            scheme,
            foreground: hint.foreground.clone(),
            background: hint.background.clone(),
            cursor: None,
        }));
    }

    fn status_seen(&self, ids: &[String]) {
        self.send(ClientMsg::StatusSeen { ids: ids.to_vec() });
    }

    fn terminate(&self) -> bool {
        self.send(ClientMsg::Terminate);
        true
    }

    fn upgrade_vt(&self, binary: &str) {
        let req = self.req();
        self.send(ClientMsg::UpgradeVt { req, binary: binary.to_string() });
    }
}

/// bridge 退出码 → 连不上的原因
fn bridge_error(code: Option<u32>, stderr: &[u8]) -> ConnectError {
    let text = String::from_utf8_lossy(stderr).trim().to_string();
    match code.map(|c| c as i32) {
        Some(BRIDGE_EXIT_GONE) => ConnectError::Gone,
        Some(BRIDGE_EXIT_EXITED) => ConnectError::Exited,
        Some(BRIDGE_EXIT_UNAVAILABLE) => ConnectError::Unavailable(text),
        Some(c) if !text.is_empty() => ConnectError::Failed(format!("{text}（bridge 退出码 {c}）")),
        Some(c) => ConnectError::Failed(format!("bridge 退出码 {c}")),
        None if !text.is_empty() => ConnectError::Failed(text),
        None => ConnectError::Failed("连接在握手前就关闭了".into()),
    }
}

/// 快照 + continuation 拼成回放文本。
///
/// continuation 是解析器停在半截序列时要补的原始字节，可能以半个 UTF-8 字符结尾——那半个
/// 字符也还压在本端的 UTF-8 解码器里，等后半截到了会作为实时输出的一部分整个吐出来。
/// 回放里再带上它就成了一个 U+FFFD 加一个完整字符，所以从 continuation 里把它去掉
pub fn replay_text(snapshot: &[u8], continuation: &[u8], decoder_pending: &[u8]) -> String {
    let cont = if !decoder_pending.is_empty() && continuation.ends_with(decoder_pending) {
        &continuation[..continuation.len() - decoder_pending.len()]
    } else {
        continuation
    };
    let mut bytes = Vec::with_capacity(snapshot.len() + cont.len());
    bytes.extend_from_slice(snapshot);
    bytes.extend_from_slice(cont);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// 打开字节流
async fn open(transport: Transport) -> Result<(mpsc::UnboundedReceiver<Wire>, mpsc::UnboundedSender<Cmd>, bool), ConnectError> {
    let (wire_tx, wire_rx) = mpsc::unbounded_channel::<Wire>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    match transport {
        Transport::Local(endpoint) => {
            let (mut r, mut w) = local_stream(&endpoint).await.map_err(|e| ConnectError::Unavailable(e.to_string()))?;
            tokio::task::spawn_local(async move {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match r.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if wire_tx.send(Wire::Bytes(buf[..n].to_vec())).is_err() {
                                return;
                            }
                        }
                    }
                }
                let _ = wire_tx.send(Wire::Closed);
            });
            tokio::task::spawn_local(async move {
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        Cmd::Frame(f) => {
                            if w.write_all(&f).await.is_err() {
                                break;
                            }
                        }
                        Cmd::Close => break,
                    }
                }
                let _ = w.shutdown().await;
            });
            Ok((wire_rx, cmd_tx, false))
        }
        Transport::Ssh { link, command } => {
            let channel = link.exec_stream(&command, false).await.map_err(|e| ConnectError::Failed(format!("{e:#}")))?;
            let (mut read, write) = channel.split();
            tokio::task::spawn_local(async move {
                loop {
                    match read.wait().await {
                        Some(ChannelMsg::Data { data }) => {
                            if wire_tx.send(Wire::Bytes(data.to_vec())).is_err() {
                                return;
                            }
                        }
                        Some(ChannelMsg::ExtendedData { data, .. }) => {
                            let _ = wire_tx.send(Wire::Stderr(data.to_vec()));
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            let _ = wire_tx.send(Wire::Exit(exit_status));
                        }
                        Some(ChannelMsg::Close) | None => break,
                        Some(_) => {}
                    }
                }
                let _ = wire_tx.send(Wire::Closed);
            });
            tokio::task::spawn_local(async move {
                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        Cmd::Frame(f) => {
                            if write.data_bytes(f).await.is_err() {
                                break;
                            }
                        }
                        Cmd::Close => break,
                    }
                }
                // 关 stdin：bridge 读到 EOF 就退出，holder 那边看到连接断开（会话照常）
                let _ = write.eof().await;
                let _ = write.close().await;
            });
            Ok((wire_rx, cmd_tx, true))
        }
    }
}

#[cfg(unix)]
async fn local_stream(
    endpoint: &str,
) -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Unpin>, Box<dyn tokio::io::AsyncWrite + Unpin>)> {
    let s = tokio::net::UnixStream::connect(endpoint).await?;
    let (r, w) = s.into_split();
    Ok((Box::new(r), Box::new(w)))
}

#[cfg(windows)]
async fn local_stream(
    endpoint: &str,
) -> std::io::Result<(Box<dyn tokio::io::AsyncRead + Unpin>, Box<dyn tokio::io::AsyncWrite + Unpin>)> {
    // ERROR_PIPE_BUSY：holder 在两次 accept 之间，下一个实例马上就建好（ipc.rs），等一下再连
    const ERROR_PIPE_BUSY: i32 = 231;
    let mut tries = 0;
    let pipe = loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint) {
            Ok(p) => break p,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 40 => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    };
    let (r, w) = tokio::io::split(pipe);
    Ok((Box::new(r), Box::new(w)))
}

/// 连上 holder、握手。成功后连接上的事都经 `cb` 送回会话核心
pub async fn connect(transport: Transport, cb: BackendCallbacks) -> Result<(Rc<SessiondBackend>, Welcome), ConnectError> {
    let (mut wire, cmd_tx, ssh) = open(transport).await?;
    let hello = Hello { proto_min: PROTO_MIN, proto_max: PROTO_MAX, role: Role::Server, client: format!("falcon-server/{FALCON_VERSION}") };
    let _ = cmd_tx.send(Cmd::Frame(ClientMsg::Hello(hello).encode()));
    let backend =
        Rc::new(SessiondBackend { tx: cmd_tx, next_req: Cell::new(1), foreground: Rc::new(RefCell::new(HashMap::new())) });
    let (welcome_tx, welcome_rx) = oneshot::channel::<Result<Welcome, ConnectError>>();
    let waiters = backend.foreground.clone();
    tokio::task::spawn_local(async move {
        let mut welcome_tx = Some(welcome_tx);
        let mut frames = FrameDecoder::new();
        let mut utf8 = Utf8Decoder::default();
        // SSH 上先要等到 bridge 的前导标记（之前的字节是登录 shell 的 rc 打出来的）
        let mut preamble: Option<Vec<u8>> = ssh.then(Vec::new);
        let mut stderr: Vec<u8> = Vec::new();
        let mut exit_code: Option<u32> = None;
        'outer: while let Some(w) = wire.recv().await {
            let bytes = match w {
                Wire::Bytes(b) => b,
                Wire::Stderr(b) => {
                    if stderr.len() < STDERR_KEEP {
                        stderr.extend_from_slice(&b[..b.len().min(STDERR_KEEP - stderr.len())]);
                    }
                    continue;
                }
                Wire::Exit(c) => {
                    exit_code = Some(c);
                    continue;
                }
                Wire::Closed => break,
            };
            let bytes = match &mut preamble {
                Some(scan) => {
                    scan.extend_from_slice(&bytes);
                    match scan.windows(BRIDGE_PREAMBLE.len()).position(|w| w == BRIDGE_PREAMBLE) {
                        Some(i) => {
                            let rest = scan[i + BRIDGE_PREAMBLE.len()..].to_vec();
                            preamble = None;
                            rest
                        }
                        None => {
                            // 只留可能是标记开头的尾巴，rc 打得再多也不攒
                            let keep = scan.len().saturating_sub(BRIDGE_PREAMBLE.len());
                            scan.drain(..keep);
                            continue;
                        }
                    }
                }
                None => bytes,
            };
            frames.push(&bytes);
            loop {
                let (k, p) = match frames.next_frame() {
                    Ok(Some(f)) => f,
                    Ok(None) => break,
                    Err(e) => {
                        log::warn!("守护进程连接上的坏帧：{e}");
                        break 'outer;
                    }
                };
                let msg = match HolderMsg::decode(k, p) {
                    Ok(m) => m,
                    Err(e) => {
                        log::warn!("守护进程发来解不开的帧 0x{k:02x}：{e:#}");
                        continue;
                    }
                };
                match msg {
                    HolderMsg::Welcome(w) => {
                        if let Some(tx) = welcome_tx.take() {
                            let _ = tx.send(Ok(w));
                        }
                    }
                    // 握手完成前到的东西不交给会话核心（还没有它的位置）
                    _ if welcome_tx.is_some() => {}
                    HolderMsg::Output { bytes, .. } => {
                        let s = utf8.write(&bytes);
                        if !s.is_empty() {
                            (cb.on_data)(s);
                        }
                    }
                    HolderMsg::Synced { snapshot, continuation, .. } => {
                        (cb.on_event)(BackendEvent::Synced(replay_text(&snapshot, &continuation, utf8.pending())));
                    }
                    HolderMsg::Status { records } => (cb.on_event)(BackendEvent::ProgramStatus(records)),
                    HolderMsg::ForegroundIs { req, name } => {
                        if let Some(tx) = waiters.borrow_mut().remove(&req) {
                            let _ = tx.send(name);
                        }
                    }
                    HolderMsg::Event(Event::VtRestarted { .. }) => (cb.on_event)(BackendEvent::VtRestarted),
                    HolderMsg::Event(Event::VtDegraded) => (cb.on_event)(BackendEvent::VtDegraded),
                    HolderMsg::Exit(s) => (cb.on_event)(BackendEvent::Exit(s)),
                    HolderMsg::Error { message } => log::warn!("守护进程报错：{message}"),
                    HolderMsg::Upgraded { ok, message, .. } => {
                        if ok {
                            log::info!("VT 进程已热升级 {message}");
                        } else {
                            log::warn!("VT 进程热升级失败：{message}");
                        }
                    }
                    _ => {}
                }
            }
        }
        // 连接结束。SSH 上 ExitStatus 可能在 Close 之后才被读到：再收一会儿
        if ssh && exit_code.is_none() {
            let deadline = tokio::time::Instant::now() + Duration::from_millis(300);
            while let Ok(Some(w)) = tokio::time::timeout_at(deadline, wire.recv()).await {
                if let Wire::Exit(c) = w {
                    exit_code = Some(c);
                    break;
                }
            }
        }
        match welcome_tx.take() {
            Some(tx) => {
                let _ = tx.send(Err(if ssh { bridge_error(exit_code, &stderr) } else { ConnectError::Failed("holder 关闭了连接".into()) }));
            }
            None => {
                if ssh {
                    (cb.on_event)(BackendEvent::BridgeExit(exit_code));
                }
                (cb.on_exit)();
            }
        }
    });
    match tokio::time::timeout(HANDSHAKE_TIMEOUT, welcome_rx).await {
        Ok(Ok(Ok(w))) => Ok((backend, w)),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(_)) => Err(ConnectError::Failed("连接在握手前就关闭了".into())),
        Err(_) => {
            backend.destroy();
            Err(ConnectError::Failed("守护进程握手超时".into()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_drops_the_half_character_still_in_the_decoder() {
        let zhong = "中".as_bytes();
        // continuation = 半截 OSC + 半个"中"；解码器里压着同样的半个字符
        let mut cont = b"\x1b]0;ti".to_vec();
        cont.extend_from_slice(&zhong[..2]);
        assert_eq!(replay_text(b"snap", &cont, &zhong[..2]), "snap\x1b]0;ti");
        assert_eq!(replay_text(b"snap", b"\x1b[3", &[]), "snap\x1b[3");
        assert_eq!(replay_text(b"snap", b"", &zhong[..1]), "snap");
    }

    #[test]
    fn bridge_exit_codes_map_to_reasons() {
        assert_eq!(bridge_error(Some(3), b""), ConnectError::Gone);
        assert_eq!(bridge_error(Some(5), b""), ConnectError::Exited);
        assert!(matches!(bridge_error(Some(4), b"x"), ConnectError::Unavailable(_)));
        assert!(matches!(bridge_error(Some(127), b"sh: falcon-sessiond: not found"), ConnectError::Failed(m) if m.contains("not found")));
        assert!(matches!(bridge_error(None, b""), ConnectError::Failed(_)));
    }
}
