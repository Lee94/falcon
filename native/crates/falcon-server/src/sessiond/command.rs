//! 会话守护进程（falcon-sessiond，ADR 0020）的纯函数：零 I/O，只产出目标三元组、文件名、
//! argv 与命令行。照 `git/command.rs` 的分层：执行在 deploy.rs / backend.rs，环境差异在 host.rs。
//!
//! 绝不拼一条"大家都能用"的命令行字符串：远端 POSIX 与远端 Windows 的转义规则不同，
//! 拼装一律经 [`crate::host`] 按宿主机类型来。

use falcon_proto::HostKind;
use falcon_proto::sessiond::{FALCON_VERSION, SpawnSpec};

use crate::git::path::join_path;
use crate::host::{build_command_line, build_pty_command_line, encode_powershell, quote_powershell};

/// 守护进程的 5 个构建目标（与 `cargo xtask sessiond` 的表一致）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessiondTarget {
    X86_64Linux,
    Aarch64Linux,
    X86_64Mac,
    Aarch64Mac,
    X86_64Windows,
}

impl SessiondTarget {
    pub const ALL: [SessiondTarget; 5] = [
        SessiondTarget::X86_64Linux,
        SessiondTarget::Aarch64Linux,
        SessiondTarget::X86_64Mac,
        SessiondTarget::Aarch64Mac,
        SessiondTarget::X86_64Windows,
    ];

    /// Rust 目标三元组（也是 `version --json` 报的 target）
    pub const fn triple(self) -> &'static str {
        match self {
            SessiondTarget::X86_64Linux => "x86_64-unknown-linux-musl",
            SessiondTarget::Aarch64Linux => "aarch64-unknown-linux-musl",
            SessiondTarget::X86_64Mac => "x86_64-apple-darwin",
            SessiondTarget::Aarch64Mac => "aarch64-apple-darwin",
            SessiondTarget::X86_64Windows => "x86_64-pc-windows-gnu",
        }
    }

    pub fn from_triple(t: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.triple() == t)
    }

    pub const fn is_windows(self) -> bool {
        matches!(self, SessiondTarget::X86_64Windows)
    }
}

/// `uname -sm` → 目标。Linux 一律用 musl 的静态构建：不挑发行版的 glibc 版本
pub fn target_from_uname(uname: &str) -> Option<SessiondTarget> {
    let mut it = uname.split_whitespace();
    let os = it.next()?.to_ascii_lowercase();
    let arch = it.next()?.to_ascii_lowercase();
    match (os.as_str(), arch.as_str()) {
        ("linux", "x86_64" | "amd64") => Some(SessiondTarget::X86_64Linux),
        ("linux", "aarch64" | "arm64") => Some(SessiondTarget::Aarch64Linux),
        ("darwin", "x86_64") => Some(SessiondTarget::X86_64Mac),
        ("darwin", "arm64" | "aarch64") => Some(SessiondTarget::Aarch64Mac),
        _ => None,
    }
}

/// `$env:PROCESSOR_ARCHITECTURE` → 目标。Windows ARM64 跑 x64 的构建（系统自带 x64 模拟，
/// 跑不起来时握手会报 verify-failed）
pub fn target_from_windows_arch(arch: &str) -> Option<SessiondTarget> {
    match arch.trim().to_ascii_uppercase().as_str() {
        "AMD64" | "ARM64" => Some(SessiondTarget::X86_64Windows),
        _ => None,
    }
}

/// 后端本机的目标
pub fn local_target() -> Option<SessiondTarget> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(SessiondTarget::X86_64Linux),
        ("linux", "aarch64") => Some(SessiondTarget::Aarch64Linux),
        ("macos", "x86_64") => Some(SessiondTarget::X86_64Mac),
        ("macos", "aarch64") => Some(SessiondTarget::Aarch64Mac),
        ("windows", "x86_64" | "aarch64") => Some(SessiondTarget::X86_64Windows),
        _ => None,
    }
}

/// 宿主机上的文件名：`falcon-sessiond-<版本>-<内容哈希前 8 位>[.exe]`。
///
/// 带内容哈希：开发构建版本号不变而内容在变，同名覆盖会换掉正在跑的 holder 的二进制
/// （Linux 上 `/proc/self/exe` 变成 "(deleted)"，VT 进程重启就起不来了）。按内容命名，
/// 旧的一个都不覆盖、不删——升级不杀会话（ADR 0020 决定四）
pub fn binary_name(target: SessiondTarget, hash8: &str) -> String {
    let exe = if target.is_windows() { ".exe" } else { "" };
    format!("falcon-sessiond-{FALCON_VERSION}-{hash8}{exe}")
}

/// 宿主机上放二进制的目录：`<falcon 根>/bin`（与 askpass 包装同目录）
pub fn bin_dir(kind: HostKind, root: &str) -> String {
    join_path(kind, &[root, "bin"])
}

pub fn binary_path(kind: HostKind, root: &str, name: &str) -> String {
    join_path(kind, &[root, "bin", name])
}

pub fn version_argv(bin: &str) -> Vec<String> {
    vec![bin.into(), "version".into(), "--json".into()]
}

pub fn spawn_argv(bin: &str, root: &str, sid: &str, spec: &SpawnSpec) -> Vec<String> {
    vec![bin.into(), "spawn".into(), "--root".into(), root.into(), "--id".into(), sid.into(), "--spec".into(), spec.encode()]
}

pub fn bridge_argv(bin: &str, root: &str, sid: &str, create: Option<&SpawnSpec>) -> Vec<String> {
    let mut argv = vec![bin.into(), "bridge".into(), "--root".into(), root.into(), "--id".into(), sid.into()];
    if let Some(spec) = create {
        argv.extend(["--create".into(), spec.encode()]);
    }
    argv
}

pub fn list_argv(bin: &str, root: &str) -> Vec<String> {
    vec![bin.into(), "list".into(), "--json".into(), "--root".into(), root.into()]
}

pub fn kill_argv(bin: &str, root: &str, sids: &[String]) -> Vec<String> {
    let mut argv = vec![bin.into(), "kill".into(), "--root".into(), root.into()];
    argv.extend(sids.iter().cloned());
    argv
}

/// 远端跑一条守护进程子命令拿输出（version / list / kill）
pub fn exec_command(kind: HostKind, argv: &[String]) -> String {
    let no_env: &[(&str, &str)] = &[];
    build_command_line(kind, argv, no_env, &[])
}

/// 远端开会话的 bridge。
///
/// POSIX：套一层**登录 shell**（`exec <shell> -l -c '…'`）：带 `--create` 时 holder 由这条命令
/// 拉起，它的环境会被会话的 shell 继承——SSH exec 拿到的是非登录 shell，`~/.profile` 里的
/// PATH 全丢（ADR 0001：在最外层套，一次修到底）。`path_prepend` 插在登录 shell 展开后的 PATH
/// 前面（sudo askpass 包装），不能写进 env.PATH 盖掉 profile 里的。
/// rc 文件往 stdout 打的字由 bridge 的前导标记挡掉（`BRIDGE_PREAMBLE`）。
///
/// Windows：见 [`windows_bridge_command`]。
pub fn bridge_command(kind: HostKind, argv: &[String], login_shell: Option<&str>, path_prepend: Option<&str>) -> String {
    match kind {
        HostKind::Windows => windows_bridge_command(argv),
        _ => {
            let no_env: &[(&str, &str)] = &[];
            build_pty_command_line(HostKind::Posix, argv, no_env, login_shell, path_prepend)
        }
    }
}

/// Windows 的命令行参数转义（`CommandLineToArgvW` 的规则）：含空白或引号的整体加双引号，
/// 引号前的反斜杠加倍、引号本身前面补反斜杠，结尾的反斜杠在收尾引号前加倍
pub fn windows_quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c == ' ' || c == '\t' || c == '"') {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                out.push(c);
                backslashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes * 2));
    out.push('"');
    out
}

/// Windows 上的 bridge：PowerShell 里**不能**用 `& exe`——PowerShell 会把原生命令的 stdout 当
/// 文本行收下再按控制台编码重新写出（换行改成 CRLF、非文本字节被改写），二进制帧流全毁。
/// 用 .NET 的 Process 起它、不重定向任何流：子进程直接继承 powershell 自己的 stdin / stdout
/// （也就是 sshd 的管道），字节原样过；再把它的退出码透传出去（bridge 用退出码报"会话没了"）。
///
/// 外层仍是 `powershell -EncodedCommand`：远端 DefaultShell 是 cmd 还是 PowerShell 都不影响（host.rs）
pub fn windows_bridge_command(argv: &[String]) -> String {
    let (exe, args) = argv.split_first().expect("argv 至少有可执行文件");
    let joined = args.iter().map(|a| windows_quote_arg(a)).collect::<Vec<_>>().join(" ");
    let script = [
        "$psi = New-Object System.Diagnostics.ProcessStartInfo".to_string(),
        format!("$psi.FileName = {}", quote_powershell(exe)),
        format!("$psi.Arguments = {}", quote_powershell(&joined)),
        "$psi.UseShellExecute = $false".to_string(),
        "$p = [System.Diagnostics.Process]::Start($psi)".to_string(),
        "$p.WaitForExit()".to_string(),
        "exit $p.ExitCode".to_string(),
    ]
    .join("; ");
    encode_powershell(&script)
}

/// POSIX 远端：从 stdin 收二进制写到 `file`。先写 `.partial` 再改名：推到一半断了不会留下
/// 一个截断的可执行文件（同 px0）；目录 0700，只给自己
pub fn posix_install_command(file: &str) -> String {
    let dir = match file.rfind('/') {
        Some(i) if i > 0 => &file[..i],
        _ => "/",
    };
    format!(
        "d={}; f={}; mkdir -p \"$d\" && chmod 700 \"$d\" && cat > \"$f.partial\" && chmod 755 \"$f.partial\" && mv -f \"$f.partial\" \"$f\"",
        crate::host::quote_posix(dir),
        crate::host::quote_posix(file)
    )
}

/// Windows 远端：stdin 上逐行 base64（ADR 0008 的行协议，`transfer.rs` 的编码器），写临时文件、
/// 核对字节数后改名。PowerShell 读不了 stdin 上的原始字节（同 windows_bridge_command 的理由）
pub fn windows_install_command(file: &str, size: u64) -> String {
    let script = [
        "$ErrorActionPreference = 'Stop'".to_string(),
        format!("$f = {}", quote_powershell(file)),
        "$t = $f + '.partial'".to_string(),
        "$d = Split-Path -Parent $f".to_string(),
        "New-Item -ItemType Directory -Force -Path $d | Out-Null".to_string(),
        "$fs = [IO.File]::Create($t)".to_string(),
        "try { while ($null -ne ($line = [Console]::In.ReadLine())) { \
         if ($line.Length -gt 0) { $b = [Convert]::FromBase64String($line); $fs.Write($b, 0, $b.Length) } } \
         } finally { $fs.Close() }"
            .to_string(),
        format!("if ((Get-Item -LiteralPath $t -Force).Length -ne {size}) {{ Remove-Item -LiteralPath $t -Force; Write-Output 'ESHORT'; exit 1 }}"),
        "Move-Item -LiteralPath $t -Destination $f -Force".to_string(),
    ]
    .join("; ");
    encode_powershell(&script)
}

/// 二进制在不在（只看文件；跑不跑得起来由随后的 version 握手判断）
pub fn exists_command(kind: HostKind, file: &str) -> String {
    match kind {
        HostKind::Windows => encode_powershell(&format!("if (Test-Path -LiteralPath {} -PathType Leaf) {{ 'yes' }}", quote_powershell(file))),
        _ => format!("[ -x {} ] && echo yes", crate::host::quote_posix(file)),
    }
}

/// `version --json` 的输出对得上这个 falcon 要的版本与目标
pub fn version_matches(stdout: &str, target: SessiondTarget) -> bool {
    stdout.lines().filter_map(|l| serde_json::from_str::<falcon_proto::sessiond::VersionInfo>(l.trim()).ok()).any(|v| {
        v.version == FALCON_VERSION
            && v.target == target.triple()
            && falcon_proto::sessiond::negotiate((falcon_proto::sessiond::PROTO_MIN, falcon_proto::sessiond::PROTO_MAX), (v.proto_min, v.proto_max))
                .is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::tests::decode;

    #[test]
    fn targets_from_probes() {
        assert_eq!(target_from_uname("Linux x86_64"), Some(SessiondTarget::X86_64Linux));
        assert_eq!(target_from_uname("Linux aarch64"), Some(SessiondTarget::Aarch64Linux));
        assert_eq!(target_from_uname("Darwin arm64"), Some(SessiondTarget::Aarch64Mac));
        assert_eq!(target_from_uname("Darwin x86_64"), Some(SessiondTarget::X86_64Mac));
        assert_eq!(target_from_uname("FreeBSD amd64"), None);
        assert_eq!(target_from_uname("Linux riscv64"), None);
        assert_eq!(target_from_windows_arch("AMD64\r"), Some(SessiondTarget::X86_64Windows));
        assert_eq!(target_from_windows_arch("ARM64"), Some(SessiondTarget::X86_64Windows));
        assert_eq!(target_from_windows_arch("x86"), None);
        for t in SessiondTarget::ALL {
            assert_eq!(SessiondTarget::from_triple(t.triple()), Some(t));
        }
    }

    #[test]
    fn binary_names_carry_version_and_hash() {
        let n = binary_name(SessiondTarget::X86_64Windows, "0123abcd");
        assert_eq!(n, format!("falcon-sessiond-{FALCON_VERSION}-0123abcd.exe"));
        assert_eq!(binary_path(HostKind::Posix, "/home/u/.falcon/", "x"), "/home/u/.falcon/bin/x");
        assert_eq!(binary_path(HostKind::Windows, "C:\\Users\\u\\.falcon", "x.exe"), "C:\\Users\\u\\.falcon\\bin\\x.exe");
    }

    #[test]
    fn posix_bridge_runs_under_a_login_shell() {
        let argv = bridge_argv("/h/.falcon/bin/sd", "/h/.falcon", "0123456789abcdef", None);
        let cmd = bridge_command(HostKind::Posix, &argv, Some("/bin/zsh"), Some("/h/.falcon/bin"));
        assert!(cmd.starts_with("exec '/bin/zsh' -l -c "), "{cmd}");
        assert!(cmd.contains("bridge"), "{cmd}");
        assert!(cmd.contains(":$PATH "), "{cmd}");
    }

    #[test]
    fn windows_args_follow_command_line_to_argv() {
        assert_eq!(windows_quote_arg("plain"), "plain");
        assert_eq!(windows_quote_arg("C:\\Users\\a b\\.falcon"), "\"C:\\Users\\a b\\.falcon\"");
        assert_eq!(windows_quote_arg("C:\\x y\\"), "\"C:\\x y\\\\\"");
        assert_eq!(windows_quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(windows_quote_arg(""), "\"\"");
    }

    #[test]
    fn windows_bridge_inherits_handles_instead_of_piping_through_powershell() {
        let argv = bridge_argv("C:\\Users\\a b\\.falcon\\bin\\sd.exe", "C:\\Users\\a b\\.falcon", "0123456789abcdef", None);
        let script = decode(&windows_bridge_command(&argv));
        assert!(script.contains("UseShellExecute = $false"), "{script}");
        assert!(!script.contains("RedirectStandard"), "不重定向：子进程直接继承 sshd 的管道");
        assert!(script.contains("--root \"C:\\Users\\a b\\.falcon\""), "{script}");
        assert!(script.contains("exit $p.ExitCode"));
        assert!(!script.contains("& '"), "不能经 PowerShell 的调用运算符：它会改写二进制输出");
    }

    #[test]
    fn version_must_match_this_falcon() {
        let ok = format!(
            r#"{{"version":"{FALCON_VERSION}","protoMin":1,"protoMax":1,"ghostty":"x","target":"x86_64-unknown-linux-musl"}}"#
        );
        assert!(version_matches(&format!("motd 噪音\n{ok}\n"), SessiondTarget::X86_64Linux));
        assert!(!version_matches(&ok, SessiondTarget::Aarch64Linux));
        assert!(!version_matches(&ok.replace(FALCON_VERSION, "0.0.1"), SessiondTarget::X86_64Linux));
        assert!(!version_matches("sh: not found", SessiondTarget::X86_64Linux));
    }

    #[test]
    fn install_commands_write_partial_then_rename() {
        let c = posix_install_command("/h/.falcon/bin/sd");
        assert!(c.contains("cat > \"$f.partial\"") && c.contains("mv -f \"$f.partial\" \"$f\""), "{c}");
        let w = decode(&windows_install_command("C:\\h\\.falcon\\bin\\sd.exe", 42));
        assert!(w.contains("FromBase64String") && w.contains("-ne 42") && w.contains("Move-Item"), "{w}");
    }
}
