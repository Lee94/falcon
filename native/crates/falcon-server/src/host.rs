//! 宿主机差异：路径构造、命令行拼装、远端探测。移植自 `packages/server/src/zellij/host.ts`
//! （Zellij 退役后从 `zellij/` 挪出来：git、文件、px0、会话守护进程都在用它）。
//!
//! 四种执行环境（本地 Unix / 本地 Windows / SSH Unix 远端 / SSH Windows 远端）
//! 归结为两类 host：posix 与 windows。本地执行走 spawn 的 argv 数组、不经 shell，
//! 因此只有 SSH 远端需要把 argv 拼成命令行字符串。
//!
//! 环境变量一律是**有序**的 `(名, 值)` 列表：TS 里是 `Object.entries`，按插入顺序展开，
//! 发到宿主机的命令串要逐字节一致（老会话、测试都依赖它），不能换成无序 map。

use base64::Engine as _;
pub use falcon_proto::HostKind;

/// falcon 在宿主机上的全部落脚点，删掉它即完成卸载
const ROOT: &str = ".falcon";
/// 产品曾名 Mojito 时的根目录。远端探测时若新目录还不在，会尝试改名过来。
const LEGACY_ROOT: &str = ".mojito";

// ---------------- 路径 ----------------

fn sep(kind: HostKind) -> &'static str {
    if kind == HostKind::Windows { "\\" } else { "/" }
}

fn trim_trailing_seps(s: &str) -> &str {
    s.trim_end_matches(['\\', '/'])
}

fn join_home(kind: HostKind, home: &str, name: &str) -> String {
    format!("{}{}{name}", trim_trailing_seps(home), sep(kind))
}

/// 远端的 falcon 根目录：宿主机 home 下的 .falcon。
///
/// 一律用探测阶段拿到的**真实 home 绝对路径**，不用 `~` 或 `$HOME`：
/// 让 shell 展开变量意味着路径不能整体加引号，含空格的用户名（Windows 上很常见）
/// 会直接把命令行拆散。
pub fn remote_root(kind: HostKind, home: &str) -> String {
    join_home(kind, home, ROOT)
}

pub fn legacy_remote_root(kind: HostKind, home: &str) -> String {
    join_home(kind, home, LEGACY_ROOT)
}

/// 把旧的 `~/.mojito` 迁到 `~/.falcon`。打印最终该用的根目录（一行）。
///
/// 逻辑：新目录还不在而旧目录是个文件夹时尝试 `mv`；无论成败，再按「新的在就用新的，
/// 否则旧的在就用旧的，否则用新路径」选取。并发两次探测时只有一个 mv 能成功，
/// 失败者会看到新目录已经在，不会退到一个已经搬走的旧路径。
///
/// 本机不能这么做：后端自己的 SQLite 开在数据目录里，进程还活着就 mv 会把库从
/// 自己脚下抽走。远端根目录里没有后端打开的文件。
///
/// 但"远端"也可能就是跑着 falcon 后端的机器：SSH 连回本机（含经局域网地址绕回来的），
/// 或那台远端自己也用 `~/.mojito` 当后端数据目录。实测出过事——测试连本机 sshd，
/// 把在用服务的数据目录整个改了名，服务靠已打开的 fd 照跑，一重启就找不到程序和库。
/// 所以旧目录里有 `secret.key`（只有后端数据目录才有，见 crypto.rs）就不搬，原地沿用。
pub fn posix_migrate_root_script(next: &str, prev: &str) -> String {
    let n = quote_posix(next);
    let p = quote_posix(prev);
    format!(
        "n={n}; p={p}; \
         if [ ! -e \"$n\" ] && [ -d \"$p\" ] && [ ! -e \"$p/secret.key\" ]; then mv \"$p\" \"$n\" || true; fi; \
         if [ -d \"$n\" ]; then printf '%s\\n' \"$n\"; \
         elif [ -d \"$p\" ]; then printf '%s\\n' \"$p\"; \
         else printf '%s\\n' \"$n\"; fi"
    )
}

pub fn windows_migrate_root_script(next: &str, prev: &str) -> String {
    let n = quote_powershell(next);
    let p = quote_powershell(prev);
    format!(
        "$n = {n}; $p = {p}; \
         if (-not (Test-Path -LiteralPath $n) -and (Test-Path -LiteralPath $p -PathType Container) \
         -and -not (Test-Path -LiteralPath (Join-Path $p 'secret.key'))) {{ \
         try {{ Move-Item -LiteralPath $p -Destination $n -ErrorAction Stop }} catch {{}} }}; \
         if (Test-Path -LiteralPath $n -PathType Container) {{ $n }} \
         elseif (Test-Path -LiteralPath $p -PathType Container) {{ $p }} \
         else {{ $n }}"
    )
}

pub fn migrate_remote_root_command(kind: HostKind, next: &str, prev: &str) -> String {
    match kind {
        HostKind::Windows => encode_powershell(&windows_migrate_root_script(next, prev)),
        _ => posix_migrate_root_script(next, prev),
    }
}

/// 迁移脚本的 stdout：取第一行非空路径。
pub fn parse_migrated_root(stdout: &str) -> Option<String> {
    stdout.split('\n').map(str::trim).find(|l| !l.is_empty()).map(str::to_string)
}

/// `{ ...base, ...extra }`：同名键原位替换值（保持在 base 里的位置），新键追加在末尾。
/// 发到宿主机的命令串按 env 的顺序展开，这个顺序要与 JS 对象展开完全一致
pub fn merge_env<K: Into<String>, V: Into<String>>(base: &mut Vec<(String, String)>, extra: impl IntoIterator<Item = (K, V)>) {
    for (k, v) in extra {
        let (k, v) = (k.into(), v.into());
        match base.iter_mut().find(|(bk, _)| *bk == k) {
            Some((_, bv)) => *bv = v,
            None => base.push((k, v)),
        }
    }
}

// ---------------- 转义 ----------------

/// POSIX sh 单引号转义：内部的单引号写成 '\''
pub fn quote_posix(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// PowerShell 单引号字面量：内部的单引号写成两个单引号
pub fn quote_powershell(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// ---------------- 命令行拼装 ----------------

/// 拼一条可交给远端默认 shell 执行的命令。
///
/// Windows 用 `-EncodedCommand`（UTF-16LE + base64）而不是 `-Command "..."`：
/// OpenSSH for Windows 的默认 shell 由注册表 DefaultShell 决定，可能是 cmd.exe、
/// PowerShell 5.1 或 7，三者的引号与转义规则各不相同。base64 串里只有
/// A-Za-z0-9+/= ，任何一层 shell 都不会改写它——这样我们只维护一套 PowerShell
/// 语法，且完全不受远端 DefaultShell 配置影响。
pub fn build_command_line<A, K, V>(kind: HostKind, argv: &[A], env: &[(K, V)], unset: &[&str]) -> String
where
    A: AsRef<str>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    if kind == HostKind::Windows {
        return encode_powershell(&powershell_script(argv, env, unset));
    }
    let parts: Vec<String> = unset
        .iter()
        .map(|k| format!("-u {k}"))
        .chain(env.iter().map(|(k, v)| format!("{}={}", k.as_ref(), quote_posix(v.as_ref()))))
        .collect();
    let cmd = argv.iter().map(|a| quote_posix(a.as_ref())).collect::<Vec<_>>().join(" ");
    if parts.is_empty() { cmd } else { format!("env {} {cmd}", parts.join(" ")) }
}

/// 生成 PowerShell 脚本片段：设环境变量后用调用运算符执行，并**如实传出退出码**。
///
/// 退出码这件事有两个坑，都是实测出来的（`powershell -EncodedCommand` + 重定向）：
///
/// 1. 不写 `exit` 时 PowerShell 只给 0 / 1：原生命令退 3 也会被压成 1。想区分
///    `curl` 的 6（域名解析不了）/ 7（连不上）/ 22（HTTP 错）/ 28（超时），
///    或 `git diff --quiet` 的 1（有改动）与 128（不是仓库），就必须自己 exit。
///    更糟的是：只要原生命令**不是最后一条语句**，它的失败会被完全吞掉退 0。
///
/// 2. 但裸写 `exit $LASTEXITCODE` 更危险。`& 'C:\缺失.exe'` 抛的是
///    CommandNotFoundException——原生命令根本没跑，`$LASTEXITCODE` 从未被赋值，
///    `exit $null` 等价于 `exit 0`，于是"命令跑不起来"被报成成功。实测确认。
///
/// 所以预置哨兵 127（POSIX 的 command-not-found 约定）：命令跑起来了 PowerShell
/// 会用真实退出码覆盖它，没跑起来就留着 127。
///
/// `unset` 是"删掉这些环境变量"，与 env 的"设成这个值"严格分开——**绝不能用空串
/// 代替删除**。PowerShell 里 `$env:X = ''` 恰好等价于删除，但 POSIX 侧
/// `env X='' cmd` 是"设成空串"，两者天差地别：实测 `GIT_DIR=''` 直接
/// `fatal: not a git repository: ''`，`GIT_INDEX_FILE=''` 更糟——git 会拿 `.lock`
/// 当索引，`status` 报出一堆并不存在的删除。
pub fn powershell_script<A, K, V>(argv: &[A], env: &[(K, V)], unset: &[&str]) -> String
where
    A: AsRef<str>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut lines = vec!["$LASTEXITCODE = 127".to_string()];
    lines.extend(unset.iter().map(|k| format!("$env:{k} = $null")));
    lines.extend(env.iter().map(|(k, v)| format!("$env:{} = {}", k.as_ref(), quote_powershell(v.as_ref()))));
    let mut call = Vec::with_capacity(argv.len());
    if let Some((exe, rest)) = argv.split_first() {
        call.push(format!("& {}", quote_powershell(exe.as_ref())));
        call.extend(rest.iter().map(|a| quote_powershell(a.as_ref())));
    }
    lines.push(call.join(" "));
    lines.push("exit $LASTEXITCODE".to_string());
    lines.join("; ")
}

/// 把 PowerShell 脚本包成 -EncodedCommand 调用。
///
/// 统一关掉进度流：`-NonInteractive` 下 PowerShell 会把进度记录序列化成 CLIXML
/// 写进 stderr（`Add-Type`、`Invoke-WebRequest` 之类都会触发）。stdout 不受影响，
/// 但我们在安装失败时把 stderr 当错误详情报给用户，夹带一堆 XML 就没法看了。
///
/// 统一把输出编码钉成 UTF-8：PowerShell 按 `[Console]::OutputEncoding`（默认是
/// 系统的 OEM 代码页，简体中文机器上是 936/GBK）解码原生命令的字节，而我们两侧的
/// 读取端都按 UTF-8 解码。在 936 机器上实测：`echo 中文路径` 不设这一行时拿到的是
/// GBK 字节，解码出来是乱码；设了就是干净的 UTF-8。含中文的路径、`dump-screen` 抓回的
/// Scrollback 全靠它。重定向到管道时不会写 BOM（同样实测过），所以解析端不用防前导 BOM。
pub fn encode_powershell(script: &str) -> String {
    let full = format!(
        "$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; {script}"
    );
    let utf16: Vec<u8> = full.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let b64 = base64::engine::general_purpose::STANDARD.encode(utf16);
    format!("powershell -NoProfile -NonInteractive -EncodedCommand {b64}")
}

/// 远端建立 PTY 会话时执行的命令。
///
/// 与上面不同的是这条会跑在交互式 PTY 里、进程要一直活着，
/// 因此 POSIX 侧用 exec 顶掉外层 shell，少一层进程。
///
/// `login_shell` 会把整条命令包进一个**登录 shell**（`sh -lc '...'`）。这是必须的：
/// SSH exec 拿到的是非登录、非交互 shell，只有 `/etc/profile` 与
/// `~/.profile`、`~/.bash_profile` 里的 PATH 一概不读——而 nvm、pyenv、cargo、
/// Homebrew、Go、`~/.local/bin` 恰恰大多写在那里。不套这一层的话，用户会发现
/// 终端里"很多已经装好的命令找不到"，但 `ssh` 进去却好好的。
///
/// 套在最外层而不是只包最终 shell，是因为会话守护进程（holder）也由这条命令拉起：
/// 它的环境会被它 spawn 的 shell 继承，一次修到底。
///
/// `path_prepend` 插到登录 shell 展开后的 PATH 前面（sudo askpass 包装）。不能写进
/// env.PATH，那会盖掉 profile 里的 PATH。
pub fn build_pty_command_line<A, K, V>(
    kind: HostKind,
    argv: &[A],
    env: &[(K, V)],
    login_shell: Option<&str>,
    path_prepend: Option<&str>,
) -> String
where
    A: AsRef<str>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    if kind == HostKind::Windows {
        return encode_powershell(&powershell_script(argv, env, &[]));
    }
    let assigns: Vec<String> =
        env.iter().map(|(k, v)| format!("{}={}", k.as_ref(), quote_posix(v.as_ref()))).collect();
    let cmd = argv.iter().map(|a| quote_posix(a.as_ref())).collect::<Vec<_>>().join(" ");
    let mut inner =
        if assigns.is_empty() { format!("exec {cmd}") } else { format!("exec env {} {cmd}", assigns.join(" ")) };
    if let Some(prepend) = path_prepend.filter(|p| !p.is_empty()) {
        inner = format!("PATH={}:$PATH {inner}", quote_posix(prepend));
    }
    match login_shell.filter(|s| !s.is_empty()) {
        Some(shell) => format!("exec {} -l -c {}", quote_posix(shell), quote_posix(&inner)),
        None => inner,
    }
}

// ---------------- 探测 ----------------

/// 一次往返拿齐宿主机信息，避免为 home / 架构 / shell 各发一次命令。
/// 输出三行：home、`uname -sm`、登录 shell。
///
/// 在 Windows 远端上这条命令会失败（没有 uname），调用方据此改走 Windows 探测。
///
/// 登录 shell 必须探测：SSH exec 是非登录、非交互的，环境里**没有 `$SHELL`**，
/// 而会话守护进程要拿它拉起会话的 shell、agent 启动脚本要把它写死进去。
pub const POSIX_PROBE_LINES: [&str; 3] = [
    r#"printf "%s\n" "$HOME""#,
    "uname -sm",
    r#"S="$SHELL"; [ -n "$S" ] || S=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7); [ -n "$S" ] || S=/bin/sh; printf "%s\n" "$S""#,
];

pub fn posix_probe() -> String {
    POSIX_PROBE_LINES.join("; ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosixProbe {
    pub home: String,
    pub uname: String,
    pub shell: String,
}

pub fn parse_posix_probe(stdout: &str) -> Option<PosixProbe> {
    let lines: Vec<&str> = stdout.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let home = *lines.first()?;
    let uname = *lines.get(1)?;
    let shell = lines.get(2).copied().unwrap_or("/bin/sh");
    Some(PosixProbe { home: home.into(), uname: uname.into(), shell: shell.into() })
}

/// Windows 探测：home、架构、登录 shell。
pub const WINDOWS_PROBE_SCRIPT: &str = "$env:USERPROFILE\n\
$env:PROCESSOR_ARCHITECTURE\n\
$s = (Get-Command powershell.exe -EA SilentlyContinue).Source; if ($s) { $s } else { $env:COMSPEC }";
// 最后一行用绝对路径而非裸名字：ConPTY 拉起 shell 时不经 PATH 解析的那条路才可靠

pub fn windows_probe() -> String {
    encode_powershell(&WINDOWS_PROBE_SCRIPT.replace('\n', "; "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsProbe {
    pub home: String,
    pub arch: String,
    pub shell: String,
}

pub fn parse_windows_probe(stdout: &str) -> Option<WindowsProbe> {
    let lines: Vec<&str> = stdout.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let home = *lines.first()?;
    let arch = *lines.get(1)?;
    Some(WindowsProbe { home: home.into(), arch: arch.into(), shell: lines.get(2).copied().unwrap_or("powershell.exe").into() })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::process::Command;

    const PREFIX: &str = "powershell -NoProfile -NonInteractive -EncodedCommand ";

    /// 解开 -EncodedCommand 的载荷（测试里多处要用）
    pub(crate) fn decode(cmd: &str) -> String {
        let b64 = cmd.strip_prefix(PREFIX).unwrap_or_else(|| panic!("not an EncodedCommand call: {cmd:.60}"));
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).unwrap()
    }

    const NO_ENV: &[(&str, &str)] = &[];

    #[test]
    fn remote_root_joins_under_home() {
        assert_eq!(remote_root(HostKind::Posix, "/home/u"), "/home/u/.falcon");
        assert_eq!(legacy_remote_root(HostKind::Posix, "/home/u"), "/home/u/.mojito");
        assert_eq!(remote_root(HostKind::Windows, "C:\\Users\\a b"), "C:\\Users\\a b\\.falcon");
        assert_eq!(legacy_remote_root(HostKind::Windows, "C:\\Users\\a b"), "C:\\Users\\a b\\.mojito");
    }

    #[test]
    fn remote_root_strips_trailing_separator() {
        assert_eq!(remote_root(HostKind::Posix, "/home/u/"), "/home/u/.falcon");
        assert_eq!(remote_root(HostKind::Windows, "C:\\Users\\u\\"), "C:\\Users\\u\\.falcon");
    }

    #[test]
    fn posix_migrate_quotes_both_paths() {
        let script = posix_migrate_root_script("/home/o'brien/.falcon", "/home/o'brien/.mojito");
        assert!(script.contains(r"n='/home/o'\''brien/.falcon'"));
        assert!(script.contains(r"p='/home/o'\''brien/.mojito'"));
        assert!(script.contains(r#"mv "$p" "$n" || true"#));
        assert!(script.contains(r#"[ ! -e "$p/secret.key" ]"#));
        assert!(script.contains(r#"[ -d "$n" ]"#));
        assert!(script.contains(r#"[ -d "$p" ]"#));
    }

    #[test]
    fn windows_migrate_uses_literal_path() {
        let script = windows_migrate_root_script("C:\\Users\\a b\\.falcon", "C:\\Users\\a b\\.mojito");
        assert!(script.contains("$n = 'C:\\Users\\a b\\.falcon'"));
        assert!(script.contains("Move-Item -LiteralPath $p -Destination $n -ErrorAction Stop"));
        assert!(script.contains("Test-Path -LiteralPath $p -PathType Container"));
        assert!(script.contains("catch {}"));
        assert!(script.contains("-not (Test-Path -LiteralPath (Join-Path $p 'secret.key'))"));
    }

    #[test]
    fn windows_migrate_is_single_encoded_command() {
        let cmd = migrate_remote_root_command(HostKind::Windows, "C:\\Users\\a b\\.falcon", "C:\\Users\\a b\\.mojito");
        assert!(cmd.starts_with(PREFIX));
        let inner = decode(&cmd);
        assert!(inner.contains("Move-Item -LiteralPath $p -Destination $n"));
        assert!(!inner.contains("-EncodedCommand"));
    }

    #[test]
    fn posix_migrate_command_is_raw_script() {
        let (next, prev) = ("/home/u/.falcon", "/home/u/.mojito");
        assert_eq!(migrate_remote_root_command(HostKind::Posix, next, prev), posix_migrate_root_script(next, prev));
    }

    #[test]
    fn parse_migrated_root_first_nonempty_line() {
        assert_eq!(parse_migrated_root("/home/u/.falcon\n").as_deref(), Some("/home/u/.falcon"));
        assert_eq!(parse_migrated_root("\r\nC:\\Users\\u\\.mojito\r\n").as_deref(), Some("C:\\Users\\u\\.mojito"));
        assert_eq!(parse_migrated_root("   \n  \n"), None);
        assert_eq!(parse_migrated_root(""), None);
    }

    fn run_posix(next: &str, prev: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(posix_migrate_root_script(next, prev)).output().unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    fn with_home(f: impl FnOnce(&std::path::Path)) {
        let dir = tempfile::Builder::new().prefix("falcon-mig-").tempdir().unwrap();
        f(dir.path());
    }

    #[cfg(unix)]
    #[test]
    fn posix_live_moves_legacy_dir() {
        with_home(|home| {
            let next = home.join(".falcon");
            let prev = home.join(".mojito");
            std::fs::create_dir(&prev).unwrap();
            std::fs::write(prev.join("marker"), "ok").unwrap();
            assert_eq!(run_posix(next.to_str().unwrap(), prev.to_str().unwrap()).trim(), next.to_str().unwrap());
            assert!(!prev.exists());
            assert_eq!(std::fs::read_to_string(next.join("marker")).unwrap(), "ok");
        });
    }

    #[cfg(unix)]
    #[test]
    fn posix_live_leaves_a_backend_data_dir_in_place() {
        // 旧目录是某个 falcon 后端的数据目录（SSH 连回了本机）：不搬，原地沿用
        with_home(|home| {
            let next = home.join(".falcon");
            let prev = home.join(".mojito");
            std::fs::create_dir(&prev).unwrap();
            std::fs::write(prev.join("secret.key"), [0u8; 32]).unwrap();
            assert_eq!(run_posix(next.to_str().unwrap(), prev.to_str().unwrap()).trim(), prev.to_str().unwrap());
            assert!(prev.join("secret.key").exists());
            assert!(!next.exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn posix_live_keeps_new_dir_when_both_exist() {
        with_home(|home| {
            let next = home.join(".falcon");
            let prev = home.join(".mojito");
            std::fs::create_dir(&next).unwrap();
            std::fs::create_dir(&prev).unwrap();
            std::fs::write(next.join("new"), "1").unwrap();
            std::fs::write(prev.join("old"), "2").unwrap();
            assert_eq!(run_posix(next.to_str().unwrap(), prev.to_str().unwrap()).trim(), next.to_str().unwrap());
            assert!(prev.join("old").exists());
            assert!(!next.join("old").exists());
        });
    }

    #[cfg(unix)]
    #[test]
    fn posix_live_second_run_is_noop() {
        with_home(|home| {
            let next = home.join(".falcon");
            let prev = home.join(".mojito");
            std::fs::create_dir(&next).unwrap();
            std::fs::write(next.join("marker"), "ok").unwrap();
            assert_eq!(run_posix(next.to_str().unwrap(), prev.to_str().unwrap()).trim(), next.to_str().unwrap());
            assert_eq!(std::fs::read_to_string(next.join("marker")).unwrap(), "ok");
        });
    }

    #[cfg(unix)]
    #[test]
    fn posix_live_fresh_host_prints_new_path() {
        with_home(|home| {
            let next = home.join(".falcon");
            let prev = home.join(".mojito");
            assert_eq!(run_posix(next.to_str().unwrap(), prev.to_str().unwrap()).trim(), next.to_str().unwrap());
            assert!(!next.exists());
        });
    }

    #[test]
    fn pty_path_prepend_keeps_login_path() {
        let cmd = build_pty_command_line(
            HostKind::Posix,
            &["/bin/zsh"],
            &[("COLORTERM", "truecolor")],
            Some("/bin/zsh"),
            Some("/home/u/.falcon/bin"),
        );
        // -c 的参数再套一层 quote_posix，单引号被 '\'' 转义，但 $PATH 仍由登录 shell 展开
        assert!(cmd.contains("/home/u/.falcon/bin"));
        assert!(cmd.contains(":$PATH "));
        assert!(cmd.contains("COLORTERM="));
    }

    #[test]
    fn command_line_posix_env_and_unset_order() {
        assert_eq!(
            build_command_line(HostKind::Posix, &["git", "status"], &[("LC_ALL", "C"), ("X", "a b")], &["GIT_DIR"]),
            "env -u GIT_DIR LC_ALL='C' X='a b' 'git' 'status'"
        );
        assert_eq!(build_command_line(HostKind::Posix, &["ls"], NO_ENV, &[]), "'ls'");
        let win = decode(&build_command_line(HostKind::Windows, &["git", "a'b"], &[("X", "1")], &["GIT_DIR"]));
        assert!(win.ends_with(
            "$LASTEXITCODE = 127; $env:GIT_DIR = $null; $env:X = '1'; & 'git' 'a''b'; exit $LASTEXITCODE"
        ));
        assert!(win.starts_with("$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8; "));
    }

    #[test]
    fn probes_parse() {
        let p = parse_posix_probe("/home/u\nLinux x86_64\n/bin/bash\n").unwrap();
        assert_eq!((p.home.as_str(), p.uname.as_str(), p.shell.as_str()), ("/home/u", "Linux x86_64", "/bin/bash"));
        assert_eq!(parse_posix_probe("/home/u\nLinux x86_64\n").unwrap().shell, "/bin/sh");
        assert_eq!(parse_posix_probe("/home/u\n"), None);
        let w = parse_windows_probe("C:\\Users\\u\r\nAMD64\r\nC:\\pwsh.exe\r\n").unwrap();
        assert_eq!((w.arch.as_str(), w.shell.as_str()), ("AMD64", "C:\\pwsh.exe"));
        assert!(decode(&windows_probe()).contains("$env:USERPROFILE; $env:PROCESSOR_ARCHITECTURE; "));
    }
}
