//! holder（`falcon-sessiond hold`）：会话生命周期的锚——它活着，会话就活着（ADR 0020 决定一）。
//!
//! 只做这些事：持 PTY、把输出原样转给每条连接、存程序状态与终端标题、探前台命令、拉起并看护 VT 进程。
//! 不碰 libghostty-vt。线程模型：PTY 读 / 写、监听、每条连接的读 / 写、VT 进程的读 / 写各一条
//! 线程，全部经一个 channel 汇到主循环，状态只在主循环里改，不加锁。
//!
//! 几条不变式：
//! - **写给 VT 进程的字节与发给连接的字节是同一个顺序**。连接发 `Sync` 时，记下此刻的偏移 N、
//!   在 VT 管道里排一个快照请求，并把**这条连接**之后的输出先扣下；快照回来发 `Synced{N}`，
//!   再放出扣下的（都 `≥ N`）。接收方因此不用拼接（设计文档 5.5）；
//! - VT 进程重启 / 热升级时，在途的快照请求**改基到当前偏移**重新发给新进程，扣下的输出
//!   都已含在新快照里，丢掉；
//! - PTY 读取绝不因为连接慢而阻塞：每条连接的发送队列有上限，超了就断开它（服务端会重连、
//!   重新 Sync）。VT 管道积压超限同理：杀掉重启。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use falcon_proto::program_status::{ProgramState, ProgramStatusStore};
use falcon_proto::sessiond::{
    Appearance, ClientMsg, Event, ExitRecord, ExitStatus, ExportFormat, FALCON_VERSION, HolderMsg, PROTO_MAX, PROTO_MIN,
    Resize, Role, Scheme, SessionMeta, Welcome, negotiate, read_frame,
};
use falcon_proto::term_modes::TermModeTracker;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::ipc;
use crate::layout::{Layout, now_ms};
use crate::log::logf;
use crate::vtproto::{self, FromVt, ToVt};

/// holder 自留的原始输出尾巴：VT 进程重启时拿它重建画面、降级时直接当快照
const TAIL_BYTES: usize = 256 * 1024;
/// 每条连接的发送队列上限
const CONN_QUEUE_MAX: usize = 8 << 20;
/// VT 管道的积压上限：VT 进程卡住了（或者慢得离谱）就杀掉重启
const VT_QUEUE_MAX: usize = 16 << 20;
/// 60 秒内崩 3 次进入降级
const VT_CRASH_WINDOW: Duration = Duration::from_secs(60);
const VT_CRASH_LIMIT: usize = 3;
/// 连上来 10 秒还不握手就断开
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Terminate 先 SIGHUP，2 秒后还在就 SIGKILL
const TERM_GRACE: Duration = Duration::from_secs(2);
/// shell 退出后最多等多久 PTY 读到 EOF（后台进程占着 slave 时永远等不到）
const EOF_GRACE: Duration = Duration::from_millis(500);
/// 有 working / blocked 记录时多久看一次前台是不是回到了 shell
const FG_CHECK_EVERY: Duration = Duration::from_secs(1);

enum Ev {
    Pty(Vec<u8>),
    PtyEof,
    ShellExit(ExitStatus),
    Accept(ipc::Stream),
    Frame(u64, ClientMsg),
    Closed(u64),
    Vt(u64, FromVt),
    VtDied(u64),
}

/// 一条连接的发送端：队列由一条写线程排空
struct Outbox {
    tx: Sender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    dead: Arc<AtomicBool>,
}

impl Outbox {
    fn send(&self, frame: Vec<u8>) -> bool {
        if self.dead.load(Ordering::Relaxed) {
            return false;
        }
        let len = frame.len();
        if self.queued.fetch_add(len, Ordering::Relaxed) + len > CONN_QUEUE_MAX {
            self.dead.store(true, Ordering::Relaxed);
            return false;
        }
        self.tx.send(frame).is_ok()
    }

    fn drained(&self) -> bool {
        self.queued.load(Ordering::Relaxed) == 0 || self.dead.load(Ordering::Relaxed)
    }
}

struct Conn {
    out: Outbox,
    closer: ipc::Closer,
    hello: bool,
    role: Role,
    connected_at: Instant,
    /// 这条连接请求的尺寸
    size: Option<Resize>,
    /// 最近一次有输入或 resize 的时刻（单调计数）。生效尺寸跟最近操作的那条连接（5.6）
    active_at: u64,
    /// 在途的 Sync（按发出顺序），值是请求时的偏移 N
    syncs: VecDeque<u64>,
    /// 有在途 Sync 时扣下的输出 (偏移, 字节)
    held: Vec<(u64, Vec<u8>)>,
}

/// 发给 VT 进程、等它回话的请求
#[derive(Debug, Clone)]
enum VtPending {
    Sync { conn: u64, req: u32 },
    Export { conn: u64, req: u32, format: ExportFormat },
    Encode,
}

struct Upgrade {
    conn: u64,
    req: u32,
    binary: PathBuf,
    /// 发出 Encode 之后喂给旧 VT 的字节：二进制快照里没有它们，要补喂给新进程
    after: Vec<u8>,
    /// 已切到新进程，等它回 Restored；期间崩了就退回旧的二进制
    restoring: Option<PathBuf>,
}

struct Vt {
    generation: u64,
    child: Option<std::process::Child>,
    tx: Option<Sender<Vec<u8>>>,
    queued: Arc<AtomicUsize>,
    version: String,
    crashes: VecDeque<Instant>,
    degraded: bool,
    binary: PathBuf,
}

struct Holder {
    layout: Layout,
    sid: String,
    session_id: String,
    started_at: i64,
    shell_pid: u32,
    master: Box<dyn MasterPty + Send>,
    pty_tx: Sender<Vec<u8>>,
    events: Sender<Ev>,
    offset: u64,
    tail: VecDeque<u8>,
    modes: TermModeTracker,
    size: Resize,
    scrollback: u32,
    appearance: Option<Appearance>,
    status: ProgramStatusStore,
    /// 前台程序设的终端标题（OSC 0 / 2，VT 进程解析）；空串 = 没有。shell 自己写的不算（见
    /// [`Holder::on_title`]）。存在这里而不是只转发：服务端重连（重启、升级）后照样拿得到——Claude
    /// Code 的会话主题只在变的时候写一次。用不用由服务端定（只认报过 OSC 7501 的程序的，见
    /// falcon-server 的 sessions/auto_title.rs）
    title: String,
    conns: HashMap<u64, Conn>,
    next_conn: u64,
    activity: u64,
    vt: Vt,
    vt_pending: BTreeMap<u32, VtPending>,
    next_vt_req: u32,
    upgrade: Option<Upgrade>,
    exit: Option<(ExitStatus, Instant)>,
    pty_eof: bool,
    terminate_at: Option<Instant>,
    fg_checked: Instant,
    /// 第一次看到前台在 shell 手里的时刻（要连续两次才清状态）
    at_shell_since: Option<Instant>,
}

pub fn run(layout: Layout, sid: String) -> Result<()> {
    crate::log::init(layout.log_path(&sid));
    #[cfg(unix)]
    unsafe {
        // 没有控制终端（setsid 过），保险起见仍忽略 SIGHUP；SIGPIPE Rust 运行时已经忽略。
        // 子进程不会继承：portable-pty 在 exec 前把这些信号重置为默认
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let res = run_inner(&layout, &sid);
    if let Err(e) = &res {
        logf!("holder 失败：{e:#}");
    }
    res
}

fn run_inner(layout: &Layout, sid: &str) -> Result<()> {
    let spec = layout.take_spec(sid)?;
    logf!("holder 启动：{} {:?}", spec.session_id, spec.argv);

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: spec.rows.max(1), cols: spec.cols.max(1), pixel_width: 0, pixel_height: 0 })
        .context("开不了 PTY")?;
    let program = spec.argv.first().cloned().context("argv 是空的")?;
    let mut cmd = CommandBuilder::new(&program);
    cmd.args(&spec.argv[1..]);
    if spec.env_clear {
        cmd.env_clear();
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if !spec.env.iter().any(|(k, _)| k == "FALCON_SESSIOND_ID") {
        cmd.env("FALCON_SESSIOND_ID", sid);
    }
    // 目录没了（项目被删 / 盘没挂）时不该起不来：退回家目录
    let home = spec.env.iter().find(|(k, _)| k == "HOME" || k == "USERPROFILE").map(|(_, v)| v.clone());
    match spec.cwd.as_deref().filter(|d| std::path::Path::new(d).is_dir()) {
        Some(d) => cmd.cwd(d),
        None => {
            if let Some(h) = home.filter(|h| std::path::Path::new(h).is_dir()) {
                cmd.cwd(h);
            }
        }
    }
    let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("起不来 {program}"))?;
    // slave 一定要关：不关的话 shell 退出后读端永远等不到 EOF
    drop(pair.slave);
    let shell_pid = child.process_id().unwrap_or(0);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    let (tx, rx) = mpsc::channel::<Ev>();

    // PTY 读
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("pty-read".into()).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Ev::Pty(buf[..n].to_vec())).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = tx.send(Ev::PtyEof);
        })?;
    }
    // PTY 写：前台程序不读输入时写会阻塞，不能卡住主循环
    let (pty_tx, pty_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name("pty-write".into()).spawn(move || {
        while let Ok(data) = pty_rx.recv() {
            if writer.write_all(&data).and_then(|()| writer.flush()).is_err() {
                break;
            }
        }
    })?;
    // 收尸
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("shell-wait".into()).spawn(move || {
            let status = match child.wait() {
                Ok(s) => exit_status(&s),
                Err(_) => ExitStatus::default(),
            };
            let _ = tx.send(Ev::ShellExit(status));
        })?;
    }

    layout.ensure_dirs()?;
    let endpoint = layout.endpoint(sid);
    let listener = ipc::listen(&endpoint).with_context(|| format!("监听不了 {endpoint}"))?;
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("accept".into()).spawn(move || {
            loop {
                match listener.accept() {
                    Ok(s) => {
                        if tx.send(Ev::Accept(s)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        logf!("accept 失败：{e}");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        })?;
    }

    let started_at = now_ms();
    layout.write_meta(&SessionMeta {
        sid: sid.into(),
        session_id: spec.session_id.clone(),
        pid: std::process::id(),
        shell_pid,
        version: FALCON_VERSION.into(),
        owner: spec.owner.clone(),
        endpoint: endpoint.clone(),
        created_at: started_at,
        argv: spec.argv.clone(),
        cwd: spec.cwd.clone(),
    })?;
    // 这次会话没有旧的退出记录（同一个 sid 不会复用，保险起见）
    let _ = std::fs::remove_file(layout.exit_path(sid));

    let mut h = Holder {
        layout: layout.clone(),
        sid: sid.into(),
        session_id: spec.session_id.clone(),
        started_at,
        shell_pid,
        master: pair.master,
        pty_tx,
        events: tx,
        offset: 0,
        tail: VecDeque::with_capacity(TAIL_BYTES),
        modes: TermModeTracker::new(),
        size: Resize { cols: spec.cols.max(1), rows: spec.rows.max(1) },
        scrollback: spec.scrollback_lines,
        appearance: spec.appearance.clone(),
        status: ProgramStatusStore::new(),
        title: String::new(),
        conns: HashMap::new(),
        next_conn: 1,
        activity: 0,
        vt: Vt {
            generation: 0,
            child: None,
            tx: None,
            queued: Arc::new(AtomicUsize::new(0)),
            version: String::new(),
            crashes: VecDeque::new(),
            degraded: false,
            binary: std::env::current_exe()?,
        },
        vt_pending: BTreeMap::new(),
        next_vt_req: 1,
        upgrade: None,
        exit: None,
        pty_eof: false,
        terminate_at: None,
        fg_checked: Instant::now(),
        at_shell_since: None,
    };
    h.start_vt(Seed::Tail);
    h.main_loop(rx);
    h.finish(&endpoint);
    Ok(())
}

fn exit_status(s: &portable_pty::ExitStatus) -> ExitStatus {
    match s.signal() {
        Some(name) => ExitStatus { code: None, signal: signal_number(name) },
        None => ExitStatus { code: Some(s.exit_code() as i32), signal: None },
    }
}

/// portable-pty 被信号杀死时只给 `strsignal()` 的名字（"Hangup"），反查回编号
#[cfg(unix)]
fn signal_number(name: &str) -> Option<i32> {
    (1..65).find(|&n| {
        let p = unsafe { libc::strsignal(n) };
        !p.is_null() && unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy() == name
    })
}

#[cfg(not(unix))]
fn signal_number(_name: &str) -> Option<i32> {
    None
}

/// VT 进程起来后先喂什么
enum Seed {
    /// 自留的原始尾巴（首次启动时为空）
    Tail,
    /// 热升级：旧进程的二进制快照 + 快照之后的字节
    Restore(Vec<u8>, Vec<u8>),
}

impl Holder {
    fn main_loop(&mut self, rx: Receiver<Ev>) {
        loop {
            if self.finished() {
                return;
            }
            let timeout = self.next_timeout();
            match rx.recv_timeout(timeout) {
                Ok(ev) => {
                    let conn = match &ev {
                        Ev::Frame(id, _) | Ev::Closed(id) => Some(*id),
                        _ => None,
                    };
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle(ev)));
                    if r.is_err() {
                        logf!("处理事件时 panic（连接 {conn:?}）");
                        // 出错的是某条连接的请求：只断开它，会话照常
                        if let Some(id) = conn {
                            self.drop_conn(id);
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick();
        }
    }

    fn finished(&self) -> bool {
        match &self.exit {
            Some((_, at)) => self.pty_eof || at.elapsed() >= EOF_GRACE,
            None => false,
        }
    }

    fn next_timeout(&self) -> Duration {
        let mut t = Duration::from_secs(1);
        if let Some((_, at)) = &self.exit {
            t = t.min(EOF_GRACE.saturating_sub(at.elapsed()));
        }
        if let Some(at) = self.terminate_at {
            t = t.min(at.saturating_duration_since(Instant::now()));
        }
        t.max(Duration::from_millis(10))
    }

    fn tick(&mut self) {
        // 还不握手的连接
        let stale: Vec<u64> =
            self.conns.iter().filter(|(_, c)| !c.hello && c.connected_at.elapsed() > HELLO_TIMEOUT).map(|(id, _)| *id).collect();
        for id in stale {
            self.drop_conn(id);
        }
        // 发送队列超限的连接
        let dead: Vec<u64> = self.conns.iter().filter(|(_, c)| c.out.dead.load(Ordering::Relaxed)).map(|(id, _)| *id).collect();
        for id in dead {
            logf!("连接 {id} 太慢或已断开，断掉它");
            self.drop_conn(id);
        }
        // Terminate 的宽限期到了
        if let Some(at) = self.terminate_at
            && Instant::now() >= at
            && self.exit.is_none()
        {
            crate::proc::kill_session(self.shell_pid, true);
            self.terminate_at = None;
        }
        // 前台回到 shell：丢掉 working / blocked（agent 的 CLI 退出后 exec 回登录 shell，
        // 不一定有 OSC 133 A，见 fg::at_shell）。每秒最多查一次（Linux 上要扫一遍 /proc），
        // 而且连续两次都在 shell 手里才算：shell 的内建命令（printf）、两条命令之间的空档里
        // 前台也是 shell 自己，只看一次会把刚报上来的状态立刻清掉（用例实测）
        let active = self.status.records().iter().any(|r| matches!(r.state, ProgramState::Working | ProgramState::Blocked));
        if !active {
            self.at_shell_since = None;
        } else if self.fg_checked.elapsed() >= FG_CHECK_EVERY {
            self.fg_checked = Instant::now();
            if crate::fg::at_shell(self.master.as_ref(), self.shell_pid) {
                match self.at_shell_since {
                    Some(t) if t.elapsed() >= FG_CHECK_EVERY => {
                        self.at_shell_since = None;
                        if self.status.drop_active() {
                            self.broadcast_status();
                        }
                    }
                    Some(_) => {}
                    None => self.at_shell_since = Some(Instant::now()),
                }
            } else {
                self.at_shell_since = None;
            }
        }
    }

    fn handle(&mut self, ev: Ev) {
        match ev {
            Ev::Pty(bytes) => self.on_output(bytes),
            Ev::PtyEof => self.pty_eof = true,
            Ev::ShellExit(s) => {
                logf!("shell 退出：{s:?}");
                self.exit = Some((s, Instant::now()));
            }
            Ev::Accept(stream) => self.accept(stream),
            Ev::Frame(id, msg) => self.on_frame(id, msg),
            Ev::Closed(id) => self.drop_conn(id),
            Ev::Vt(generation, msg) if generation == self.vt.generation => self.on_vt(msg),
            Ev::VtDied(generation) if generation == self.vt.generation => self.on_vt_died(),
            Ev::Vt(..) | Ev::VtDied(_) => {}
        }
    }

    // ------------------------------------------------------------ 输出

    fn on_output(&mut self, bytes: Vec<u8>) {
        let at = self.offset;
        self.offset += bytes.len() as u64;
        self.tail.extend(bytes.iter().copied());
        let over = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..over);
        self.modes.track(&bytes);
        if let Some(u) = &mut self.upgrade
            && u.restoring.is_none()
        {
            u.after.extend_from_slice(&bytes);
        }
        self.vt_send(ToVt::Feed { replay: false, bytes: bytes.clone() });
        let frame = HolderMsg::Output { offset: at, bytes: bytes.clone() }.encode();
        for c in self.conns.values_mut() {
            if !c.hello {
                continue;
            }
            if c.syncs.is_empty() {
                c.out.send(frame.clone());
            } else {
                c.held.push((at, bytes.clone()));
            }
        }
    }

    fn tail_bytes(&mut self) -> Vec<u8> {
        self.tail.make_contiguous().to_vec()
    }

    /// 没有 VT 进程可问时的快照：原始尾巴。尾巴截断过就从第一个换行之后开始，
    /// 免得以半截转义序列开头
    fn raw_snapshot(&mut self) -> Vec<u8> {
        let tail = self.tail_bytes();
        if self.offset as usize > tail.len() {
            if let Some(i) = tail.iter().position(|&b| b == b'\n') {
                return tail[i + 1..].to_vec();
            }
        }
        tail
    }

    // ------------------------------------------------------------ 连接

    fn accept(&mut self, stream: ipc::Stream) {
        let id = self.next_conn;
        self.next_conn += 1;
        let Ok(closer) = stream.closer() else { return };
        let Ok((mut r, mut w)) = stream.split() else { return };
        let (otx, orx) = mpsc::channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        let dead = Arc::new(AtomicBool::new(false));
        {
            let queued = queued.clone();
            let dead = dead.clone();
            let events = self.events.clone();
            let _ = std::thread::Builder::new().name(format!("conn-{id}-w")).spawn(move || {
                while let Ok(frame) = orx.recv() {
                    let len = frame.len();
                    if w.write_all(&frame).and_then(|()| w.flush()).is_err() {
                        dead.store(true, Ordering::Relaxed);
                        let _ = events.send(Ev::Closed(id));
                        return;
                    }
                    queued.fetch_sub(len, Ordering::Relaxed);
                }
            });
        }
        {
            let events = self.events.clone();
            let _ = std::thread::Builder::new().name(format!("conn-{id}-r")).spawn(move || {
                loop {
                    match read_frame(&mut r) {
                        Ok(Some((k, p))) => match ClientMsg::decode(k, p) {
                            Ok(msg) => {
                                if events.send(Ev::Frame(id, msg)).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                logf!("连接 {id} 发来坏帧：{e:#}");
                                break;
                            }
                        },
                        Ok(None) | Err(_) => break,
                    }
                }
                let _ = events.send(Ev::Closed(id));
            });
        }
        self.conns.insert(
            id,
            Conn {
                out: Outbox { tx: otx, queued, dead },
                closer,
                hello: false,
                role: Role::Unknown,
                connected_at: Instant::now(),
                size: None,
                active_at: 0,
                syncs: VecDeque::new(),
                held: Vec::new(),
            },
        );
    }

    fn drop_conn(&mut self, id: u64) {
        let Some(c) = self.conns.remove(&id) else { return };
        c.closer.close();
        // 它在途的 VT 请求作废（回来了也没人收）
        self.vt_pending.retain(|_, p| !matches!(p, VtPending::Sync { conn, .. } | VtPending::Export { conn, .. } if *conn == id));
        if c.role == Role::Attach {
            logf!("attach 断开");
        }
        self.apply_effective_size();
    }

    fn send(&self, id: u64, msg: &HolderMsg) {
        if let Some(c) = self.conns.get(&id) {
            c.out.send(msg.encode());
        }
    }

    fn broadcast(&self, msg: &HolderMsg) {
        let frame = msg.encode();
        for c in self.conns.values().filter(|c| c.hello) {
            c.out.send(frame.clone());
        }
    }

    fn broadcast_status(&self) {
        self.broadcast(&HolderMsg::Status { records: self.status.records().to_vec() });
    }

    /// 终端标题变了：是 shell 自己写的就当没有。
    ///
    /// shell 也写标题——oh-my-zsh 在提示符时写 `user@host: ~/dir`、执行命令前写命令行——它和程序
    /// 写的在字节流里长得一样，只有写的那一刻谁在前台分得开，而这一刻只有 holder 看得到（服务端
    /// 隔着一来一回的探测，分不清标题是程序起来之前还是之后写的）。shell 写标题说明它在前台，之前
    /// 那个程序已经不在了，所以顺手清掉。Windows 上 `at_shell` 恒为 false，标题都留着；那边 ConPTY
    /// 吞掉 OSC 7501，服务端本来也不会用
    fn on_title(&mut self, title: String) {
        if title == self.title {
            return;
        }
        let title = if crate::fg::at_shell(self.master.as_ref(), self.shell_pid) { String::new() } else { title };
        self.set_title(title);
    }

    /// 标题变了才发：程序可能反复写同一个
    fn set_title(&mut self, title: String) {
        if title != self.title {
            self.title = title;
            self.broadcast(&HolderMsg::Event(Event::Title { title: self.title.clone() }));
        }
    }

    fn on_frame(&mut self, id: u64, msg: ClientMsg) {
        let hello = self.conns.get(&id).is_some_and(|c| c.hello);
        if !hello {
            let ClientMsg::Hello(hi) = msg else {
                // 第一帧必须是 Hello
                self.drop_conn(id);
                return;
            };
            let Some(proto) = negotiate((PROTO_MIN, PROTO_MAX), (hi.proto_min, hi.proto_max)) else {
                self.send(
                    id,
                    &HolderMsg::Error { message: format!("协议不兼容：holder {PROTO_MIN}–{PROTO_MAX}，对端 {}–{}", hi.proto_min, hi.proto_max) },
                );
                self.drop_conn(id);
                return;
            };
            logf!("连接 {id} 握手：{:?} {}（协议 {proto}）", hi.role, hi.client);
            let welcome = HolderMsg::Welcome(Welcome {
                proto,
                version: FALCON_VERSION.into(),
                vt_version: self.vt.version.clone(),
                vt_degraded: self.vt.degraded,
                cols: self.size.cols,
                rows: self.size.rows,
                pid: self.shell_pid,
                started_at: self.started_at,
                session_id: self.session_id.clone(),
                offset: self.offset,
            });
            self.send(id, &welcome);
            self.send(id, &HolderMsg::Status { records: self.status.records().to_vec() });
            // 空的也发：断线期间标题可能被清掉了（RIS），对端手上那个已经不对
            self.send(id, &HolderMsg::Event(Event::Title { title: self.title.clone() }));
            if let Some(c) = self.conns.get_mut(&id) {
                c.hello = true;
                c.role = hi.role;
            }
            return;
        }
        match msg {
            ClientMsg::Hello(_) => {}
            ClientMsg::Input(bytes) => {
                self.touch(id);
                let _ = self.pty_tx.send(bytes);
            }
            ClientMsg::Resize(r) => {
                if let Some(c) = self.conns.get_mut(&id) {
                    c.size = Some(Resize { cols: r.cols.max(1), rows: r.rows.max(1) });
                }
                self.touch(id);
            }
            ClientMsg::Sync { req } => self.sync(id, req),
            ClientMsg::Export { req, format } => {
                if self.vt_alive() {
                    let vreq = self.vt_req(VtPending::Export { conn: id, req, format });
                    self.vt_send(ToVt::Export { req: vreq, format });
                } else {
                    let bytes = self.raw_snapshot();
                    self.send(id, &HolderMsg::Exported { req, bytes });
                }
            }
            ClientMsg::Appearance(a) => self.set_appearance(a),
            ClientMsg::Foreground { req } => {
                let name = crate::fg::foreground(self.master.as_ref(), self.shell_pid);
                self.send(id, &HolderMsg::ForegroundIs { req, name });
            }
            ClientMsg::StatusSeen { ids } => {
                if self.status.seen(&ids) {
                    self.broadcast_status();
                }
            }
            ClientMsg::Terminate => {
                logf!("连接 {id} 要求结束会话");
                crate::proc::kill_session(self.shell_pid, false);
                if self.terminate_at.is_none() {
                    self.terminate_at = Some(Instant::now() + TERM_GRACE);
                }
            }
            ClientMsg::Ping => self.send(id, &HolderMsg::Pong),
            ClientMsg::UpgradeVt { req, binary } => self.start_upgrade(id, req, PathBuf::from(binary)),
            ClientMsg::Unknown(_) => {}
        }
    }

    fn touch(&mut self, id: u64) {
        self.activity += 1;
        let a = self.activity;
        if let Some(c) = self.conns.get_mut(&id) {
            c.active_at = a;
        }
        self.apply_effective_size();
    }

    /// 生效尺寸：最近有过输入或 resize 的那条连接请求的尺寸（5.6）。人手 attach 断开后
    /// 自然落回 falcon-server 的请求
    fn apply_effective_size(&mut self) {
        let want = self.conns.values().filter(|c| c.size.is_some()).max_by_key(|c| c.active_at).and_then(|c| c.size);
        let Some(want) = want else { return };
        if want == self.size {
            return;
        }
        self.size = want;
        let _ = self.master.resize(PtySize { rows: want.rows, cols: want.cols, pixel_width: 0, pixel_height: 0 });
        self.vt_send(ToVt::Resize(want));
    }

    fn set_appearance(&mut self, a: Appearance) {
        let flipped = matches!((self.appearance.as_ref().and_then(|p| p.scheme), a.scheme), (Some(old), Some(new)) if old != new);
        self.appearance = Some(a.clone());
        self.vt_send(ToVt::Appearance(a.clone()));
        // 深浅真的翻转、且程序订了 2031 时才注入通知：没人订时这串字节会被前台程序当键盘输入
        if flipped && self.modes.theme_notify() {
            let msg: &[u8] = if a.scheme == Some(Scheme::Light) { b"\x1b[?997;2n" } else { b"\x1b[?997;1n" };
            let _ = self.pty_tx.send(msg.to_vec());
        }
    }

    // ------------------------------------------------------------ 快照

    fn sync(&mut self, id: u64, req: u32) {
        if !self.vt_alive() {
            let snapshot = self.raw_snapshot();
            self.send(id, &HolderMsg::Synced { req, offset: self.offset, snapshot, continuation: Vec::new() });
            return;
        }
        let offset = self.offset;
        let Some(c) = self.conns.get_mut(&id) else { return };
        c.syncs.push_back(offset);
        let vreq = self.vt_req(VtPending::Sync { conn: id, req });
        self.vt_send(ToVt::Snapshot { req: vreq });
    }

    fn on_synced(&mut self, conn: u64, req: u32, snapshot: Vec<u8>, continuation: Vec<u8>) {
        let Some(c) = self.conns.get_mut(&conn) else { return };
        let Some(offset) = c.syncs.pop_front() else { return };
        c.out.send(HolderMsg::Synced { req, offset, snapshot, continuation }.encode());
        // 放出扣下的：没有别的在途 Sync 就全放，否则只放下一个切点之前的
        let until = c.syncs.front().copied().unwrap_or(u64::MAX);
        let held = std::mem::take(&mut c.held);
        for (at, bytes) in held {
            if at < until {
                c.out.send(HolderMsg::Output { offset: at, bytes }.encode());
            } else {
                c.held.push((at, bytes));
            }
        }
    }

    // ------------------------------------------------------------ VT 进程

    fn vt_alive(&self) -> bool {
        !self.vt.degraded && self.vt.tx.is_some()
    }

    fn vt_req(&mut self, p: VtPending) -> u32 {
        let r = self.next_vt_req;
        self.next_vt_req = self.next_vt_req.wrapping_add(1).max(1);
        self.vt_pending.insert(r, p);
        r
    }

    fn vt_send(&mut self, msg: ToVt) {
        let Some(tx) = &self.vt.tx else { return };
        if self.vt.degraded {
            return;
        }
        let frame = msg.encode();
        let len = frame.len();
        if self.vt.queued.fetch_add(len, Ordering::Relaxed) + len > VT_QUEUE_MAX {
            logf!("VT 进程积压超过 {} MB，杀掉重启", VT_QUEUE_MAX >> 20);
            self.vt.queued.fetch_sub(len, Ordering::Relaxed);
            if let Some(child) = &mut self.vt.child {
                let _ = child.kill();
            }
            return;
        }
        if tx.send(frame).is_err() {
            self.vt.queued.fetch_sub(len, Ordering::Relaxed);
        }
    }

    fn start_vt(&mut self, seed: Seed) {
        self.vt.generation += 1;
        let generation = self.vt.generation;
        if let Some(mut old) = self.vt.child.take() {
            let _ = old.kill();
            let _ = old.wait();
        }
        self.vt.tx = None;
        self.vt.version.clear();
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.layout.log_path(&self.sid)).ok();
        let mut cmd = std::process::Command::new(&self.vt.binary);
        cmd.args([
            "vt",
            "--cols",
            &self.size.cols.to_string(),
            "--rows",
            &self.size.rows.to_string(),
            "--scrollback",
            &self.scrollback.to_string(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(match log {
            Some(f) => std::process::Stdio::from(f),
            None => std::process::Stdio::null(),
        });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // CREATE_NO_WINDOW：holder 没有控制台，VT 进程也别弹出一个
            cmd.creation_flags(0x0800_0000);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                logf!("起不来 VT 进程（{}）：{e}", self.vt.binary.display());
                self.vt_crashed();
                return;
            }
        };
        let mut stdin = child.stdin.take().expect("piped");
        let mut stdout = child.stdout.take().expect("piped");
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let queued = Arc::new(AtomicUsize::new(0));
        {
            let queued = queued.clone();
            let _ = std::thread::Builder::new().name(format!("vt{generation}-w")).spawn(move || {
                while let Ok(frame) = rx.recv() {
                    let len = frame.len();
                    if stdin.write_all(&frame).is_err() {
                        return;
                    }
                    queued.fetch_sub(len, Ordering::Relaxed);
                }
            });
        }
        {
            let events = self.events.clone();
            let _ = std::thread::Builder::new().name(format!("vt{generation}-r")).spawn(move || {
                loop {
                    match vtproto::read_frame(&mut stdout) {
                        Ok(Some((k, p))) => {
                            if let Ok(msg) = FromVt::decode(k, p)
                                && events.send(Ev::Vt(generation, msg)).is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                let _ = events.send(Ev::VtDied(generation));
            });
        }
        self.vt.child = Some(child);
        self.vt.tx = Some(tx);
        self.vt.queued = queued;

        match seed {
            Seed::Tail => {
                let tail = self.tail_bytes();
                if !tail.is_empty() {
                    self.vt_send(ToVt::Feed { replay: true, bytes: tail });
                }
            }
            Seed::Restore(snapshot, after) => {
                self.vt_send(ToVt::Restore(snapshot));
                if !after.is_empty() {
                    self.vt_send(ToVt::Feed { replay: true, bytes: after });
                }
            }
        }
        self.vt_send(ToVt::Resize(self.size));
        if let Some(a) = self.appearance.clone() {
            self.vt_send(ToVt::Appearance(a));
        }
        self.reissue_pending();
    }

    /// 新 VT 进程起来后，旧进程没答完的请求重新发：快照改基到当前偏移，扣下的输出都已含在
    /// 新快照里，丢掉
    fn reissue_pending(&mut self) {
        let pending: Vec<VtPending> = std::mem::take(&mut self.vt_pending).into_values().collect();
        let offset = self.offset;
        for c in self.conns.values_mut() {
            for n in c.syncs.iter_mut() {
                *n = offset;
            }
            c.held.clear();
        }
        for p in pending {
            match p {
                VtPending::Sync { conn, req } => {
                    let vreq = self.vt_req(VtPending::Sync { conn, req });
                    self.vt_send(ToVt::Snapshot { req: vreq });
                }
                VtPending::Export { conn, req, format } => {
                    let vreq = self.vt_req(VtPending::Export { conn, req, format });
                    self.vt_send(ToVt::Export { req: vreq, format });
                }
                // 旧进程的二进制快照拿不回来了：升级失败（on_vt_died 里已经回话）
                VtPending::Encode => {}
            }
        }
    }

    fn on_vt(&mut self, msg: FromVt) {
        match msg {
            FromVt::Ready(r) => {
                logf!("VT 进程就绪（ghostty {}）", r.ghostty);
                self.vt.version = r.ghostty;
            }
            FromVt::PtyWrite(b) => {
                let _ = self.pty_tx.send(b);
            }
            FromVt::Status(report) => {
                if self.status.apply(report, now_ms()) {
                    self.broadcast_status();
                }
            }
            FromVt::Prompt(p) => {
                // 规范：新提示符开始时必须丢掉 working / blocked
                if p.kind == "A" && self.status.drop_active() {
                    self.broadcast_status();
                }
            }
            FromVt::Title(title) => self.on_title(title),
            FromVt::Reset => {
                if self.status.reset() {
                    self.broadcast_status();
                }
                // RIS 也把窗口标题复位
                self.set_title(String::new());
            }
            FromVt::Snapshotted { req, snapshot, continuation } => {
                if let Some(VtPending::Sync { conn, req: creq }) = self.vt_pending.remove(&req) {
                    self.on_synced(conn, creq, snapshot, continuation);
                }
            }
            FromVt::Exported { req, bytes } => {
                if let Some(VtPending::Export { conn, req: creq, .. }) = self.vt_pending.remove(&req) {
                    self.send(conn, &HolderMsg::Exported { req: creq, bytes });
                }
            }
            FromVt::Encoded { req, bytes } => {
                if let Some(VtPending::Encode) = self.vt_pending.remove(&req) {
                    self.finish_encode(bytes);
                }
            }
            FromVt::Restored(r) => {
                let Some(u) = self.upgrade.take() else { return };
                if !r.ok {
                    // 新版本解不了旧快照：退回原始尾巴重建（设计文档 2.3）
                    logf!("新 VT 进程恢复快照失败（{}），用原始尾巴重建", r.message);
                    let tail = self.tail_bytes();
                    self.vt_send(ToVt::Feed { replay: true, bytes: tail });
                }
                logf!("VT 进程已换成 {}", self.vt.binary.display());
                let message = if r.ok { String::new() } else { format!("快照不兼容，已用原始尾巴重建：{}", r.message) };
                self.send(u.conn, &HolderMsg::Upgraded { req: u.req, ok: true, message });
                self.broadcast(&HolderMsg::Event(Event::VtRestarted { at_offset: self.offset }));
            }
            FromVt::Unknown(_) => {}
        }
    }

    fn vt_crashed(&mut self) {
        let now = Instant::now();
        self.vt.crashes.push_back(now);
        while self.vt.crashes.front().is_some_and(|t| now.duration_since(*t) > VT_CRASH_WINDOW) {
            self.vt.crashes.pop_front();
        }
        if self.vt.crashes.len() >= VT_CRASH_LIMIT {
            logf!("VT 进程 {} 秒内崩了 {} 次，降级为纯转发", VT_CRASH_WINDOW.as_secs(), self.vt.crashes.len());
            self.vt.degraded = true;
            self.vt.tx = None;
            self.vt.child = None;
            // 在途的请求就地用原始尾巴回答
            let pending: Vec<VtPending> = std::mem::take(&mut self.vt_pending).into_values().collect();
            let snapshot = self.raw_snapshot();
            let offset = self.offset;
            for p in pending {
                match p {
                    VtPending::Sync { conn, req } => {
                        if let Some(c) = self.conns.get_mut(&conn) {
                            c.syncs.clear();
                            c.held.clear();
                        }
                        self.send(conn, &HolderMsg::Synced { req, offset, snapshot: snapshot.clone(), continuation: Vec::new() });
                    }
                    VtPending::Export { conn, req, .. } => self.send(conn, &HolderMsg::Exported { req, bytes: snapshot.clone() }),
                    VtPending::Encode => {}
                }
            }
            self.broadcast(&HolderMsg::Event(Event::VtDegraded));
            return;
        }
        self.start_vt(Seed::Tail);
        self.broadcast(&HolderMsg::Event(Event::VtRestarted { at_offset: self.offset }));
    }

    fn on_vt_died(&mut self) {
        if let Some(mut c) = self.vt.child.take() {
            let _ = c.wait();
        }
        self.vt.tx = None;
        logf!("VT 进程退出了");
        if let Some(u) = self.upgrade.take() {
            match u.restoring {
                // 新二进制起来就崩：退回旧的，这次不算升级
                Some(old) => {
                    logf!("新 VT 进程（{}）起不来，退回 {}", self.vt.binary.display(), old.display());
                    self.vt.binary = old;
                    self.send(u.conn, &HolderMsg::Upgraded { req: u.req, ok: false, message: "新的 VT 进程起不来，已退回".into() });
                }
                None => self.send(u.conn, &HolderMsg::Upgraded { req: u.req, ok: false, message: "旧 VT 进程在交接前退出".into() }),
            }
        }
        if self.exit.is_some() {
            return;
        }
        self.vt_crashed();
    }

    // ------------------------------------------------------------ 热升级（设计文档 2.3）

    fn start_upgrade(&mut self, conn: u64, req: u32, binary: PathBuf) {
        let fail = |me: &Self, message: &str| me.send(conn, &HolderMsg::Upgraded { req, ok: false, message: message.into() });
        if self.upgrade.is_some() {
            return fail(self, "已经有一次升级在进行");
        }
        if !binary.is_file() {
            return fail(self, &format!("没有这个文件：{}", binary.display()));
        }
        if self.vt.degraded {
            // 降级了就直接起新的：用原始尾巴重建，顺带解除降级
            self.vt.degraded = false;
            self.vt.crashes.clear();
            let old = std::mem::replace(&mut self.vt.binary, binary);
            self.upgrade = Some(Upgrade { conn, req, binary: self.vt.binary.clone(), after: Vec::new(), restoring: Some(old) });
            self.start_vt(Seed::Tail);
            // 没有快照可恢复：直接算完成
            let u = self.upgrade.take().expect("刚放进去");
            self.send(u.conn, &HolderMsg::Upgraded { req: u.req, ok: true, message: "VT 进程曾降级，已用原始尾巴重建".into() });
            return;
        }
        if !self.vt_alive() {
            return fail(self, "VT 进程不在");
        }
        let vreq = self.vt_req(VtPending::Encode);
        self.upgrade = Some(Upgrade { conn, req, binary, after: Vec::new(), restoring: None });
        self.vt_send(ToVt::Encode { req: vreq });
    }

    fn finish_encode(&mut self, snapshot: Vec<u8>) {
        let Some(u) = &mut self.upgrade else { return };
        if snapshot.is_empty() {
            let (conn, req) = (u.conn, u.req);
            self.upgrade = None;
            self.send(conn, &HolderMsg::Upgraded { req, ok: false, message: "旧 VT 进程导不出快照".into() });
            return;
        }
        let after = std::mem::take(&mut u.after);
        let old = std::mem::replace(&mut self.vt.binary, u.binary.clone());
        u.restoring = Some(old);
        logf!("热升级 VT 进程：快照 {} 字节，之后 {} 字节", snapshot.len(), after.len());
        self.start_vt(Seed::Restore(snapshot, after));
    }

    // ------------------------------------------------------------ 收尾

    fn finish(&mut self, endpoint: &str) {
        let status = self.exit.map(|(s, _)| s).unwrap_or_default();
        // 规范：附着的进程退出时必须丢掉 working / blocked
        if self.status.drop_active() {
            self.broadcast_status();
        }
        let _ = self.layout.write_exit(&self.sid, &ExitRecord { status, at: now_ms() });
        self.broadcast(&HolderMsg::Exit(status));
        let _ = std::fs::remove_file(self.layout.meta_path(&self.sid));
        #[cfg(unix)]
        let _ = std::fs::remove_file(endpoint);
        #[cfg(not(unix))]
        let _ = endpoint;
        if let Some(mut c) = self.vt.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        // 把 Exit 帧送出去再走：最多等 2 秒
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !self.conns.values().all(|c| c.out.drained()) {
            std::thread::sleep(Duration::from_millis(20));
        }
        for c in self.conns.values() {
            c.closer.close();
        }
        logf!("holder 退出：{status:?}");
    }
}
