//! 异地互联：本机上的 Tailscale（tsnet）与 NetBird（client/embed）。
//!
//! 两个库都是 Go，链不进这个 Rust 进程。所以各放一个小 sidecar（`native/mesh/`），
//! 这里只负责找二进制、拉起、读它 stdout 上的一行 JSON、停掉。和 cloudflared 同一条
//! 边界：只在 falcon 后端本机跑，不进引擎线程，不往远端宿主机装。
//!
//! 入网之后另一台 Falcon 仍是一条 SSH Host——把这里报出来的地址填进去即可。
//! mesh 身份不是 Falcon 的登录身份。
//!
//! 密钥从环境变量来，不进 argv、不入库、不进协议：
//! `FALCON_TSNET_AUTHKEY` / `FALCON_TSNET_CONTROL_URL`（Headscale）、
//! `FALCON_NB_SETUP_KEY` / `FALCON_NB_MANAGEMENT_URL`。
//! 二进制定位：`FALCON_TSNET_BIN` / `FALCON_NB_BIN` 优先，否则
//! `<dataDir>/bin/`，再否则开发构建的 `native/target-mesh/`。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use falcon_proto::{MeshProviderStatus, MeshState, MeshStatus};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Tailscale,
    Netbird,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tailscale" => Some(Provider::Tailscale),
            "netbird" => Some(Provider::Netbird),
            _ => None,
        }
    }

    fn bin_name(self) -> &'static str {
        match self {
            Provider::Tailscale => "falcon-tsnet",
            Provider::Netbird => "falcon-netbird",
        }
    }

    fn env_bin(self) -> &'static str {
        match self {
            Provider::Tailscale => "FALCON_TSNET_BIN",
            Provider::Netbird => "FALCON_NB_BIN",
        }
    }

    fn configured(self) -> bool {
        let key = match self {
            Provider::Tailscale => "FALCON_TSNET_AUTHKEY",
            Provider::Netbird => "FALCON_NB_SETUP_KEY",
        };
        std::env::var(key).is_ok_and(|v| !v.trim().is_empty())
    }

    /// sidecar 要的环境变量。只透传 mesh 自己的，不把整个进程环境交出去。
    ///
    /// NetBird 的管理地址不在这里：它有两个来源（环境变量、设置里存的），由 MeshHost 决定。
    fn child_env(self) -> Vec<(&'static str, String)> {
        let names: &[&str] = match self {
            Provider::Tailscale => &["FALCON_TSNET_AUTHKEY", "FALCON_TSNET_CONTROL_URL", "FALCON_TSNET_HOSTNAME"],
            Provider::Netbird => &["FALCON_NB_SETUP_KEY", "FALCON_NB_HOSTNAME"],
        };
        names.iter().filter_map(|k| std::env::var(k).ok().map(|v| (*k, v))).collect()
    }
}

/// sidecar stdout 的一行。字段比协议少：state 用 sidecar 自己的词。
#[derive(Deserialize)]
struct Line {
    state: String,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    ipv4: Option<String>,
    #[serde(default)]
    ipv6: Option<String>,
    #[serde(default, rename = "dnsName")]
    dns_name: Option<String>,
    #[serde(default)]
    fqdn: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn map_state(s: &str) -> MeshState {
    match s {
        "starting" => MeshState::Starting,
        "online" => MeshState::Online,
        "needs-login" => MeshState::NeedsLogin,
        "offline" => MeshState::Starting,
        "error" => MeshState::Error,
        _ => MeshState::Unknown,
    }
}

struct Slot {
    child: Option<Child>,
    status: MeshProviderStatus,
}

impl Slot {
    fn idle(provider: Provider) -> Self {
        Slot {
            child: None,
            status: MeshProviderStatus {
                state: MeshState::Off,
                hostname: None,
                ipv4: None,
                ipv6: None,
                dns_name: None,
                // 存着的 setup key 在 load_stored 里才灌进来，那时再把 configured 改对。
                // 环境变量这一路这里就看得到。
                configured: provider.configured(),
                error: None,
            },
        }
    }
}

type Slots = Arc<Mutex<(Slot, Slot)>>;

/// 两个 sidecar 的生命周期。挂在 AppState 上，`Send + Sync`，不进引擎。
///
/// `Clone` 只克隆那把锁的 `Arc`，不是两份进程。退出路径留一个克隆，本体跟着 AppState 走。
#[derive(Clone)]
pub struct MeshHost {
    data_dir: PathBuf,
    slots: Slots,
    /// 设置里存的 NetBird 管理地址。环境变量 `FALCON_NB_MANAGEMENT_URL` 优先于它。
    netbird_url: Arc<Mutex<String>>,
    /// 设置里存的 setup key，已经解密。环境变量 `FALCON_NB_SETUP_KEY` 优先于它。
    /// 只活在内存里，不进日志、不进协议。
    netbird_key: Arc<Mutex<String>>,
}

impl MeshHost {
    pub fn new(data_dir: &Path) -> Self {
        MeshHost {
            data_dir: data_dir.to_path_buf(),
            slots: Arc::new(Mutex::new((Slot::idle(Provider::Tailscale), Slot::idle(Provider::Netbird)))),
            netbird_url: Arc::new(Mutex::new(String::new())),
            netbird_key: Arc::new(Mutex::new(String::new())),
        }
    }

    /// 启动时把库里存的管理地址灌进来。不在 `new` 里读库：MeshHost 不该认识 Db。
    pub fn set_netbird_url(&self, url: String) {
        *self.netbird_url.lock().expect("mesh url") = url;
    }

    pub fn netbird_url(&self) -> String {
        self.netbird_url.lock().expect("mesh url").clone()
    }

    pub fn set_netbird_key(&self, key: String) {
        *self.netbird_key.lock().expect("mesh key") = key;
        let mut g = self.slots.lock().expect("mesh");
        g.1.status.configured = Provider::Netbird.configured() || self.has_stored_key();
    }

    fn has_stored_key(&self) -> bool {
        !self.netbird_key.lock().expect("mesh key").trim().is_empty()
    }

    /// NetBird 的 sidecar 现在是不是拉起来了。关着的时候改地址不用重启。
    pub fn netbird_running(&self) -> bool {
        let g = self.slots.lock().expect("mesh");
        !matches!(g.1.status.state, MeshState::Off | MeshState::Unavailable)
    }

    pub fn status(&self) -> MeshStatus {
        let g = self.slots.lock().expect("mesh");
        MeshStatus {
            tailscale: g.0.status.clone(),
            netbird: g.1.status.clone(),
            netbird_management_url: String::new(),
            restarted: false,
        }
    }

    fn slot(slots: &mut (Slot, Slot), p: Provider) -> &mut Slot {
        match p {
            Provider::Tailscale => &mut slots.0,
            Provider::Netbird => &mut slots.1,
        }
    }

    /// 启用或停掉一种。停掉是同步的（杀进程）；启用把进程拉起来就返回，状态随后从 stdout 更新。
    pub fn set_enabled(&self, provider: Provider, enabled: bool) -> MeshProviderStatus {
        if !enabled {
            let mut g = self.slots.lock().expect("mesh");
            let slot = Self::slot(&mut g, provider);
            if let Some(mut child) = slot.child.take() {
                let _ = child.start_kill();
            }
            *slot = Slot::idle(provider);
            return slot.status.clone();
        }
        match self.locate(provider) {
            None => {
                let mut g = self.slots.lock().expect("mesh");
                let slot = Self::slot(&mut g, provider);
                slot.status.state = MeshState::Unavailable;
                slot.status.error = Some(format!(
                    "找不到 {}。先在 native/mesh 里 go build，或设 {}",
                    provider.bin_name(),
                    provider.env_bin()
                ));
                slot.status.clone()
            }
            Some(bin) => self.spawn(provider, bin),
        }
    }

    /// 设置里存的值只在环境变量没设时生效。环境变量是一次性覆盖，不用改库。
    fn extra_env(&self, provider: Provider) -> Vec<(&'static str, String)> {
        match provider {
            Provider::Tailscale => Vec::new(),
            Provider::Netbird => {
                let mut env = Vec::new();
                let url = if std::env::var("FALCON_NB_MANAGEMENT_URL").is_ok_and(|v| !v.trim().is_empty()) {
                    std::env::var("FALCON_NB_MANAGEMENT_URL").unwrap()
                } else {
                    self.netbird_url()
                };
                if !url.trim().is_empty() {
                    env.push(("FALCON_NB_MANAGEMENT_URL", url));
                }
                // 环境变量优先：设了就让 child_env 那条透传，这里不再给一份。
                if !std::env::var("FALCON_NB_SETUP_KEY").is_ok_and(|v| !v.trim().is_empty()) {
                    let key = self.netbird_key.lock().expect("mesh key").clone();
                    if !key.trim().is_empty() {
                        env.push(("FALCON_NB_SETUP_KEY", key));
                    }
                }
                env
            }
        }
    }

    fn locate(&self, provider: Provider) -> Option<PathBuf> {
        if let Some(p) = std::env::var(provider.env_bin()).ok().filter(|s| !s.is_empty()) {
            let path = PathBuf::from(&p);
            if path.is_file() {
                return Some(path);
            }
        }
        let bundled = self.data_dir.join("bin").join(provider.bin_name());
        if bundled.is_file() {
            return Some(bundled);
        }
        // 发布构建把本平台的 sidecar gzip 编了进来：第一次用时释放到数据目录，之后走上面那条。
        if let Some(bytes) = embedded_bytes(provider) {
            if let Some(path) = release_embedded(&bundled, &bytes) {
                return Some(path);
            }
        }
        // 开发构建：sidecar 落在 native/target-mesh/。从仓库里 cargo run 时 cwd 通常是 native/。
        let dev = Path::new("target-mesh").join(provider.bin_name());
        if dev.is_file() {
            return Some(dev);
        }
        None
    }

    fn spawn(&self, provider: Provider, bin: PathBuf) -> MeshProviderStatus {
        let dir = self.data_dir.join("mesh").join(provider.bin_name());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let mut g = self.slots.lock().expect("mesh");
            let slot = Self::slot(&mut g, provider);
            slot.status.state = MeshState::Error;
            slot.status.error = Some(format!("建状态目录失败：{e}"));
            return slot.status.clone();
        }
        let mut cmd = Command::new(&bin);
        cmd.arg(&dir)
            .envs(provider.child_env())
            .envs(self.extra_env(provider))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let mut g = self.slots.lock().expect("mesh");
                let slot = Self::slot(&mut g, provider);
                slot.status.state = MeshState::Error;
                slot.status.error = Some(format!("拉起 {} 失败：{e}", bin.display()));
                return slot.status.clone();
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        {
            let mut g = self.slots.lock().expect("mesh");
            let slot = Self::slot(&mut g, provider);
            // 先停掉上一份，免得两份抢同一个状态目录
            if let Some(mut prev) = slot.child.take() {
                let _ = prev.start_kill();
            }
            slot.child = Some(child);
            slot.status.state = MeshState::Starting;
            slot.status.error = None;
            slot.status.configured = provider.configured();
        }
        if let Some(out) = stdout {
            let slots = Arc::clone(&self.slots);
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(parsed) = serde_json::from_str::<Line>(&line) {
                        apply_line(&slots, provider, parsed);
                    }
                }
                // stdout 关了 = 进程退出。还停在 starting 就说明没入上网。
                let mut g = slots.lock().expect("mesh");
                let slot = MeshHost::slot(&mut g, provider);
                if slot.child.is_some() && slot.status.state == MeshState::Starting {
                    slot.status.state = MeshState::Error;
                    if slot.status.error.is_none() {
                        slot.status.error = Some("sidecar 退出了".into());
                    }
                }
                slot.child = None;
            });
        }
        if let Some(err) = stderr {
            let name = provider.bin_name();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // 登录 URL 在这里。不转给界面（里面没有密钥，但也不该进协议），留在服务端日志里。
                    log::info!("[{name}] {line}");
                }
            });
        }
        let g = self.slots.lock().expect("mesh");
        match provider {
            Provider::Tailscale => g.0.status.clone(),
            Provider::Netbird => g.1.status.clone(),
        }
    }

    /// 后端退出时收掉 sidecar。不调用的话 kill_on_drop 也会在最后一个 MeshHost 丢掉时杀。
    pub fn shutdown(&self) {
        let mut g = self.slots.lock().expect("mesh");
        stop_one(&mut g.0);
        stop_one(&mut g.1);
    }
}

/// 发布构建编进来的本平台 sidecar。开发构建没有，返回 None。
fn embedded_bytes(provider: Provider) -> Option<Vec<u8>> {
    #[cfg(feature = "embed-mesh")]
    {
        use std::io::Read as _;
        let gz: &[u8] = match provider {
            Provider::Tailscale => include_bytes!(concat!(env!("FALCON_EMBED_MESH_DIR"), "/falcon-tsnet.gz")),
            Provider::Netbird => include_bytes!(concat!(env!("FALCON_EMBED_MESH_DIR"), "/falcon-netbird.gz")),
        };
        let mut out = Vec::new();
        return match flate2::read::GzDecoder::new(gz).read_to_end(&mut out) {
            Ok(_) => Some(out),
            Err(e) => {
                log::warn!("解不开内置的 {}：{e}", provider.bin_name());
                None
            }
        };
    }
    #[cfg(not(feature = "embed-mesh"))]
    {
        let _ = provider;
        None
    }
}

fn release_embedded(dest: &Path, bytes: &[u8]) -> Option<PathBuf> {
    if dest.metadata().is_ok_and(|m| m.len() == bytes.len() as u64) {
        return Some(dest.to_path_buf());
    }
    let dir = dest.parent()?;
    std::fs::create_dir_all(dir).ok()?;
    let tmp = dir.join(format!(".{}.tmp-{}", dest.file_name()?.to_string_lossy(), std::process::id()));
    std::fs::write(&tmp, bytes).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).ok()?;
    }
    if std::fs::rename(&tmp, dest).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    Some(dest.to_path_buf())
}

fn stop_one(slot: &mut Slot) {
    if let Some(mut child) = slot.child.take() {
        let _ = child.start_kill();
    }
}

fn apply_line(slots: &Slots, provider: Provider, line: Line) {
    let mut g = slots.lock().expect("mesh");
    let slot = MeshHost::slot(&mut g, provider);
    slot.status.state = map_state(&line.state);
    slot.status.hostname = line.hostname.filter(|s| !s.is_empty());
    slot.status.ipv4 = line.ipv4.filter(|s| !s.is_empty());
    slot.status.ipv6 = line.ipv6.filter(|s| !s.is_empty());
    slot.status.dns_name = line.dns_name.or(line.fqdn).filter(|s| !s.is_empty());
    slot.status.error = line.error.filter(|s| !s.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_words_match_the_sidecar() {
        assert_eq!(map_state("online"), MeshState::Online);
        assert_eq!(map_state("needs-login"), MeshState::NeedsLogin);
        assert_eq!(map_state("starting"), MeshState::Starting);
        assert_eq!(map_state("nope"), MeshState::Unknown);
    }

    #[test]
    fn status_line_round_trips() {
        let line: Line = serde_json::from_str(
            r#"{"state":"online","hostname":"falcon","ipv4":"100.64.0.1","dnsName":"falcon.example.ts.net"}"#,
        )
        .unwrap();
        assert_eq!(line.ipv4.as_deref(), Some("100.64.0.1"));
        assert_eq!(line.dns_name.as_deref(), Some("falcon.example.ts.net"));
        let host = MeshHost::new(Path::new("/tmp"));
        apply_line(&host.slots, Provider::Tailscale, line);
        let st = host.status();
        assert_eq!(st.tailscale.state, MeshState::Online);
        assert_eq!(st.netbird_management_url, "");
        assert_eq!(st.tailscale.dns_name.as_deref(), Some("falcon.example.ts.net"));
        assert_eq!(st.netbird.state, MeshState::Off);
    }

    #[test]
    fn missing_binary_is_unavailable_not_an_error_page() {
        let host = MeshHost::new(Path::new("/tmp/falcon-mesh-test-absent"));
        let st = host.set_enabled(Provider::Netbird, true);
        assert_eq!(st.state, MeshState::Unavailable);
        assert!(st.error.unwrap().contains("falcon-netbird"));
    }
}
