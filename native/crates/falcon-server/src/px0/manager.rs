//! 移植自 `packages/server/src/px0/manager.ts`。TS 没有它的单测；文件末的用例是 Rust 侧补的
//! （本地项目，用临时目录里的假 px0 脚本跑真 pty；SSH 那条要真远端，没有自动化）。
//!
//! px0 实例的生命周期（ADR 0017 决定三、五、六）。一个项目最多一个实例，只在内存里。
//!
//! 两边都让 px0 挂在一个 pty 上，靠挂断收尸：px0 没有鉴权，一个没人管的孤儿就是
//! 一个谁都能读仓库的服务，而 falcon 被 kill -9、崩溃、断网时我们没有机会去杀它。
//! - 本地项目：portable-pty 起在后端本机，听本机 127.0.0.1 的随机端口。后端进程一没，
//!   pty 主端随之关闭，px0 作为会话首进程收到 SIGHUP 退出（实测普通 spawn 会留孤儿）。
//!   **别改回普通 spawn**。
//! - SSH 项目：经项目链路开一条**带 pty 的** exec 通道跑 px0，通道一关（含断链）
//!   远端 sshd 挂断 pty。px0 听远端 127.0.0.1 的随机端口；反代的每条连接直接经
//!   forward_out 打过去，后端本机不另开监听端口——本机再多开一个回环端口就多一个
//!   能绕过 falcon 登录的口子。
//!
//! 打开入口页才拉起（[`Px0Manager::open`]），没有在途请求且 IDLE 内没有新请求就停：px0 页面开着
//! 时总挂着一条 SSE，所以这等于标签页关了。
//!
//! # 执行模型（与 TS 的差别）
//!
//! - 管理器跑在会话核心的单线程 LocalSet 上（Node 事件循环的语义），攥着 `Rc<SshLink>`，
//!   本身是 `!Send` 的：`open` / `stop` / `shutdown` / `connect` 都要在 LocalSet 上调。
//!   `open` 与空闲回收用 `spawn_local`。
//! - TS 的 `acquire` 交出实例、`release` 还回来；这里 [`Px0Manager::acquire`] 交出一张
//!   [`Px0Lease`]（`Send`，丢掉即 release），反代可以把它带到 axum 的处理线程上，跟着响应体
//!   一直活到响应写完 / 客户端断开。在途计数与最后使用时刻是原子量。
//! - 反代要的上游连接：[`Px0Manager::connect`]（LocalSet 上调，本地是一条 TcpStream，远端是
//!   一条 forward_out 通道）。本地实例还可以用 [`Px0Lease::local_addr`] 在任何线程上直接连。
//! - 本地 px0 的退出不靠读端 EOF 判断，而是单独一条线程 `wait()` 子进程：pty 从端的 fd
//!   要是被别的子进程继承走（macOS 上建 fd 与补 CLOEXEC 之间的竞争，见 meegle/client.rs 的
//!   `run_to_end`），EOF 就永远不来。
//! - 输出的尾巴按字节截（TS 按 UTF-16 码元），只是给起不来时看最后几行，差别无所谓。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Read as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use falcon_core::px0::px0_base_path;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use russh::client::Msg;
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf, Sig};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Notify, mpsc, watch};

use super::bin::{DownloadFn, PX0_BIN_ENV, ensure_local_px0, ensure_px0_asset, http_download_fn, px0_asset_present};
use super::command::{
    PX0_VERSION, Px0ArgsInput, expand_home, js, local_px0_target, parse_listen_port, parse_px0_version,
    posix_install_command, posix_launch_command, posix_version_command, px0_args, px0_target_from_uname,
    remote_px0_path, tail_lines,
};
use super::proxy::{Px0PageState, Px0Stage};
use crate::db::ProjectRow;
use crate::exec::{Exec as _, Utf8Decoder};
use crate::sessions::local::{BaseEnvFn, local_base_env_fn};
use crate::sessions::ssh::SshLink;
use crate::zellij::host::HostKind;

const LISTEN_WAIT: Duration = Duration::from_secs(30);
const STOP_GRACE: Duration = Duration::from_secs(2);
/// 远端：宽限期后关了通道，再等它真的关上最多这么久
const CLOSE_WAIT: Duration = Duration::from_millis(500);
const IDLE: Duration = Duration::from_secs(15 * 60);
const SWEEP: Duration = Duration::from_secs(60);
/// 起不来的原因留多久。入口页每秒刷新一次，留一分钟足够让用户看到；过了这个窗口
/// 再打开就当新的一次尝试，不让一次陈年失败永远挡在那里。
const ERROR_TTL: Duration = Duration::from_secs(60);
/// 启动输出只留尾巴：起不来时给用户看最后几行，px0 之后的日志没人看
const LOG_KEEP: usize = 16_384;
/// 本地 px0 退出（wait 返回）后，pty 里剩下的输出再等多久才算"退出了"：最后一批输出
/// （起不来的原因）要先进 LogTail
const EXIT_DRAIN: Duration = Duration::from_millis(100);

/// 反代拿到的上游连接（本地 TcpStream / 远端 forward_out 通道）
pub trait Px0Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> Px0Io for T {}
pub type Px0Stream = Box<dyn Px0Io>;

/// 在途请求数与最后使用时刻（空闲回收看它）。原子量：租约会被带到别的线程上丢掉
struct Usage {
    inflight: AtomicUsize,
    last_used: Mutex<Instant>,
}

impl Usage {
    fn new() -> Self {
        Usage { inflight: AtomicUsize::new(0), last_used: Mutex::new(Instant::now()) }
    }

    fn touch(&self) {
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn idle_at(&self, now: Instant) -> bool {
        let last = *self.last_used.lock().unwrap_or_else(|e| e.into_inner());
        self.inflight.load(Ordering::SeqCst) == 0 && now.saturating_duration_since(last) > IDLE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Upstream {
    /// 后端本机 127.0.0.1 上的端口
    Local(u16),
    /// 宿主机 127.0.0.1 上的端口，经项目链路 forward_out 过去
    Remote(u16),
}

/// 一次反代请求对某个跑着的 px0 的占用（TS 的 `acquire` / `release` 那一对）。
/// 丢掉即 release：响应写完、客户端断开都会走到这里。`Send`，可以跟着响应体走
pub struct Px0Lease {
    project_id: String,
    instance: u64,
    upstream: Upstream,
    usage: Arc<Usage>,
}

impl Px0Lease {
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// 本地实例的监听地址：反代可以在任何线程上直接连，不必回 LocalSet。远端实例为 None
    /// （走 [`Px0Manager::connect`]）
    pub fn local_addr(&self) -> Option<SocketAddr> {
        match self.upstream {
            Upstream::Local(port) => Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
            Upstream::Remote(_) => None,
        }
    }
}

impl Drop for Px0Lease {
    fn drop(&mut self) {
        // Math.max(0, inflight - 1)
        let _ = self.usage.inflight.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1)));
        self.usage.touch();
    }
}

/// 本机 pty 上的 px0
struct LocalProc {
    pid: Option<u32>,
    /// wait 已经返回（pid 可能被复用了，别再往它发信号）
    reaped: Arc<AtomicBool>,
    killer: RefCell<Box<dyn ChildKiller + Send + Sync>>,
    /// 主端留着：它一关 px0 就收到 SIGHUP（后端退出时正是靠这个收尸）
    _master: Box<dyn MasterPty + Send>,
}

/// 远端带 pty 的 exec 通道上的 px0
struct RemoteProc {
    link: Rc<SshLink>,
    write: ChannelWriteHalf<Msg>,
}

enum Proc {
    Local(LocalProc),
    Remote(RemoteProc),
}

/// 一个跑着的 px0
struct Live {
    project_id: String,
    /// 实例编号：租约认它，同一项目停了再起的是另一个实例
    id: u64,
    upstream: Upstream,
    usage: Arc<Usage>,
    /// 我们主动停的：退出时不要记成故障
    stopping: Cell<bool>,
    /// 已经退出了（可能早于登记进 live）
    dead: Cell<bool>,
    proc: Proc,
    exited: watch::Receiver<bool>,
    log: Rc<RefCell<LogTail>>,
}

#[derive(Debug)]
struct Starting {
    stage: Cell<Px0Stage>,
    cancelled: Cell<bool>,
}

struct ErrorEntry {
    message: String,
    at: Instant,
}

/// 起实例的失败：被取消（不记错误）或给用户看的原因
enum StartError {
    Cancelled,
    Failed(String),
}

impl From<String> for StartError {
    fn from(message: String) -> Self {
        StartError::Failed(message)
    }
}

fn failed(e: anyhow::Error) -> StartError {
    StartError::Failed(format!("{e:#}"))
}

/// 可注入的外部依赖。生产上是 [`Px0Options::from_env`]
pub struct Px0Options {
    /// FALCON_PX0_BIN：本机用自己的 px0（不校验版本），调试用
    pub local_bin: Option<String>,
    /// 本机 px0 的基底环境（login shell 解析出来的 PATH，px0 才找得到 claude / gh）
    pub base_env: BaseEnvFn,
    pub download: DownloadFn,
}

impl Px0Options {
    pub fn from_env() -> Self {
        Px0Options {
            local_bin: std::env::var(PX0_BIN_ENV).ok().filter(|v| !v.is_empty()),
            base_env: local_base_env_fn(),
            download: http_download_fn(),
        }
    }
}

pub struct Px0Manager {
    data_dir: PathBuf,
    link_for: Rc<dyn Fn(&ProjectRow) -> Rc<SshLink>>,
    opts: Px0Options,
    live: RefCell<HashMap<String, Rc<Live>>>,
    starting: RefCell<HashMap<String, Rc<Starting>>>,
    errors: RefCell<HashMap<String, ErrorEntry>>,
    sweeper: RefCell<Option<tokio::task::JoinHandle<()>>>,
    next_id: Cell<u64>,
    me: Weak<Px0Manager>,
}

impl Px0Manager {
    /// 生产配置。要在 LocalSet 上用（见文件头）；空闲回收在第一次 `open` 时才开始转，
    /// 构造本身不要求 LocalSet
    pub fn new(data_dir: PathBuf, link_for: Rc<dyn Fn(&ProjectRow) -> Rc<SshLink>>) -> Rc<Self> {
        Self::with_options(data_dir, link_for, Px0Options::from_env())
    }

    pub fn with_options(data_dir: PathBuf, link_for: Rc<dyn Fn(&ProjectRow) -> Rc<SshLink>>, opts: Px0Options) -> Rc<Self> {
        Rc::new_cyclic(|me| Px0Manager {
            data_dir,
            link_for,
            opts,
            live: RefCell::default(),
            starting: RefCell::default(),
            errors: RefCell::default(),
            sweeper: RefCell::new(None),
            next_id: Cell::new(0),
            me: me.clone(),
        })
    }

    fn rc(&self) -> Rc<Self> {
        self.me.upgrade().expect("管理器还活着（方法是从 Rc 上调的）")
    }

    /// 入口页调用。跑着就返回 None（调用方直接反代）；否则按需拉起，返回该给用户看的
    /// 页面状态。retry：用户点了重试，无视还没过期的失败。
    pub fn open(&self, row: &ProjectRow, retry: bool) -> Option<Px0PageState> {
        if self.live.borrow().contains_key(&row.id) {
            return None;
        }
        if let Some(st) = self.starting.borrow().get(&row.id) {
            return Some(Px0PageState::Starting { stage: st.stage.get() });
        }
        if let Some(err) = self.errors.borrow().get(&row.id)
            && !retry
            && err.at.elapsed() < ERROR_TTL
        {
            return Some(Px0PageState::Error { message: err.message.clone() });
        }
        self.errors.borrow_mut().remove(&row.id);
        self.ensure_sweeper();

        let state = Rc::new(Starting { stage: Cell::new(Px0Stage::Preparing), cancelled: Cell::new(false) });
        // TS 的 startNow 在第一个 await 之前就把本地项目的阶段定了（下载 / 启动），入口页第一眼
        // 看到的就是它；spawn_local 要等下一轮才跑，这里先照样定好
        if row.project_type == "local" && row.working_dir.as_deref().map(js::trim).is_some_and(|d| !d.is_empty()) {
            state.stage.set(self.local_stage());
        }
        self.starting.borrow_mut().insert(row.id.clone(), state.clone());
        let me = self.rc();
        let row = row.clone();
        let st = state.clone();
        tokio::task::spawn_local(async move {
            if let Err(StartError::Failed(message)) = me.start_now(&row, &st).await
                && !st.cancelled.get()
            {
                me.errors.borrow_mut().insert(row.id.clone(), ErrorEntry { message, at: Instant::now() });
            }
            // finally：只清自己那条
            let mut starting = me.starting.borrow_mut();
            if starting.get(&row.id).is_some_and(|s| Rc::ptr_eq(s, &st)) {
                starting.remove(&row.id);
            }
        });
        Some(Px0PageState::Starting { stage: state.stage.get() })
    }

    /// 反代每个请求开始时调用：跑着就记一次使用并交出租约，没跑返回 None
    pub fn acquire(&self, project_id: &str) -> Option<Px0Lease> {
        let live = self.live.borrow().get(project_id).cloned()?;
        live.usage.inflight.fetch_add(1, Ordering::SeqCst);
        live.usage.touch();
        Some(Px0Lease { project_id: live.project_id.clone(), instance: live.id, upstream: live.upstream, usage: live.usage.clone() })
    }

    /// 给这次请求开一条到 px0 的新连接（每个请求一条，不进 keep-alive 池：远端的连接是
    /// 一条 forward_out 通道）。租约所指的实例已经停了（远端）就报错，反代回 502
    pub async fn connect(&self, lease: &Px0Lease) -> std::io::Result<Px0Stream> {
        match lease.upstream {
            Upstream::Local(port) => Ok(Box::new(tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await?)),
            Upstream::Remote(port) => {
                let link = self
                    .live
                    .borrow()
                    .get(&lease.project_id)
                    .filter(|l| l.id == lease.instance)
                    .and_then(|l| match &l.proc {
                        Proc::Remote(r) => Some(r.link.clone()),
                        Proc::Local(_) => None,
                    });
                let Some(link) = link else {
                    return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "px0 已停止"));
                };
                let channel = link.forward_out("127.0.0.1", port).await.map_err(std::io::Error::other)?;
                Ok(Box::new(channel.into_stream()))
            }
        }
    }

    /// 停掉某个项目的 px0（项目删了 / 工作目录改了 / 存档了）。没在跑也不报错
    pub async fn stop(&self, project_id: &str) {
        if let Some(st) = self.starting.borrow_mut().remove(project_id) {
            st.cancelled.set(true);
        }
        self.errors.borrow_mut().remove(project_id);
        let live = self.live.borrow_mut().remove(project_id);
        if let Some(live) = live {
            teardown(&live).await;
        }
    }

    /// 后端退出前调用：停掉所有实例（附录 A：后端退出时必须杀掉 px0）
    pub async fn shutdown(&self) {
        if let Some(sweeper) = self.sweeper.borrow_mut().take() {
            sweeper.abort();
        }
        for st in self.starting.borrow().values() {
            st.cancelled.set(true);
        }
        self.starting.borrow_mut().clear();
        let lives: Vec<Rc<Live>> = self.live.borrow_mut().drain().map(|(_, l)| l).collect();
        futures::future::join_all(lives.iter().map(|l| teardown(l))).await;
    }

    fn ensure_sweeper(&self) {
        if self.sweeper.borrow().is_some() {
            return;
        }
        let me = self.me.clone();
        let handle = tokio::task::spawn_local(async move {
            let mut tick = tokio::time::interval(SWEEP);
            // interval 的第一下立刻就到；setInterval 是满一个周期才第一次触发
            tick.tick().await;
            loop {
                tick.tick().await;
                let Some(me) = me.upgrade() else { return };
                me.sweep(Instant::now());
            }
        });
        *self.sweeper.borrow_mut() = Some(handle);
    }

    fn sweep(&self, now: Instant) {
        let idle: Vec<String> =
            self.live.borrow().iter().filter(|(_, l)| l.usage.idle_at(now)).map(|(id, _)| id.clone()).collect();
        for id in idle {
            let me = self.rc();
            tokio::task::spawn_local(async move { me.stop(&id).await });
        }
    }

    async fn start_now(&self, row: &ProjectRow, state: &Starting) -> Result<(), StartError> {
        let Some(dir) = row.working_dir.as_deref().map(js::trim).filter(|d| !d.is_empty()) else {
            return Err("这个项目没有工作目录。多仓库容器请在成员或附属项目上打开 px0。".to_string().into());
        };
        let base_path = px0_base_path(&row.id);
        let live = if row.project_type == "local" {
            self.start_local(&row.id, dir, &base_path, state).await?
        } else {
            self.start_remote(row, dir, &base_path, state).await?
        };
        if state.cancelled.get() {
            teardown(&live).await;
            return Err(StartError::Cancelled);
        }
        // 刚起来就退出了：on_died 已经把原因记下，别拿一句更含糊的覆盖掉
        if live.dead.get() {
            return Ok(());
        }
        self.live.borrow_mut().insert(row.id.clone(), live);
        Ok(())
    }

    fn next_instance(&self) -> u64 {
        self.next_id.set(self.next_id.get() + 1);
        self.next_id.get()
    }

    /// 本地项目起步时的阶段：本机已有资产（或 FALCON_PX0_BIN）是「启动」，否则「下载」
    fn local_stage(&self) -> Px0Stage {
        let env_bin = self.opts.local_bin.as_deref();
        let present = local_px0_target(None, None).is_some_and(|t| px0_asset_present(&self.data_dir, t, env_bin));
        if present { Px0Stage::Launching } else { Px0Stage::Downloading }
    }

    async fn start_local(&self, project_id: &str, dir: &str, base_path: &str, state: &Starting) -> Result<Rc<Live>, StartError> {
        let env_bin = self.opts.local_bin.as_deref();
        state.stage.set(self.local_stage());
        let bin = ensure_local_px0(&self.data_dir, env_bin, &self.opts.download).await.map_err(|e| e.0)?;
        check_cancelled(state)?;
        state.stage.set(Px0Stage::Launching);
        // 与 cloudflared 同一份基底环境：login shell 解析出来的 PATH，px0 才找得到 claude / gh
        let base = (self.opts.base_env)().await;
        check_cancelled(state)?;
        let args = px0_args(&Px0ArgsInput { base_path: base_path.to_string(), dir: dir.to_string() });
        let (proc, events) = spawn_pty(&bin, &args, &base).map_err(|e| format!("px0 没能启动：{e:#}"))?;

        let log = Rc::new(RefCell::new(LogTail::default()));
        let data = Rc::new(Notify::new());
        let (exit_tx, exited) = watch::channel(false);
        // 之后输出一直被读走——不读的话 pty 缓冲写满，px0 下一次打日志就会卡住
        tokio::task::spawn_local(pump_local(events, log.clone(), data.clone(), exit_tx));

        let port = match wait_for_port(&log, &data, exited.clone()).await {
            Ok(port) => port,
            Err(message) => {
                kill_local(&proc, exited).await;
                return Err(message.into());
            }
        };
        let live = Rc::new(Live {
            project_id: project_id.to_string(),
            id: self.next_instance(),
            upstream: Upstream::Local(port),
            usage: Arc::new(Usage::new()),
            stopping: Cell::new(false),
            dead: Cell::new(false),
            proc: Proc::Local(proc),
            exited,
            log,
        });
        self.watch_exit(live.clone());
        Ok(live)
    }

    async fn start_remote(&self, row: &ProjectRow, dir: &str, base_path: &str, state: &Starting) -> Result<Rc<Live>, StartError> {
        let link = (self.link_for)(row);
        let facts = link.host_facts().await.map_err(|e| e.message)?;
        check_cancelled(state)?;
        if facts.kind != HostKind::Posix {
            return Err("px0 暂不支持 Windows 宿主机。".to_string().into());
        }
        let Some(target) = px0_target_from_uname(facts.uname.as_deref().unwrap_or("")) else {
            let uname = facts.uname.as_deref().unwrap_or("未知平台");
            return Err(format!("px0 没有这台宿主机的构建（{uname}）。").into());
        };

        let bin = remote_px0_path(&facts.root, None);
        let probe = link.exec(&posix_version_command(&bin), None).await.map_err(failed)?;
        check_cancelled(state)?;
        if parse_px0_version(&probe.stdout).as_deref() != Some(PX0_VERSION) {
            // 远端看的是本机有没有该平台的资产；本机的 FALCON_PX0_BIN 不算
            state.stage.set(if px0_asset_present(&self.data_dir, target, None) { Px0Stage::Installing } else { Px0Stage::Downloading });
            let local = ensure_px0_asset(&self.data_dir, target, &self.opts.download).await.map_err(|e| e.0)?;
            check_cancelled(state)?;
            state.stage.set(Px0Stage::Installing);
            let bytes = tokio::fs::read(&local).await.map_err(|e| format!("读 px0 失败：{e}（{}）", local.display()))?;
            let res = link.exec_with_input(&posix_install_command(&bin), &bytes).await.map_err(failed)?;
            if res.code != Some(0) {
                let why = js::trim(&res.stderr);
                let why = if why.is_empty() { format!("exit {}", code_text(res.code)) } else { why.to_string() };
                return Err(format!("往宿主机写 px0 失败：{why}").into());
            }
            check_cancelled(state)?;
        }

        state.stage.set(Px0Stage::Launching);
        let shell = row.shell.as_deref().map(js::trim).filter(|s| !s.is_empty()).map_or_else(|| facts.shell.clone(), str::to_string);
        let args = px0_args(&Px0ArgsInput { base_path: base_path.to_string(), dir: expand_home(dir, &facts.home) });
        let channel = link.exec_stream(&posix_launch_command(&shell, &bin, &args), true).await.map_err(failed)?;
        let (read, write) = channel.split();
        let log = Rc::new(RefCell::new(LogTail::default()));
        let data = Rc::new(Notify::new());
        let (exit_tx, exited) = watch::channel(false);
        // 有 pty 时 stderr 已经并进 stdout，stderr 那条基本不会来数据，照听无妨
        tokio::task::spawn_local(pump_remote(read, log.clone(), data.clone(), exit_tx));

        let port = match wait_for_port(&log, &data, exited.clone()).await {
            Ok(port) => port,
            Err(message) => {
                close_remote(&write, exited).await;
                return Err(message.into());
            }
        };
        let live = Rc::new(Live {
            project_id: row.id.clone(),
            id: self.next_instance(),
            upstream: Upstream::Remote(port),
            usage: Arc::new(Usage::new()),
            stopping: Cell::new(false),
            dead: Cell::new(false),
            proc: Proc::Remote(RemoteProc { link, write }),
            exited,
            log,
        });
        self.watch_exit(live.clone());
        Ok(live)
    }

    /// 进程 / 通道一没就记下来（TS 的 `void exited.then(() => this.onDied(live, log))`）
    fn watch_exit(&self, live: Rc<Live>) {
        let me = self.me.clone();
        let mut exited = live.exited.clone();
        tokio::task::spawn_local(async move {
            let _ = exited.wait_for(|e| *e).await;
            if let Some(me) = me.upgrade() {
                me.on_died(&live);
            } else {
                live.dead.set(true);
            }
        });
    }

    /// px0 进程 / 通道没了。我们自己停的不算故障；否则留下原因给入口页
    fn on_died(&self, live: &Rc<Live>) {
        live.dead.set(true);
        {
            let mut map = self.live.borrow_mut();
            if map.get(&live.project_id).is_some_and(|l| Rc::ptr_eq(l, live)) {
                map.remove(&live.project_id);
            }
        }
        if live.stopping.get() {
            return;
        }
        let tail = tail_lines(&live.log.borrow().text, None, None);
        let message =
            if tail.is_empty() { "px0 已退出（SSH 链路断开或进程被杀）".to_string() } else { format!("px0 已退出：\n{tail}") };
        self.errors.borrow_mut().insert(live.project_id.clone(), ErrorEntry { message, at: Instant::now() });
    }
}

fn check_cancelled(state: &Starting) -> Result<(), StartError> {
    if state.cancelled.get() { Err(StartError::Cancelled) } else { Ok(()) }
}

fn code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "null".to_string(), |c| c.to_string())
}

async fn teardown(live: &Live) {
    live.stopping.set(true);
    match &live.proc {
        Proc::Local(p) => kill_local(p, live.exited.clone()).await,
        Proc::Remote(p) => close_remote(&p.write, live.exited.clone()).await,
    }
}

#[derive(Default)]
struct LogTail {
    text: String,
}

impl LogTail {
    fn push(&mut self, s: &str) {
        self.text.push_str(s);
        if self.text.len() > LOG_KEEP * 2 {
            let mut cut = self.text.len() - LOG_KEEP;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
        }
    }
}

/// 等 px0 打出监听地址。`data` 是「又来了一批输出」的通知（内容已由 LogTail 收走）
async fn wait_for_port(log: &RefCell<LogTail>, data: &Notify, mut exited: watch::Receiver<bool>) -> Result<u16, String> {
    let detail = || {
        let tail = tail_lines(&log.borrow().text, None, None);
        if tail.is_empty() { String::new() } else { format!("：\n{tail}") }
    };
    let check = || parse_listen_port(&log.borrow().text);
    let deadline = tokio::time::sleep(LISTEN_WAIT);
    tokio::pin!(deadline);
    loop {
        if let Some(port) = check() {
            return Ok(port);
        }
        tokio::select! {
            _ = data.notified() => {}
            _ = exited.wait_for(|e| *e) => {
                // 退出前最后一批输出可能刚到，先再看一眼
                return check().ok_or_else(|| format!("px0 没能启动{}", detail()));
            }
            _ = &mut deadline => return Err(format!("等不到 px0 的监听地址{}", detail())),
        }
    }
}

enum PtyEvent {
    Data(Vec<u8>),
    Exit,
}

/// 在本机 pty 上起 px0（node-pty 的 `spawn(bin, args, { name: "dumb", cols: 200, rows: 50, env })`）。
/// 读线程把输出送回来；等待线程在 px0 退出时送一个 Exit
fn spawn_pty(bin: &Path, args: &[String], base: &[(String, String)]) -> anyhow::Result<(LocalProc, mpsc::UnboundedReceiver<PtyEvent>)> {
    let pair = native_pty_system().openpty(PtySize { rows: 50, cols: 200, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.args(args);
    cmd.env_clear();
    for (k, v) in base {
        cmd.env(k, v);
    }
    cmd.env("NO_COLOR", "1");
    // node-pty 把 name 写进 TERM
    cmd.env("TERM", "dumb");
    let mut child = pair.slave.spawn_command(cmd)?;
    // slave 一定要关：不关的话子进程退出后读端永远等不到 EOF
    drop(pair.slave);
    let pid = child.process_id();
    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader()?;
    let (tx, rx) = mpsc::unbounded_channel();
    let reaped = Arc::new(AtomicBool::new(false));

    let data_tx = tx.clone();
    std::thread::Builder::new().name("falcon-px0-read".into()).spawn(move || {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if data_tx.send(PtyEvent::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    })?;
    let reaped_flag = reaped.clone();
    std::thread::Builder::new().name("falcon-px0-wait".into()).spawn(move || {
        // 收尸：不 wait 会留僵尸进程
        let _ = child.wait();
        reaped_flag.store(true, Ordering::SeqCst);
        std::thread::sleep(EXIT_DRAIN);
        let _ = tx.send(PtyEvent::Exit);
    })?;
    Ok((LocalProc { pid, reaped, killer: RefCell::new(killer), _master: pair.master }, rx))
}

async fn pump_local(
    mut events: mpsc::UnboundedReceiver<PtyEvent>,
    log: Rc<RefCell<LogTail>>,
    data: Rc<Notify>,
    exit_tx: watch::Sender<bool>,
) {
    let mut decoder = Utf8Decoder::default();
    while let Some(event) = events.recv().await {
        match event {
            PtyEvent::Data(bytes) => {
                log.borrow_mut().push(&decoder.write(&bytes));
                data.notify_one();
            }
            PtyEvent::Exit => break,
        }
    }
    let _ = exit_tx.send(true);
}

async fn pump_remote(mut read: ChannelReadHalf, log: Rc<RefCell<LogTail>>, data: Rc<Notify>, exit_tx: watch::Sender<bool>) {
    let (mut out, mut err) = (Utf8Decoder::default(), Utf8Decoder::default());
    loop {
        match read.wait().await {
            Some(ChannelMsg::Data { data: bytes }) => {
                log.borrow_mut().push(&out.write(&bytes));
                data.notify_one();
            }
            Some(ChannelMsg::ExtendedData { data: bytes, .. }) => {
                log.borrow_mut().push(&err.write(&bytes));
                data.notify_one();
            }
            // 通道关了（含 SSH 断链）
            Some(ChannelMsg::Close) | None => break,
            Some(_) => {}
        }
    }
    let _ = exit_tx.send(true);
}

/// 停本机 px0：先 TERM，宽限期过了再 KILL。Windows 上 node-pty 的 kill 不收信号名，
/// 两步都是 TerminateProcess
async fn kill_local(proc: &LocalProc, mut exited: watch::Receiver<bool>) {
    if *exited.borrow() {
        return;
    }
    signal_local(proc, false);
    if tokio::time::timeout(STOP_GRACE, exited.wait_for(|e| *e)).await.is_err() {
        signal_local(proc, true);
    }
}

fn signal_local(proc: &LocalProc, force: bool) {
    // 已经没了（wait 返回过），pid 可能被别人用了
    if proc.reaped.load(Ordering::SeqCst) {
        return;
    }
    #[cfg(unix)]
    if let Some(pid) = proc.pid {
        unsafe {
            libc::kill(pid as libc::pid_t, if force { libc::SIGKILL } else { libc::SIGTERM });
        }
        return;
    }
    let _ = force;
    let _ = proc.killer.borrow_mut().kill();
}

/// 停远端 px0：先发 TERM（它会顺手关掉拉起的语言服务器），宽限期过了不管结果都关通道——
/// pty 一挂断 px0 就收到 SIGHUP。sshd 不支持 signal 请求时就只剩后一条路。
async fn close_remote(write: &ChannelWriteHalf<Msg>, mut exited: watch::Receiver<bool>) {
    if *exited.borrow() {
        return;
    }
    // 通道已经在关了的话会失败，无所谓
    let _ = write.signal(Sig::TERM).await;
    if tokio::time::timeout(STOP_GRACE, exited.wait_for(|e| *e)).await.is_err() {
        let _ = write.close().await;
        let _ = tokio::time::timeout(CLOSE_WAIT, exited.wait_for(|e| *e)).await;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::future::Future;
    use std::os::unix::fs::PermissionsExt as _;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;
    use crate::px0::bin::Px0BinError;

    /// 假 px0：一段 sh 脚本，参数与环境写进临时目录，按 `body` 行事
    struct Fake {
        dir: tempfile::TempDir,
    }

    impl Fake {
        fn new(body: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let script = format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{args}'\nenv > '{env}'\n{body}\n",
                args = dir.path().join("args").display(),
                env = dir.path().join("env").display(),
            );
            let bin = dir.path().join("px0");
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake { dir }
        }

        fn manager(&self) -> Rc<Px0Manager> {
            self.manager_with(Some(self.dir.path().join("px0").to_string_lossy().into_owned()))
        }

        fn manager_with(&self, local_bin: Option<String>) -> Rc<Px0Manager> {
            let env = Arc::new(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]);
            let opts = Px0Options {
                local_bin,
                base_env: Arc::new(move || {
                    let env = env.clone();
                    Box::pin(async move { env })
                }),
                download: Arc::new(|_| Box::pin(async { Err(Px0BinError("测试里不下载".into())) })),
            };
            let link_for: Rc<dyn Fn(&ProjectRow) -> Rc<SshLink>> = Rc::new(|_| panic!("本地项目不该要链路"));
            Px0Manager::with_options(self.dir.path().join("data"), link_for, opts)
        }

        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
        }
    }

    fn project(dir: &Path) -> ProjectRow {
        ProjectRow {
            id: "p1".into(),
            name: "演示".into(),
            project_type: "local".into(),
            working_dir: Some(format!("  {}  ", dir.display())),
            ..Default::default()
        }
    }

    async fn settle(mgr: &Px0Manager, row: &ProjectRow) -> Option<Px0PageState> {
        for _ in 0..400 {
            match mgr.open(row, false) {
                Some(Px0PageState::Starting { .. }) => tokio::time::sleep(Duration::from_millis(25)).await,
                other => return other,
            }
        }
        panic!("px0 一直在启动");
    }

    fn local<F: Future<Output = ()>>(f: F) -> impl Future<Output = ()> {
        async move { tokio::task::LocalSet::new().run_until(f).await }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn starts_on_a_pty_proxies_and_stops() {
        local(async {
            let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = upstream.local_addr().unwrap().port();
            let fake = Fake::new(&format!(
                "[ -t 1 ] && echo tty=yes\necho 'px0 0.1.16 starting'\necho \"url: http://127.0.0.1:{port}/px0/p1/\"\nexec sleep 30"
            ));
            let mgr = fake.manager();
            let row = project(fake.dir.path());
            // 本地项目、本机已有 px0：第一眼就是「正在启动」
            assert_eq!(mgr.open(&row, false), Some(Px0PageState::Starting { stage: Px0Stage::Launching }));
            assert!(mgr.acquire("p1").is_none());
            assert_eq!(settle(&mgr, &row).await, None);

            // 参数：固定 flag、base path、去掉首尾空白的工作目录；环境：pty、NO_COLOR、TERM=dumb
            let args: Vec<String> = fake.read("args").lines().map(str::to_string).collect();
            let dir = fake.dir.path().display().to_string();
            assert_eq!(
                args,
                [
                    "-host", "127.0.0.1", "-port", "0", "-no-open", "-no-telemetry", "-no-update", "-no-color",
                    "-base-path", "/px0/p1/", &dir,
                ]
            );
            let env = fake.read("env");
            assert!(env.contains("NO_COLOR=1") && env.contains("TERM=dumb"), "{env}");
            let live = mgr.live.borrow().get("p1").cloned().unwrap();
            assert!(live.log.borrow().text.contains("tty=yes"), "px0 应当挂在 pty 上：{}", live.log.borrow().text);

            // 租约：本地实例可以直连，也可以经 connect
            let lease = mgr.acquire("p1").unwrap();
            assert_eq!(lease.project_id(), "p1");
            assert_eq!(lease.local_addr(), Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port))));
            let mut stream = mgr.connect(&lease).await.unwrap();
            let (mut accepted, _) = upstream.accept().await.unwrap();
            stream.write_all(b"ping").await.unwrap();
            let mut buf = [0u8; 4];
            accepted.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping");

            // 在途时不算空闲；丢掉租约后过了 IDLE 才算
            assert!(!live.usage.idle_at(Instant::now() + IDLE * 2));
            drop(lease);
            assert!(!live.usage.idle_at(Instant::now()));
            assert!(live.usage.idle_at(Instant::now() + IDLE * 2));

            // 主动停：TERM 生效，不记错误
            let started = Instant::now();
            mgr.stop("p1").await;
            assert!(started.elapsed() < STOP_GRACE, "TERM 应当让它立刻退出");
            assert!(*live.exited.borrow());
            assert!(mgr.acquire("p1").is_none());
            // 再打开是新的一次启动，不是错误页
            assert_eq!(mgr.open(&row, false), Some(Px0PageState::Starting { stage: Px0Stage::Launching }));
            assert_eq!(settle(&mgr, &row).await, None);
            mgr.shutdown().await;
            assert!(mgr.acquire("p1").is_none());
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_px0_that_dies_at_startup_shows_why_until_retry() {
        local(async {
            let fake = Fake::new("echo 'listen tcp: permission denied'\nexit 1");
            let mgr = fake.manager();
            let row = project(fake.dir.path());
            let Some(Px0PageState::Error { message }) = settle(&mgr, &row).await else { panic!("应当是错误页") };
            assert_eq!(message, "px0 没能启动：\nlisten tcp: permission denied");
            // 一分钟内再打开还是这页；点重试才重新起
            assert_eq!(mgr.open(&row, false), Some(Px0PageState::Error { message }));
            assert!(matches!(mgr.open(&row, true), Some(Px0PageState::Starting { .. })));
            assert!(matches!(settle(&mgr, &row).await, Some(Px0PageState::Error { .. })));
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_px0_that_exits_later_is_recorded() {
        local(async {
            let fake = Fake::new("echo 'url: http://127.0.0.1:9/px0/p1/'\nsleep 1\necho 'panic: boom'\nexit 2");
            let mgr = fake.manager();
            let row = project(fake.dir.path());
            assert_eq!(settle(&mgr, &row).await, None);
            for _ in 0..200 {
                if mgr.acquire("p1").is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(mgr.acquire("p1").is_none());
            let Some(Px0PageState::Error { message }) = mgr.open(&row, false) else { panic!("应当是错误页") };
            assert!(message.starts_with("px0 已退出：\n") && message.ends_with("panic: boom"), "{message}");
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stopping_while_starting_cancels_without_an_error() {
        local(async {
            let fake = Fake::new("sleep 1\necho 'url: http://127.0.0.1:9/px0/p1/'\nexec sleep 30");
            let mgr = fake.manager();
            let row = project(fake.dir.path());
            assert!(matches!(mgr.open(&row, false), Some(Px0PageState::Starting { .. })));
            tokio::time::sleep(Duration::from_millis(200)).await;
            mgr.stop("p1").await;
            // 启动跑完后发现被取消：拆掉、不登记、不记错误
            tokio::time::sleep(Duration::from_millis(1500)).await;
            assert!(mgr.acquire("p1").is_none());
            assert!(mgr.errors.borrow().is_empty());
            assert!(mgr.live.borrow().is_empty() && mgr.starting.borrow().is_empty());
        })
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sweep_stops_idle_instances_and_bad_setups_explain_themselves() {
        local(async {
            let fake = Fake::new("echo 'url: http://localhost:9/px0/p1/'\nexec sleep 30");
            let mgr = fake.manager();
            let row = project(fake.dir.path());
            assert_eq!(settle(&mgr, &row).await, None);
            let live = mgr.live.borrow().get("p1").cloned().unwrap();
            mgr.sweep(Instant::now());
            assert!(mgr.acquire("p1").is_some(), "还没空闲");
            mgr.sweep(Instant::now() + IDLE * 2);
            let _ = tokio::time::timeout(Duration::from_secs(5), live.exited.clone().wait_for(|e| *e)).await;
            assert!(*live.exited.borrow());
            assert!(mgr.acquire("p1").is_none());
            // 空闲回收不是故障
            assert!(mgr.errors.borrow().is_empty());

            // 没有工作目录（多仓库容器）
            let mut container = row.clone();
            container.id = "p2".into();
            container.working_dir = Some("   ".into());
            mgr.open(&container, false);
            let Some(Px0PageState::Error { message }) = settle(&mgr, &container).await else { panic!("应当是错误页") };
            assert_eq!(message, "这个项目没有工作目录。多仓库容器请在成员或附属项目上打开 px0。");

            // FALCON_PX0_BIN 指了不存在的文件
            let opts_mgr = fake.manager_with(Some("/nonexistent/falcon-test/px0".into()));
            let Some(Px0PageState::Error { message }) = settle(&opts_mgr, &row).await else { panic!("应当是错误页") };
            assert_eq!(message, "FALCON_PX0_BIN 指向的文件不存在：/nonexistent/falcon-test/px0");
        })
        .await;
    }

    #[test]
    fn log_tail_keeps_the_end_on_a_char_boundary() {
        let mut log = LogTail::default();
        log.push(&"中".repeat(LOG_KEEP));
        assert!(log.text.len() <= LOG_KEEP + 3);
        assert!(log.text.chars().all(|c| c == '中'));
        log.push("末尾");
        assert!(log.text.ends_with("末尾"));
    }
}

/// 编译期的约束：租约与上游连接要能交给 axum 的处理线程（反代在那边跑 hyper）
#[cfg(test)]
mod send_check {
    use super::*;

    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}

    #[test]
    fn lease_and_stream_cross_threads() {
        send_sync::<Px0Lease>();
        send::<Px0Stream>();
        send::<russh::ChannelStream<Msg>>();
    }
}
