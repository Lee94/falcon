//! 移植自 `packages/server/src/cloudflared/command.ts`（含 `command.test.ts` 的全部用例）。
//!
//! cloudflared Quick Tunnel 的命令构造与输出解析。
//!
//! 纯函数，零 I/O。CLI 只在 falcon 后端本机 spawn（远端 HTTP 服务先经 SSH
//! 本地转发接到本机），直接 spawn 不经 shell，argv 数组没有转义问题。
//!
//! 脾气（2026.10.0 实测 / 2026.9.1 文档与源码对过）：
//! - Quick Tunnel 是 `cloudflared tunnel --url http://127.0.0.1:PORT`，不需要
//!   Cloudflare 账号。得到的是随机 `*.trycloudflare.com`，进程一退出 URL 作废。
//! - 公网 URL 打在 stderr 的 ASCII 框里；metrics 端口上还有 `/quicktunnel`
//!   `{"hostname":"…"}`（hostname 可能不带 https://）。两条路都认。
//! - `--no-autoupdate`：我们锁定版本，不要让它自己把 ~/.falcon/bin 里的二进制换掉。
//! - `--http-host-header`：vite / webpack 一类 dev server 默认只认 localhost，
//!   公网 Host 是 trycloudflare.com 时会 403。回环目标一律改写成 localhost。
//!
//! # 移植约定
//!
//! - TS 的默认参数（`platform = process.platform`、`version = CLOUDFLARED_VERSION`、
//!   `n = 4`……）在 Rust 里是 `Option`，`None` 即默认值。平台 / 架构沿用 Node 的叫法
//!   （`darwin` / `linux` / `win32`，`arm64` / `x64`），缺省时由本进程的编译目标换算。
//! - 正则：不带 `u` 标志的 `/…/i` 只把 ASCII 字母当成大小写不敏感，`\s` 是 JS 的空白集，
//!   `\d` 只是 ASCII 数字；Rust 的 `(?i)` / `\s` / `\d` 都是 Unicode 语义，所以写成显式字符类
//!   （见文件末的 `js`）。
//!
//! 留到 S6 的函数（下载与进程管理）：`bin.ts` 的 `bundledBinName`、`installPath`、
//! `CloudflaredBinError`、`ensureCloudflared`、`ensureNow`、`downloadLocked`、
//! `findExtractedBinary`、`fetchToFile`、`versionOf`、`whichCloudflared`（连同
//! `bin.test.ts`）；进程生命周期在 `sessions/share.ts`。

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

pub const CLOUDFLARED_VERSION: &str = "2026.10.0";

pub const DEFAULT_BASE_URL: &str = "https://github.com/cloudflare/cloudflared/releases/download";

/// 日志里那条公网地址。只认 trycloudflare.com，避免把别的 https 链接当隧道。
///
/// TS 的 `/https:\/\/[a-z0-9-]+\.trycloudflare\.com/i`
pub static QUICK_TUNNEL_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("{}[a-zA-Z0-9-]+{}", js::ci("https://"), js::ci(r"\.trycloudflare\.com"))).unwrap()
});

/// `/Starting metrics server on (127\.0\.0\.1:\d+)/i`
static METRICS_ADDR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{}[0-9]+)", js::ci(r"Starting metrics server on (127\.0\.0\.1:"))).unwrap());

/// darwin 是 tgz，linux / windows 是裸二进制
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudflaredAssetKind {
    Binary,
    Tgz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudflaredAsset {
    /// GitHub release 资产名
    pub name: &'static str,
    pub kind: CloudflaredAssetKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelArgs {
    /// cloudflared 要打的本机 origin，已是 `http://host:port`
    pub origin_url: String,
    /// 发给 origin 的 Host。回环时是 localhost:port
    pub http_host_header: String,
    /// metrics 监听，形如 127.0.0.1:41234
    pub metrics: String,
}

/// 本平台对应的官方资产。没有构建返回 None，调用方降级为「架构不支持」。
/// 命名跟 GitHub release 走：darwin/linux 的 x64 叫 amd64，不是 x64。
///
/// `platform` / `arch` 是 Node 的叫法，缺省为本进程（`process.platform` / `process.arch`）。
pub fn cloudflared_asset(platform: Option<&str>, arch: Option<&str>) -> Option<CloudflaredAsset> {
    use CloudflaredAssetKind::{Binary, Tgz};
    let platform = platform.unwrap_or(node::platform());
    let arch = arch.unwrap_or(node::arch());
    let (name, kind) = match (platform, arch) {
        ("darwin", "arm64") => ("cloudflared-darwin-arm64.tgz", Tgz),
        ("darwin", "x64") => ("cloudflared-darwin-amd64.tgz", Tgz),
        ("linux", "arm64") => ("cloudflared-linux-arm64", Binary),
        ("linux", "x64") => ("cloudflared-linux-amd64", Binary),
        ("win32", "x64") => ("cloudflared-windows-amd64.exe", Binary),
        _ => return None,
    };
    Some(CloudflaredAsset { name, kind })
}

/// `version` 缺省为 [`CLOUDFLARED_VERSION`]，`base_url` 缺省为 [`DEFAULT_BASE_URL`]
pub fn download_url(asset: &CloudflaredAsset, version: Option<&str>, base_url: Option<&str>) -> String {
    let base = base_url.unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/');
    format!("{base}/{}/{}", version.unwrap_or(CLOUDFLARED_VERSION), asset.name)
}

/// `cloudflared --version` → 2026.10.0。对不上就当没装好，触发重下。
pub fn parse_cloudflared_version(text: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(&format!(r"{}{}+([0-9]+\.[0-9]+\.[0-9]+)", js::ci("cloudflared version"), js::WS)).unwrap()
    });
    RE.captures(text).map(|m| m[1].to_string())
}

pub fn origin_url(host: &str, port: u16) -> String {
    if host.contains(':') { format!("http://[{host}]:{port}") } else { format!("http://{host}:{port}") }
}

/// 回环写成 localhost：vite 的 allowedHosts 默认含 localhost、不含 127.0.0.1
/// 当 Host，更不含 trycloudflare.com。
pub fn http_host_header(host: &str, port: u16) -> String {
    let loopback = host == "127.0.0.1" || host == "localhost" || host == "::1";
    if loopback {
        format!("localhost:{port}")
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

pub fn tunnel_args(input: &TunnelArgs) -> Vec<String> {
    vec![
        "tunnel".into(),
        "--no-autoupdate".into(),
        "--url".into(),
        input.origin_url.clone(),
        "--http-host-header".into(),
        input.http_host_header.clone(),
        "--metrics".into(),
        input.metrics.clone(),
    ]
}

pub fn extract_quick_tunnel_url(text: &str) -> Option<String> {
    let m = QUICK_TUNNEL_URL_RE.find(text)?;
    Some(m.as_str().trim_end_matches('/').to_string())
}

pub fn extract_metrics_addr(text: &str) -> Option<String> {
    METRICS_ADDR_RE.captures(text).map(|m| m[1].to_string())
}

/// `/quicktunnel` 的 JSON。hostname 有时是裸域名，有时已经带 https://。
pub fn parse_quick_tunnel_metrics(body: &str) -> Option<String> {
    static HOST_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(&format!("^[a-zA-Z0-9-]+{}$", js::ci(r"\.trycloudflare\.com"))).unwrap());
    // JSON.parse 失败、或解出来不是带字符串 hostname 的对象，都是 null
    let json: Value = serde_json::from_str(body).ok()?;
    let Some(Value::String(hostname)) = json.get("hostname") else { return None };
    let trimmed = js::trim(hostname);
    if trimmed.is_empty() {
        return None;
    }
    // .replace(/^https:\/\//i, "").replace(/\/+$/, "")
    let raw = match trimmed.get(..8) {
        Some(p) if p.eq_ignore_ascii_case("https://") => &trimmed[8..],
        _ => trimmed,
    };
    let raw = raw.trim_end_matches('/');
    if !HOST_RE.is_match(raw) {
        return None;
    }
    Some(format!("https://{raw}"))
}

/// 进程退出时给用户看的最后几行，剥空行、截长度。`n` 缺省 4，`max` 缺省 280（UTF-16 码元）。
pub fn last_log_lines(text: &str, n: Option<usize>, max: Option<usize>) -> String {
    let n = n.unwrap_or(4);
    let max = max.unwrap_or(280);
    // split(/\r?\n/).map(trim).filter(Boolean)
    let lines: Vec<&str> =
        text.split('\n').map(|l| js::trim(l.strip_suffix('\r').unwrap_or(l))).filter(|l| !l.is_empty()).collect();
    // lines.slice(-n)：n 为 0 时是 slice(-0) = slice(0)，整个数组
    let start = if n == 0 { 0 } else { lines.len().saturating_sub(n) };
    let tail = lines[start..].join(" · ");
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
mod js {
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

    pub(super) fn trim(s: &str) -> &str {
        s.trim_matches(is_ws)
    }

    /// 正则里的 `\s`
    pub(super) const WS: &str =
        r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

    /// 不带 `u` 标志的 `/…/i` 的源码 → Rust 正则：ASCII 字母展开成 `[xX]`，`\` 转义原样保留。
    /// 只用在没有 `.`、字符类与 `\s` 一类转义的片段上。
    pub(super) fn ci(pattern: &str) -> String {
        let mut out = String::with_capacity(pattern.len() * 4);
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    out.push(c);
                    out.extend(chars.next());
                }
                c if c.is_ascii_alphabetic() => {
                    out.push('[');
                    out.push(c.to_ascii_lowercase());
                    out.push(c.to_ascii_uppercase());
                    out.push(']');
                }
                c => {
                    debug_assert!(c != '[' && c != '.', "ci() 不支持字符类与 .");
                    out.push(c);
                }
            }
        }
        out
    }

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

    /// cloudflaredAsset · maps darwin/linux x64 to amd64 asset names
    #[test]
    fn asset_maps_x64_to_amd64() {
        use CloudflaredAssetKind::{Binary, Tgz};
        assert_eq!(
            cloudflared_asset(Some("darwin"), Some("arm64")),
            Some(CloudflaredAsset { name: "cloudflared-darwin-arm64.tgz", kind: Tgz })
        );
        assert_eq!(
            cloudflared_asset(Some("darwin"), Some("x64")),
            Some(CloudflaredAsset { name: "cloudflared-darwin-amd64.tgz", kind: Tgz })
        );
        assert_eq!(
            cloudflared_asset(Some("linux"), Some("x64")),
            Some(CloudflaredAsset { name: "cloudflared-linux-amd64", kind: Binary })
        );
        assert_eq!(
            cloudflared_asset(Some("linux"), Some("arm64")),
            Some(CloudflaredAsset { name: "cloudflared-linux-arm64", kind: Binary })
        );
        assert_eq!(cloudflared_asset(Some("linux"), Some("ia32")), None);
    }

    /// downloadUrl · pins the locked version and does not follow latest
    #[test]
    fn download_url_pins_locked_version() {
        let asset = cloudflared_asset(Some("darwin"), Some("arm64")).unwrap();
        assert_eq!(
            download_url(&asset, None, None),
            format!(
                "https://github.com/cloudflare/cloudflared/releases/download/{CLOUDFLARED_VERSION}/cloudflared-darwin-arm64.tgz"
            )
        );
    }

    /// parseCloudflaredVersion · reads the semver out of --version output
    #[test]
    fn parse_version_reads_semver() {
        assert_eq!(
            parse_cloudflared_version("cloudflared version 2026.10.0 (built 2026-10-05-17:37 UTC)").as_deref(),
            Some("2026.10.0")
        );
        assert_eq!(parse_cloudflared_version("not a version"), None);
    }

    /// originUrl / httpHostHeader · brackets IPv6 in the origin URL
    #[test]
    fn origin_url_brackets_ipv6() {
        assert_eq!(origin_url("::1", 80), "http://[::1]:80");
        assert_eq!(origin_url("127.0.0.1", 5173), "http://127.0.0.1:5173");
    }

    /// originUrl / httpHostHeader · rewrites loopback Host to localhost so vite allowedHosts accepts it
    #[test]
    fn http_host_header_rewrites_loopback() {
        assert_eq!(http_host_header("127.0.0.1", 5173), "localhost:5173");
        assert_eq!(http_host_header("::1", 80), "localhost:80");
        assert_eq!(http_host_header("localhost", 3000), "localhost:3000");
        assert_eq!(http_host_header("api.internal", 8080), "api.internal:8080");
    }

    /// tunnelArgs · emits argv, never a shell string, and disables autoupdate
    #[test]
    fn tunnel_args_emit_argv_and_disable_autoupdate() {
        assert_eq!(
            tunnel_args(&TunnelArgs {
                origin_url: "http://127.0.0.1:5173".into(),
                http_host_header: "localhost:5173".into(),
                metrics: "127.0.0.1:41234".into(),
            }),
            [
                "tunnel",
                "--no-autoupdate",
                "--url",
                "http://127.0.0.1:5173",
                "--http-host-header",
                "localhost:5173",
                "--metrics",
                "127.0.0.1:41234",
            ]
        );
    }

    /// extractQuickTunnelUrl · pulls the trycloudflare URL out of the ASCII box
    #[test]
    fn extract_url_from_ascii_box() {
        let log = "
INF Requesting new quick Tunnel on trycloudflare.com...
+--------------------------------------------------------------------------------------------+
|  Your quick Tunnel has been created! Visit it at (it may take some time to be reachable):  |
|  https://quiet-marble-otter.trycloudflare.com                                                    |
+--------------------------------------------------------------------------------------------+
";
        assert_eq!(extract_quick_tunnel_url(log).as_deref(), Some("https://quiet-marble-otter.trycloudflare.com"));
    }

    /// extractQuickTunnelUrl · ignores other https links so a docs URL is not treated as the tunnel
    #[test]
    fn extract_url_ignores_other_links() {
        assert_eq!(extract_quick_tunnel_url("see https://developers.cloudflare.com/tunnel"), None);
    }

    /// extractMetricsAddr / parseQuickTunnelMetrics · reads the metrics bind line
    #[test]
    fn extract_metrics_addr_reads_bind_line() {
        assert_eq!(
            extract_metrics_addr("INF Starting metrics server on 127.0.0.1:20241/metrics").as_deref(),
            Some("127.0.0.1:20241")
        );
    }

    /// extractMetricsAddr / parseQuickTunnelMetrics · accepts hostname with or without https and rejects other domains
    #[test]
    fn parse_metrics_accepts_hostname_forms() {
        assert_eq!(
            parse_quick_tunnel_metrics(r#"{"hostname":"quiet-marble-otter.trycloudflare.com"}"#).as_deref(),
            Some("https://quiet-marble-otter.trycloudflare.com")
        );
        assert_eq!(
            parse_quick_tunnel_metrics(r#"{"hostname":"https://quiet-marble-otter.trycloudflare.com/"}"#).as_deref(),
            Some("https://quiet-marble-otter.trycloudflare.com")
        );
        assert_eq!(parse_quick_tunnel_metrics(r#"{"hostname":"evil.example"}"#), None);
        assert_eq!(parse_quick_tunnel_metrics("not json"), None);
    }

    /// lastLogLines · keeps the tail and drops blanks
    #[test]
    fn last_log_lines_keeps_tail_drops_blanks() {
        assert_eq!(last_log_lines("a\n\nb\nc\nd\n", Some(3), None), "b · c · d");
    }

    // ---- 以下是 Rust 侧补的：JS 语义边角 ----

    #[test]
    fn case_insensitivity_is_ascii_only() {
        assert_eq!(
            extract_quick_tunnel_url("HTTPS://Quiet-Otter.TryCloudflare.COM/").as_deref(),
            Some("HTTPS://Quiet-Otter.TryCloudflare.COM")
        );
        // 长 s（U+017F）在 Rust 的 (?i) 里会配上 s，JS 不带 u 的 /i 不会
        assert_eq!(extract_quick_tunnel_url("http\u{17F}://x.trycloudflare.com"), None);
        assert_eq!(parse_cloudflared_version("cloudflared version\u{3000}1.2.3").as_deref(), Some("1.2.3"));
        // \d 只是 ASCII 数字
        assert_eq!(parse_cloudflared_version("cloudflared version １.２.３"), None);
    }

    #[test]
    fn last_log_lines_edges() {
        // slice(-0) 是整个数组
        assert_eq!(last_log_lines("a\r\nb\r\n", Some(0), None), "a · b");
        // 按 UTF-16 码元从尾部截
        assert_eq!(last_log_lines("前缀\n结尾", None, Some(4)), "· 结尾");
        assert_eq!(last_log_lines("x\u{1F600}y", None, Some(2)), "y");
    }

    #[test]
    fn metrics_body_must_be_object_with_string_hostname() {
        assert_eq!(parse_quick_tunnel_metrics("null"), None);
        assert_eq!(parse_quick_tunnel_metrics(r#"{"hostname":"  "}"#), None);
        assert_eq!(parse_quick_tunnel_metrics(r#"{"hostname":7}"#), None);
        assert_eq!(
            parse_quick_tunnel_metrics(r#"{"hostname":" HTTPS://a-b.trycloudflare.com// "}"#).as_deref(),
            Some("https://a-b.trycloudflare.com")
        );
    }

    #[test]
    fn local_asset_defaults_to_this_process() {
        let here = cloudflared_asset(None, None);
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(here.map(|a| a.name), Some("cloudflared-darwin-arm64.tgz"));
        }
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(here.map(|a| a.name), Some("cloudflared-linux-amd64"));
        }
    }
}
