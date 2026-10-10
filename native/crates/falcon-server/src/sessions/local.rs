//! 本地宿主（后端所在机器）的会话：Zellij 准备、会话查询、PTY 附着。移植自
//! `packages/server/src/sessions/local.ts`，以及 `loginEnv.ts` / `zellij/exec.ts` 的运行时部分。
//!
//! PTY 用 portable-pty（Unix openpty / Windows ConPTY）。读端是阻塞的：一个 PTY 一条读线程、
//! 一条写线程，读到的字节经 channel 回到会话核心的 LocalSet 上按块流式解码（PTY 读块会把
//! UTF-8 多字节字符切开）。

use std::cell::RefCell;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use falcon_proto::{NonDurableReason, ZellijInstallFailure};
use futures::FutureExt as _;
use futures::future::Shared;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::io::AsyncReadExt as _;
use tokio_util::sync::CancellationToken;

use super::backend::{AttachResult, Backend, BackendCallbacks, SessionGoneError, normalize_captured};
use super::login_env::{
    PROBE_OUTPUT_CAP, PROBE_TIMEOUT_MS, RESOLVING_ENV_VAR, detect_fallback_lang, has_locale, login_env_probe_args,
    merge_base_env, node_platform, parse_login_env, pick_login_probe_shell,
};
use super::scroll_plugin::ensure_local_scroll_plugin;
use super::ssh_zellij::AttachOptions;
use crate::askpass::install::prepend_path;
use crate::exec::{ExecResult, LocalBoxFuture, LocalExec, Utf8Decoder, local_exec};
use crate::term_env::apply_term_pty_env;
use crate::zellij::command::{self as zcmd, ScrollPosition, SessionProfile};
use crate::zellij::host::{Downloader, HostKind, HostLayout, build_command_line, encode_powershell, merge_env};
use crate::zellij::install::{InstallError, InstallOptions, StageFn, ensure_zellij};
use crate::zellij::version::local_target;

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

/// 本地宿主的 Zellij 就绪状态。durable=false 时 reason 必定有值。
#[derive(Debug, Clone)]
pub struct LocalZellij {
    pub durable: bool,
    pub reason: Option<NonDurableReason>,
    /// 失败详情（命令 stderr 或异常消息），供用户判断该修什么再重试
    pub detail: Option<String>,
    pub layout: Option<HostLayout>,
    /// 滚动位置插件已就位且已预授权（ADR 0019）；新会话据此决定用哪套配置
    pub scroll: bool,
}

impl LocalZellij {
    pub(crate) fn non_durable(reason: NonDurableReason, detail: Option<String>) -> Self {
        LocalZellij { durable: false, reason: Some(reason), detail, layout: None, scroll: false }
    }
}

type BaseEnv = Shared<LocalBoxFuture<'static, Rc<Vec<(String, String)>>>>;

/// 本机这台宿主：整个后端进程一份，挂在会话核心上
pub struct LocalHost {
    data_dir: PathBuf,
    prepared: RefCell<Option<LocalZellij>>,
    base_env: RefCell<Option<BaseEnv>>,
}

impl LocalHost {
    pub fn new(data_dir: PathBuf) -> Self {
        LocalHost { data_dir, prepared: RefCell::new(None), base_env: RefCell::new(None) }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 准备本地宿主的 Zellij。整个后端进程只做一次，结果缓存。
    ///
    /// 本地二进制落在后端的 --data-dir 下（不是硬编码 home）：用户指定了数据目录，
    /// 依赖就该跟着走。一律用 falcon 锁定的版本，无视系统 PATH 里可能存在的 Zellij——
    /// 版本锁定的全部价值就在于测试矩阵封闭，开一个"用户系统版本"的口子就等于放弃它。
    pub async fn prepare(&self, on_stage: Option<StageFn<'_>>, cancel: Option<&CancellationToken>) -> LocalZellij {
        if let Some(p) = self.prepared.borrow().clone() {
            return p;
        }
        // Windows 上先问 Job Object：在 Job 里就拿不到持久性，装了也白装
        if is_process_in_job().await {
            let state = LocalZellij::non_durable(NonDurableReason::WindowsJobObject, None);
            *self.prepared.borrow_mut() = Some(state.clone());
            return state;
        }
        let Some(target) = local_target() else {
            let state = LocalZellij::non_durable(NonDurableReason::ArchUnsupported, None);
            *self.prepared.borrow_mut() = Some(state.clone());
            return state;
        };
        let opts = InstallOptions {
            kind: local_kind(),
            root: self.data_dir.to_string_lossy().into_owned(),
            target,
            base_url: None,
            downloader: local_downloader().await,
            has_tar: Some(local_has_tar().await),
            on_stage,
            cancel,
        };
        let state = match ensure_zellij(&LocalExec, &opts).await {
            Ok(layout) => {
                // 插件部署失败不挡开会话，新会话退回老配置（没有滚动条）而已
                let scroll = local_kind() == HostKind::Posix && ensure_local_scroll_plugin(&layout, target, &home_dir());
                LocalZellij { durable: true, reason: None, detail: None, layout: Some(layout), scroll }
            }
            Err(InstallError { reason, detail, .. }) => {
                let failed = LocalZellij::non_durable(
                    NonDurableReason::from_wire(reason.as_str()).unwrap_or(NonDurableReason::VerifyFailed),
                    detail,
                );
                // 取消是用户这一次的选择，不是这台机器的属性——缓存它会让后续会话
                // 全部莫名其妙地非持久，直到后端重启
                if reason == ZellijInstallFailure::Cancelled {
                    return failed;
                }
                failed
            }
        };
        *self.prepared.borrow_mut() = Some(state.clone());
        state
    }

    /// 已探测到的本地状态；None = 还没探测过。供 /api/system 报告用——它不该触发下载
    pub fn peek(&self) -> Option<LocalZellij> {
        self.prepared.borrow().clone()
    }

    /// 重试用：清掉缓存的准备结果
    pub fn reset(&self) {
        self.prepared.borrow_mut().take();
    }

    /// 测试用：直接给定准备结果，不去装 Zellij
    #[cfg(test)]
    pub(crate) fn set_prepared(&self, state: LocalZellij) {
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
    /// 进程环境缺 PATH 补全与 LANG，而 pane 里的 shell 是非 login 起的，修不回来。
    pub async fn base_env(&self) -> Rc<Vec<(String, String)>> {
        let pending = self.base_env.borrow().clone();
        let fut = match pending {
            Some(f) => f,
            None => {
                let f: LocalBoxFuture<'static, _> = Box::pin(async { Rc::new(resolve_base_env().await) });
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

/// 本地是否有可用的下载工具
async fn local_downloader() -> Downloader {
    let probe = if local_kind() == HostKind::Windows { "where curl.exe" } else { "command -v curl || command -v wget" };
    let res = local_exec(probe, None).await;
    if !res.ok() {
        return Downloader::None;
    }
    let out = res.stdout.to_lowercase();
    if out.contains("curl") {
        Downloader::Curl
    } else if out.contains("wget") {
        Downloader::Wget
    } else {
        Downloader::None
    }
}

/// Windows 10 1803 起内置 tar.exe（bsdtar，能解 zip）
async fn local_has_tar() -> bool {
    if local_kind() != HostKind::Windows {
        return true;
    }
    local_exec("where tar.exe", None).await.ok()
}

/// 后端进程是否处于 Job Object 中。
///
/// Zellij 在 Windows 上还没有让 server 脱离父进程 Job 的能力（PR #5195 至今未合并），
/// 所以后端若身处 Job 中，后端一退出 Zellij server 会被连坐杀掉——持久性直接归零，而且是
/// **静默**失效。这比诚实标注"非持久"更坏。用 PowerShell 的 P/Invoke 问一次 IsProcessInJob，
/// 避免为一个布尔值引入 FFI。查不出来时按最坏情况处理：宁可标非持久，也不要骗用户。
async fn is_process_in_job() -> bool {
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

// ---- Zellij 操作 ----

async fn zellij(layout: &HostLayout, args: Vec<String>) -> ExecResult {
    let argv: Vec<String> = std::iter::once(layout.bin.clone()).chain(args).collect();
    local_exec(&build_command_line(local_kind(), &argv, &zcmd::zellij_env(layout), &[]), None).await
}

pub async fn local_has_session(layout: &HostLayout, session_id: &str) -> bool {
    // `ls` 一个 session 都没有时退出码是 1（stderr 打 "No active zellij sessions found."），
    // 所以非零退出码不代表出错，直接按"没有"处理即可。
    let res = zellij(layout, zcmd::list_args(layout)).await;
    res.ok() && zcmd::parse_live_sessions(&res.stdout).contains(&zcmd::zellij_session_name(session_id))
}

pub async fn local_capture(layout: &HostLayout, session_id: &str) -> String {
    // 多一次调用换来的是接回时真能拿到内容，理由见 dump_screen_args
    let panes = zellij(layout, zcmd::list_panes_args(layout, session_id)).await;
    let Some(pane_id) = panes.ok().then(|| zcmd::parse_terminal_pane_id(&panes.stdout)).flatten() else {
        return String::new();
    };
    let res = zellij(layout, zcmd::dump_screen_args(layout, session_id, &pane_id)).await;
    if !res.ok() {
        return String::new();
    }
    normalize_captured(&format!("{}\n", crate::term_env::js::trim_end(&res.stdout)))
}

/// 会话聚焦 pane 的前台命令；空闲或问不到时为 None。见 parse_client_running_command
pub async fn local_foreground(layout: &HostLayout, session_id: &str) -> Option<String> {
    let res = zellij(layout, zcmd::list_clients_args(layout, session_id)).await;
    res.ok().then(|| zcmd::parse_client_running_command(&res.stdout)).flatten()
}

/// 会话里 terminal pane 的数字 id（滚动位置插件按它找 pane）；问不到为 None
pub async fn local_terminal_pane(layout: &HostLayout, session_id: &str) -> Option<u32> {
    let res = zellij(layout, zcmd::list_panes_args(layout, session_id)).await;
    res.ok().then(|| zcmd::parse_terminal_pane_id(&res.stdout)).flatten().and_then(|id| zcmd::parse_terminal_pane_number(&id))
}

/// 插件没回话（会话里没有插件、或卡住了）时最多等多久
const SCROLL_PIPE_TIMEOUT: Duration = Duration::from_millis(3000);

/// 问滚动位置插件（ADR 0019）。会话里没有插件时回 None。local_exec 的 stdin 本来就是
/// /dev/null，`zellij pipe` 读到 EOF 就会退出；仍然套一个超时防插件卡住
pub async fn local_scroll_pipe(layout: &HostLayout, session_id: &str, pane: u32, seek: Option<f64>) -> Option<ScrollPosition> {
    let argv: Vec<String> = std::iter::once(layout.bin.clone()).chain(zcmd::scroll_pipe_args(layout, session_id, pane, seek)).collect();
    let cmd = build_command_line(local_kind(), &argv, &zcmd::zellij_env(layout), &[]);
    let res = tokio::time::timeout(SCROLL_PIPE_TIMEOUT, local_exec(&cmd, None)).await.ok()?;
    zcmd::parse_scroll_reply(&res.stdout)
}

pub async fn local_kill(layout: &HostLayout, session_id: &str) {
    let _ = zellij(layout, zcmd::delete_session_args(layout, session_id)).await;
}

/// 列出该宿主机上所有 falcon 建的会话名（孤儿会话检测用）
pub async fn local_list_falcon_sessions(layout: &HostLayout) -> Vec<String> {
    let res = zellij(layout, zcmd::list_args(layout)).await;
    if !res.ok() {
        return Vec::new();
    }
    zcmd::parse_live_sessions(&res.stdout).into_iter().filter(|n| zcmd::is_falcon_session(n)).collect()
}

// ---- 附着 ----

pub async fn attach_local(host: &LocalHost, opts: &AttachOptions, cb: BackendCallbacks) -> anyhow::Result<AttachResult> {
    let base = host.base_env().await;
    let mut captured_history = None;

    let (program, args, mut env, cwd) = if opts.durable {
        let layout = opts.layout.as_ref().ok_or_else(|| anyhow::anyhow!("持久会话缺少 Zellij 布局"))?;
        if opts.reattach {
            if !local_has_session(layout, &opts.session_id).await {
                return Err(SessionGoneError.into());
            }
            captured_history = Some(local_capture(layout, &opts.session_id).await);
        }
        let mut env: Vec<(String, String)> = base.as_ref().clone();
        merge_env(&mut env, zcmd::zellij_env(layout));
        let env = apply_term_pty_env(env.into_iter().map(|(k, v)| (k, Some(v))), opts.appearance);
        // 始终显式给 shell：Zellij 在 $SHELL 缺失时 pane 起不来（见 POSIX_PROBE 注释）
        let profile = SessionProfile {
            cwd: opts.cwd.clone(),
            shell: Some(opts.shell.clone().unwrap_or_else(default_local_shell)),
            scroll: opts.scroll,
        };
        (layout.bin.clone(), zcmd::attach_args(layout, &opts.session_id, &profile), env, None)
    } else {
        let env = apply_term_pty_env(base.iter().map(|(k, v)| (k.clone(), Some(v.clone()))), opts.appearance);
        (opts.shell.clone().unwrap_or_else(default_local_shell), Vec::new(), env, opts.cwd.clone())
    };
    if let Some(bin) = &opts.askpass_bin {
        prepend_path(&mut env, bin);
        merge_env(&mut env, [("FALCON_SESSION_ID", opts.session_id.as_str())]);
    }

    let pair = native_pty_system().openpty(PtySize { rows: opts.rows, cols: opts.cols, pixel_width: 0, pixel_height: 0 })?;
    let mut cmd = CommandBuilder::new(&program);
    cmd.args(&args);
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
        // 仅非持久会话有意义：持久会话外层 PTY 的前台永远是 Zellij 客户端。
        // Windows 上问不到前台进程，报出去只会造成误弹确认
        report_process: !opts.durable && cfg!(unix),
    };
    Ok(AttachResult { backend: Rc::new(backend), durable: opts.durable, captured_history })
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
