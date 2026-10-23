//! 服务端自重启：`POST /api/system/restart`（设置 → 关于的「重启服务端」按钮）背后的机制。
//!
//! 两种处境两条路：
//! - **受管**（launchd `KeepAlive` / systemd `Restart=always`）：进程退出就行，init 会拉起
//!   新实例，启动参数、日志归置照旧。判定见 [`supervised`]。
//! - **不受管**（`cargo run`、手动起的二进制）：没人替我们拉，退出前先派一个
//!   `falcon respawn` 帮手进程守着本进程 pid，等它死后以同一 argv 重开一个。
//!
//! 帮手是同一个二进制的隐藏子命令（main.rs 在 `service` 之前先认它）：不占 shell、不经
//! PATH、没有跨平台的命令行转义问题。被守护的实例绝不派帮手——否则 launchd 拉起一份、
//! 帮手又拉起一份，两个进程抢同一个端口。

use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use crate::service::SYSTEMD_NAME;
#[cfg(target_os = "macos")]
use crate::service::launchd_domain;

/// 等被替掉的进程退出的上限：关停路径自己留了 5s 收子进程，这里给得再宽些
const WAIT_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(150);

/// 本进程退出后会不会有 init 把它拉回来。
///
/// systemd：`SYSTEMD_EXEC_PID` 是 ExecStart 主进程的 pid（v243+），== 自己才说明我们
/// 就是那个服务本体——这变量会继承给子孙进程，只查存在会把 systemd 会话里手动起的
/// 实例误判成受管，退出就再也起不来了。更老的 systemd 没这个变量，退到
/// `/proc/self/cgroup` 里找 `falcon.service`。
/// launchd：不给进程打环境标记，问 `launchctl print` 报出来的 pid 是不是自己——
/// ppid==1 也可能是父进程死后被过继的孤儿，同样不可信。
pub fn supervised() -> bool {
    systemd_supervised() || launchd_supervised()
}

#[cfg(target_os = "linux")]
fn systemd_supervised() -> bool {
    if std::env::var("SYSTEMD_EXEC_PID").ok().and_then(|v| v.parse::<u32>().ok()) == Some(std::process::id()) {
        return true;
    }
    let unit = format!("{SYSTEMD_NAME}.service");
    std::fs::read_to_string("/proc/self/cgroup")
        .is_ok_and(|cg| cg.lines().any(|l| l.rsplit('/').next() == Some(unit.as_str())))
}

#[cfg(not(target_os = "linux"))]
fn systemd_supervised() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn launchd_supervised() -> bool {
    use crate::service::launchd_target;
    let uid = unsafe { libc::getuid() };
    let target = launchd_target(&launchd_domain(uid));
    let Ok(out) = Command::new("launchctl").args(["print", &target]).output() else {
        return false;
    };
    launchd_print_has_pid(&String::from_utf8_lossy(&out.stdout), std::process::id())
}

#[cfg(not(target_os = "macos"))]
fn launchd_supervised() -> bool {
    false
}

/// `launchctl print` 的输出里有没有一行 `pid = <pid>`。子段落里的键会重复，
/// 但 job 本体报的 pid 恒在最外层（与 service.rs 状态摘取同一口径，只认键的首次出现）
#[cfg(any(target_os = "macos", test))]
fn launchd_print_has_pid(print_output: &str, pid: u32) -> bool {
    print_output.lines().map(str::trim).any(|l| {
        l.strip_prefix("pid")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|v| v.trim().parse::<u32>() == Ok(pid))
    })
}

/// 派一个 `respawn` 帮手：等本进程死后以同一 argv 重开服务端。只在 [`supervised`] 为假时调。
///
/// 帮手重开时按 `current_exe` + 原 argv 起新进程；启动参数（--host/--port/--data-dir）都
/// 在 argv 里，env 里的 FALCON_* 走默认环境继承，与首次启动同一口径。
pub fn spawn_respawn_helper() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(&exe);
    cmd.arg("respawn")
        .arg(std::process::id().to_string())
        .arg("--")
        .arg(&exe)
        .args(std::env::args().skip(1))
        .stdin(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn().map(|_| ())
}

/// 脱离本进程所在的会话 / 控制台：父进程死后帮手还要活着，重开出来的服务端也不能跟着
/// 终端一起被 SIGHUP 带走（Windows 上没有会话概念，spawn 出来天然独立）
#[cfg(unix)]
fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // DETACHED_PROCESS：不继承父控制台，控制台关了也牵连不到它
    cmd.creation_flags(0x00000008);
}

#[cfg(not(any(unix, windows)))]
fn detach(_: &mut Command) {}

/// `falcon respawn <pid> -- <argv…>`：等 `pid` 退出后以 `argv` 重开一个进程。
/// main 直接 `std::process::exit` 返回值。
pub fn run_respawn_cli(argv: &[String]) -> i32 {
    match respawn(argv) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("respawn: {e:#}");
            1
        }
    }
}

fn respawn(argv: &[String]) -> anyhow::Result<()> {
    const USAGE: &str = "用法: falcon respawn <pid> -- <argv…>";
    let pid: u32 = argv.first().map(String::as_str).and_then(|s| s.parse().ok()).ok_or_else(|| anyhow::anyhow!(USAGE))?;
    anyhow::ensure!(argv.get(1).map(String::as_str) == Some("--"), "{USAGE}");
    let target = argv.get(2..).unwrap_or(&[]);
    anyhow::ensure!(!target.is_empty(), "{USAGE}");
    let deadline = Instant::now() + WAIT_DEADLINE;
    while pid_alive(pid) {
        anyhow::ensure!(Instant::now() < deadline, "等 {pid} 退出超时");
        std::thread::sleep(POLL_INTERVAL);
    }
    let mut cmd = Command::new(&target[0]);
    cmd.args(&target[1..]).stdin(std::process::Stdio::null());
    detach(&mut cmd);
    cmd.spawn()?;
    Ok(())
}

/// pid 还在不在（僵尸也算死：服务端退出后到被 reaper 收走之间有窗口，不等它）
fn pid_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, RefreshKind, System};
    let pid = Pid::from_u32(pid);
    let mut sys = System::new_with_specifics(RefreshKind::new().with_processes(ProcessRefreshKind::new()));
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]));
    sys.process(pid).is_some_and(|p| p.status() != ProcessStatus::Zombie)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_print_pid_match() {
        let out = "gui/501/com.falcon.server = {\n\tstate = running\n\tpid = 123\n\tlast exit code = 0\n}\n";
        assert!(launchd_print_has_pid(out, 123));
        assert!(!launchd_print_has_pid(out, 124));
        assert!(!launchd_print_has_pid("", 123));
        // 子段落里的 pid 也会被认——无碍：判的是"我们自己是不是那个 job"，整段输出里
        // 出现我们的 pid 只可能因为它就是 job 本体
    }

    #[cfg(unix)]
    #[test]
    fn respawn_waits_and_spawns() {
        // 先造一个已死的 pid：spawn 一个立刻退出的子进程并 wait 掉（等它 reaped 再轮到
        // respawn 确认 pid 已消失），argv 用 `true`——存在即成功，不留新进程
        let mut dead = Command::new("true").spawn().unwrap();
        dead.wait().unwrap();
        let pid = dead.id();
        // pid 刚被收走理论上可能立刻复用，但测试进程里没有什么会立刻 spawn 新进程
        assert_eq!(run_respawn_cli(&[pid.to_string(), "--".into(), "true".into()]), 0);
    }

    #[test]
    fn respawn_cli_usage() {
        assert_ne!(run_respawn_cli(&[]), 0);
        assert_ne!(run_respawn_cli(&["x".into(), "--".into(), "true".into()]), 0);
        assert_ne!(run_respawn_cli(&["1".into(), "--".into()]), 0);
    }
}
