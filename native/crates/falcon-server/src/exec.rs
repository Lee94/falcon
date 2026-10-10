//! 在宿主机上执行一条命令行。`ExecResult` 与执行器契约移植自 `packages/server/src/zellij/install.ts`
//! 的 `ExecResult` / `ExecFn`，本地执行器移植自 `zellij/exec.ts` 的 `localExec`。
//!
//! **铁律：非零退出码是正常返回值，绝不当错误**。判错一律显式查 `code`；只有执行本身
//! 起不来（SSH 通道开不了、链路断了）才是 `Err`——那是链路故障。本地执行器连 spawn 失败
//! 也收成 `code: None`（Node 版就是这样），所以它永远不返回 `Err`。
//!
//! 执行器是 `!Send` 的：会话核心跑在单线程的 LocalSet 上（与 Node 的事件循环同一个语义，
//! 见 core.rs），SSH 执行器攥着 `Rc` 的链路状态。

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// 一次执行的结果。stdout / stderr 是整段收完后一次按 UTF-8 解码的（逐块解码会切坏多字节字符）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ExecResult {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

pub type LocalBoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// 在某台宿主机上执行一条命令行。本地为经 shell 的 spawn，远端为 SSH exec。
/// `cancel` 触发时尽快结束并返回（本地杀进程，远端关通道）
pub trait Exec {
    fn exec<'a>(
        &'a self,
        command_line: &'a str,
        cancel: Option<&'a CancellationToken>,
    ) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>>;
}

/// 后端本机的执行器
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalExec;

impl Exec for LocalExec {
    fn exec<'a>(
        &'a self,
        command_line: &'a str,
        cancel: Option<&'a CancellationToken>,
    ) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
        Box::pin(async move { Ok(local_exec(command_line, cancel).await) })
    }
}

/// 交给本地 shell 执行的命令（Node `spawn(cmd, { shell: true })` 的等价物）。
///
/// Unix 是 `/bin/sh -c`；Windows 上 Node 实际执行的是 `cmd.exe /d /s /c "<cmd>"`，这里用
/// `raw_arg` 原样复刻（Rust 默认的参数转义会把引号再包一层）。我们发给 Windows 的都是
/// `powershell ... -EncodedCommand <base64>`，base64 里没有 cmd 会改写的字符。
pub fn shell_command(command_line: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut cmd = tokio::process::Command::new(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()));
        cmd.raw_arg(format!("/d /s /c \"{command_line}\""));
        // windowsHide：别闪出一个黑色控制台窗口
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c").arg(command_line);
        cmd
    }
}

/// 经本地 shell 执行。与 Node 版的一处刻意差别：stdin 给 `/dev/null` 而不是一根永不关闭的
/// 管道——Node 版因此专门为 `zellij pipe` 绕开了 localExec（它要读到 EOF 才退出），
/// 这里直接给 EOF，等于把那个坑填平了；没有命令依赖"stdin 一直开着"。
pub async fn local_exec(command_line: &str, cancel: Option<&CancellationToken>) -> ExecResult {
    let mut cmd = shell_command(command_line);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ExecResult { code: None, stdout: String::new(), stderr: e.to_string() },
    };
    let mut out = child.stdout.take().expect("stdout 是 piped");
    let mut err = child.stderr.take().expect("stderr 是 piped");
    // 攒字节、结束时一次解码：逐块转字符串会切坏跨包的 UTF-8 多字节字符
    let collect = async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let (a, b) = tokio::join!(out.read_to_end(&mut stdout), err.read_to_end(&mut stderr));
        let _ = (a, b);
        let status = child.wait().await;
        (status, stdout, stderr)
    };
    let (status, stdout, stderr) = match cancel {
        Some(token) => tokio::select! {
            r = collect => r,
            _ = token.cancelled() => {
                // kill_on_drop 收尸；被取消的命令没有有意义的输出
                return ExecResult { code: None, stdout: String::new(), stderr: "已取消".into() };
            }
        },
        None => collect.await,
    };
    ExecResult {
        code: status.ok().and_then(|s| s.code()),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

/// 流式 UTF-8 解码（Node `StringDecoder` 的等价物）：TCP 分包 / PTY 读块会把多字节字符
/// 切开，直接按块解码会产生 U+FFFD——终端里大量中文输出时表现为偶发乱码。把跨块的
/// 半个字符留到下一块；真正非法的字节照 StringDecoder 换成 U+FFFD。
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    pub fn write(&mut self, chunk: &[u8]) -> String {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(chunk);
        let mut out = String::with_capacity(buf.len());
        let mut rest: &[u8] = &buf;
        loop {
            match std::str::from_utf8(rest) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).expect("前缀合法"));
                    match e.error_len() {
                        // 末尾是半个字符：留到下一块
                        None => {
                            self.pending = after.to_vec();
                            break;
                        }
                        Some(n) => {
                            out.push('\u{FFFD}');
                            rest = &after[n..];
                        }
                    }
                }
            }
        }
        out
    }

    /// 流结束：残留的半个字符按非法处理
    pub fn end(&mut self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            "\u{FFFD}".into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nonzero_exit_is_a_result_not_an_error() {
        let r = local_exec("printf 'out中文'; printf err >&2; exit 3", None).await;
        assert_eq!(r.code, Some(3));
        assert_eq!(r.stdout, "out中文");
        assert_eq!(r.stderr, "err");
        let ok = local_exec("true", None).await;
        assert!(ok.ok());
    }

    #[tokio::test]
    async fn stdin_is_eof() {
        // 读 stdin 的命令立刻拿到 EOF，不会挂住
        let r = local_exec("cat; echo done", None).await;
        assert_eq!(r.stdout, "done\n");
    }

    #[tokio::test]
    async fn cancel_kills_the_command() {
        let token = CancellationToken::new();
        let t2 = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            t2.cancel();
        });
        let started = std::time::Instant::now();
        let r = local_exec("sleep 5", Some(&token)).await;
        assert_eq!(r.code, None);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn decoder_keeps_split_characters() {
        let bytes = "a中b🦅c".as_bytes();
        let mut d = Utf8Decoder::default();
        let mut out = String::new();
        for b in bytes {
            out.push_str(&d.write(std::slice::from_ref(b)));
        }
        out.push_str(&d.end());
        assert_eq!(out, "a中b🦅c");
    }

    #[test]
    fn decoder_replaces_invalid_bytes() {
        let mut d = Utf8Decoder::default();
        assert_eq!(d.write(b"a\xffb"), "a\u{FFFD}b");
        assert_eq!(d.write(&[0xe4, 0xb8]), "");
        assert_eq!(d.end(), "\u{FFFD}");
        assert_eq!(d.write(b"x"), "x");
    }
}
