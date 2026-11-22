//! 客户端内置 Tailscale 节点的全局控制器（ADR 0030）。
//!
//! 整个客户端进程共用一台 userspace 节点：
//! - 偏好（开关、设备名、控制服务器）存 `Prefs`（键 `falcon.tailnet`）；
//! - auth key 只写不读，暂存在平台密码库的保留 id 下，节点成功上线或退出登录时清掉；
//! - 懒加载：开关未打开时不调 `Tailnet::global()`（不 dlopen，不启动 Go 运行时），
//!   只按候选路径查文件是否存在以决定是否显示「此构建未包含 Tailscale」。

use std::rc::Rc;
use std::time::Duration;

use falcon_platform::Platform;
use falcon_tailnet::{LoadError, ProxyInfo, StartConfig, State, Status, Tailnet};
use gpui_kit::{App, AppContext, Context, Entity, Global, Task};
use serde::{Deserialize, Serialize};

use crate::prefs::Prefs;

const PREF_KEY: &str = "falcon.tailnet";
/// 平台密码库中保存 auth key 的保留条目 id（只写不读，ADR 0030 决定四）。
const AUTH_KEY_SECRET_ID: &str = "__tailnet_auth_key__";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailnetConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub control_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryState {
    /// 候选路径存在库文件（尚未 dlopen 或已成功加载）。
    Available,
    /// 当前构建未包含 `libfalcon_tailnet`。
    Missing,
    /// 库存在但 dlopen / ABI 校验失败。
    LoadFailed(String),
}

struct GlobalTailnet(Entity<TailnetController>);
impl Global for GlobalTailnet {}

pub fn init(cx: &mut App) {
    let entity = cx.new(TailnetController::new);
    cx.set_global(GlobalTailnet(entity));
}

pub fn entity(cx: &App) -> Entity<TailnetController> {
    cx.global::<GlobalTailnet>().0.clone()
}

pub struct TailnetController {
    platform: Rc<dyn Platform>,
    config: TailnetConfig,
    default_hostname: String,
    library: LibraryState,
    status: Status,
    proxy: Option<ProxyInfo>,
    busy: bool,
    last_error: Option<String>,
    poll_task: Option<Task<()>>,
    op_gen: u64,
}

impl TailnetController {
    fn new(cx: &mut Context<Self>) -> Self {
        let platform = falcon_platform::get(cx);
        let config: TailnetConfig = Prefs::global(cx)
            .get(PREF_KEY)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let default_hostname = default_hostname(platform.device_hostname().as_deref());
        let library = detect_library(&*platform);
        let mut this = Self {
            platform,
            config,
            default_hostname,
            library,
            status: Status::stopped(),
            proxy: None,
            busy: false,
            last_error: None,
            poll_task: None,
            op_gen: 0,
        };
        if this.config.enabled && this.library == LibraryState::Available {
            this.start_node(cx);
        }
        this
    }

    pub fn config(&self) -> &TailnetConfig {
        &self.config
    }

    pub fn default_hostname(&self) -> &str {
        &self.default_hostname
    }

    pub fn effective_hostname(&self) -> String {
        let trimmed = self.config.hostname.trim();
        if trimmed.is_empty() {
            self.default_hostname.clone()
        } else {
            sanitize_hostname(trimmed)
        }
    }

    pub fn library(&self) -> &LibraryState {
        &self.library
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn proxy(&self) -> Option<&ProxyInfo> {
        self.proxy.as_ref()
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if enabled {
            if self.library == LibraryState::Missing {
                return;
            }
            self.config.enabled = true;
            self.save_pref(cx);
            self.start_node(cx);
        } else {
            self.config.enabled = false;
            self.save_pref(cx);
            self.stop_node(cx);
        }
    }

    /// 保存高级/设备名配置；传入非空 `auth_key` 时只写进密码库、不回显。节点若已开启则立刻重启应用。
    pub fn update_settings(
        &mut self,
        hostname: String,
        control_url: String,
        auth_key: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.config.hostname = hostname.trim().to_owned();
        self.config.control_url = control_url.trim().to_owned();
        self.save_pref(cx);
        if let Some(key) = auth_key.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) {
            self.platform.store_password(AUTH_KEY_SECRET_ID, Some(&key));
        }
        if self.config.enabled && self.library != LibraryState::Missing {
            self.start_node(cx);
        } else {
            cx.notify();
        }
    }

    pub fn retry(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.library == LibraryState::Missing {
            return;
        }
        self.config.enabled = true;
        self.save_pref(cx);
        self.start_node(cx);
    }

    pub fn logout(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(state_dir) = self.platform.tailnet_state_dir() else {
            return;
        };
        self.op_gen += 1;
        let gen = self.op_gen;
        self.busy = true;
        self.poll_task = None;
        self.config.enabled = false;
        self.save_pref(cx);
        self.platform.store_password(AUTH_KEY_SECRET_ID, None);
        self.last_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    match Tailnet::global() {
                        Ok(tn) => tn.logout(&state_dir).map_err(|e| e.to_string()),
                        Err(LoadError::NotFound { .. }) => {
                            if state_dir.exists() {
                                let _ = std::fs::remove_dir_all(&state_dir);
                            }
                            Ok(())
                        }
                        Err(e) => Err(e.to_string()),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.op_gen != gen {
                    return;
                }
                this.busy = false;
                this.status = Status::stopped();
                this.proxy = None;
                falcon_client::set_tailnet_state(Status::stopped(), None);
                if let Err(err) = result {
                    this.last_error = Some(err);
                }
                cx.notify();
                cx.refresh_windows();
            });
        })
        .detach();
    }

    fn save_pref(&self, cx: &mut App) {
        if let Ok(json) = serde_json::to_string(&self.config) {
            cx.global_mut::<Prefs>().set(PREF_KEY, json);
        }
    }

    fn start_node(&mut self, cx: &mut Context<Self>) {
        let Some(state_dir) = self.platform.tailnet_state_dir() else {
            self.library = LibraryState::Missing;
            cx.notify();
            return;
        };
        self.op_gen += 1;
        let gen = self.op_gen;
        self.busy = true;
        self.last_error = None;
        self.poll_task = None;
        self.status = Status { state: State::Starting, ..Status::stopped() };
        self.proxy = None;
        falcon_client::set_tailnet_state(self.status.clone(), None);
        cx.notify();

        let hostname = self.effective_hostname();
        let control_url = (!self.config.control_url.trim().is_empty())
            .then(|| self.config.control_url.trim().to_owned());
        let auth_key = self
            .platform
            .password(AUTH_KEY_SECRET_ID)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let cfg = StartConfig { state_dir, hostname, control_url, auth_key };

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let tn = Tailnet::global().map_err(StartFailure::Load)?;
                    tn.start(&cfg).map_err(|e| StartFailure::Node(e.to_string()))?;
                    let st = tn.status().unwrap_or_else(|e| Status {
                        state: State::Error,
                        error: Some(e.to_string()),
                        ..Status::stopped()
                    });
                    let prox = if st.state == State::Running { tn.proxy().ok().flatten() } else { None };
                    Ok::<_, StartFailure>((st, prox))
                })
                .await;

            let _ = this.update(cx, |this, cx| {
                if this.op_gen != gen {
                    return;
                }
                this.busy = false;
                match outcome {
                    Ok((st, prox)) => {
                        this.library = LibraryState::Available;
                        if st.state == State::Running {
                            this.platform.store_password(AUTH_KEY_SECRET_ID, None);
                        }
                        this.apply_poll(st, prox, cx);
                        this.ensure_polling(gen, cx);
                    }
                    Err(StartFailure::Load(LoadError::NotFound { .. })) => {
                        this.library = LibraryState::Missing;
                        this.config.enabled = false;
                        this.save_pref(cx);
                        this.apply_poll(Status::stopped(), None, cx);
                    }
                    Err(StartFailure::Load(err)) => {
                        let msg = err.to_string();
                        this.library = LibraryState::LoadFailed(msg.clone());
                        this.last_error = Some(msg.clone());
                        this.apply_poll(
                            Status { state: State::Error, error: Some(msg), ..Status::stopped() },
                            None,
                            cx,
                        );
                    }
                    Err(StartFailure::Node(msg)) => {
                        this.last_error = Some(msg.clone());
                        this.apply_poll(
                            Status { state: State::Error, error: Some(msg), ..Status::stopped() },
                            None,
                            cx,
                        );
                    }
                }
            });
        })
        .detach();
    }

    fn stop_node(&mut self, cx: &mut Context<Self>) {
        self.op_gen += 1;
        let gen = self.op_gen;
        self.busy = true;
        self.poll_task = None;
        self.last_error = None;
        cx.notify();

        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move {
                    if let Ok(tn) = Tailnet::global() {
                        tn.stop();
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.op_gen != gen {
                    return;
                }
                this.busy = false;
                this.apply_poll(Status::stopped(), None, cx);
            });
        })
        .detach();
    }

    fn ensure_polling(&mut self, gen: u64, cx: &mut Context<Self>) {
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let delay = this
                    .read_with(cx, |this, _| match this.status.state {
                        State::Running => Duration::from_secs(5),
                        _ => Duration::from_secs(1),
                    })
                    .unwrap_or(Duration::from_secs(5));
                cx.background_executor().timer(delay).await;
                let polled = cx
                    .background_executor()
                    .spawn(async move {
                        let Ok(tn) = Tailnet::global() else {
                            return None;
                        };
                        let st = tn.status().unwrap_or_else(|e| Status {
                            state: State::Error,
                            error: Some(e.to_string()),
                            ..Status::stopped()
                        });
                        let prox = if st.state == State::Running {
                            tn.proxy().ok().flatten()
                        } else {
                            None
                        };
                        Some((st, prox))
                    })
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.op_gen != gen || !this.config.enabled {
                            return false;
                        }
                        if let Some((st, prox)) = polled {
                            if st.state == State::Running {
                                this.platform.store_password(AUTH_KEY_SECRET_ID, None);
                            }
                            this.apply_poll(st, prox, cx);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    fn apply_poll(&mut self, status: Status, proxy: Option<ProxyInfo>, cx: &mut Context<Self>) {
        let changed = self.status != status || self.proxy != proxy;
        if let Some(url) = &status.login_url
            && self.status.login_url.as_deref() != Some(url.as_str())
        {
            log::info!("内置 Tailscale 等待登录：{}", strip_url_query(url));
        }
        self.status = status.clone();
        self.proxy = proxy.clone();
        falcon_client::set_tailnet_state(status, proxy);
        if changed {
            cx.notify();
            cx.refresh_windows();
        }
    }
}

enum StartFailure {
    Load(LoadError),
    Node(String),
}

fn detect_library(platform: &dyn Platform) -> LibraryState {
    if platform.tailnet_state_dir().is_none() {
        return LibraryState::Missing;
    }
    let found = falcon_tailnet::candidates().into_iter().any(|p| p.is_file());
    if found { LibraryState::Available } else { LibraryState::Missing }
}

/// 日志中绝不输出登录 URL 的 query 部分（ADR 0030「安全」）。
fn strip_url_query(url: &str) -> &str {
    url.split(&['?', '#'][..]).next().unwrap_or(url)
}

fn default_hostname(raw_host: Option<&str>) -> String {
    let suffix = raw_host
        .map(|h| h.split('.').next().unwrap_or(h))
        .map(sanitize_hostname)
        .unwrap_or_default();
    if suffix.is_empty() {
        "falcon-client".to_owned()
    } else {
        sanitize_hostname(&format!("falcon-{suffix}"))
    }
}

fn sanitize_hostname(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if (ch == '-' || ch == '_' || ch == ' ') && !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= 63 {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hostname_sanitizes_and_prefixes() {
        assert_eq!(default_hostname(Some("Fays-MacBook-Pro.local")), "falcon-fays-macbook-pro");
        assert_eq!(default_hostname(Some("my_workstation")), "falcon-my-workstation");
        assert_eq!(default_hostname(None), "falcon-client");
    }

    #[test]
    fn strip_url_query_drops_secret_params() {
        assert_eq!(
            strip_url_query("https://login.tailscale.com/a/abc123?token=secret#frag"),
            "https://login.tailscale.com/a/abc123"
        );
    }
}
