//! Windows 上起子进程时抑制控制台窗口。
//!
//! 服务端与客户端的 Windows release 都是 GUI 子系统（各自 main.rs 顶上的 `windows_subsystem`）：
//! 自己没有控制台，**起一个控制台程序（schtasks / powershell / taskkill / curl /
//! falcon-sessiond …）时系统会给它新开一个可见窗口**——这是「后台服务弹终端」的另一半来源
//! （前一半是计划任务给服务端本体分配控制台，靠 GUI 子系统解决）。以前服务端是控制台程序，
//! 子进程继承那个（已隐藏的）控制台，只有少数几处显式设了标志；改 GUI 子系统后每一处 spawn
//! 都得设，统一走这里，别再各自写 creation_flags。
//!
//! 只给「没有其它 creation_flags」的 spawn 用：`creation_flags` 是整体覆盖，叠在
//! DETACHED_PROCESS 上会把脱离标志冲掉。要脱离父进程控制台 / 会话的走各处的 `detach()`——
//! DETACHED_PROCESS 本身就不带控制台，CREATE_NO_WINDOW 与它互斥、会被系统忽略。

/// `cmd.no_window()`：非 Windows 上是空操作，调用点不用写 cfg
pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            self.creation_flags(CREATE_NO_WINDOW);
        }
        self
    }
}

impl NoWindow for tokio::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        self.creation_flags(CREATE_NO_WINDOW);
        self
    }
}

/// CREATE_NO_WINDOW：给子进程一个不可见的控制台（而不是让它继承父进程的、或新开一个可见的）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;