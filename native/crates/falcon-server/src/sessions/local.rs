//! 本地宿主（后端所在机器）的会话。移植自 `packages/server/src/sessions/local.ts` 与
//! `loginEnv.ts` 的运行时部分。
//!
//! - 持久会话：宿主机就是本机，会话守护进程（falcon-sessiond，ADR 0020）拷进 `--data-dir` 后
//!   `spawn` 一个 holder，后端直接连它的 Unix socket / 命名管道；
//! - 非持久会话：后端自己开 PTY。portable-pty（Unix openpty / Windows ConPTY），读端是阻塞的：
//!   一个 PTY 一条读线程、一条写线程，读到的字节经 channel 回到会话核心的 LocalSet 上按块
//!   流式解码（PTY 读块会把 UTF-8 多字节字符切开）。

use std::cell::RefCell;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use falcon_proto::sessiond::{ExitRecord, ListEntry, SessionMeta, SpawnResult, Welcome, sid_of};
use falcon_proto::{HostKind, InstallFailure, NonDurableReason};
use futures::FutureExt as _;
use futures::future::{BoxFuture, Shared};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::io::AsyncReadExt as _;

use super::backend::{AttachResult, Backend, BackendCallbacks};
use super::login_env::{
    PROBE_OUTPUT_CAP, PROBE_TIMEOUT_MS, RESOLVING_ENV_VAR, detect_fallback_lang, has_locale, login_env_probe_args,
    merge_base_env, node_platform, parse_login_env, pick_login_probe_shell,
};
use super::ssh_host::{AttachOptions, spawn_spec};
use crate::askpass::install::prepend_path;
use crate::exec::{LocalBoxFuture, Utf8Decoder, local_exec};
use crate::host::{encode_powershell, merge_env};
use crate::sessiond::backend::{ConnectError, SessiondBackend, Transport, connect};
use crate::sessiond::command::{kill_argv, list_argv, spawn_argv};
use crate::sessiond::deploy::{Deployed, ensure_local};
use crate::term_env::apply_term_pty_env;

pub fn local_kind() -> HostKind {
    if cfg!(windows) { HostKind::Windows } else { HostKind::Posix }
}

pub fn default_local_shell() -> String {
    if cfg!(windows) {
        return "powershell.exe".into();
    }
    std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/bash".into())
}

pub fn home_dir() -> String {
    crate::config::home_dir().to_string_lossy().into_owned()
}

/// 本地宿主的持久能力。durable=false 时 reason 必定有值。
#[derive(Debug, Clone)]
pub struct LocalSessiond {
    pub durable: bool,
    pub reason: Option<NonDurableReason>,
    /// 失败详情，供用户判断该修什么再重试
    pub detail: Option<String>,
    pub deployed: Option<Deployed>,
}

impl LocalSessiond {
    pub(crate) fn non_durable(reason: NonDurableReason, detail: Option<String>) -> Self {
        LocalSessiond { durable: false, reason: Some(reason), detail, deployed: None }
    }
}

type BaseEnv = Shared<LocalBoxFuture<'static, Rc<Vec<(String, String)>>>>;

/// 本机这台宿主：整个后端进程一份，挂在会话核心上
pub struct LocalHost {
    data_dir: PathBuf,
    prepared: RefCell<Option<LocalSessiond>>,
    base_env: RefCell<Option<BaseEnv>>,
}

impl LocalHost {
    pub fn new(data_dir: PathBuf) -> Self {
        LocalHost { data_dir, prepared: RefCell::new(None), base_env: RefCell::new(None) }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 准备本地宿主的会话守护进程：拷进 `<dataDir>/bin/` 并握手。整个后端进程只做一次，结果缓存。
    ///
    /// Windows 上后端在 Job Object 里也照样试：守护进程自己会用 breakaway / WMI 脱离（spawn.rs），
    /// 真脱离不了时 spawn 会失败，建会话那一步再标非持久（windows-job-object）。
    pub async fn prepare(&self) -> LocalSessiond {
        if let Some(p) = self.prepared.borrow().clone() {
            return p;
        }
        let state = match ensure_local(&self.data_dir).await {
            Ok(deployed) => LocalSessiond { durable: true, reason: None, detail: None, deployed: Some(deployed) },
            Err(e) => LocalSessiond::non_durable(NonDurableReason::from(e.reason), e.detail),
        };
        *self.prepared.borrow_mut() = Some(state.clone());
        state
    }

    /// 已探测到的本地状态；None = 还没探测过。供 /api/system 报告用——它不该触发部署
    pub fn peek(&self) -> Option<LocalSessiond> {
        self.prepared.borrow().clone()
    }

    /// 重试用：清掉缓存的准备结果
    pub fn reset(&self) {
        self.prepared.borrow_mut().take();
    }

    /// 测试用：直接给定准备结果，不去部署守护进程
    #[cfg(test)]
    pub(crate) fn set_prepared(&self, state: LocalSessiond) {
        *self.prepared.borrow_mut() = Some(state);
    }

    /// 测试用：直接给定基底环境，不去跑用户的 login shell（那会执行用户真实的 rc 文件）
    #[cfg(test)]
    pub(crate) fn set_base_env(&self, env: Vec<(String, String)>) {
        let f: LocalBoxFuture<'static, _> = Box::pin(std::future::ready(Rc::new(env)));
        *self.base_env.borrow_mut() = Some(f.shared());
    }

    /// 本地 PTY 的基底环境（缓存）。附着时每次 await 它；启动时预热一次，首个本地会话
    /// 就不用等 login shell 起完。
    ///
    /// 基底不是服务端进程自己的环境而是 login 解析结果：后端可能由 launchd / IDE 启动，
    /// 进程环境缺 PATH 补全与 LANG，而会话里的 shell 是非 login 起的，修不回来。
    pub async fn base_env(&self) -> Rc<Vec<(String, String)>> {
        let pending = self.base_env.borrow().clone();
        let fut = match pending {
            Some(f) => f,
            None => {
                // 底下是进程级的那一份（local_base_env）：meegle / px0 也用它，login shell 只跑一次
                let f: LocalBoxFuture<'static, _> = Box::pin(async { Rc::new(local_base_env().await.as_ref().clone()) });
                let shared = f.shared();
                *self.base_env.borrow_mut() = Some(shared.clone());
                shared
            }
        };
        fut.await
    }
}

/// 进程环境（不含我们自己会写进去的东西——Rust 版不改写自身环境，不存在 SEA 把
/// `FALCON_*` 漏进会话的问题）
fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// 在后端本机起进程时用的基底环境，可注入。TS 里是模块级的 `resolveLocalBaseEnv()`，PTY、
/// cloudflared、meegle、px0 共用同一个缓存的 Promise；Rust 版做成函数值，生产上给
/// [`local_base_env_fn`]，测试给一份固定环境，不去跑用户的 login shell。
///
/// 是 `Send + Sync` 的：meegle 客户端挂在 axum 的多线程状态里。
pub type BaseEnvFn = Arc<dyn Fn() -> BoxFuture<'static, Arc<Vec<(String, String)>>> + Send + Sync>;

/// 进程级缓存的本地基底环境（TS 的 `resolveLocalBaseEnv()`）：login shell 整个进程只跑一次，
/// 任何线程都能等。[`LocalHost::base_env`] 也落到这一份上
pub async fn local_base_env() -> Arc<Vec<(String, String)>> {
    static CELL: tokio::sync::OnceCell<Arc<Vec<(String, String)>>> = tokio::sync::OnceCell::const_new();
    CELL.get_or_init(|| async { Arc::new(resolve_base_env().await) }).await.clone()
}

/// [`local_base_env`] 包成 [`BaseEnvFn`]
pub fn local_base_env_fn() -> BaseEnvFn {
    Arc::new(|| Box::pin(local_base_env()))
}

async fn resolve_base_env() -> Vec<(String, String)> {
    let env = process_env();
    // Windows 没有 login shell / rc 这套机制，编码走的也是 PowerShell 的路
    if cfg!(windows) {
        return env;
    }
    // 两条探测并行：login shell 要几百毫秒，locale 探测是两条毫秒级命令
    let shell = pick_login_probe_shell(passwd_shell().as_deref(), std::env::var("SHELL").ok().as_deref());
    let need_lang = !has_locale(&env);
    let (stdout, fallback) = tokio::join!(run_login_shell(&shell), async {
        if need_lang { Some(detect_fallback_lang(|cmd| async move { local_exec(cmd, None).await }, node_platform()).await) } else { None }
    });
    let login = stdout.as_deref().and_then(parse_login_env);
    merge_base_env(&env, login.as_deref(), fallback.as_deref())
}

/// 跑一次 login shell 探测，拿 stdout。任何失败（spawn 报错、超时、输出超限）都返回 None；
/// 退出码不看——rc 末尾一句 `exit 1` 或失败的命令不该否定已经打印出来的 env，标记在不在
/// 由 parse_login_env 判断。
///
/// FALCON_RESOLVING_ENV=1 打给用户的 rc：想跳过重活（nvm、耗时的补全初始化）的可以用它判断，
/// 等价于 VS Code 的 VSCODE_RESOLVING_ENVIRONMENT。
async fn run_login_shell(shell: &str) -> Option<String> {
    let mut child = tokio::process::Command::new(shell)
        .args(login_env_probe_args())
        // rc 文件普遍假设从 home 起步；也避免探测把仓库目录当 cwd 产生副作用
        .current_dir(home_dir())
        .env(RESOLVING_ENV_VAR, "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut out = child.stdout.take()?;
    let read = async {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = out.read(&mut chunk).await.ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            // rc 疯狂输出（死循环、二进制喷屏）时的止损上限
            if buf.len() > PROBE_OUTPUT_CAP {
                return None;
            }
        }
        let _ = child.wait().await;
        Some(String::from_utf8_lossy(&buf).into_owned())
    };
    tokio::time::timeout(Duration::from_millis(PROBE_TIMEOUT_MS), read).await.ok().flatten()
}

/// passwd 条目里的登录 shell。launchd / 服务环境里 SHELL 往往缺失，而 zsh 用户的 PATH
/// 都写在 zsh 的 rc 里——用错 shell 探出来的环境是空的
#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
    if rc != 0 || result.is_null() || pwd.pw_shell.is_null() {
        return None;
    }
    let shell = unsafe { std::ffi::CStr::from_ptr(pwd.pw_shell) }.to_string_lossy().into_owned();
    (!shell.is_empty()).then_some(shell)
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<String> {
    None
}

/// 后端进程是否处于 Job Object 中。只用来给失败起名：守护进程拉不起来、而后端又在 Job 里时，
/// 原因多半就是 Job 不允许脱离（WMI 也被禁了），标 windows-job-object 让用户知道该查什么。
/// 用 PowerShell 的 P/Invoke 问一次 IsProcessInJob，避免为一个布尔值引入 FFI。
pub(crate) async fn is_process_in_job() -> bool {
    if !cfg!(windows) {
        return false;
    }
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if let Some(v) = CACHE.get() {
        return *v;
    }
    let script = [
        "$sig = '[DllImport(\"kernel32.dll\", SetLastError=true)] public static extern bool IsProcessInJob(IntPtr h, IntPtr job, out bool result);'",
        "$k = Add-Type -MemberDefinition $sig -Name Win32Job -Namespace FalconNative -PassThru",
        "$r = $false",
        "$ok = $k::IsProcessInJob([System.Diagnostics.Process]::GetCurrentProcess().Handle, [IntPtr]::Zero, [ref]$r)",
        "if ($ok) { if ($r) { 'yes' } else { 'no' } } else { 'unknown' }",
    ]
    .join("; ");
    // 这条 PowerShell 查的是它自己的进程，但子进程默认继承父进程的 Job，等价于问后端
    let res = local_exec(&encode_powershell(&script), None).await;
    let in_job = res.stdout.trim() != "no";
    *CACHE.get_or_init(|| in_job)
}

// ---- 附着 ----

/// 非持久会话：后端自己开 PTY 跑 shell
pub async fn attach_local(host: &LocalHost, opts: &AttachOptions, cb: BackendCallbacks) -> anyhow::Result<AttachResult> {
    let base = host.base_env().await;
    let mut env = apply_term_pty_env(base.iter().map(|(k, v)| (k.clone(), Some(v.clone()))), opts.appearance, local_kind());
    let program = opts.shell.clone().unwrap_or_else(default_local_shell);
    let cwd = opts.cwd.clone();
    if let Some(bin) = &opts.askpass_bin {
        prepend_path(&mut env, bin);
        merge_env(&mut env, [("FALCON_SESSION_ID", opts.session_id.as_str())]);
    }

    let pair = native_pty_system().openpty(PtySize { rows: opts.rows, cols: opts.cols, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new(&program);
    cmd.env_clear();
    for (k, v) in &env {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd.filter(|d| !d.is_empty()) {
        cmd.cwd(dir);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    // slave 一定要关：不关的话子进程退出后读端永远等不到 EOF
    drop(pair.slave);
    let pid = child.process_id();
    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;

    // 读线程 → LocalSet：字节流式解码后回调
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::Builder::new().name("falcon-pty-read".into()).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
        // 收尸：不 wait 会留僵尸进程
        let _ = child.wait();
    })?;
    tokio::task::spawn_local(async move {
        let mut decoder = Utf8Decoder::default();
        while let Some(bytes) = rx.recv().await {
            let s = decoder.write(&bytes);
            if !s.is_empty() {
                (cb.on_data)(s);
            }
        }
        (cb.on_exit)();
    });

    // 写线程：Backend::write 是同步的，往 PTY 写可能阻塞（前台程序不读输入时）
    let (wtx, wrx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name("falcon-pty-write".into()).spawn(move || {
        while let Ok(data) = wrx.recv() {
            if writer.write_all(&data).and_then(|()| writer.flush()).is_err() {
                break;
            }
        }
    })?;

    let backend = LocalPtyBackend {
        master: Mutex::new(pair.master),
        writer: wtx,
        killer: Mutex::new(killer),
        pid,
        // Windows 上问不到前台进程，报出去只会造成误弹确认
        report_process: cfg!(unix),
    };
    Ok(AttachResult { backend: Rc::new(backend), durable: false })
}

// ---- 守护进程会话 ----

/// 本机上一个守护进程会话现在的情况（holder 留在 `<dataDir>/sessiond/sessions/` 的文件）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalSessionState {
    /// holder 还活着（连不上多半是刚起来 / 卡住，稍后再试）
    Alive,
    /// shell 正常结束了，留下了退出记录
    Exited,
    /// 什么都没留下：不在了
    Gone,
}

fn sessions_dir(deployed: &Deployed) -> PathBuf {
    Path::new(&deployed.root).join(falcon_proto::sessiond::SESSIOND_DIR).join("sessions")
}

fn read_meta(deployed: &Deployed, sid: &str) -> Option<SessionMeta> {
    serde_json::from_slice(&std::fs::read(sessions_dir(deployed).join(format!("{sid}.json"))).ok()?).ok()
}

pub fn local_session_state(deployed: &Deployed, session_id: &str) -> LocalSessionState {
    let sid = sid_of(session_id);
    let dir = sessions_dir(deployed);
    if serde_json::from_slice::<ExitRecord>(&std::fs::read(dir.join(format!("{sid}.exit"))).unwrap_or_default()).is_ok() {
        return LocalSessionState::Exited;
    }
    match read_meta(deployed, &sid) {
        Some(m) if pid_alive(m.pid) => LocalSessionState::Alive,
        _ => LocalSessionState::Gone,
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows：命名管道随 holder 一起消失，而连接那一步已经对"管道忙"重试过（sessiond/backend.rs）。
/// 走到这里说明连不上：有 meta 也按不在了算（holder 被强杀时 meta 来不及删）
#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// 会话的连接端点：meta 里记的为准（holder 实际监听的那个），没有就按同一套规则算
fn endpoint(deployed: &Deployed, sid: &str) -> String {
    if let Some(m) = read_meta(deployed, sid) {
        return m.endpoint;
    }
    #[cfg(unix)]
    {
        falcon_proto::sessiond::socket_path(&deployed.root, sid, unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        falcon_proto::sessiond::pipe_name(&deployed.root, sid)
    }
}

/// 跑一条守护进程子命令拿 stdout（spawn / list / kill）。15 秒兜底：spawn 自己等就绪最多 10 秒
async fn run_sessiond(argv: &[String]) -> Result<String, String> {
    let (bin, args) = argv.split_first().expect("argv");
    let child = tokio::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(15), child).await {
        Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(Ok(out)) => Err([String::from_utf8_lossy(&out.stdout).trim().to_string(), String::from_utf8_lossy(&out.stderr).trim().to_string()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("守护进程 15 秒内没有回话".into()),
    }
}

/// 持久会话：新建时先 `spawn` 一个 holder，再直接连它
pub async fn open_local_sessiond(
    host: &LocalHost,
    opts: &AttachOptions,
    cb: BackendCallbacks,
) -> Result<(Rc<SessiondBackend>, Welcome), ConnectError> {
    let deployed = opts.deployed.as_ref().ok_or_else(|| ConnectError::Failed("缺少本机的守护进程".into()))?;
    let sid = sid_of(&opts.session_id);
    if !opts.reattach {
        let base = host.base_env().await;
        let mut env = apply_term_pty_env(base.iter().map(|(k, v)| (k.clone(), Some(v.clone()))), opts.appearance, local_kind());
        if let Some(bin) = &opts.askpass_bin {
            prepend_path(&mut env, bin);
            merge_env(&mut env, [("FALCON_SESSION_ID", opts.session_id.as_str())]);
        }
        merge_env(&mut env, [("FALCON_SESSIOND_ID", sid.as_str())]);
        let shell = opts.shell.clone().unwrap_or_else(default_local_shell);
        // 本机：服务端已经解析好整份登录环境，子进程只拿这一份
        let spec = spawn_spec(opts, vec![shell], env, true);
        let out = run_sessiond(&spawn_argv(&deployed.bin, &deployed.root, &sid, &spec)).await.map_err(ConnectError::Failed)?;
        let res: SpawnResult =
            serde_json::from_str(out.trim()).map_err(|_| ConnectError::Failed(format!("守护进程的回话看不懂：{}", out.trim())))?;
        if !res.ok {
            return Err(ConnectError::Failed(res.error.unwrap_or_else(|| "守护进程起不来".into())));
        }
    }
    match connect(Transport::Local(endpoint(deployed, &sid)), cb.clone()).await {
        Ok(c) => Ok(c),
        // 连不上：看 holder 留下的文件判断是退出了、不在了，还是暂时连不上
        Err(ConnectError::Unavailable(e)) | Err(ConnectError::Failed(e)) => match local_session_state(deployed, &opts.session_id) {
            LocalSessionState::Exited => Err(ConnectError::Exited),
            LocalSessionState::Gone => Err(ConnectError::Gone),
            LocalSessionState::Alive => Err(ConnectError::Unavailable(e)),
        },
        Err(e) => Err(e),
    }
}

/// 本机这个数据目录下的全部守护进程会话（孤儿清理用）
pub async fn local_list_sessions(deployed: &Deployed) -> Option<Vec<ListEntry>> {
    let out = run_sessiond(&list_argv(&deployed.bin, &deployed.root)).await.ok()?;
    serde_json::from_str(out.trim()).ok()
}

/// 连不上 holder 时按 pid 结束会话
pub async fn local_kill(deployed: &Deployed, sids: &[String]) {
    if !sids.is_empty() {
        let _ = run_sessiond(&kill_argv(&deployed.bin, &deployed.root, sids)).await;
    }
}

/// 守护进程拉不起来时的原因：Windows 上后端在 Job 里多半就是它
pub async fn spawn_failure_reason() -> NonDurableReason {
    if is_process_in_job().await { NonDurableReason::WindowsJobObject } else { NonDurableReason::from(InstallFailure::VerifyFailed) }
}

struct LocalPtyBackend {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: std::sync::mpsc::Sender<Vec<u8>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    report_process: bool,
}

impl Backend for LocalPtyBackend {
    fn write(&self, data: &str) {
        let _ = self.writer.send(data.as_bytes().to_vec());
    }

    fn resize(&self, cols: u16, rows: u16) {
        // PTY 已退出时 resize 会失败，忽略
        let _ = self.master.lock().unwrap_or_else(|e| e.into_inner()).resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    fn destroy(&self) {
        // 同 node-pty 的 kill()：Unix 上发 SIGHUP（shell 来得及收尾、存历史），不是 SIGKILL
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGHUP);
            }
            return;
        }
        let _ = self.killer.lock().unwrap_or_else(|e| e.into_inner()).kill();
    }

    fn process_name(&self) -> Option<String> {
        if !self.report_process {
            return None;
        }
        #[cfg(unix)]
        {
            let pgid = self.master.lock().unwrap_or_else(|e| e.into_inner()).process_group_leader()?;
            process_name_of(pgid)
        }
        #[cfg(not(unix))]
        None
    }
}

/// 前台进程组 leader 的名字（node-pty 的 `IPty.process`）
#[cfg(target_os = "macos")]
fn process_name_of(pid: libc::pid_t) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn process_name_of(pid: libc::pid_t) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim_end().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// 测试用的最小环境：PATH 与 HOME，别的一概不带（不跑 login shell）
    pub(crate) fn test_env() -> Vec<(String, String)> {
        vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("HOME".into(), std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())),
            ("LANG".into(), "en_US.UTF-8".into()),
        ]
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pty_round_trip_and_exit() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let host = LocalHost::new(dir.path().to_path_buf());
                host.set_base_env(test_env());
                let out = Rc::new(RefCell::new(String::new()));
                let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<()>();
                let exit_tx = RefCell::new(Some(exit_tx));
                let o2 = out.clone();
                let cb = BackendCallbacks {
                    on_event: Rc::new(|_| {}),
                    on_data: Rc::new(move |s| o2.borrow_mut().push_str(&s)),
                    on_exit: Rc::new(move || {
                        if let Some(tx) = exit_tx.borrow_mut().take() {
                            let _ = tx.send(());
                        }
                    }),
                };
                let opts = AttachOptions {
                    session_id: "s".into(),
                    shell: Some("/bin/sh".into()),
                    cwd: Some(dir.path().to_string_lossy().into_owned()),
                    durable: false,
                    cols: 120,
                    rows: 30,
                    ..Default::default()
                };
                let r = attach_local(&host, &opts, cb).await.unwrap();
                r.backend.write("printf '%s|%s\\n' \"$TERM\" 中文; stty size; exit\n");
                tokio::time::timeout(Duration::from_secs(10), exit_rx).await.unwrap().unwrap();
                let text = out.borrow().clone();
                assert!(text.contains("xterm-256color|中文"), "{text:?}");
                assert!(text.contains("30 120"), "{text:?}");
            })
            .await;
    }
}
