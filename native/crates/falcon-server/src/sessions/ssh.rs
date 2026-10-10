//! 一个 SSH 项目（或已保存主机）与远端之间的连接。移植自 `packages/server/src/sessions/ssh.ts`。
//!
//! 同项目的多个会话复用同一条连接（多 channel）。连接断开时通知 `on_down` 的订阅者，
//! 重连由 SessionManager 编排。这一份是传输层（S3）：连接、首次连接记指纹（TOFU）、认证、
//! exec / 带 stdin 的 exec / 流式通道、正反两向的端口转发。探测、Zellij 安装与会话附着在
//! 同目录的 `ssh_zellij.rs`。
//!
//! 库用 russh（设计文档决定四）：一条连接上要同时跑多路 PTY、转发与探测，libssh2 的 Session
//! 不是线程安全的，做不到。与 Node 版 ssh2 必须对齐的几处：
//! - **TOFU 指纹格式**：`hex(sha256(主机公钥的 SSH wire blob))`，即 ssh2 的 `hostHash: "sha256"`。
//!   格式一变，所有已保存的主机都会报指纹不符（测试里有与 ssh2 实测对拍的用例）；
//! - 握手超时 15s，keepalive 15s × 3；
//! - 远端转发**先登记 acceptor 再 tcpip-forward**，入站连接按监听端口分发，不按目标 IP
//!   （sshd 回报的目标 IP 可能是 127.0.0.1 / 0.0.0.0 / 实际网卡）。
//!
//! 链路对象跑在会话核心的 LocalSet 上（`Rc`），russh 的会话任务自己在 tokio 上跑。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt as _;
use futures::future::Shared;
use russh::client::{self, Msg};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::crypto::SecretBox;
use crate::db::{Db, ProjectRow};
use crate::exec::{Exec, ExecResult, LocalBoxFuture};

/// 握手（含认证）的超时，同 ssh2 的 readyTimeout
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_MAX: usize = 3;

/// 链路故障。可克隆：并发的 `get_client` 共享同一次连接尝试的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkError {
    pub message: String,
    /// 主机密钥指纹与首次连接时记录的不一致
    pub host_key_mismatch: bool,
}

impl LinkError {
    fn new(message: impl Into<String>) -> Self {
        LinkError { message: message.into(), host_key_mismatch: false }
    }
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LinkError {}

/// `hex(sha256(blob))`：与 ssh2 `hostHash: "sha256"` 的 hostVerifier 入参同一个值
pub fn host_key_fingerprint(key: &PublicKeyOrCertificate) -> String {
    let blob = match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => key.to_bytes(),
        PublicKeyOrCertificate::Certificate(cert) => cert.to_bytes(),
    };
    hex::encode(Sha256::digest(blob.unwrap_or_default()))
}

/// 远端转发进来的连接按监听端口分发给对应的订阅者
type Acceptors = Arc<Mutex<HashMap<u32, mpsc::UnboundedSender<Channel<Msg>>>>>;

struct Handler {
    db: Arc<Db>,
    host: String,
    port: u16,
    mismatch: Arc<AtomicBool>,
    acceptors: Acceptors,
    down: Option<oneshot::Sender<()>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let hash = host_key_fingerprint(key);
        match self.db.get_known_host(&self.host, self.port) {
            None => {
                // TOFU：首次连接记录指纹
                self.db.save_known_host(&self.host, self.port, &hash);
                Ok(true)
            }
            Some(known) if known == hash => Ok(true),
            Some(_) => {
                self.mismatch.store(true, Ordering::SeqCst);
                Ok(false)
            }
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        _connected_address: &str,
        connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let target = self.acceptors.lock().unwrap_or_else(|e| e.into_inner()).get(&connected_port).cloned();
        match target {
            Some(tx) => {
                reply.accept().await;
                let _ = tx.send(channel);
            }
            // 没人登记这个端口：丢掉 reply 即拒绝
            None => drop(reply),
        }
        Ok(())
    }

    async fn disconnected(&mut self, reason: client::DisconnectReason<Self::Error>) -> Result<(), Self::Error> {
        if let Some(tx) = self.down.take() {
            let _ = tx.send(());
        }
        match reason {
            client::DisconnectReason::ReceivedDisconnect(_) => Ok(()),
            client::DisconnectReason::Error(e) => Err(e),
        }
    }
}

/// 一条建好的连接
pub struct Connection {
    handle: client::Handle<Handler>,
}

type Connecting = Shared<LocalBoxFuture<'static, Result<Rc<Connection>, LinkError>>>;

pub struct SshLink {
    project: RefCell<ProjectRow>,
    db: Arc<Db>,
    secrets: Arc<SecretBox>,
    conn: RefCell<Option<Rc<Connection>>>,
    connecting: RefCell<Option<Connecting>>,
    acceptors: Acceptors,
    /// 远端 127.0.0.1 上 askpass 反连 Falcon 的端口；链路断开后作废
    pub(crate) askpass_port: Cell<Option<u16>>,
    on_up: RefCell<Vec<Rc<dyn Fn()>>>,
    on_down: RefCell<Vec<Rc<dyn Fn()>>>,
    /// 探测与 Zellij 的缓存（ssh_zellij.rs）
    pub(crate) remote: RefCell<super::ssh_zellij::RemoteCache>,
    me: Weak<SshLink>,
}

impl SshLink {
    pub fn new(project: ProjectRow, db: Arc<Db>, secrets: Arc<SecretBox>) -> Rc<Self> {
        Rc::new_cyclic(|me| SshLink {
            project: RefCell::new(project),
            db,
            secrets,
            conn: RefCell::new(None),
            connecting: RefCell::new(None),
            acceptors: Arc::default(),
            askpass_port: Cell::new(None),
            on_up: RefCell::default(),
            on_down: RefCell::default(),
            remote: RefCell::default(),
            me: me.clone(),
        })
    }

    pub fn project(&self) -> ProjectRow {
        self.project.borrow().clone()
    }

    pub fn update_project(&self, row: ProjectRow) {
        *self.project.borrow_mut() = row;
    }

    pub(crate) fn db(&self) -> &Arc<Db> {
        &self.db
    }

    /// 连上（含重连成功）时调用
    pub fn on_up(&self, f: impl Fn() + 'static) {
        self.on_up.borrow_mut().push(Rc::new(f));
    }

    /// 已连上的链路断开时调用（`dispose` 不触发）
    pub fn on_down(&self, f: impl Fn() + 'static) {
        self.on_down.borrow_mut().push(Rc::new(f));
    }

    fn emit(list: &RefCell<Vec<Rc<dyn Fn()>>>) {
        // 先拷一份：回调里可能再订阅 / dispose
        let fns: Vec<_> = list.borrow().clone();
        for f in fns {
            f();
        }
    }

    pub fn is_connected(&self) -> bool {
        self.conn.borrow().is_some()
    }

    /// 拿到连接；没连就连，正在连就并入同一次尝试
    pub async fn get_client(&self) -> Result<Rc<Connection>, LinkError> {
        if let Some(c) = self.conn.borrow().clone() {
            return Ok(c);
        }
        let pending = self.connecting.borrow().clone();
        let fut = match pending {
            Some(f) => f,
            None => {
                let me = self.me.upgrade().expect("链路还活着");
                let fut: LocalBoxFuture<'static, _> = Box::pin(async move {
                    let r = me.connect().await;
                    me.connecting.borrow_mut().take();
                    r
                });
                let shared = fut.shared();
                *self.connecting.borrow_mut() = Some(shared.clone());
                shared
            }
        };
        fut.await
    }

    async fn connect(&self) -> Result<Rc<Connection>, LinkError> {
        let p = self.project();
        let host = p.ssh_host.clone().unwrap_or_default();
        let port = p.ssh_port.unwrap_or(22) as u16;
        let username = p.ssh_username.clone().unwrap_or_default();
        let secret = match &p.ssh_secret_enc {
            Some(enc) => Some(self.secrets.decrypt(enc).map_err(|e| LinkError::new(format!("{e:#}")))?),
            None => None,
        };

        let mismatch = Arc::new(AtomicBool::new(false));
        let (down_tx, down_rx) = oneshot::channel();
        let handler = Handler {
            db: self.db.clone(),
            host: host.clone(),
            port,
            mismatch: mismatch.clone(),
            acceptors: self.acceptors.clone(),
            down: Some(down_tx),
        };
        let config = Arc::new(client::Config {
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: KEEPALIVE_MAX,
            inactivity_timeout: None,
            nodelay: true,
            ..Default::default()
        });

        let attempt = async {
            let mut handle = client::connect(config, (host.as_str(), port), handler).await?;
            authenticate(&mut handle, &p, &username, secret.as_deref()).await?;
            anyhow::Ok(handle)
        };
        let handle = match tokio::time::timeout(READY_TIMEOUT, attempt).await {
            Ok(Ok(h)) => h,
            _ if mismatch.load(Ordering::SeqCst) => {
                return Err(LinkError {
                    message: format!("主机密钥指纹与首次连接时记录的不一致（{host}），已拒绝连接"),
                    host_key_mismatch: true,
                });
            }
            Ok(Err(e)) => return Err(LinkError::new(format!("{e:#}"))),
            Err(_) => return Err(LinkError::new("Timed out while waiting for handshake")),
        };

        let conn = Rc::new(Connection { handle });
        *self.conn.borrow_mut() = Some(conn.clone());
        // 断线：只认这一条还是当前连接的时候（dispose 之后的旧连接断开不算）
        let me = self.me.clone();
        let watched = Rc::downgrade(&conn);
        tokio::task::spawn_local(async move {
            let _ = down_rx.await;
            let Some(link) = me.upgrade() else { return };
            let current = link.conn.borrow().as_ref().is_some_and(|c| watched.upgrade().is_some_and(|w| Rc::ptr_eq(c, &w)));
            if current {
                link.conn.borrow_mut().take();
                link.askpass_port.set(None);
                Self::emit(&link.on_down);
            }
        });
        Self::emit(&self.on_up);
        Ok(conn)
    }

    pub fn dispose(&self) {
        self.conn.borrow_mut().take();
        self.connecting.borrow_mut().take();
        self.askpass_port.set(None);
        self.acceptors.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // 丢掉 Handle 即断开：russh 的会话任务在发送端关闭后自行收尾
    }

    // ---- 远端命令 ----

    /// 同 [`Exec::exec`]，但把 `input` 写进远端命令的 stdin，写完即 EOF（粘贴图片、
    /// `zellij pipe` 这类要读到 EOF 才退出的命令）
    pub async fn exec_with_input(&self, command_line: &str, input: &[u8]) -> anyhow::Result<ExecResult> {
        let conn = self.get_client().await?;
        let channel = conn.handle.channel_open_session().await?;
        channel.exec(true, command_line.as_bytes()).await?;
        channel.data_bytes(bytes::Bytes::copy_from_slice(input)).await?;
        channel.eof().await?;
        Ok(collect(channel, None).await)
    }

    /// 同 exec，但把通道原样交给调用方：写入端是命令的 stdin，读取端是 stdout，`ExitStatus`
    /// 带退出码。文件下载 / 上传按流走，"攒成字符串"装不下一个几百 MB 的文件。
    ///
    /// pty：给命令分一个伪终端。常驻的远端服务（px0，ADR 0017）要它——没有 pty 时
    /// 通道关了 sshd 也不杀进程，有 pty 时通道一关进程就收到 SIGHUP。代价是 stderr
    /// 并进 stdout、换行变 \r\n。
    pub async fn exec_stream(&self, command_line: &str, pty: bool) -> anyhow::Result<Channel<Msg>> {
        let conn = self.get_client().await?;
        let channel = conn.handle.channel_open_session().await?;
        if pty {
            channel.request_pty(true, "dumb", 200, 50, 0, 0, &[]).await?;
        }
        channel.exec(true, command_line.as_bytes()).await?;
        Ok(channel)
    }

    /// 交互式 PTY 通道（会话附着用）：先申请 pty 再 exec
    pub async fn exec_pty(&self, command_line: &str, cols: u16, rows: u16) -> anyhow::Result<Channel<Msg>> {
        let conn = self.get_client().await?;
        let channel = conn.handle.channel_open_session().await?;
        channel.request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[]).await?;
        channel.exec(true, command_line.as_bytes()).await?;
        Ok(channel)
    }

    /// direct-tcpip：经远端连它那边的 `dest_host:dest_port`（本地转发、远端公网发布、px0 反代）
    pub async fn forward_out(&self, dest_host: &str, dest_port: u16) -> anyhow::Result<Channel<Msg>> {
        let conn = self.get_client().await?;
        Ok(conn.handle.channel_open_direct_tcpip(dest_host, dest_port as u32, "127.0.0.1", 0).await?)
    }

    /// 远端监听 `bind_host:bind_port`，进来的连接从返回的接收端里出来。
    /// 必须先登记 acceptor 再 tcpip-forward，否则握手窗口里的连接会被拒。
    pub async fn add_remote_forward(
        &self,
        bind_host: &str,
        bind_port: u16,
    ) -> anyhow::Result<mpsc::UnboundedReceiver<Channel<Msg>>> {
        let conn = self.get_client().await?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.acceptors.lock().unwrap_or_else(|e| e.into_inner()).insert(bind_port as u32, tx);
        if let Err(e) = conn.handle.tcpip_forward(bind_host, bind_port as u32).await {
            self.acceptors.lock().unwrap_or_else(|e| e.into_inner()).remove(&(bind_port as u32));
            return Err(e.into());
        }
        Ok(rx)
    }

    pub async fn remove_remote_forward(&self, bind_host: &str, bind_port: u16) {
        self.acceptors.lock().unwrap_or_else(|e| e.into_inner()).remove(&(bind_port as u32));
        let conn = self.conn.borrow().clone();
        if let Some(conn) = conn {
            let _ = conn.handle.cancel_tcpip_forward(bind_host, bind_port as u32).await;
        }
    }
}

impl Exec for SshLink {
    fn exec<'a>(
        &'a self,
        command_line: &'a str,
        cancel: Option<&'a CancellationToken>,
    ) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
        Box::pin(async move {
            let conn = self.get_client().await?;
            let channel = conn.handle.channel_open_session().await?;
            channel.exec(true, command_line.as_bytes()).await?;
            Ok(collect(channel, cancel).await)
        })
    }
}

/// 收完一条 exec 通道：stdout / stderr 攒字节、通道关闭时一次解码（逐块解码会把跨包的
/// UTF-8 多字节序列切成 U+FFFD——中文路径 / dump-screen 必踩）。退出码来自 exit-status，
/// 被信号杀掉的没有（`None`，同 ssh2 的 null）
async fn collect(mut channel: Channel<Msg>, cancel: Option<&CancellationToken>) -> ExecResult {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut code = None;
    loop {
        let msg = match cancel {
            Some(token) => tokio::select! {
                m = channel.wait() => m,
                _ = token.cancelled() => {
                    let _ = channel.close().await;
                    break;
                }
            },
            None => channel.wait().await,
        };
        match msg {
            Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.extend_from_slice(&data),
            Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status as i32),
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    ExecResult {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

async fn authenticate(
    handle: &mut client::Handle<Handler>,
    p: &ProjectRow,
    username: &str,
    secret: Option<&str>,
) -> anyhow::Result<()> {
    let method = p.ssh_auth_method.as_deref().unwrap_or("");
    let ok = match method {
        "key" => {
            let path = p.ssh_key_path.as_deref().unwrap_or("");
            let pem = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("读私钥 {path} 失败：{e}"))?;
            let key = russh::keys::decode_secret_key(&pem, secret)?;
            let hash = rsa_hash(handle, &key).await;
            handle
                .authenticate_publickey(username, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
                .success()
        }
        "password" => handle.authenticate_password(username, secret.unwrap_or("")).await?.success(),
        "agent" => authenticate_agent(handle, username).await?,
        other => anyhow::bail!("不支持的认证方式：{other}"),
    };
    if !ok {
        anyhow::bail!("All configured authentication methods failed");
    }
    Ok(())
}

/// RSA 钥用服务端声明支持的哈希（rsa-sha2-512 / 256），不支持扩展协商的老服务端退回 ssh-rsa
async fn rsa_hash(handle: &client::Handle<Handler>, key: &russh::keys::PrivateKey) -> Option<HashAlg> {
    if !key.algorithm().is_rsa() {
        return None;
    }
    handle.best_supported_rsa_hash().await.ok().flatten().flatten()
}

async fn authenticate_agent(handle: &mut client::Handle<Handler>, username: &str) -> anyhow::Result<bool> {
    #[cfg(unix)]
    let mut agent = russh::keys::agent::client::AgentClient::connect_env().await?;
    #[cfg(windows)]
    let mut agent = match std::env::var("SSH_AUTH_SOCK") {
        Ok(path) => russh::keys::agent::client::AgentClient::connect_named_pipe(path).await?,
        Err(_) => russh::keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await?,
    };
    for identity in agent.request_identities().await? {
        let key = identity.public_key().into_owned();
        let hash = if key.algorithm().is_rsa() { handle.best_supported_rsa_hash().await.ok().flatten().flatten() } else { None };
        match handle.authenticate_publickey_with(username, key, hash, &mut agent).await {
            Ok(r) if r.success() => return Ok(true),
            Ok(_) => continue,
            Err(e) => log::debug!("agent 钥匙认证失败：{e:?}"),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_sha256_of_wire_blob() {
        // 任取一把 ed25519 公钥：ssh2 的 hostHash 是对 base64 解出来的 blob 做 sha256
        let line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl test";
        let key = russh::keys::PublicKey::from_openssh(line).unwrap();
        use base64::Engine as _;
        let blob = base64::engine::general_purpose::STANDARD.decode(line.split(' ').nth(1).unwrap()).unwrap();
        let expected = hex::encode(Sha256::digest(&blob));
        let ours = host_key_fingerprint(&PublicKeyOrCertificate::PublicKey { key, hash_alg: None });
        assert_eq!(ours, expected);
    }
}
