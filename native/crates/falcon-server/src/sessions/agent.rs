//! Agent 会话：把会话的"开场命令"从 shell 换成 claude / codex / grok 这类 CLI（ADR 0013）。
//! 移植自 `packages/server/src/sessions/agent.ts`。
//!
//! 实现上给 Zellij / PTY 的仍然是一个 shell——只不过这个 shell 是我们写在宿主机上的
//! 一段启动脚本。不能把 CLI 本身当 shell 传下去：
//! - `--default-shell` 是 PathBuf，不接受参数（见 zellij/command.rs 的 session_options），
//!   "先跑 CLI 再落回 shell"这件事只能由脚本自己完成；
//! - CLI 一退出就没人占着 pane 了，Zellij 会把 pane 连同会话一起收掉——用户按一次
//!   Ctrl-D 整个会话就没了。脚本最后 exec 真实登录 shell 正是为此。
//!
//! 脚本里的登录 shell 是**生成时写死的绝对路径**，不读 $SHELL：Windows 远端那条路径上
//! 我们自己把 SHELL 设成了传给 Zellij 的 shell（见 ssh.ts 的 WMI 兜底），脚本再去读
//! $SHELL 就会递归调用自己。
//!
//! CLI 不在 PATH 上不算错误：提示一句然后照常落回 shell——把会话开起来比报错有用，
//! 用户可以就地 `npm i -g` 装完再开一个。
//!
//! 留到 S3 的函数：`writeLocalLauncher`（写本机文件系统、chmod）。

use std::path::{Path, PathBuf};

pub use falcon_proto::{SESSION_AGENTS, SessionAgent};

use crate::git::path::join_path;
use crate::zellij::host::{HostKind, encode_powershell, quote_posix, quote_powershell};

/// CLI 的可执行名。三个都是 npm 全局包，装完就在 PATH 上。
///
/// `Unknown` 是 falcon-proto 给反序列化留的兜底，服务端收请求时已用 `SessionAgent::from_wire`
/// 挡掉（`api/sessions.rs`），到不了这里；真到了也只是生成一个"找不到 unknown、落回 shell"的脚本，无害。
pub fn agent_bin(agent: SessionAgent) -> &'static str {
    match agent {
        SessionAgent::Claude => "claude",
        SessionAgent::Codex => "codex",
        SessionAgent::Grok => "grok",
        SessionAgent::Unknown => "unknown",
    }
}

/// 启动脚本文件名。同一个 agent 在同一台宿主机上只有一份，内容幂等覆盖。
pub fn launcher_name(agent: SessionAgent, kind: HostKind) -> String {
    let ext = if kind == HostKind::Windows { ".cmd" } else { ".sh" };
    format!("falcon-{}{ext}", agent.as_str())
}

/// 宿主机上放启动脚本的目录：<falcon 根>/agents
pub fn launcher_dir(kind: HostKind, root: &str) -> String {
    join_path(kind, &[root, "agents"])
}

/// 本地宿主：脚本落在后端的 --data-dir 下，与 askpass 包装同级。
///
/// TS 用的是 `path.join`（后端本机的路径规则），这里对应 `std::path`。`path.join` 还会
/// 顺手规范化 `.` / `..` 与重复分隔符，`Path::join` 不会——数据目录是启动时解析好的
/// 绝对路径，碰不到这种差别。
pub fn local_launcher_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agents")
}

/// 本地宿主：写脚本，返回绝对路径。每次附着都重写（理由见 manager 的 session_shell）
pub fn write_local_launcher(data_dir: &Path, agent: SessionAgent, shell: &str) -> std::io::Result<PathBuf> {
    let dir = local_launcher_dir(data_dir);
    std::fs::create_dir_all(&dir)?;
    let kind = if cfg!(windows) { HostKind::Windows } else { HostKind::Posix };
    let file = dir.join(launcher_name(agent, kind));
    std::fs::write(&file, launcher_body(agent, kind, shell))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(file)
}

/// POSIX 启动脚本。真正干活的是 `<shell> -i -l -c`（三个 flag 分开写，
/// 短参合并各 shell 不一，见 login_env.rs）：
///
/// - `-l` 登录：读 ~/.profile / ~/.zprofile、/etc/zprofile（path_helper）。
/// - `-i` 交互：不少人把 PATH / nvm / `~/.local/bin` 写在 ~/.zshrc 且用
///   `[[ $- == *i* ]]` 守卫。纯 `zsh -l -c` 是 login 非交互，不读 .zshrc——
///   远端 Linux 上 claude 装在 ~/.local/bin、PATH 只在 .zshrc 里，表现为
///   「新建 Claude 会话变成一句找不到、再落回普通 shell」。
///
/// CLI 退出后 exec 的那个也带 `-i -l`，不靠「有 TTY 就算交互」这层默认。
fn posix_launcher_body(agent: SessionAgent, shell: &str) -> String {
    let bin = agent_bin(agent);
    let inner = format!(
        "if command -v {bin} >/dev/null 2>&1; then {bin}; \
         else printf '%s\\n' {} >&2; fi; \
         exec {} -i -l",
        quote_posix(&format!("falcon: PATH 里找不到 {bin}，先装好它再开这种会话。")),
        quote_posix(shell),
    );
    [
        "#!/bin/sh".to_string(),
        format!("# falcon: 以 {bin} 开场的会话。CLI 退出后落回登录 shell，会话不跟着结束。"),
        format!("exec {} -i -l -c {}", quote_posix(shell), quote_posix(&inner)),
        String::new(),
    ]
    .join("\n")
}

/// Windows 启动脚本（.cmd）。提示语只用 ASCII：cmd 按 OEM 代码页读脚本文件，
/// 中文在默认 936 / 437 下必乱码。
fn windows_launcher_body(agent: SessionAgent, shell: &str) -> String {
    let bin = agent_bin(agent);
    [
        "@echo off".to_string(),
        format!("rem falcon: session that opens with {bin}; falls back to the shell on exit"),
        format!("where {bin} >nul 2>&1"),
        "if errorlevel 1 (".to_string(),
        format!("  echo falcon: {bin} was not found on PATH; install it first."),
        ") else (".to_string(),
        format!("  call {bin}"),
        ")".to_string(),
        format!("\"{shell}\""),
        String::new(),
    ]
    .join("\r\n")
}

pub fn launcher_body(agent: SessionAgent, kind: HostKind, shell: &str) -> String {
    if kind == HostKind::Windows { windows_launcher_body(agent, shell) } else { posix_launcher_body(agent, shell) }
}

/// 远端：建目录 + 写脚本的一条命令。退出码 = 链上第一个失败者。
/// Windows 走 -EncodedCommand，与其它远端命令同一套（CLAUDE.md 的约定）。
pub fn write_remote_launcher_command(kind: HostKind, dir: &str, agent: SessionAgent, shell: &str) -> String {
    let name = launcher_name(agent, kind);
    let body = launcher_body(agent, kind, shell);
    if kind == HostKind::Windows {
        let file = join_path(kind, &[dir, &name]);
        return encode_powershell(&format!(
            "New-Item -ItemType Directory -Force -Path {} | Out-Null; Set-Content -LiteralPath {} -Value {}",
            quote_powershell(dir),
            quote_powershell(&file),
            quote_powershell(&body),
        ));
    }
    format!(
        "d={}; mkdir -p \"$d\" && printf %s {} > \"$d\"/{name} && chmod 755 \"$d\"/{name}",
        quote_posix(dir),
        quote_posix(&body),
    )
}

/// 远端脚本的绝对路径（写入成功后交给 --default-shell）
pub fn remote_launcher_path(kind: HostKind, root: &str, agent: SessionAgent) -> String {
    join_path(kind, &[launcher_dir(kind, root), launcher_name(agent, kind)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    const AGENTS: [SessionAgent; 3] = [SessionAgent::Claude, SessionAgent::Codex, SessionAgent::Grok];

    // ---- launcherBody（POSIX） ----

    /// 经交互登录 shell 跑 CLI，退出后 exec 回登录 shell
    #[test]
    fn posix_body_runs_cli_in_interactive_login_shell_then_execs_back() {
        let body = launcher_body(SessionAgent::Claude, HostKind::Posix, "/bin/zsh");
        assert!(body.starts_with("#!/bin/sh\n"));
        assert!(body.contains("command -v claude"));
        // 两处 shell：-i -l -c 外壳，与 CLI 退出后接手的那个
        assert_eq!(body.matches("'/bin/zsh'").count(), 2);
        // 纯 -l -c 是非交互，zsh 不读 .zshrc，远端 ~/.local/bin 里的 claude 会找不到
        assert!(body.contains("-i -l -c "));
        assert_eq!(body.matches("-i -l").count(), 2);
    }

    /// 绝不读 $SHELL——Windows 那条路径上它就是这个脚本自己，会递归
    #[test]
    fn posix_body_never_reads_dollar_shell() {
        for agent in AGENTS {
            assert!(!launcher_body(agent, HostKind::Posix, "/bin/bash").contains("$SHELL"));
        }
    }

    /// shell 路径里的单引号照 POSIX 规则转义
    #[test]
    fn posix_body_escapes_single_quotes_in_shell_path() {
        let body = launcher_body(SessionAgent::Grok, HostKind::Posix, "/home/o'brien/bin/fish");
        assert!(body.contains(r"'/home/o'\''brien/bin/fish'"));
    }

    /// CLI 缺失只提示不失败：仍然落回 shell
    #[test]
    fn posix_body_missing_cli_only_warns_and_still_falls_back() {
        let body = launcher_body(SessionAgent::Codex, HostKind::Posix, "/bin/sh");
        assert!(body.contains("else printf"));
        assert!(body.contains("exec '/bin/sh' -i -l"));
    }

    // ---- launcherBody（Windows） ----

    fn windows_body() -> String {
        launcher_body(SessionAgent::Claude, HostKind::Windows, "C:\\Windows\\System32\\powershell.exe")
    }

    /// 是 CRLF 的 .cmd，末尾交给 shell 接手
    #[test]
    fn windows_body_is_crlf_cmd_handing_over_to_shell() {
        let body = windows_body();
        assert!(body.contains("\r\n"));
        assert!(body.starts_with("@echo off"));
        assert!(body.contains("\"C:\\Windows\\System32\\powershell.exe\""));
    }

    /// 提示语只用 ASCII：cmd 按 OEM 代码页读脚本，中文必乱码
    #[test]
    fn windows_body_is_ascii_only() {
        assert!(windows_body().is_ascii());
    }

    // ---- launcherName / remoteLauncherPath ----

    /// 按宿主机类型给后缀
    #[test]
    fn launcher_name_suffix_by_host_kind() {
        assert_eq!(launcher_name(SessionAgent::Claude, HostKind::Posix), "falcon-claude.sh");
        assert_eq!(launcher_name(SessionAgent::Claude, HostKind::Windows), "falcon-claude.cmd");
    }

    /// 远端路径落在 <root>/agents 下
    #[test]
    fn remote_launcher_path_under_root_agents() {
        assert_eq!(
            remote_launcher_path(HostKind::Posix, "/home/fay/.falcon", SessionAgent::Grok),
            "/home/fay/.falcon/agents/falcon-grok.sh"
        );
        assert_eq!(
            remote_launcher_path(HostKind::Windows, "C:\\Users\\fay\\.falcon", SessionAgent::Codex),
            "C:\\Users\\fay\\.falcon\\agents\\falcon-codex.cmd"
        );
    }

    // ---- writeRemoteLauncherCommand ----

    /// POSIX：建目录、写文件、加可执行位，一条 && 链
    #[test]
    fn write_remote_posix_mkdir_write_chmod_in_one_chain() {
        let cmd = write_remote_launcher_command(HostKind::Posix, "/home/fay/.falcon/agents", SessionAgent::Claude, "/bin/zsh");
        assert!(cmd.starts_with("d='/home/fay/.falcon/agents'; mkdir -p \"$d\" && "));
        assert!(cmd.contains("chmod 755 \"$d\"/falcon-claude.sh"));
    }

    /// Windows：走 -EncodedCommand，输出只有 base64 字符
    #[test]
    fn write_remote_windows_uses_encoded_command_with_base64_only() {
        let cmd = write_remote_launcher_command(
            HostKind::Windows,
            "C:\\Users\\fay\\.falcon\\agents",
            SessionAgent::Codex,
            "powershell.exe",
        );
        let b64 = cmd.split("-EncodedCommand ").nth(1).unwrap();
        assert!(!b64.is_empty() && b64.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)));
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert!(script.contains("New-Item -ItemType Directory"));
        assert!(script.contains("falcon-codex.cmd"));
    }

    // ---- agentBin ----

    /// 三个 CLI 的可执行名
    #[test]
    fn agent_bin_names_of_the_three_clis() {
        assert_eq!(agent_bin(SessionAgent::Claude), "claude");
        assert_eq!(agent_bin(SessionAgent::Codex), "codex");
        assert_eq!(agent_bin(SessionAgent::Grok), "grok");
    }

    // ---- 以下不在 agent.test.ts 里 ----

    /// 逐字节对照 TS 产出：宿主机上已有的脚本是这份内容，幂等覆盖不该改出差异
    #[test]
    fn extra_bodies_match_ts_byte_for_byte() {
        assert_eq!(
            launcher_body(SessionAgent::Claude, HostKind::Posix, "/bin/zsh"),
            "#!/bin/sh\n\
             # falcon: 以 claude 开场的会话。CLI 退出后落回登录 shell，会话不跟着结束。\n\
             exec '/bin/zsh' -i -l -c 'if command -v claude >/dev/null 2>&1; then claude; \
             else printf '\\''%s\\n'\\'' '\\''falcon: PATH 里找不到 claude，先装好它再开这种会话。'\\'' >&2; fi; \
             exec '\\''/bin/zsh'\\'' -i -l'\n"
        );
        assert_eq!(
            windows_body(),
            "@echo off\r\nrem falcon: session that opens with claude; falls back to the shell on exit\r\n\
             where claude >nul 2>&1\r\nif errorlevel 1 (\r\n  echo falcon: claude was not found on PATH; install it first.\r\n\
             ) else (\r\n  call claude\r\n)\r\n\"C:\\Windows\\System32\\powershell.exe\"\r\n"
        );
        assert_eq!(local_launcher_dir(Path::new("/data")), PathBuf::from("/data/agents"));
    }

    /// 真跑一遍生成的 sh 命令：落盘内容与 launcher_body 一致、可执行；脚本本身能跑通
    /// "找不到 CLI → 提示 → 落回 shell"（shell 换成一个把参数打出来的假 shell）
    #[cfg(unix)]
    #[test]
    fn extra_posix_write_command_runs_under_sh() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let tmp = tempfile::Builder::new().prefix("falcon-agent-").tempdir().unwrap();
        let fake_shell = tmp.path().join("o'brien sh");
        std::fs::write(&fake_shell, "#!/bin/sh\nprintf 'shell:%s\\n' \"$*\"\n").unwrap();
        std::fs::set_permissions(&fake_shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let fake_shell = fake_shell.to_str().unwrap();

        let dir = tmp.path().join("a b").join("agents");
        let dir_s = dir.to_str().unwrap();
        // 用不存在的 agent 名的 CLI：Unknown 生成 `command -v unknown`
        let cmd = write_remote_launcher_command(HostKind::Posix, dir_s, SessionAgent::Unknown, fake_shell);
        let out = Command::new("sh").arg("-c").arg(&cmd).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let file = dir.join("falcon-unknown.sh");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            launcher_body(SessionAgent::Unknown, HostKind::Posix, fake_shell)
        );
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o755);

        // 假 shell 收到的就是 `-i -l -c <inner>`，自己不执行 inner；所以这里只核对参数形状
        let run = Command::new("sh").arg(&file).env("PATH", "/usr/bin:/bin").output().unwrap();
        let stdout = String::from_utf8(run.stdout).unwrap();
        assert!(stdout.starts_with("shell:-i -l -c if command -v unknown "), "{stdout}");
    }
}
