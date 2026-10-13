//! 进程小工具：探活、按名字核对、结束进程。各平台各一份。

/// 进程名（不带路径与 `.exe`）。取不到为 None
pub fn name_of(pid: u32) -> Option<String> {
    imp::name_of(pid)
}

/// pid 还在，而且是 falcon-sessiond（防 pid 被复用：重启之后 meta 里的 pid 可能是别人的）
pub fn is_sessiond(pid: u32) -> bool {
    pid != 0 && imp::alive(pid) && name_of(pid).is_none_or(|n| n.starts_with("falcon-sessio"))
}

pub fn alive(pid: u32) -> bool {
    pid != 0 && imp::alive(pid)
}

/// 结束一个会话的进程：Unix 先给 shell 的进程组发 SIGHUP（shell 来得及收尾、存历史），
/// `hard` 时补 SIGKILL；Windows 直接结束进程树
pub fn kill_session(shell_pid: u32, hard: bool) {
    imp::kill_session(shell_pid, hard)
}

pub fn kill(pid: u32) {
    imp::kill(pid)
}

#[cfg(unix)]
mod imp {
    pub fn alive(pid: u32) -> bool {
        let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(target_os = "macos")]
    pub fn name_of(pid: u32) -> Option<String> {
        let mut buf = [0u8; 256];
        let n = unsafe { libc::proc_name(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn name_of(pid: u32) -> Option<String> {
        // comm 截到 15 个字符：falcon-sessiond-0.3.0 只剩 "falcon-sessiond"，前缀比对够用
        std::fs::read_to_string(format!("/proc/{pid}/comm")).ok().map(|s| s.trim_end().to_string()).filter(|s| !s.is_empty())
    }

    pub fn kill_session(shell_pid: u32, hard: bool) {
        let sig = if hard { libc::SIGKILL } else { libc::SIGHUP };
        unsafe {
            // shell 是 PTY 会话的 leader，进程组号就是它的 pid
            libc::kill(-(shell_pid as libc::pid_t), sig);
            libc::kill(shell_pid as libc::pid_t, sig);
        }
    }

    pub fn kill(pid: u32) {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, TerminateProcess,
    };

    pub fn alive(pid: u32) -> bool {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code) != 0;
            CloseHandle(h);
            ok && code == STILL_ACTIVE as u32
        }
    }

    /// 全部进程的 (pid, 父 pid, 映像名)
    pub fn snapshot() -> Vec<(u32, u32, String)> {
        let mut out = Vec::new();
        unsafe {
            let snap: HANDLE = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return out;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            if Process32FirstW(snap, &mut e) != 0 {
                loop {
                    let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                    let name = String::from_utf16_lossy(&e.szExeFile[..len]);
                    out.push((e.th32ProcessID, e.th32ParentProcessID, name));
                    if Process32NextW(snap, &mut e) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        out
    }

    pub fn strip_exe(name: &str) -> String {
        let lower = name.to_ascii_lowercase();
        match lower.strip_suffix(".exe") {
            Some(_) => name[..name.len() - 4].to_string(),
            None => name.to_string(),
        }
    }

    pub fn name_of(pid: u32) -> Option<String> {
        snapshot().into_iter().find(|(p, _, _)| *p == pid).map(|(_, _, n)| strip_exe(&n))
    }

    /// 一个进程及其全部子孙，子孙在前（先杀叶子，免得父进程在中途又拉起新的）
    pub fn tree(root: u32) -> Vec<u32> {
        let all = snapshot();
        let mut order = vec![root];
        let mut i = 0;
        while i < order.len() {
            let p = order[i];
            for (pid, ppid, _) in &all {
                if *ppid == p && *pid != p && !order.contains(pid) {
                    order.push(*pid);
                }
            }
            i += 1;
        }
        order.reverse();
        order
    }

    pub fn kill(pid: u32) {
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }

    pub fn kill_session(shell_pid: u32, _hard: bool) {
        for pid in tree(shell_pid) {
            kill(pid);
        }
    }
}

#[cfg(windows)]
pub use imp::{snapshot, strip_exe};
