//! 客户端内置 Tailscale 节点路由与探测（ADR 0030 决定二 / 四 / 五）。
//!
//! 整个客户端进程共享一个内置节点的状态与回环 SOCKS5 代理；每个 [`crate::FalconClient`] 按自己的
//! [`ServerRoute`] 在每次发请求或建 WS 连接时现判是否经代理出去。

use std::future::Future;
use std::net::Ipv4Addr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use falcon_proto::{SystemInfo, TailscaleDevice};
use falcon_tailnet::{ProxyInfo, State, Status};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::error_chain;
use crate::runtime::{self, MaybeSend};

/// 服务端条目的网络路由选项（ADR 0030 决定四）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerRoute {
    #[default]
    Auto,
    Direct,
    Tailnet,
}

/// 单次连接判定的路由结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRoute {
    Direct,
    Tailnet(ProxyInfo),
    /// 显式要求经内置 Tailscale，但节点当前不可用。
    TailnetUnavailable(String),
}

#[derive(Clone)]
struct TailnetSnapshot {
    status: Status,
    proxy: Option<ProxyInfo>,
}

impl Default for TailnetSnapshot {
    fn default() -> Self {
        Self { status: Status::stopped(), proxy: None }
    }
}

static GLOBAL_TAILNET: LazyLock<RwLock<TailnetSnapshot>> =
    LazyLock::new(|| RwLock::new(TailnetSnapshot::default()));

/// 由客户端内置 Tailscale 控制器同步最新状态与 SOCKS5 代理信息。
pub fn set_tailnet_state(status: Status, proxy: Option<ProxyInfo>) {
    let mut guard = GLOBAL_TAILNET.write();
    guard.status = status;
    guard.proxy = proxy;
}

/// 读取当前内置节点的最新状态。
pub fn tailnet_status() -> Status {
    GLOBAL_TAILNET.read().status.clone()
}

/// 读取当前内置节点的 SOCKS5 代理信息（仅 `Running` 时有值）。
pub fn tailnet_proxy() -> Option<ProxyInfo> {
    GLOBAL_TAILNET.read().proxy.clone()
}

pub(crate) fn resolve_route_from(
    snapshot_status: &Status,
    snapshot_proxy: Option<&ProxyInfo>,
    host: &str,
    route: ServerRoute,
) -> ResolvedRoute {
    match route {
        ServerRoute::Direct => ResolvedRoute::Direct,
        ServerRoute::Tailnet => {
            if snapshot_status.state == State::Running
                && let Some(proxy) = snapshot_proxy
            {
                ResolvedRoute::Tailnet(proxy.clone())
            } else {
                ResolvedRoute::TailnetUnavailable(unavailable_message(snapshot_status))
            }
        }
        ServerRoute::Auto => {
            if snapshot_status.state == State::Running
                && snapshot_status.has_peer(host)
                && let Some(proxy) = snapshot_proxy
            {
                ResolvedRoute::Tailnet(proxy.clone())
            } else {
                ResolvedRoute::Direct
            }
        }
    }
}

pub fn resolve_route_for_host(host: &str, route: ServerRoute) -> ResolvedRoute {
    let snap = GLOBAL_TAILNET.read();
    resolve_route_from(&snap.status, snap.proxy.as_ref(), host, route)
}

pub fn resolve_route_for_url(url: &str, route: ServerRoute) -> ResolvedRoute {
    let Some(host) = Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)) else {
        return ResolvedRoute::Direct;
    };
    resolve_route_for_host(&host, route)
}

pub fn is_url_routed_via_tailnet(url: &str, route: ServerRoute) -> bool {
    matches!(resolve_route_for_url(url, route), ResolvedRoute::Tailnet(_))
}

/// 主机名看起来像 tailnet 地址（`*.ts.net` 或 `100.64.0.0/10`）。只用于断连时的提示文案，不用于 `auto` 路由判定。
pub fn looks_like_tailnet_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.ends_with(".ts.net") {
        return true;
    }
    if let Ok(ip) = h.parse::<Ipv4Addr>() {
        let [a, b, _, _] = ip.octets();
        return a == 100 && (64..=127).contains(&b);
    }
    false
}

/// 当连接失败且条目可能依赖内置 Tailscale 时，返回给用户看的状态提示。
pub fn tailnet_failure_hint(url: &str, route: ServerRoute) -> Option<String> {
    if route == ServerRoute::Direct {
        return None;
    }
    let host = Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned))?;
    if route == ServerRoute::Auto && !looks_like_tailnet_host(&host) {
        return None;
    }
    let snap = GLOBAL_TAILNET.read();
    match snap.status.state {
        State::NeedsLogin | State::NeedsApproval | State::Starting | State::Error => {
            Some(unavailable_message(&snap.status))
        }
        State::Stopped if route == ServerRoute::Tailnet => Some(unavailable_message(&snap.status)),
        State::Stopped | State::Running => None,
    }
}

fn unavailable_message(status: &Status) -> String {
    match status.state {
        State::Stopped => "内置 Tailscale 未开启".to_owned(),
        State::Starting => "内置 Tailscale 正在连接…".to_owned(),
        State::NeedsLogin => "内置 Tailscale 尚未登录".to_owned(),
        State::NeedsApproval => "内置 Tailscale 等待管理员审批".to_owned(),
        State::Error => match status.error.as_deref().filter(|s| !s.is_empty()) {
            Some(err) => format!("内置 Tailscale 出错：{err}"),
            None => "内置 Tailscale 出错".to_owned(),
        },
        State::Running => "内置 Tailscale 代理尚未就绪".to_owned(),
    }
}

/// 探测单个内置 Tailscale peer 是否在指定端口运行 Falcon（ADR 0030 决定五）。
#[derive(Debug, Clone)]
pub struct TailnetProbeHit {
    pub device_id: String,
    pub host: String,
    pub port: u16,
    pub system: SystemInfo,
}

/// 并发探测在线的内置 Tailscale 设备是否运行 Falcon（单台超时 1.5s，最多同时 8 个）。
pub fn probe_tailnet_servers(
    proxy: ProxyInfo,
    devices: Vec<TailscaleDevice>,
    port: u16,
) -> impl Future<Output = Vec<TailnetProbeHit>> + MaybeSend + 'static {
    async move {
        runtime::run(async move {
            let client = match build_probe_client(&proxy) {
                Ok(c) => Arc::new(c),
                Err(_) => return Vec::new(),
            };
            let sem = Arc::new(tokio::sync::Semaphore::new(8));
            let mut handles = Vec::new();
            for dev in devices.into_iter().filter(|d| d.online) {
                let target_host = dev.connection_host().to_owned();
                if target_host.is_empty() {
                    continue;
                }
                let client = client.clone();
                let sem = sem.clone();
                let device_id = dev.id;
                handles.push(tokio::spawn(async move {
                    let _permit = sem.acquire().await.ok()?;
                    let url = format!("http://{target_host}:{port}/api/system");
                    let resp = client.get(&url).send().await.ok()?;
                    if !resp.status().is_success() {
                        return None;
                    }
                    let system = resp.json::<SystemInfo>().await.ok()?;
                    if system.version.is_empty() && system.platform.is_empty() {
                        return None;
                    }
                    Some(TailnetProbeHit { device_id, host: target_host, port, system })
                }));
            }
            let mut hits = Vec::new();
            for h in handles {
                if let Ok(Some(hit)) = h.await {
                    hits.push(hit);
                }
            }
            hits
        })
        .await
        .unwrap_or_default()
    }
}

fn build_probe_client(proxy: &ProxyInfo) -> Result<reqwest::Client, String> {
    let tls = crate::tls::client_config()?;
    let reqwest_proxy = reqwest::Proxy::all(proxy.socks5h_url()).map_err(|e| error_chain(&e))?;
    reqwest::Client::builder()
        .use_preconfigured_tls((*tls).clone())
        .user_agent(concat!("falcon-native/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_millis(1500))
        .timeout(Duration::from_millis(1500))
        .no_proxy()
        .proxy(reqwest_proxy)
        .build()
        .map_err(|e| error_chain(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proxy() -> ProxyInfo {
        ProxyInfo { addr: "127.0.0.1:41641".into(), user: "tsnet".into(), pass: "secret".into() }
    }

    fn running_status() -> Status {
        Status {
            state: State::Running,
            login_url: None,
            self_node: None,
            peers: vec![TailscaleDevice {
                id: "n1".into(),
                name: "studio".into(),
                host: "studio.tail123.ts.net".into(),
                dns_name: Some("studio.tail123.ts.net".into()),
                ipv4: Some("100.88.1.2".into()),
                os: Some("macOS".into()),
                online: true,
                owner: None,
            }],
            error: None,
        }
    }

    #[test]
    fn auto_routes_only_known_peers_when_running() {
        let st = running_status();
        let p = sample_proxy();
        assert_eq!(resolve_route_from(&st, Some(&p), "studio.tail123.ts.net", ServerRoute::Auto), ResolvedRoute::Tailnet(p.clone()));
        assert_eq!(resolve_route_from(&st, Some(&p), "studio", ServerRoute::Auto), ResolvedRoute::Tailnet(p.clone()));
        assert_eq!(resolve_route_from(&st, Some(&p), "100.88.1.2", ServerRoute::Auto), ResolvedRoute::Tailnet(p.clone()));
        // 别的 tailnet / 局域网地址不走内置节点
        assert_eq!(resolve_route_from(&st, Some(&p), "100.88.1.99", ServerRoute::Auto), ResolvedRoute::Direct);
        assert_eq!(resolve_route_from(&st, Some(&p), "127.0.0.1", ServerRoute::Auto), ResolvedRoute::Direct);
    }

    #[test]
    fn direct_always_bypasses_and_tailnet_errors_when_not_running() {
        let st = running_status();
        let p = sample_proxy();
        assert_eq!(resolve_route_from(&st, Some(&p), "studio.tail123.ts.net", ServerRoute::Direct), ResolvedRoute::Direct);

        let mut needs_login = Status::stopped();
        needs_login.state = State::NeedsLogin;
        assert_eq!(resolve_route_from(&needs_login, None, "studio.tail123.ts.net", ServerRoute::Auto), ResolvedRoute::Direct);
        assert!(matches!(
            resolve_route_from(&needs_login, None, "studio.tail123.ts.net", ServerRoute::Tailnet),
            ResolvedRoute::TailnetUnavailable(msg) if msg.contains("尚未登录")
        ));
    }

    #[test]
    fn looks_like_tailnet_host_detects_ts_net_and_cgnat() {
        assert!(looks_like_tailnet_host("box.tail123.ts.net"));
        assert!(looks_like_tailnet_host("100.64.0.1"));
        assert!(looks_like_tailnet_host("100.127.255.254"));
        assert!(!looks_like_tailnet_host("100.63.0.1"));
        assert!(!looks_like_tailnet_host("192.168.1.2"));
    }
}
