//! 前台命令（设计文档第 9 节）。服务端拿它出会话的自动标题、关窗前问「还有程序在跑吗」。
//!
//! - Unix：PTY 的前台进程组（`tcgetpgrp`），取组长的名字（macOS `proc_name`，Linux `/proc/<pid>/comm`）；
//! - Windows：没有前台进程组。沿 shell 的子孙往下找最深的那个（同深取 pid 大的），跳过 ConPTY
//!   自己的 conhost / OpenConsole。是启发式的，但比 Zellij 时代恒为空要好。

use portable_pty::MasterPty;

/// 前台进程组 leader 的完整信息（名字、pid、argv、工作目录）
#[derive(Debug, Clone, Default)]
pub struct FgInfo {
    pub name: Option<String>,
    pub pid: Option<u32>,
    pub argv: Option<Vec<String>>,
    pub cwd: Option<String>,
}

/// 前台命令名；在 shell 自己手里（或问不到）时返回 shell 的名字，由服务端判断是不是 shell
#[allow(dead_code)]
pub fn foreground(master: &dyn MasterPty, shell_pid: u32) -> Option<String> {
    imp::foreground_info(master, shell_pid).name
}

pub fn foreground_info(master: &dyn MasterPty, shell_pid: u32) -> FgInfo {
    imp::foreground_info(master, shell_pid)
}

/// Read only this process's argv; does not inspect its environment or descendants.
#[cfg(unix)]
pub fn process_argv(pid: u32) -> Option<Vec<String>> {
    imp::argv_of(pid)
}

/// 前台回到了 shell 自己：进程组就是 shell 的、而且 shell 没有子进程。holder 拿它清程序状态、
/// 判断终端标题是不是 shell 自己写的。
///
/// 只看进程组不够：agent 会话的启动脚本是 `sh -c`（没有作业控制），CLI 和脚本在同一个
/// 进程组里，前台进程组一直是 shell 的 pid——再看一眼有没有子进程才分得清
pub fn at_shell(master: &dyn MasterPty, shell_pid: u32) -> bool {
    imp::at_shell(master, shell_pid)
}

#[cfg(unix)]
mod imp {
    use portable_pty::MasterPty;

    use super::FgInfo;

    pub fn foreground_info(master: &dyn MasterPty, shell_pid: u32) -> FgInfo {
        let pgid = master.process_group_leader().map(|p| p as u32).filter(|&p| p != 0).unwrap_or(shell_pid);
        FgInfo {
            name: crate::proc::name_of(pgid),
            pid: (pgid != 0).then_some(pgid),
            argv: argv_of(pgid),
            cwd: cwd_of(pgid),
        }
    }

    #[cfg(target_os = "macos")]
    fn cwd_of(pid: u32) -> Option<String> {
        unsafe {
            let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
            let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
            let n = libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&mut info as *mut libc::proc_vnodepathinfo).cast(),
                size,
            );
            if n != size {
                return None;
            }
            let ptr = info.pvi_cdir.vip_path.as_ptr().cast::<libc::c_char>();
            if ptr.is_null() || *ptr == 0 {
                return None;
            }
            Some(std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned())
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn argv_of(pid: u32) -> Option<Vec<String>> {
        unsafe {
            let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
            let mut size: libc::size_t = 0;
            if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 || size < 4 {
                return None;
            }
            let mut buf = vec![0u8; size];
            if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 || size < 4 {
                return None;
            }
            buf.truncate(size);
            let argc = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]).max(0) as usize;
            if argc == 0 {
                return None;
            }
            let rest = &buf[4..];
            // 跳过 exec_path 及随后的对齐 NUL
            let end_exec = rest.iter().position(|&b| b == 0)?;
            let start_args = rest[end_exec..].iter().position(|&b| b != 0).map(|i| end_exec + i)?;
            let mut out = Vec::with_capacity(argc.min(64));
            for part in rest[start_args..].split(|&b| b == 0) {
                if out.len() >= argc {
                    break;
                }
                out.push(String::from_utf8_lossy(part).into_owned());
            }
            (!out.is_empty()).then_some(out)
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn cwd_of(pid: u32) -> Option<String> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) fn argv_of(pid: u32) -> Option<Vec<String>> {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let mut parts: Vec<String> = bytes.split(|&b| b == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        while parts.last().is_some_and(|s| s.is_empty()) {
            parts.pop();
        }
        (!parts.is_empty()).then_some(parts)
    }

    pub fn at_shell(master: &dyn MasterPty, shell_pid: u32) -> bool {
        master.process_group_leader().map(|p| p as u32) == Some(shell_pid) && !has_children(shell_pid)
    }

    #[cfg(target_os = "macos")]
    fn has_children(pid: u32) -> bool {
        let mut buf = [0 as libc::pid_t; 16];
        let n = unsafe {
            libc::proc_listchildpids(pid as libc::pid_t, buf.as_mut_ptr().cast(), std::mem::size_of_val(&buf) as libc::c_int)
        };
        n > 0
    }

    #[cfg(not(target_os = "macos"))]
    fn has_children(pid: u32) -> bool {
        // 内核开了 CONFIG_PROC_CHILDREN（主流发行版都开）就有现成的：程序写标题时 holder 要现查一次
        // （Claude Code 干活时标题每秒换一下），扫 /proc 太重。只列这个线程的子进程，shell 是单线程的
        if let Ok(list) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            return !list.trim().is_empty();
        }
        // 没开就扫一遍 stat
        let Ok(dir) = std::fs::read_dir("/proc") else { return false };
        for e in dir.flatten() {
            let name = e.file_name();
            let Some(p) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
            if p == pid {
                continue;
            }
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat")) else { continue };
            // comm 可能带空格和括号：从最后一个 ')' 之后数字段
            let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else { continue };
            if rest.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) == Some(pid) {
                return true;
            }
        }
        false
    }
}

#[cfg(windows)]
mod imp {
    use portable_pty::MasterPty;

    use super::FgInfo;
    use crate::proc::{snapshot, strip_exe};

    pub fn foreground_info(_master: &dyn MasterPty, shell_pid: u32) -> FgInfo {
        let all = snapshot();
        let skip = |n: &str| {
            let n = n.to_ascii_lowercase();
            n == "conhost.exe" || n == "openconsole.exe"
        };
        let mut best: Option<(usize, u32, String)> = None;
        let mut frontier = vec![(shell_pid, 0usize)];
        let mut seen = vec![shell_pid];
        while let Some((pid, depth)) = frontier.pop() {
            if let Some((_, _, name)) = all.iter().find(|(p, _, _)| *p == pid) {
                if !skip(name) && best.as_ref().is_none_or(|(d, p, _)| (depth, pid) > (*d, *p)) {
                    best = Some((depth, pid, name.clone()));
                }
            }
            for (child, parent, _) in &all {
                if *parent == pid && !seen.contains(child) {
                    seen.push(*child);
                    frontier.push((*child, depth + 1));
                }
            }
        }
        FgInfo {
            name: best.as_ref().map(|(_, _, n)| strip_exe(n)),
            pid: best.as_ref().map(|(_, p, _)| *p),
            argv: None,
            cwd: None,
        }
    }

    pub fn at_shell(_master: &dyn MasterPty, _shell_pid: u32) -> bool {
        // ConPTY 吞掉 OSC 7501，Windows 上没有程序状态要清
        false
    }
}
