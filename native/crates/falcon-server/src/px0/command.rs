//! 移植自 `packages/server/src/px0/command.ts`（含 `command.test.ts` 的全部用例；用 /bin/sh
//! 实跑命令行的三条放在 `#[cfg(unix)]` 下）。
//!
//! px0（ADR 0017）的资产映射、argv 与输出解析。
//!
//! 纯函数，零 I/O。本机直接 spawn argv 数组；远端只支持 POSIX，命令行在这里用
//! quote_posix 拼好交给 SshLink（Windows 宿主机 v1 不做，见 ADR）。
//!
//! 脾气（v0.1.16 实测 / 源码对过）：
//! - **没有任何鉴权**。会改东西的 POST（派 agent 编辑、commit / push、写会话、PR 评论、
//!   装语言服务器）过它的 localPost：Host 必须是 IP 或 localhost、Origin 的 host 必须
//!   等于 Host；读文件 / 搜索 / diff 这些 GET 什么都不查。只能听 127.0.0.1，前面必须
//!   挡着 falcon 的登录（反代怎么对付 localPost 见 proxy.rs）。
//! - `-port 0` 让系统挑端口；端口只能从 stdout 的 `url: http://127.0.0.1:PORT/...`
//!   那一行读。`-quiet` 会连这一行一起吞掉，不能加。
//! - 遥测默认开（PostHog），`-no-telemetry` 关。
//! - 0.1.11 起**默认启动时自我更新**：下新版换掉自己的二进制再原地重新 exec，
//!   `-no-update` 关。必须关：字节是钉死 sha256 的（换掉就绕过了校验，远端下次
//!   `-version` 对不上还会被我们重推）、进程是挂在 pty 上收尸的、端口是从首次启动的
//!   stdout 里读的——重启一次这三件事都会落空。
//! - 不带 `-no-open` 会去开宿主机上的浏览器。
//! - `-base-path` 之下的一切（静态资源、`/api/*`、SSE 的 `/api/stream`）都挂在这个前缀里，
//!   反代原样转发路径即可，不用改写。
//! - `-version` 打印 `px0 0.1.16 (linux/amd64)`。
//!
//! # 移植约定
//!
//! - TS 的默认参数（`platform = process.platform`、`version = PX0_VERSION`、`n = 6`……）在
//!   Rust 里是 `Option`，`None` 即默认值。平台 / 架构沿用 Node 的叫法（`darwin` / `linux` /
//!   `win32`，`arm64` / `x64`），缺省时由本进程的编译目标换算。
//! - 正则里 JS 的 `\s` / `\d` 写成显式字符类（Rust 的是 Unicode 语义），见文件末的 `js`。
//! - `-base-path` 的值由 [`falcon_core::px0::px0_base_path`] 给（S6 的 manager 调）。
//!
//! 不在纯函数层的：`bin.ts`（下载与校验）→ [`super::bin`]，`manager.ts`（实例、空闲回收、
//! pty 收尸）→ [`super::manager`]；`routes.ts` 的 `registerPx0Routes`、`forward` 随 api 层的
//! 路由一起移植。`js::trim` 开到 crate 内，manager 处理工作目录 / shell 时用。

use std::sync::LazyLock;

use falcon_proto::HostKind;
use regex::Regex;

use crate::git::path::join_path;
use crate::zellij::host::quote_posix;

pub const PX0_VERSION: &str = "0.1.16";

pub const DEFAULT_BASE_URL: &str = "https://github.com/px0-ai/px0/releases/download";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Px0Os {
    Linux,
    Darwin,
    Windows,
}

impl Px0Os {
    pub fn as_str(self) -> &'static str {
        match self {
            Px0Os::Linux => "linux",
            Px0Os::Darwin => "darwin",
            Px0Os::Windows => "windows",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Px0Arch {
    Amd64,
    Arm64,
}

impl Px0Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Px0Arch::Amd64 => "amd64",
            Px0Arch::Arm64 => "arm64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Px0Target {
    pub os: Px0Os,
    pub arch: Px0Arch,
}

/// 锁定版本各资产的 sha256，取自该 release 的 checksums.txt。
///
/// 写死在源码里而不是运行时去拉 checksums.txt：px0 的服务端没有鉴权、能直接驱动
/// agent 改代码，值得钉死字节，而不是信 release 页面上当下挂着的东西。
/// 升级 PX0_VERSION 时一起换。只列我们支持的六个平台。查表用 [`px0_sha256`]。
pub const PX0_SHA256: [(&str, &str); 6] = [
    ("px0-0.1.16-darwin-amd64", "de1b4e99f240f73c87aaf60b3c87209ca6135b1b493c758f7bc38c7c17713ab3"),
    ("px0-0.1.16-darwin-arm64", "0c40d3af1f0634cd67ee1a2231abc1a20919c0b595f46db252aea0225edcf0fc"),
    ("px0-0.1.16-linux-amd64", "7d9051f6358a820ea165b9459ff84c1fdca0a62d5394077827d082724e48b015"),
    ("px0-0.1.16-linux-arm64", "c62c9c2a7abf21f2ace2731c091a5f51b6d416c333116cde3e7d3888bbf78f09"),
    ("px0-0.1.16-windows-amd64.exe", "b5e5258771607caf40c484a05f2a89ca609b548c9b52c4454ea4753c31597b5c"),
    ("px0-0.1.16-windows-arm64.exe", "52497563e3e67b57a69da9cdc536e06f4faf1d84ec86bbaf6c3229c8e3e1cc6c"),
];

/// TS 的 `PX0_SHA256[asset]`
pub fn px0_sha256(asset: &str) -> Option<&'static str> {
    PX0_SHA256.iter().find(|(name, _)| *name == asset).map(|(_, hash)| *hash)
}

fn arch_of(machine: &str) -> Option<Px0Arch> {
    match machine {
        "x86_64" | "amd64" | "x64" => Some(Px0Arch::Amd64),
        "aarch64" | "arm64" => Some(Px0Arch::Arm64),
        _ => None,
    }
}

/// 远端 `uname -sm` → 资产平台。认不出返回 None，调用方报「没有这个平台的构建」
pub fn px0_target_from_uname(uname: &str) -> Option<Px0Target> {
    // uname.trim().split(/\s+/)
    let mut parts = js::trim(uname).split(js::is_ws).filter(|s| !s.is_empty());
    let (os, machine) = (parts.next()?, parts.next()?);
    let arch = arch_of(machine)?;
    match os {
        "Linux" => Some(Px0Target { os: Px0Os::Linux, arch }),
        "Darwin" => Some(Px0Target { os: Px0Os::Darwin, arch }),
        _ => None,
    }
}

/// 后端本机的资产平台。`platform` / `arch` 是 Node 的叫法，缺省为本进程
pub fn local_px0_target(platform: Option<&str>, arch: Option<&str>) -> Option<Px0Target> {
    let a = arch_of(arch.unwrap_or(node::arch()))?;
    match platform.unwrap_or(node::platform()) {
        "linux" => Some(Px0Target { os: Px0Os::Linux, arch: a }),
        "darwin" => Some(Px0Target { os: Px0Os::Darwin, arch: a }),
        "win32" => Some(Px0Target { os: Px0Os::Windows, arch: a }),
        _ => None,
    }
}

/// GitHub release 资产名，如 px0-0.1.16-linux-amd64 / px0-0.1.16-windows-amd64.exe。
/// `version` 缺省为 [`PX0_VERSION`]
pub fn px0_asset_name(target: Px0Target, version: Option<&str>) -> String {
    format!(
        "px0-{}-{}-{}{}",
        version.unwrap_or(PX0_VERSION),
        target.os.as_str(),
        target.arch.as_str(),
        if target.os == Px0Os::Windows { ".exe" } else { "" }
    )
}

/// `version` 缺省为 [`PX0_VERSION`]，`base_url` 缺省为 [`DEFAULT_BASE_URL`]
pub fn px0_download_url(asset: &str, version: Option<&str>, base_url: Option<&str>) -> String {
    let base = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
    format!("{base}/v{}/{asset}", version.unwrap_or(PX0_VERSION))
}

/// `px0 -version` → 0.1.16。对不上就当没装好，重推一份
pub fn parse_px0_version(text: &str) -> Option<String> {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!(r"px0{}+v?([0-9]+\.[0-9]+\.[0-9]+)", js::WS)).unwrap());
    RE.captures(text).map(|m| m[1].to_string())
}

static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());

/// 从 px0 的启动输出里找监听端口。输出可能带颜色、可能是 pty 的 \r\n，
/// 也可能在 url 行之前夹着登录 shell 的 motd——只认 `url:` 后面那个回环地址。
pub fn parse_listen_port(text: &str) -> Option<u16> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"url:{}*http://(?:127\.0\.0\.1|localhost|\[::1\]):([0-9]{{1,5}})/", js::WS)).unwrap()
    });
    let plain = ANSI_RE.replace_all(text, "");
    let port: u32 = RE.captures(&plain)?[1].parse().ok()?;
    (port > 0 && port < 65536).then_some(port as u16)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Px0ArgsInput {
    /// `/px0/<项目 id>/`，见 falcon_core::px0::px0_base_path
    pub base_path: String,
    /// 工作目录（宿主机上的绝对路径）
    pub dir: String,
}

pub fn px0_args(input: &Px0ArgsInput) -> Vec<String> {
    vec![
        "-host".into(),
        "127.0.0.1".into(),
        "-port".into(),
        "0".into(),
        "-no-open".into(),
        "-no-telemetry".into(),
        // 不许它自我更新：钉死的字节、pty 收尸、从 stdout 读端口都靠进程不被换掉（见文件头）
        "-no-update".into(),
        "-no-color".into(),
        "-base-path".into(),
        input.base_path.clone(),
        input.dir.clone(),
    ]
}

/// 远端的 px0 放在 <falcon 根>/bin，带版本号，与 Zellij 同一个目录。`version` 缺省为
/// [`PX0_VERSION`]
pub fn remote_px0_path(root: &str, version: Option<&str>) -> String {
    let name = format!("px0-{}", version.unwrap_or(PX0_VERSION));
    join_path(HostKind::Posix, &[root, "bin", &name])
}

/// 远端的工作目录允许写成 `~/x`：px0 收到的是引号里的字面量，不会替我们展开
pub fn expand_home(dir: &str, home: &str) -> String {
    if dir == "~" {
        return home.to_string();
    }
    if let Some(rest) = dir.strip_prefix("~/") {
        return join_path(HostKind::Posix, &[home, rest]);
    }
    dir.to_string()
}

/// 远端 px0 的版本。文件不在时 shell 报 not found，解析不出版本，调用方据此重推
pub fn posix_version_command(bin: &str) -> String {
    format!("{} -version 2>/dev/null", quote_posix(bin))
}

/// 从 stdin 收二进制写到远端。先写 .partial 再改名：推到一半断了不会留下一个
/// 截断的可执行文件，下次 -version 对不上会重推。
pub fn posix_install_command(file: &str) -> String {
    // file.replace(/\/[^/]*$/, "") || "/"：去掉最后一个 / 及其后的部分
    let dir = match file.rfind('/') {
        Some(i) => &file[..i],
        None => file,
    };
    let dir = if dir.is_empty() { "/" } else { dir };
    format!(
        "d={}; f={}; mkdir -p \"$d\" && cat > \"$f.partial\" && chmod 755 \"$f.partial\" && mv -f \"$f.partial\" \"$f\"",
        quote_posix(dir),
        quote_posix(file)
    )
}

/// 远端启动命令。要配合带 pty 的 exec 通道用（SshLink.execStream 的 pty 选项）：
///
/// - 外层 `exec <登录 shell> -i -l -c`：理由同 agent 启动脚本（ADR 0013）——px0 派编辑
///   给 claude / codex，查 PR 要 gh / GITHUB_TOKEN，这些多半只在交互登录 shell 的
///   rc 文件里才进 PATH / 环境。三个 flag 分开写。
/// - 内层再 `exec` 一次，px0 顶替 shell 成为 pty 的会话首进程：通道一关它就收到
///   SIGHUP 退出，不会在远端留孤儿。
pub fn posix_launch_command<S: AsRef<str>>(shell: &str, bin: &str, args: &[S]) -> String {
    let quoted: Vec<String> = std::iter::once(bin).chain(args.iter().map(AsRef::as_ref)).map(quote_posix).collect();
    let inner = format!("exec {}", quoted.join(" "));
    format!("exec {} -i -l -c {}", quote_posix(shell), quote_posix(&inner))
}

/// 起不来时给用户看的最后几行：剥颜色、剥空行、截长度。`n` 缺省 6，`max` 缺省 600
/// （UTF-16 码元）
pub fn tail_lines(text: &str, n: Option<usize>, max: Option<usize>) -> String {
    let n = n.unwrap_or(6);
    let max = max.unwrap_or(600);
    let plain = ANSI_RE.replace_all(text, "");
    // split(/\r?\n/).map(trim).filter(Boolean)
    let lines: Vec<&str> =
        plain.split('\n').map(|l| js::trim(l.strip_suffix('\r').unwrap_or(l))).filter(|l| !l.is_empty()).collect();
    // lines.slice(-n)：n 为 0 时是 slice(-0) = slice(0)，整个数组
    let start = if n == 0 { 0 } else { lines.len().saturating_sub(n) };
    let tail = lines[start..].join("\n");
    js::utf16_tail(&tail, max).to_string()
}

/// Node 的 `process.platform` / `process.arch` 叫法
mod node {
    pub(super) fn platform() -> &'static str {
        match std::env::consts::OS {
            "macos" => "darwin",
            "windows" => "win32",
            other => other,
        }
    }

    pub(super) fn arch() -> &'static str {
        match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            "x86" => "ia32",
            other => other,
        }
    }
}

/// 照抄 TS 语义要用到的几处 JavaScript 行为（与 meegle/command.rs 的 `js` 同源；
/// falcon-server 还没有公共的落脚点，先各带一份）。
pub(crate) mod js {
    /// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套
    pub(super) fn is_ws(c: char) -> bool {
        matches!(
            c,
            '\u{9}'..='\u{d}'
                | ' '
                | '\u{a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    }

    pub(crate) fn trim(s: &str) -> &str {
        s.trim_matches(is_ws)
    }

    /// 正则里的 `\s`
    pub(super) const WS: &str =
        r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

    /// `s.length <= max ? s : s.slice(s.length - max)`，长度按 UTF-16 码元算。
    ///
    /// 截断点落在代理对中间时 JS 会留下半个代理，Rust 的 `str` 表示不了，这里整个字符不要。
    pub(super) fn utf16_tail(s: &str, max: usize) -> &str {
        let len: usize = s.chars().map(char::len_utf16).sum();
        if len <= max {
            return s;
        }
        let skip = len - max;
        let mut units = 0;
        for (i, c) in s.char_indices() {
            if units >= skip {
                return &s[i..];
            }
            units += c.len_utf16();
        }
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// px0TargetFromUname · maps Linux / Darwin on x86_64 and arm64
    #[test]
    fn target_from_uname_maps_linux_and_darwin() {
        let t = |os, arch| Some(Px0Target { os, arch });
        assert_eq!(px0_target_from_uname("Linux x86_64"), t(Px0Os::Linux, Px0Arch::Amd64));
        assert_eq!(px0_target_from_uname("Linux aarch64\n"), t(Px0Os::Linux, Px0Arch::Arm64));
        assert_eq!(px0_target_from_uname("Darwin arm64"), t(Px0Os::Darwin, Px0Arch::Arm64));
        assert_eq!(px0_target_from_uname("Darwin x86_64"), t(Px0Os::Darwin, Px0Arch::Amd64));
    }

    /// px0TargetFromUname · returns null for platforms we do not ship
    #[test]
    fn target_from_uname_rejects_unshipped() {
        assert_eq!(px0_target_from_uname("FreeBSD amd64"), None);
        assert_eq!(px0_target_from_uname("Linux armv7l"), None);
        assert_eq!(px0_target_from_uname(""), None);
    }

    /// localPx0Target · maps node platform / arch
    #[test]
    fn local_target_maps_node_names() {
        let t = |os, arch| Some(Px0Target { os, arch });
        assert_eq!(local_px0_target(Some("darwin"), Some("arm64")), t(Px0Os::Darwin, Px0Arch::Arm64));
        assert_eq!(local_px0_target(Some("linux"), Some("x64")), t(Px0Os::Linux, Px0Arch::Amd64));
        assert_eq!(local_px0_target(Some("win32"), Some("x64")), t(Px0Os::Windows, Px0Arch::Amd64));
        assert_eq!(local_px0_target(Some("linux"), Some("ia32")), None);
        assert_eq!(local_px0_target(Some("freebsd"), Some("x64")), None);
    }

    /// px0AssetName / PX0_SHA256 · names assets like the GitHub release
    #[test]
    fn asset_name_like_github_release() {
        assert_eq!(
            px0_asset_name(Px0Target { os: Px0Os::Linux, arch: Px0Arch::Amd64 }, None),
            format!("px0-{PX0_VERSION}-linux-amd64")
        );
        assert_eq!(
            px0_asset_name(Px0Target { os: Px0Os::Windows, arch: Px0Arch::Arm64 }, None),
            format!("px0-{PX0_VERSION}-windows-arm64.exe")
        );
    }

    /// px0AssetName / PX0_SHA256 · pins a sha256 for every supported target of the locked version
    #[test]
    fn sha256_pinned_for_every_target() {
        for os in [Px0Os::Linux, Px0Os::Darwin, Px0Os::Windows] {
            for arch in [Px0Arch::Amd64, Px0Arch::Arm64] {
                let hash = px0_sha256(&px0_asset_name(Px0Target { os, arch }, None)).unwrap_or("");
                assert!(
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "{os:?}/{arch:?}"
                );
            }
        }
    }

    /// px0AssetName / PX0_SHA256 · downloads the locked tag, not latest
    #[test]
    fn download_url_uses_locked_tag() {
        assert_eq!(
            px0_download_url("px0-0.1.10-linux-amd64", Some("0.1.10"), None),
            "https://github.com/px0-ai/px0/releases/download/v0.1.10/px0-0.1.10-linux-amd64"
        );
    }

    /// parsePx0Version · reads `px0 -version`
    #[test]
    fn parse_version_reads_banner() {
        assert_eq!(parse_px0_version("px0 0.1.10 (linux/amd64)\n").as_deref(), Some("0.1.10"));
        assert_eq!(parse_px0_version("sh: 1: /x/px0-0.1.10: not found"), None);
        assert_eq!(parse_px0_version(""), None);
    }

    /// parseListenPort · reads the url line of px0's banner
    #[test]
    fn listen_port_from_banner() {
        let banner = [
            "",
            "px0 0.1.16",
            "  workspace:  /Users/fay/Code/mojito",
            "  url:        http://127.0.0.1:55342/px0/abc/",
            "",
            "ctrl-c to stop",
        ]
        .join("\n");
        assert_eq!(parse_listen_port(&banner), Some(55342));
    }

    /// parseListenPort · copes with pty CRLF, colors and a motd before the banner
    #[test]
    fn listen_port_copes_with_crlf_colors_motd() {
        let text = "Welcome to Ubuntu 24.04\r\nLast login: today\r\n\
                    \x20 url:        \x1b[36mhttp://127.0.0.1:41234/px0/p1/\x1b[0m\r\n";
        assert_eq!(parse_listen_port(text), Some(41234));
    }

    /// parseListenPort · ignores unrelated URLs and an incomplete line
    #[test]
    fn listen_port_ignores_unrelated() {
        assert_eq!(parse_listen_port("see https://px0.ai/docs"), None);
        assert_eq!(parse_listen_port("  url:        http://127.0.0.1:41"), None);
        assert_eq!(parse_listen_port("url: http://0.0.0.0:7777/"), None);
    }

    /// px0Args · binds loopback on a free port, no browser, no telemetry, no self-update
    #[test]
    fn args_bind_loopback_no_open_no_telemetry_no_update() {
        let args = px0_args(&Px0ArgsInput { base_path: "/px0/p1/".into(), dir: "/srv/app".into() });
        let flag = |name: &str| args[args.iter().position(|a| a == name).unwrap() + 1].as_str();
        let has = |name: &str| args.iter().any(|a| a == name);
        assert_eq!(flag("-host"), "127.0.0.1");
        assert_eq!(flag("-port"), "0");
        assert_eq!(flag("-base-path"), "/px0/p1/");
        assert!(has("-no-open"));
        assert!(has("-no-telemetry"));
        // 0.1.11 起默认启动时自我更新、原地重新 exec，换掉钉死哈希的二进制
        assert!(has("-no-update"));
        // -quiet 会把 url 行也吞掉，端口就解析不出来了
        assert!(!has("-quiet"));
        assert_eq!(args.last().map(String::as_str), Some("/srv/app"));
    }

    /// remote paths · puts the binary next to zellij under <root>/bin with the version
    #[test]
    fn remote_path_under_root_bin() {
        assert_eq!(remote_px0_path("/home/u/.falcon/", None), format!("/home/u/.falcon/bin/px0-{PX0_VERSION}"));
    }

    /// remote paths · expands ~ in the working dir
    #[test]
    fn expand_home_in_working_dir() {
        assert_eq!(expand_home("~", "/home/u"), "/home/u");
        assert_eq!(expand_home("~/code/app", "/home/u"), "/home/u/code/app");
        assert_eq!(expand_home("/srv/~x", "/home/u"), "/srv/~x");
    }

    /// 在本机 sh 上真跑一遍拼出来的命令行，验引号与退出码
    #[cfg(unix)]
    mod posix_commands {
        use std::io::Write as _;
        use std::process::{Command, Output, Stdio};

        use super::*;

        fn sh(cmd: &str, input: &[u8]) -> Output {
            let mut child = Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn /bin/sh");
            child.stdin.take().unwrap().write_all(input).unwrap();
            child.wait_with_output().unwrap()
        }

        fn stdout(out: &Output) -> String {
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        /// posix commands · install writes stdin atomically and makes it executable
        #[test]
        fn install_writes_stdin_atomically_and_executable() {
            let tmp = tempfile::Builder::new().prefix("falcon px0 '").tempdir().unwrap();
            let file = tmp.path().join("bin").join("px0-x");
            let file = file.to_str().unwrap();
            let out = sh(&posix_install_command(file), b"#!/bin/sh\necho 'px0 9.9.9 (test)'\n");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            assert!(!std::path::Path::new(&format!("{file}.partial")).exists());
            let out = sh(&posix_version_command(file), b"");
            assert!(out.status.success());
            assert_eq!(parse_px0_version(&stdout(&out)).as_deref(), Some("9.9.9"));
        }

        /// posix commands · version probe of a missing binary yields no version (and no throw on stdout)
        #[test]
        fn version_probe_of_missing_binary_yields_none() {
            let out = sh(&posix_version_command("/nonexistent/px0 it's"), b"");
            assert_eq!(parse_px0_version(&stdout(&out)), None);
        }

        /// posix commands · launch runs the binary with args intact through a login shell
        #[test]
        fn launch_keeps_args_intact_through_login_shell() {
            let out = sh(&posix_launch_command("/bin/sh", "/usr/bin/printf", &["%s|", "a b", "it's", "$HOME"]), b"");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(stdout(&out), "a b|it's|$HOME|");
        }
    }

    /// tailLines · keeps the last lines without colors or blanks
    #[test]
    fn tail_lines_drop_colors_and_blanks() {
        assert_eq!(tail_lines("a\r\n\r\n\x1b[31mb\x1b[0m\nc\n", Some(2), None), "b\nc");
    }

    // ---- 以下是 Rust 侧补的 ----

    #[test]
    fn install_command_dir_edges() {
        // 没有 / 时原样当目录（与 TS 的 replace 一致），根下的文件目录是 /
        assert!(posix_install_command("/px0").starts_with("d='/'; f='/px0';"));
        assert!(posix_install_command("px0").starts_with("d='px0'; f='px0';"));
    }

    #[test]
    fn uname_split_uses_js_whitespace() {
        assert_eq!(
            px0_target_from_uname("\u{feff}Linux\t x86_64 extra"),
            Some(Px0Target { os: Px0Os::Linux, arch: Px0Arch::Amd64 })
        );
        assert_eq!(px0_target_from_uname("Linux"), None);
    }

    #[test]
    fn listen_port_range() {
        assert_eq!(parse_listen_port("url: http://localhost:0/"), None);
        assert_eq!(parse_listen_port("url: http://[::1]:65535/"), Some(65535));
        assert_eq!(parse_listen_port("url: http://localhost:65536/"), None);
    }
}
