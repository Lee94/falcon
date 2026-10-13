//! `spawn`：起一个会话的 holder，等它监听就绪再返回（设计文档第 4 节）。
//!
//! holder 必须脱离调用方，否则调用方一走会话就跟着死：
//! - Unix：`setsid` 进新会话，没有控制终端，stdio 指到空设备。spawn 自己随后退出，holder 被
//!   init 收养；
//! - Windows：先用 `CREATE_BREAKAWAY_FROM_JOB` 脱离调用方所在的 Job（sshd 的会话、
//!   被托管的后端都在 Job 里，Job 关闭时整棵进程树陪葬）。Job 不允许脱离时 CreateProcess
//!   返回拒绝访问，改用 WMI `Win32_Process.Create`——WMI 拉起的进程父进程是 WmiPrvSE，
//!   完全在调用方的 Job 之外（远端 Zellij 时代实测能熬过 SSH 断开）。
//!
//! 启动参数经文件交给 holder（`sessions/<sid>.spec`，读完即删），不走命令行：里面有整份
//! 环境变量，Windows 的命令行与 WMI 都有长度上限，也不该出现在进程列表里。

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use falcon_proto::sessiond::{SpawnResult, SpawnSpec};

use crate::ipc;
use crate::layout::Layout;

/// 等 holder 就绪的上限
const READY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn spawn(layout: &Layout, sid: &str, spec: &SpawnSpec) -> Result<SpawnResult> {
    layout.ensure_dirs()?;
    if let Some(meta) = layout.holder_alive(sid)
        && ipc::connect(&meta.endpoint).is_ok()
    {
        return Ok(SpawnResult { ok: true, pid: meta.shell_pid, existing: true, error: None });
    }
    // 上一个同名会话留下的退出记录不能让人误以为新会话已经退出
    let _ = std::fs::remove_file(layout.exit_path(sid));
    layout.write_spec(sid, spec)?;
    let launched = launch(layout, sid)?;
    let started = Instant::now();
    loop {
        if let Some(meta) = layout.read_meta(sid)
            && ipc::connect(&meta.endpoint).is_ok()
        {
            return Ok(SpawnResult { ok: true, pid: meta.shell_pid, existing: false, error: None });
        }
        if launched.exited() {
            bail!("holder 起来就退出了：{}", log_tail(layout, sid));
        }
        if started.elapsed() > READY_TIMEOUT {
            bail!("holder {} 秒内没有就绪：{}", READY_TIMEOUT.as_secs(), log_tail(layout, sid));
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// holder 日志的最后几行，报错时带上，免得用户再去宿主机上翻
fn log_tail(layout: &Layout, sid: &str) -> String {
    let text = std::fs::read_to_string(layout.log_path(sid)).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(5)..].join(" | ");
    if tail.is_empty() { "（没有日志）".into() } else { tail }
}

enum Launched {
    Child(std::process::Child),
    /// WMI 拉起的：只有 pid
    #[cfg_attr(unix, allow(dead_code))]
    Pid(u32),
}

impl Launched {
    fn exited(&self) -> bool {
        match self {
            // holder 是我们的子进程：它退出后成了僵尸，kill(pid, 0) 照样成功，
            // 所以 Unix 上用 waitpid(WNOHANG) 判断（顺便收尸）
            Launched::Child(c) => child_exited(c),
            Launched::Pid(p) => !crate::proc::alive(*p),
        }
    }
}

#[cfg(unix)]
fn child_exited(c: &std::process::Child) -> bool {
    let mut status = 0;
    let r = unsafe { libc::waitpid(c.id() as libc::pid_t, &mut status, libc::WNOHANG) };
    r == c.id() as libc::pid_t || r == -1
}

#[cfg(windows)]
fn child_exited(c: &std::process::Child) -> bool {
    !crate::proc::alive(c.id())
}

fn hold_args(layout: &Layout, sid: &str) -> Vec<String> {
    vec!["hold".into(), "--root".into(), layout.root.to_string_lossy().into_owned(), "--id".into(), sid.into()]
}

#[cfg(unix)]
fn launch(layout: &Layout, sid: &str) -> Result<Launched> {
    use std::os::unix::process::CommandExt as _;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(hold_args(layout, sid))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .current_dir(&layout.root);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(Launched::Child(cmd.spawn().context("起不来 holder")?))
}

#[cfg(windows)]
fn launch(layout: &Layout, sid: &str) -> Result<Launched> {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const ERROR_ACCESS_DENIED: i32 = 5;
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.args(hold_args(layout, sid))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .current_dir(&layout.root)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    match cmd.spawn() {
        Ok(c) => Ok(Launched::Child(c)),
        Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
            crate::log::write("Job 不允许脱离，改走 WMI 拉起 holder");
            wmi_launch(&exe, layout, sid).map(Launched::Pid)
        }
        Err(e) => Err(e).context("起不来 holder"),
    }
}

/// 经 WMI 拉起 holder，返回它的 pid
#[cfg(windows)]
fn wmi_launch(exe: &std::path::Path, layout: &Layout, sid: &str) -> Result<u32> {
    use base64::Engine as _;
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    // CreateProcess 的命令行：路径里不会有双引号（Windows 不允许），整体加双引号即可
    let cmdline = format!("\"{}\" hold --root \"{}\" --id {sid}", exe.display(), layout.root.display());
    let script = format!(
        "$ProgressPreference = 'SilentlyContinue'; \
         $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{ CommandLine = {}; CurrentDirectory = {} }}; \
         if ($r.ReturnValue -ne 0) {{ Write-Error ('Win32_Process.Create failed: ' + $r.ReturnValue); exit 1 }}; \
         $r.ProcessId",
        q(&cmdline),
        q(&layout.root.to_string_lossy()),
    );
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
        .output()
        .context("起不来 powershell")?;
    if !out.status.success() {
        bail!("WMI 拉起 holder 失败：{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().context("WMI 没有返回 pid")
}
