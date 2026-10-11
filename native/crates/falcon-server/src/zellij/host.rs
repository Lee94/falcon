//! 宿主机差异：路径构造与命令行拼装。移植自 `packages/server/src/zellij/host.ts`。
//!
//! 四种执行环境（本地 Unix / 本地 Windows / SSH Unix 远端 / SSH Windows 远端）
//! 归结为两类 host：posix 与 windows。本地执行走 spawn 的 argv 数组、不经 shell，
//! 因此只有 SSH 远端需要把 argv 拼成命令行字符串。
//!
//! 环境变量一律是**有序**的 `(名, 值)` 列表：TS 里是 `Object.entries`，按插入顺序展开，
//! 发到宿主机的命令串要逐字节一致（老会话、测试都依赖它），不能换成无序 map。

use std::ops::Deref;

use base64::Engine as _;
pub use falcon_proto::HostKind;

use super::command::ZellijPaths;
use super::version::{ZellijTarget, binary_name};

/// falcon 在宿主机上的全部落脚点，删掉它即完成卸载（macOS 的 Zellij cache 除外）
const ROOT: &str = ".falcon";
/// 产品曾名 Mojito 时的根目录。远端探测时若新目录还不在，会尝试改名过来。
const LEGACY_ROOT: &str = ".mojito";

// ---------------- 路径 ----------------

/// falcon 在宿主机上用到的全部路径。`Deref` 到 [`ZellijPaths`]：TS 里它是
/// `HostLayout extends ZellijPaths`，凡是收 `&ZellijPaths` 的地方都能直接给 `&layout`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLayout {
    pub zellij: ZellijPaths,
    /// 二进制所在目录
    pub bin_dir: String,
    /// falcon 根目录
    pub root: String,
    /// layout 文件所在目录
    pub layout_dir: String,
}

impl Deref for HostLayout {
    type Target = ZellijPaths;

    fn deref(&self) -> &ZellijPaths {
        &self.zellij
    }
}

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
/// 自己脚下抽走。远端根目录里没有后端打开的文件，Zellij 的 socket 跟着目录一起
/// 改名，客户端按新路径 connect 仍是同一个 inode。
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

/// 基于 falcon 根目录的绝对路径构造。
///
/// 远端的 root 是探测后解析出来的路径（优先 `<home>/.falcon`，必要时从
/// `.mojito` 迁过来），本地的 root 是后端的 `--data-dir`——本地不该硬编码
/// home，用户指定了数据目录就该落在那儿。
pub fn host_layout(kind: HostKind, root: &str, target: ZellijTarget) -> HostLayout {
    let s = sep(kind);
    let j = |parts: &[&str]| parts.join(s);
    let base = trim_trailing_seps(root);
    let zellij = j(&[base, "zellij"]);
    HostLayout {
        zellij: ZellijPaths {
            bin: j(&[base, "bin", &binary_name(target)]),
            socket_dir: j(&[&zellij, "sock"]),
            config_dir: j(&[&zellij, "config"]),
            config_file: j(&[&zellij, "config", "config.kdl"]),
            data_dir: j(&[&zellij, "data"]),
            cache_dir: j(&[&zellij, "cache"]),
            layout_file: j(&[&zellij, "layouts", "falcon.kdl"]),
            scroll_config_file: j(&[&zellij, "config", "scroll.kdl"]),
            scroll_plugin_file: j(&[&zellij, "plugins", "falcon-scroll.wasm"]),
        },
        root: base.to_string(),
        bin_dir: j(&[base, "bin"]),
        layout_dir: j(&[&zellij, "layouts"]),
    }
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
/// 套在最外层而不是只包最终 shell，是因为 Zellij server 也由这条命令拉起：
/// server 的环境会被它 spawn 的每一个 shell 继承，一次修到底。
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

/// 把一条命令包成"生在 sshd 进程树之外"的调用。仅 Windows 需要也仅 Windows 可用。
///
/// Win32-OpenSSH 在 exec 通道关闭时会终结该通道的整个进程树（Job Object），而
/// Zellij 的 server 进程没有脱离父 Job 的能力（上游 PR #5195 未合并）。实测比
/// 文档说的更严酷：不用等 SSH 断开，创建会话的那条 exec 通道一关 server 就被杀。
/// 经 WMI Win32_Process.Create 拉起的进程父进程是 WmiPrvSE，完全在 sshd 的
/// Job 之外，实测能熬过整条 SSH 连接的断开与重连。
///
/// 内层用 `-Command "<脚本>"` 而**不是** `-EncodedCommand`：后者会把内层再做一次
/// base64(UTF-16LE)，叠加外层的同样一层，整体膨胀约 7 倍——带上终端深浅那几个
/// 环境变量后，最终命令行会顶穿远端 DefaultShell（cmd.exe）8191 字符的上限，
/// 报 "The command line is too long."（实测踩到）。powershell_script 产出的脚本
/// 全部用单引号字面量、不含双引号，可整体塞进 `-Command "..."`，再作为字符串
/// 嵌进外层的单引号（doubling 转义）——只有外层做一次编码，体积减半有余。
///
/// 外层等内层进程退出并尽力转出退出码。两个实测出来的细节：
/// 1. Get-Process 拿到的对象要先摸一次 .Handle，进程退出后才读得到 ExitCode；
/// 2. 进程在 Get-Process 之前就退掉的话既等不到也拿不到退出码，只能按 0 处理。
///
/// 所以退出码只是尽力而为——调用方必须用 list-sessions 之类的事实核验结果，
/// 不能只信退出码。WMI 本身的失败（DCOM 被禁、权限不足）会如实退非零。
///
/// `cwd` 走 Win32_Process.Create 的 CurrentDirectory 参数：不传时新进程继承
/// 调用方 WmiPrvSE 的目录（System32）。Zellij 的 `--default-cwd` 在
/// create-background 路径上会被上游丢掉（见 attach_session 处的注释），初始 pane
/// 的 cwd 退化为继承 server 进程的目录，所以必须在这里把 server 生在项目目录里。
pub fn build_detached_command_line<A, K, V>(argv: &[A], env: &[(K, V)], cwd: Option<&str>) -> String
where
    A: AsRef<str>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let inner_cmd = format!(
        "powershell -NoProfile -NonInteractive -Command \"{}\"",
        powershell_script(argv, env, &[])
    );
    let mut cim_args = vec![format!("CommandLine = {}", quote_powershell(&inner_cmd))];
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        cim_args.push(format!("CurrentDirectory = {}", quote_powershell(cwd)));
    }
    let script = [
        format!(
            "$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{ {} }}",
            cim_args.join("; ")
        ),
        "if ($r.ReturnValue -ne 0) { Write-Error ('Win32_Process.Create failed: ' + $r.ReturnValue); exit 1 }"
            .to_string(),
        "$p = Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue".to_string(),
        // 60s 兜底：内层命令挂死时不能让 SSH exec 永远不返回
        "if ($p) { $null = $p.Handle; if (-not $p.WaitForExit(60000)) { exit 124 }; exit $p.ExitCode }".to_string(),
        "exit 0".to_string(),
    ]
    .join("; ");
    encode_powershell(&script)
}

// ---------------- 探测 ----------------

/// 一次往返拿齐宿主机信息，避免为 home / 架构 / 下载工具 / shell 各发一次命令。
/// 输出四行：home、`uname -sm`、可用的下载工具、登录 shell。
///
/// 在 Windows 远端上这条命令会失败（没有 uname），调用方据此改走 Windows 探测。
///
/// 登录 shell 必须探测，不能指望 Zellij 自己 fallback：SSH exec 是非登录、
/// 非交互的，环境里**没有 `$SHELL`**，而实测 Zellij 在 `$SHELL` 为空时 pane
/// 根本起不来（不是文档说的退到 /bin/sh），表现为会话建成了但屏幕全空。
pub const POSIX_PROBE_LINES: [&str; 4] = [
    r#"printf "%s\n" "$HOME""#,
    "uname -sm",
    "if command -v curl >/dev/null 2>&1; then echo curl; elif command -v wget >/dev/null 2>&1; then echo wget; else echo none; fi",
    r#"S="$SHELL"; [ -n "$S" ] || S=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7); [ -n "$S" ] || S=/bin/sh; printf "%s\n" "$S""#,
];

pub fn posix_probe() -> String {
    POSIX_PROBE_LINES.join("; ")
}

/// 下载工具。Windows 探测只会给出 curl / none
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Downloader {
    Curl,
    Wget,
    None,
}

impl Downloader {
    pub fn as_str(self) -> &'static str {
        match self {
            Downloader::Curl => "curl",
            Downloader::Wget => "wget",
            Downloader::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PosixProbe {
    pub home: String,
    pub uname: String,
    pub downloader: Downloader,
    pub shell: String,
}

pub fn parse_posix_probe(stdout: &str) -> Option<PosixProbe> {
    let lines: Vec<&str> = stdout.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let home = *lines.first()?;
    let uname = *lines.get(1)?;
    let downloader = match lines.get(2).copied() {
        Some("curl") => Downloader::Curl,
        Some("wget") => Downloader::Wget,
        _ => Downloader::None,
    };
    let shell = lines.get(3).copied().unwrap_or("/bin/sh");
    Some(PosixProbe { home: home.into(), uname: uname.into(), downloader, shell: shell.into() })
}

/// Windows 探测。tar.exe 自 Windows 10 1803 起内置（bsdtar，能解 zip），
/// curl.exe 同期内置；老系统上两者可能缺失，据此降级。
pub const WINDOWS_PROBE_SCRIPT: &str = "$env:USERPROFILE\n\
$env:PROCESSOR_ARCHITECTURE\n\
if (Get-Command curl.exe -EA SilentlyContinue) { 'curl' } else { 'none' }\n\
if (Get-Command tar.exe -EA SilentlyContinue) { 'tar' } else { 'none' }\n\
$s = (Get-Command powershell.exe -EA SilentlyContinue).Source; if ($s) { $s } else { $env:COMSPEC }";
// 最后一行用绝对路径而非裸名字：Zellij 在 Windows 上解析 shell 名有已知问题（#4964）

pub fn windows_probe() -> String {
    encode_powershell(&WINDOWS_PROBE_SCRIPT.replace('\n', "; "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsProbe {
    pub home: String,
    pub arch: String,
    pub downloader: Downloader,
    pub has_tar: bool,
    pub shell: String,
}

pub fn parse_windows_probe(stdout: &str) -> Option<WindowsProbe> {
    let lines: Vec<&str> = stdout.split('\n').map(str::trim).filter(|l| !l.is_empty()).collect();
    let home = *lines.first()?;
    let arch = *lines.get(1)?;
    Some(WindowsProbe {
        home: home.into(),
        arch: arch.into(),
        downloader: if lines.get(2) == Some(&"curl") { Downloader::Curl } else { Downloader::None },
        has_tar: lines.get(3) == Some(&"tar"),
        shell: lines.get(4).copied().unwrap_or("powershell.exe").into(),
    })
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
    fn detached_wraps_single_encoded_inner_command() {
        let cmd = build_detached_command_line(
            &["C:\\Users\\a b\\.falcon\\bin\\zellij.exe", "attach", "mj-x", "--create-background"],
            &[("ZELLIJ_SOCKET_DIR", "C:\\Users\\a b\\.falcon\\zellij\\sock")],
            None,
        );
        let outer = decode(&cmd);
        assert!(outer.contains("Invoke-CimMethod -ClassName Win32_Process -MethodName Create"));
        // 内层是 -Command 明文（不再二次 base64），外层单引号里出现该整段
        let re = regex::Regex::new(r"CommandLine = '(.+?)' \}").unwrap();
        let inner = re.captures(&outer).expect("inner CommandLine not found")[1].replace("''", "'");
        assert!(inner.starts_with("powershell -NoProfile -NonInteractive -Command \""));
        assert!(inner.contains("$env:ZELLIJ_SOCKET_DIR = 'C:\\Users\\a b\\.falcon\\zellij\\sock'"));
        assert!(inner.contains("& 'C:\\Users\\a b\\.falcon\\bin\\zellij.exe' 'attach' 'mj-x' '--create-background'"));
        // 内层不得再是 EncodedCommand——那正是撑爆命令行的双层编码
        assert!(!inner.contains("-EncodedCommand"));
    }

    #[test]
    fn detached_passes_cwd_as_current_directory() {
        let outer = decode(&build_detached_command_line(&["x.exe"], NO_ENV, Some("C:\\Users\\a b\\repo's")));
        // 不传 cwd 时新进程继承 WmiPrvSE 的 System32；上游丢 --default-cwd 的兜底
        assert!(outer.contains("CurrentDirectory = 'C:\\Users\\a b\\repo''s' }"));
        assert!(!decode(&build_detached_command_line(&["x.exe"], NO_ENV, None)).contains("CurrentDirectory"));
    }

    #[test]
    fn detached_stays_under_cmd_limit_with_full_attach_env() {
        // 复刻真实 attach_session 的最坏输入：长 shell 路径 + zellij + 终端深浅 env
        let bin = "C:\\Users\\fay\\.falcon\\bin\\zellij-0.44.3.exe";
        let root = "C:\\Users\\fay\\.falcon\\zellij";
        let data = format!("{root}\\data");
        let layout = format!("{root}\\layouts\\falcon.kdl");
        let argv = [
            bin, "--data-dir", &data, "attach", "mj-0123456789abcdef", "--create-background", "options",
            "--default-layout", &layout, "--default-mode", "locked", "--pane-frames", "false",
            "--simplified-ui", "true", "--session-serialization", "false", "--scroll-buffer-size", "2000",
            "--show-startup-tips", "false", "--show-release-notes", "false", "--default-cwd", "C:\\Users\\fay",
            "--default-shell", "C:\\WINDOWS\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        ];
        let sock = format!("{root}\\sock");
        let conf = format!("{root}\\config");
        let cache = format!("{root}\\cache");
        let env = [
            ("ZELLIJ_SOCKET_DIR", sock.as_str()),
            ("ZELLIJ_CONFIG_DIR", conf.as_str()),
            ("XDG_CACHE_HOME", cache.as_str()),
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "falcon"),
            ("COLORFGBG", "15;0"),
            ("GROK_APPEARANCE", "dark"),
            ("LC_GROK_APPEARANCE", "dark"),
        ];
        let cmd = build_detached_command_line(&argv, &env, None);
        assert!(cmd.len() < 8000, "command line too long: {}", cmd.len());
    }

    #[test]
    fn detached_propagates_failures_with_bounded_wait() {
        let outer = decode(&build_detached_command_line(&["x.exe"], NO_ENV, None));
        assert!(outer.contains("if ($r.ReturnValue -ne 0)"));
        assert!(outer.contains("WaitForExit(60000)"));
        assert!(outer.contains("exit $p.ExitCode"));
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
    fn layout_paths() {
        let l = host_layout(HostKind::Windows, "C:\\Users\\u\\.falcon\\", super::super::version::ZellijTarget::X86_64WindowsMsvc);
        assert_eq!(l.root, "C:\\Users\\u\\.falcon");
        assert!(l.bin.ends_with("\\bin\\zellij-0.45.1.exe"));
        assert_eq!(l.socket_dir, "C:\\Users\\u\\.falcon\\zellij\\sock");
        assert_eq!(l.layout_file, "C:\\Users\\u\\.falcon\\zellij\\layouts\\falcon.kdl");
        assert_eq!(l.layout_dir, "C:\\Users\\u\\.falcon\\zellij\\layouts");
    }

    #[test]
    fn probes_parse() {
        let p = parse_posix_probe("/home/u\nLinux x86_64\nwget\n/bin/bash\n").unwrap();
        assert_eq!((p.home.as_str(), p.downloader, p.shell.as_str()), ("/home/u", Downloader::Wget, "/bin/bash"));
        assert_eq!(parse_posix_probe("/home/u\nLinux x86_64\n").unwrap().shell, "/bin/sh");
        assert_eq!(parse_posix_probe("/home/u\n"), None);
        let w = parse_windows_probe("C:\\Users\\u\r\nAMD64\r\ncurl\r\ntar\r\nC:\\pwsh.exe\r\n").unwrap();
        assert!(w.has_tar && w.downloader == Downloader::Curl && w.shell == "C:\\pwsh.exe");
        assert!(decode(&windows_probe()).contains("$env:USERPROFILE; $env:PROCESSOR_ARCHITECTURE; "));
    }
}
