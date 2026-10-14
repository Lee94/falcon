//! 前台命令（设计文档第 9 节）。服务端拿它出会话的自动标题、关窗前问「还有程序在跑吗」。
//!
//! - Unix：PTY 的前台进程组（`tcgetpgrp`），取组长的名字（macOS `proc_name`，Linux `/proc/<pid>/comm`）；
//! - Windows：没有前台进程组。沿 shell 的子孙往下找最深的那个（同深取 pid 大的），跳过 ConPTY
//!   自己的 conhost / OpenConsole。是启发式的，但比 Zellij 时代恒为空要好。

use portable_pty::MasterPty;

/// 前台命令名；在 shell 自己手里（或问不到）时返回 shell 的名字，由服务端判断是不是 shell
pub fn foreground(master: &dyn MasterPty, shell_pid: u32) -> Option<String> {
    imp::foreground(master, shell_pid)
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

    pub fn foreground(master: &dyn MasterPty, shell_pid: u32) -> Option<String> {
        let pgid = master.process_group_leader().map(|p| p as u32).unwrap_or(shell_pid);
        crate::proc::name_of(pgid)
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

    use crate::proc::{snapshot, strip_exe};

    pub fn foreground(_master: &dyn MasterPty, shell_pid: u32) -> Option<String> {
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
        best.map(|(_, _, n)| strip_exe(&n))
    }

    pub fn at_shell(_master: &dyn MasterPty, _shell_pid: u32) -> bool {
        // ConPTY 吞掉 OSC 7501，Windows 上没有程序状态要清
        false
    }
}
