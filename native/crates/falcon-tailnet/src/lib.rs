//! 客户端内置的 Tailscale 节点（ADR 0030）。
//!
//! 节点本身是 Go 写的 tsnet，编成动态库 libfalcon_tailnet（`native/tailnet/`，
//! `cargo xtask tailnet`），这里用 dlopen 加载并包成安全的 Rust 接口。库是可选的：开发构建
//! 与没打进库的安装包里 [`Tailnet::global`] 返回 [`LoadError::NotFound`]，界面据此显示
//! 「此构建未包含 Tailscale」，其余功能照常。
//!
//! 约束（ADR 0030 决定一）：
//!
//! - Go 共享库**卸载不了**。加载过的库永不 dlclose，连 ABI 对不上的也一样（Go 运行时在
//!   dlopen 时就已经起来了）。
//! - 一个进程最多一个节点。[`Tailnet::start`] 在已有节点时就是重启。
//! - 所有调用都会阻塞（`start` / `logout` 要和控制面说话，最长约十秒），别在界面线程上调。
//!
//! 节点对外只露一个本机回环的 SOCKS5 代理，带每次启动随机生成的口令（[`ProxyInfo`]），
//! 客户端经 tailnet 的连接都走它（决定二）。

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use falcon_proto::TailscaleDevice;

#[cfg(not(target_family = "wasm"))]
mod ffi;
#[cfg(not(target_family = "wasm"))]
pub use ffi::{Tailnet, candidates};

/// 与 Go 侧 `abiVersion` 对应。导出签名或 JSON 形状有不兼容变化时两边一起加一
pub const ABI_VERSION: i32 = 1;

/// 库文件名：macOS `libfalcon_tailnet.dylib`、Windows `falcon_tailnet.dll`、其余 `libfalcon_tailnet.so`
pub const LIB_STEM: &str = "falcon_tailnet";

/// 节点状态，与 Go 侧 `state*` 常量一一对应
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// 没起，或已经停了
    Stopped,
    /// 起了，还在和控制面握手
    Starting,
    /// 要登录：打开 [`Status::login_url`]
    NeedsLogin,
    /// 登录了，等 tailnet 管理员在后台批准这台设备
    NeedsApproval,
    /// 在 tailnet 里，可以经代理连 peer
    Running,
    /// 起节点失败或查不到状态，见 [`Status::error`]
    Error,
}

/// 本机节点自己
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SelfNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<String>,
    /// tailnet 名（管理后台里显示的那个）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailnet: Option<String>,
    /// MagicDNS 后缀，例如 `tail1234.ts.net`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub magic_dns_suffix: Option<String>,
}

/// `ftn_status` 的结果
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub state: State,
    /// 只在 `needs_login` 时有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_url: Option<String>,
    /// 只在 `running` 时有
    #[serde(default, rename = "self", skip_serializing_if = "Option::is_none")]
    pub self_node: Option<SelfNode>,
    /// tailnet 里的其他设备，在线的在前、按名字排好；与服务端 `/api/tailscale/devices`
    /// 的设备同形同口径
    #[serde(default)]
    pub peers: Vec<TailscaleDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Status {
    pub fn stopped() -> Self {
        Self { state: State::Stopped, login_url: None, self_node: None, peers: Vec::new(), error: None }
    }

    /// URL 的主机名是不是这个 tailnet 里的设备：等于某个 peer 的 MagicDNS 名、短名
    /// （DNS 名第一段）或 Tailscale IPv4。大小写不敏感，忽略末尾的根点。
    ///
    /// 这是服务端条目 `route = auto` 的判定（ADR 0030 决定四）。只看 100.64/10 不够：
    /// 系统 Tailscale 也可能在线，连的是另一个 tailnet。
    pub fn has_peer(&self, host: &str) -> bool {
        if self.state != State::Running {
            return false;
        }
        let host = host.trim().trim_end_matches('.').trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() {
            return false;
        }
        self.peers.iter().any(|p| {
            let dns = p.dns_name.as_deref().unwrap_or("");
            let short = dns.split('.').next().unwrap_or("");
            [dns, short, p.ipv4.as_deref().unwrap_or(""), p.host.as_str()]
                .iter()
                .any(|cand| !cand.is_empty() && cand.eq_ignore_ascii_case(host))
        })
    }
}

/// 回环 SOCKS5 代理的地址与口令。口令每次起节点随机生成、只在内存里：`Debug` 不打印它，
/// 也别写进日志或落盘
#[derive(Deserialize, Clone, PartialEq, Eq)]
pub struct ProxyInfo {
    /// `127.0.0.1:<端口>`
    pub addr: String,
    pub user: String,
    pub pass: String,
}

impl fmt::Debug for ProxyInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyInfo").field("addr", &self.addr).field("user", &self.user).field("pass", &"<redacted>").finish()
    }
}

impl ProxyInfo {
    /// 给 reqwest 用的代理 URL：`socks5h://user:pass@127.0.0.1:port`。`h` 表示域名交给代理
    /// 解析，MagicDNS 名由 tsnet 解析（ADR 0030 决定二）
    pub fn socks5h_url(&self) -> String {
        format!("socks5h://{}:{}@{}", userinfo_escape(&self.user), userinfo_escape(&self.pass), self.addr)
    }
}

/// URL userinfo 里只留 RFC 3986 的 unreserved 字符，其余百分号编码
fn userinfo_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 起节点的参数
#[derive(Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StartConfig {
    /// 节点身份存在这里，重启不用重新登录。Go 侧建成 0700
    pub state_dir: PathBuf,
    /// 在 tailnet 里的设备名
    pub hostname: String,
    /// 自建控制面（Headscale）的地址；不填就是 Tailscale 官方
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_url: Option<String>,
    /// 预授权 key。只在这一次调用里交给 Go，Go 侧 Start 后即清掉；客户端也别存
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_key: Option<String>,
}

impl fmt::Debug for StartConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StartConfig")
            .field("state_dir", &self.state_dir)
            .field("hostname", &self.hostname)
            .field("control_url", &self.control_url)
            .field("auth_key", &self.auth_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// 安卓交给节点的一块网卡（ADR 0030 决定六）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NetInterface {
    pub name: String,
    pub index: i32,
    pub mtu: i32,
    pub up: bool,
    /// CIDR，例如 `192.168.1.5/24`、`fe80::1/64`
    pub addrs: Vec<String>,
}

/// 安卓从 ConnectivityManager 读到的网卡与 DNS 服务器
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct InterfaceList {
    pub interfaces: Vec<NetInterface>,
    /// DNS 服务器 IP
    pub dns: Vec<String>,
}

/// 找不到或加载不了库
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    /// 这个构建没带库。界面显示「此构建未包含 Tailscale」
    #[error("没有找到 Tailscale 库（找过：{}）", searched.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("、"))]
    NotFound { searched: Vec<PathBuf> },
    /// 库在，但 dlopen 或取符号失败（缺符号、架构不对、系统太老……）
    #[error("加载 {path} 失败：{message}")]
    Load { path: PathBuf, message: String },
    /// 库与这个客户端不是一套
    #[error("{path} 的接口版本是 {found}，这个客户端要 {expected}")]
    AbiMismatch { path: PathBuf, found: i32, expected: i32 },
    /// 这个平台没有内置节点（浏览器版）
    #[error("这个平台不支持内置 Tailscale")]
    Unsupported,
}

/// 调用节点出错
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Go 侧报的错误，原文
    #[error("{0}")]
    Node(String),
    /// 和 Go 侧之间的 JSON 不对（多半是库与客户端版本不配套）
    #[error("Tailscale 库返回的数据解析不了：{0}")]
    Json(#[from] serde_json::Error),
    /// 参数里有 NUL 之类交不过去的内容
    #[error("参数无效：{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(name: &str, dns: &str, ip: &str) -> TailscaleDevice {
        TailscaleDevice {
            id: format!("n-{name}"),
            name: name.into(),
            host: dns.into(),
            dns_name: Some(dns.into()),
            ipv4: Some(ip.into()),
            os: None,
            online: true,
            owner: None,
        }
    }

    #[test]
    fn status_parses_go_shape() {
        let js = r#"{"state":"running","self":{"name":"me","dnsName":"me.tail.ts.net","ipv4":"100.1.1.1","magicDnsSuffix":"tail.ts.net"},
            "peers":[{"id":"na","name":"work","host":"work.tail.ts.net","dnsName":"work.tail.ts.net","ipv4":"100.2.2.2","online":true}],
            "somethingNew":1}"#;
        let st: Status = serde_json::from_str(js).unwrap();
        assert_eq!(st.state, State::Running);
        assert_eq!(st.self_node.as_ref().unwrap().magic_dns_suffix.as_deref(), Some("tail.ts.net"));
        assert_eq!(st.peers.len(), 1);
        assert!(st.login_url.is_none());

        let st: Status = serde_json::from_str(r#"{"state":"needs_login","loginUrl":"https://login.tailscale.com/a/x"}"#).unwrap();
        assert_eq!(st.state, State::NeedsLogin);
        assert!(st.peers.is_empty());
        assert!(serde_json::from_str::<Status>(r#"{"state":"needs_approval","peers":[]}"#).is_ok());
    }

    #[test]
    fn has_peer_matches_dns_short_name_and_ip() {
        let mut st = Status::stopped();
        st.peers = vec![peer("work", "work.tail.ts.net", "100.2.2.2")];
        assert!(!st.has_peer("work.tail.ts.net"), "没在 running 就不算");
        st.state = State::Running;
        for host in ["work.tail.ts.net", "WORK.tail.ts.net.", "work", "100.2.2.2"] {
            assert!(st.has_peer(host), "{host}");
        }
        for host in ["", "work.other.ts.net", "100.2.2.3", "wor"] {
            assert!(!st.has_peer(host), "{host}");
        }
    }

    #[test]
    fn proxy_url_escapes_and_debug_redacts() {
        let p = ProxyInfo { addr: "127.0.0.1:41641".into(), user: "tsnet".into(), pass: "a/b:c@d".into() };
        assert_eq!(p.socks5h_url(), "socks5h://tsnet:a%2Fb%3Ac%40d@127.0.0.1:41641");
        assert!(!format!("{p:?}").contains("a/b"));
    }

    #[test]
    fn start_config_json_and_debug() {
        let c = StartConfig { state_dir: "/tmp/ts".into(), hostname: "falcon-mac".into(), control_url: None, auth_key: Some("tskey-x".into()) };
        let js = serde_json::to_string(&c).unwrap();
        assert_eq!(js, r#"{"stateDir":"/tmp/ts","hostname":"falcon-mac","authKey":"tskey-x"}"#);
        assert!(!format!("{c:?}").contains("tskey"));
    }
}
