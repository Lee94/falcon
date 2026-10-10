//! 服务端配置：一台 falcon 服务端 = 名称 + 基址。一个窗口只连一台（设计 §2 决定七）。
//!
//! - 本机配置永远存在、不可删，指向 App 托管的 launchd 服务（`http://127.0.0.1:4923`）；
//! - 远处的配置由用户在"连接到…"里增删，基址是 `http(s)://host:port`，通常在反向代理后面；
//! - 访问密码默认存钥匙串（按配置分条）。server 的登录 token 只在它的内存里，服务端一重启
//!   就全部失效——远处的服务端升级、重启都会碰到。存着密码，客户端收到 401 / 4401 时自动
//!   重登一次，不用为此改 server 去加一种长效令牌。
//!
//! 工作区状态（列排布、侧栏展开……）按配置分开存：id 只在一台服务端内唯一。
//!
//! 浏览器版只有一份配置：页面所在的源。id 就用 `local`——localStorage 天然按源分，偏好键
//! 不加配置后缀，与 web 存的键一致；访问密码不存（浏览器里没有钥匙串，登录态在 cookie 里）。

#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;

use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

#[cfg(not(target_family = "wasm"))]
use crate::prefs::{data_dir, write_atomic};

pub const LOCAL_PROFILE_ID: &str = "local";
#[cfg(not(target_family = "wasm"))]
pub const LOCAL_URL: &str = "http://127.0.0.1:4923";
#[cfg(not(target_family = "wasm"))]
const KEYCHAIN_SERVICE: &str = "com.falcon.app";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    pub url: String,
    /// 用户确认过"非回环地址走明文 HTTP"（只问一次）
    #[serde(default)]
    pub plaintext_ok: bool,
}

impl ServerProfile {
    pub fn is_local(&self) -> bool {
        self.id == LOCAL_PROFILE_ID
    }

    /// 基址是不是回环地址（本机配置、或用户手填的 127.0.0.1 / localhost）
    pub fn is_loopback(&self) -> bool {
        url::Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .is_some_and(|h| h == "127.0.0.1" || h == "localhost" || h == "::1" || h == "[::1]")
    }

    /// 非回环的明文 HTTP：密码与终端内容都在网上裸奔，要让用户知道
    pub fn is_insecure(&self) -> bool {
        self.url.starts_with("http://") && !self.is_loopback()
    }

    /// 工作区状态存放目录
    #[cfg(not(target_family = "wasm"))]
    pub fn state_dir(&self) -> PathBuf {
        data_dir().join("servers").join(&self.id)
    }

    #[cfg(target_family = "wasm")]
    pub fn password(&self) -> Option<String> {
        None
    }

    #[cfg(target_family = "wasm")]
    pub fn store_password(&self, _password: Option<&str>) {}

    #[cfg(not(target_family = "wasm"))]
    pub fn password(&self) -> Option<String> {
        keyring::Entry::new(KEYCHAIN_SERVICE, &self.id)
            .ok()?
            .get_password()
            .ok()
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn store_password(&self, password: Option<&str>) {
        let Ok(entry) = keyring::Entry::new(KEYCHAIN_SERVICE, &self.id) else {
            return;
        };
        let result = match password {
            Some(p) => entry.set_password(p),
            None => entry.delete_credential(),
        };
        if let Err(err) = result {
            // 没有条目时删除会报 NoEntry，那是正常的
            if password.is_some() {
                log::warn!("钥匙串写入失败：{err}");
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfilesFile {
    #[serde(default)]
    profiles: Vec<ServerProfile>,
    /// 上次连上的服务端（认证通过、进到工作区），下次启动就开它。不记"退出时开着哪些窗口"：
    /// GPUI 退出时（`App::shutdown`）先清空全部窗口再逐个释放，释放回调里看到的窗口列表
    /// 永远是空的，正常退出根本记不住；而且开着却没连上的窗口（Windows 上没有本机服务，
    /// "本机"必然连不上）也不该下次再开一遍
    #[serde(default)]
    last_connected: Option<String>,
}

pub struct Profiles {
    #[cfg(not(target_family = "wasm"))]
    path: PathBuf,
    file: ProfilesFile,
}

impl Global for Profiles {}

impl Profiles {
    /// 浏览器：唯一的配置就是页面的源（name 放主机名，标题条上显示它）
    #[cfg(target_family = "wasm")]
    pub fn init(cx: &mut App) {
        let location = web_sys::window().map(|w| w.location());
        let origin = location.as_ref().and_then(|l| l.origin().ok()).unwrap_or_default();
        let host = location.as_ref().and_then(|l| l.host().ok()).unwrap_or_default();
        let profile = ServerProfile { id: LOCAL_PROFILE_ID.into(), name: host, url: origin, plaintext_ok: true };
        cx.set_global(Profiles { file: ProfilesFile { profiles: vec![profile], last_connected: None } });
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn init(cx: &mut App) {
        let path = data_dir().join("profiles.json");
        let mut file: ProfilesFile = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // "本机"连哪儿每次启动现算，不信存下来的：开发时 FALCON_LOCAL_URL 指向 pnpm dev:server
        // 或临时实例；否则按已安装服务的端口（自定义过 --port 的也能连上）；都没有才是默认 4923
        let url = std::env::var("FALCON_LOCAL_URL").ok().unwrap_or_else(|| {
            crate::local_service::installed_service_args()
                .map(|a| a.local_url())
                .unwrap_or_else(|| LOCAL_URL.to_string())
        });
        if !file.profiles.iter().any(|p| p.id == LOCAL_PROFILE_ID) {
            file.profiles.insert(
                0,
                ServerProfile {
                    id: LOCAL_PROFILE_ID.into(),
                    name: String::new(),
                    url,
                    plaintext_ok: true,
                },
            );
        } else if let Some(p) = file.profiles.iter_mut().find(|p| p.id == LOCAL_PROFILE_ID) {
            p.url = url;
        }
        cx.set_global(Profiles { path, file });
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn all(&self) -> &[ServerProfile] {
        &self.file.profiles
    }

    pub fn get(&self, id: &str) -> Option<&ServerProfile> {
        self.file.profiles.iter().find(|p| p.id == id)
    }

    /// 启动时开哪台：上次连上的那台（配置已被删就算了），没有就是本机
    pub fn startup_profile(&self) -> ServerProfile {
        self.file
            .last_connected
            .as_deref()
            .and_then(|id| self.get(id))
            .or_else(|| self.get(LOCAL_PROFILE_ID))
            .cloned()
            .expect("本机配置在 init 里补齐，永远存在")
    }

    /// 某台服务端连上了（认证通过）：记下来，下次启动默认开它
    pub fn mark_connected(&mut self, id: &str) {
        if self.file.last_connected.as_deref() != Some(id) {
            self.file.last_connected = Some(id.to_string());
            self.save();
        }
    }

    pub fn upsert(&mut self, profile: ServerProfile) {
        match self.file.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(p) => *p = profile,
            None => self.file.profiles.push(profile),
        }
        self.save();
    }

    pub fn remove(&mut self, id: &str) {
        if id == LOCAL_PROFILE_ID {
            return;
        }
        if let Some(p) = self.get(id).cloned() {
            p.store_password(None);
        }
        self.file.profiles.retain(|p| p.id != id);
        if self.file.last_connected.as_deref() == Some(id) {
            self.file.last_connected = None;
        }
        self.save();
    }

    #[cfg(target_family = "wasm")]
    fn save(&self) {}

    #[cfg(not(target_family = "wasm"))]
    fn save(&self) {
        let bytes = serde_json::to_vec_pretty(&self.file).unwrap_or_default();
        if let Err(err) = write_atomic(&self.path, &bytes) {
            log::warn!("服务端配置写盘失败：{err}");
        }
    }
}

pub fn new_profile_id() -> String {
    use web_time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("srv-{nanos:x}")
}
