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
//! 配置表存哪儿、密码进哪个钥匙串、"本机"连哪个端口都归平台（falcon-platform 的
//! [`ServerSource`]）。浏览器版只有一份配置：页面所在的源。id 就用 `local`——localStorage
//! 天然按源分，偏好键不加配置后缀，沿用旧 React 版存的键；访问密码不存（浏览器里没有钥匙串，
//! 登录态在 cookie 里）。

use std::rc::Rc;

use falcon_platform::{Platform, ServerSource};
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};

pub const LOCAL_PROFILE_ID: &str = "local";

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
    file: ProfilesFile,
    platform: Rc<dyn Platform>,
}

impl Global for Profiles {}

impl Profiles {
    pub fn init(cx: &mut App) {
        let platform = falcon_platform::get(cx);
        let file = match platform.server_source() {
            // 浏览器：唯一的配置就是页面的源（name 放主机名，标题条上显示它）
            ServerSource::PageOrigin { origin, host } => ProfilesFile {
                profiles: vec![ServerProfile { id: LOCAL_PROFILE_ID.into(), name: host, url: origin, plaintext_ok: true }],
                last_connected: None,
            },
            // 原生："本机"连哪儿每次启动现算（平台给的 local_url），不信存下来的
            ServerSource::Profiles { local_url } => {
                let mut file: ProfilesFile =
                    platform.load_profiles().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
                if let Some(p) = file.profiles.iter_mut().find(|p| p.id == LOCAL_PROFILE_ID) {
                    p.url = local_url;
                } else {
                    file.profiles.insert(
                        0,
                        ServerProfile {
                            id: LOCAL_PROFILE_ID.into(),
                            name: String::new(),
                            url: local_url,
                            plaintext_ok: true,
                        },
                    );
                }
                file
            }
        };
        cx.set_global(Profiles { file, platform });
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
        self.platform.store_password(id, None);
        self.file.profiles.retain(|p| p.id != id);
        if self.file.last_connected.as_deref() == Some(id) {
            self.file.last_connected = None;
        }
        self.save();
    }

    fn save(&self) {
        // 浏览器里配置表是现算的（ServerSource::PageOrigin），平台那边也不存
        self.platform.store_profiles(&serde_json::to_string_pretty(&self.file).unwrap_or_default());
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
