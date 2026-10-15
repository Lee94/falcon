//! 往对话终端里跑一条命令：内置 agent 的 `run_command`（docs/design/ai-assistant.md 4.4）。
//!
//! 命令打进用户也在用的那个交互 shell，用户在面板的实时区域里看到的就是 agent 做的。步骤：
//! 1. 取或建这台机器的对话终端，确保接上；
//! 2. 等 shell 停在提示符上：标记说「在提示符上」就走；还没见过标记（刚接回）就问守护进程前台
//!    是不是 shell；前台被别的命令 / 全屏程序占着就等（告诉卡片一声），等太久报错让模型换办法；
//! 3. 上膛（shell_marks.rs 的 `arm`），先清掉输入行上可能有的半截输入（Ctrl-E Ctrl-U），再写
//!    命令 + 回车；多行命令包在括号粘贴里，免得每行各回车一次、各算一条命令；
//! 4. `C` 之后的输出实时交给卡片，`D` 带回退出码；超时不杀命令（它跑在用户的终端里，用户看得见、
//!    停得了），把已有输出交回去；停止 = 往 shell 送一个 Ctrl-C。
//!
//! shell 没有集成（sh / dash / tcsh、Windows 宿主机、集成脚本没写成）时收不到标记，命令改成
//! 独立进程执行：本机 `sh -c`，SSH 走 exec 通道。同一台机器上一次只跑一条（多段对话共用一个 shell）。

use std::time::Duration;

use tokio::sync::oneshot;
use std::time::Instant;

use super::*;

/// 等 shell 空出来最多等这么久
const WAIT_SHELL: Duration = Duration::from_secs(60);

/// 正在替 agent 收输出的那条命令：实时输出交给卡片，收完交回结果
pub(super) struct ShellWaiter {
    pub on_chunk: Rc<dyn Fn(&str)>,
    pub done: oneshot::Sender<(String, Option<i32>)>,
}

/// 一条命令的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRun {
    /// 原始输出（带 ANSI）
    pub output: String,
    pub exit_code: Option<i32>,
    /// 到点还没跑完（命令还在用户的终端里跑着）
    pub timed_out: bool,
    /// false = shell 没有集成，命令是在后台单独起的进程里跑的
    pub in_shell: bool,
}

/// 写进 shell 的字节：先清输入行（Ctrl-E 到行尾、Ctrl-U 删到行首），再写命令、回车。
/// 多行命令用括号粘贴包起来：zsh / bash ≥ 5.1 / fish 会把整段当成一次输入，只回车一次。
/// 更老的 bash 不认括号粘贴，多行命令在那里会坏——模型那边的提示词让它尽量写成一行
pub fn shell_input(command: &str) -> String {
    let cmd = command.trim_end_matches(['\n', '\r']);
    let body = if cmd.contains('\n') { format!("\x1b[200~{cmd}\x1b[201~") } else { cmd.to_string() };
    format!("\x05\x15{body}\r")
}

impl SessionManager {
    /// 在这个对话终端里跑一条命令。`on_wait` 在要等 shell 空出来时调一次，`on_chunk` 收实时
    /// 输出。被停止返回 `Err("已停止")`。终端丢了不在这里另开：那是对话的事（hub 先 ensure）
    pub async fn run_in_session(
        &self,
        session_id: &str,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
        on_wait: Rc<dyn Fn()>,
        on_chunk: Rc<dyn Fn(&str)>,
    ) -> Result<ShellRun, String> {
        let lock = self.machine_run_locks.borrow_mut().entry(session_id.to_string()).or_default().clone();
        let _turn = tokio::select! {
            turn = lock.lock() => turn,
            _ = cancel.cancelled() => return Err(CANCELLED.into()),
        };

        let row = self.ensure_attached(session_id, true).await.map_err(|e| format!("终端没接上：{e}"))?;
        if row.state == "dead" {
            return Err("这段对话的终端已经结束了".into());
        }
        let owner = row.project_id.clone();
        let entry = self.entry(session_id).ok_or("终端没接上")?;
        let integrated = entry.s.borrow().shell.as_ref().is_some_and(|t| t.integrated);
        if !integrated {
            let project = owner_row(&self.db, &owner).ok_or("这台机器已经不在了")?;
            let cwd = entry.s.borrow().shell.as_ref().and_then(|t| t.cwd().map(str::to_string));
            return self.run_detached(&project, cwd, command, timeout, cancel).await;
        }

        // ---- 等 shell 停在提示符上 ----
        let deadline = Instant::now() + WAIT_SHELL;
        let mut told = false;
        loop {
            if cancel.is_cancelled() {
                return Err(CANCELLED.into());
            }
            let state = entry.s.borrow().shell.as_ref().map(|t| t.state()).unwrap_or_default();
            match state {
                PromptState::AtPrompt => break,
                PromptState::Unknown if matches!(self.probe_foreground(session_id).await, Probe::Shell) => {
                    if let Some(t) = entry.s.borrow_mut().shell.as_mut() {
                        t.assume_prompt();
                    }
                    break;
                }
                _ => {}
            }
            if !told {
                told = true;
                on_wait();
            }
            if Instant::now() > deadline {
                return Err("Shell 一直被别的命令或全屏程序占着，没法执行。请在终端里处理完再试".into());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(300)) => {}
                _ = cancel.cancelled() => return Err(CANCELLED.into()),
            }
        }

        // ---- 上膛、写命令 ----
        let backend = entry.backend().ok_or("对话终端没接上")?;
        let (tx, rx) = oneshot::channel();
        {
            let mut s = entry.s.borrow_mut();
            if let Some(t) = s.shell.as_mut() {
                t.arm();
            }
            s.shell_waiter = Some(ShellWaiter { on_chunk, done: tx });
        }
        backend.write(&shell_input(command));

        let disarm = |entry: &LiveEntry| {
            let mut s = entry.s.borrow_mut();
            s.shell_waiter = None;
            s.shell.as_mut().and_then(|t| t.disarm()).unwrap_or_default()
        };
        tokio::select! {
            res = rx => match res {
                Ok((output, exit_code)) => Ok(ShellRun { output, exit_code, timed_out: false, in_shell: true }),
                // 收输出的人被丢了：会话结束 / 被终止
                Err(_) => Err("对话终端在命令跑完之前断开了".into()),
            },
            _ = tokio::time::sleep(timeout) => {
                let output = disarm(&entry);
                Ok(ShellRun { output, exit_code: None, timed_out: true, in_shell: true })
            }
            _ = cancel.cancelled() => {
                backend.write("\x03");
                disarm(&entry);
                Err(CANCELLED.into())
            }
        }
    }

    /// shell 没有集成：独立进程跑。本机 `sh -c`（登录环境、当前目录跟着 shell 报的走，没有就家目录），
    /// SSH 走 exec 通道（远端默认 shell）。拿不到实时输出，跑完一次给
    async fn run_detached(
        &self,
        project: &ProjectRow,
        cwd: Option<String>,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<ShellRun, String> {
        let wrapped = format!("exec 2>&1; {command}");
        if project.project_type == "local" {
            let env = self.local.base_env().await;
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.arg("-c").arg(&wrapped).env_clear().envs(env.iter().map(|(k, v)| (k, v))).kill_on_drop(true);
            cmd.current_dir(cwd.or_else(|| project.working_dir.clone()).unwrap_or_else(|| "/".into()));
            cmd.stdin(std::process::Stdio::null());
            let child = cmd.output();
            return tokio::select! {
                out = child => {
                    let out = out.map_err(|e| format!("命令起不来：{e}"))?;
                    Ok(ShellRun {
                        output: String::from_utf8_lossy(&out.stdout).into_owned(),
                        exit_code: out.status.code(),
                        timed_out: false,
                        in_shell: false,
                    })
                }
                // 独立进程没人看得见：到点就杀（kill_on_drop）
                _ = tokio::time::sleep(timeout) => Ok(ShellRun { output: String::new(), exit_code: None, timed_out: true, in_shell: false }),
                _ = cancel.cancelled() => Err(CANCELLED.into()),
            };
        }
        let link = self.get_link(project);
        let windows = link.host_facts().await.is_ok_and(|f| f.kind == crate::host::HostKind::Windows);
        let line = match &cwd {
            // Windows 宿主机：exec 通道上是它的默认 shell，原样交给它
            _ if windows => command.to_string(),
            Some(dir) => format!("cd {} && {wrapped}", crate::host::quote_posix(dir)),
            None => wrapped,
        };
        tokio::select! {
            res = link.exec(&line, Some(cancel)) => {
                let res = res.map_err(|e| format!("命令没跑起来：{e}"))?;
                Ok(ShellRun { output: res.stdout + &res.stderr, exit_code: res.code, timed_out: false, in_shell: false })
            }
            _ = tokio::time::sleep(timeout) => Ok(ShellRun { output: String::new(), exit_code: None, timed_out: true, in_shell: false }),
        }
    }
}

pub const CANCELLED: &str = "已停止";

#[cfg(test)]
mod tests {
    use super::shell_input;

    #[test]
    fn input_clears_the_line_then_runs_once() {
        assert_eq!(shell_input("ls -la"), "\x05\x15ls -la\r");
        assert_eq!(shell_input("ls\n"), "\x05\x15ls\r");
        assert_eq!(shell_input("a\nb"), "\x05\x15\x1b[200~a\nb\x1b[201~\r");
    }
}
