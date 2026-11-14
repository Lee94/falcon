//! 平台能力接口（与具体 UI 渲染框架解耦，ADR 0015 / ADR 0028）。
//!
//! 界面层不写平台特定的系统调用，凡是"这台机器上有没有 / 怎么做"的事都问这里的 [`Platform`]：
//! 偏好与工作区存哪儿、服务端从哪儿来、访问密码、本机服务、下载目录、交给别的应用的链接、应用图标与应用更新。
//! NSH 统一客户端经 [`shared`] 读取线程安全的平台实现（默认落到 [`DefaultPlatform`]），
//! 过渡期保留 [`install`] / [`get`] 供旧桌面入口无感挂接。

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use raw_window_handle::{HasWindowHandle, HandleError, WindowHandle};

/// 逻辑像素矩形（取代原先绑定 GPUI 的 `Bounds<Pixels>`）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 包装动态分发的 `HasWindowHandle` 引用，便于传给接收 `&W where W: HasWindowHandle` 的底层库（如 `wry`）。
pub struct RawWindow<'a>(pub &'a dyn HasWindowHandle);

impl HasWindowHandle for RawWindow<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

/// 跑在什么地方
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlatformInfo {
    /// 键位按 mac 习惯（⌘ 系列）还是 Ctrl+Shift 系列
    pub mac: bool,
    pub windows: bool,
}

impl PlatformInfo {
    /// 窗口左上角有系统画的红绿灯（原生 macOS），标题条要给它让位
    pub fn traffic_lights(&self) -> bool {
        self.mac
    }

    /// 窗口按钮要自己画（原生 Windows：无边框窗口，标题条右边三颗按钮 + 拖动区）
    pub fn custom_window_controls(&self) -> bool {
        self.windows
    }
}

/// 本机服务托管的结果
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    /// 端口起来了
    Ready,
    /// 等了一轮还是连不上（服务装不上 / 起不来）
    Unreachable(String),
}

/// 本机服务的局域网访问状态
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanAccessState {
    /// 是否监听非回环地址（如 `0.0.0.0`）允许局域网设备连接
    pub enabled: bool,
    /// 当前服务端口
    pub port: u16,
    /// 本机局域网网卡对应的可访问基址（如 `http://10.233.21.9:4923`）
    pub urls: Vec<String>,
}

/// 本机托管服务的运行状态（供 macOS 菜单栏状态项与设置页查询）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServicePresence {
    NotInstalled,
    Stopped,
    Running { pid: Option<u32> },
}

/// 对本机托管服务的启停控制动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceControl {
    Start,
    Stop,
    Restart,
}

/// App 托管的本机服务（原生 macOS：Falcon.app 里捆着服务程序，launchd 托管）
pub trait LocalService: Send + Sync {
    /// 装好 / 拉起服务并等端口起来。**阻塞**，放在后台执行器上调
    fn ensure_running(&self, url: &str) -> ServiceStatus;

    /// 查询托管服务在 launchd / 计划任务中的运行状态。不支持的平台返回 `None`。**阻塞**，放在后台执行器上调
    fn presence(&self) -> Option<ServicePresence> {
        None
    }

    /// 对托管服务执行启动 / 停止 / 重启。**阻塞**，放在后台执行器上调
    fn control(&self, op: ServiceControl) -> Result<ServicePresence, String> {
        let _ = op;
        Err("当前平台不支持托管本机服务".into())
    }

    /// 查询托管服务的局域网监听状态。不支持托管服务的平台返回 `None`。**阻塞**，放在后台执行器上调
    fn lan_access(&self) -> Option<LanAccessState> {
        None
    }

    /// 切换托管服务的局域网监听（`--host 0.0.0.0` vs 默认回环）并重新拉起服务。**阻塞**，放在后台执行器上调
    fn set_lan_access(&self, enabled: bool) -> Result<LanAccessState, String> {
        let _ = enabled;
        Err("当前平台不支持托管本机服务".into())
    }
}

/// 独立子窗口网页视图句柄（旧 GPUI 路径使用；NSH 路径直接使用内建 `web_view` 元素）。
pub trait HtmlView {
    fn set_visible(&self, visible: bool);
    /// 窗口内容区的逻辑像素
    fn set_bounds(&self, bounds: LogicalRect);
    /// 把键盘焦点还给宿主窗口
    fn focus_parent(&self);
}

/// 平台能力接口。
pub trait Platform {
    fn info(&self) -> PlatformInfo;

    // ---------------- 偏好（KvStore）----------------

    /// 启动时把偏好整份读进来
    fn load_prefs(&self) -> BTreeMap<String, String>;

    /// 一个键改了。`all` 是改完之后的整份
    fn store_pref(&self, key: &str, value: &str, all: &BTreeMap<String, String>);

    // ---------------- 工作区（按服务端配置各一份）----------------

    /// 落盘的工作区（形状沿用 `falcon.workspace`）
    fn load_workspace(&self, profile_id: &str) -> Option<String>;
    fn store_workspace(&self, profile_id: &str, json: &str);

    // ---------------- 服务端 ----------------

    /// "本机"这条配置此刻该连的基址（每次启动现算）
    fn local_server_url(&self) -> String;

    /// 服务端配置表（JSON）
    fn load_profiles(&self) -> Option<String>;
    fn store_profiles(&self, json: &str);

    /// 记住的访问密码
    fn password(&self, profile_id: &str) -> Option<String>;
    /// `None` = 删掉
    fn store_password(&self, profile_id: &str, password: Option<&str>);

    /// App 托管的本机服务
    fn local_service(&self) -> Option<Arc<dyn LocalService>> {
        None
    }

    // ---------------- 下载 ----------------

    /// 多个文件下载时缺省落在这个目录（"下载"文件夹）
    fn downloads_dir(&self) -> PathBuf;

    // ---------------- HTML 预览 ----------------

    /// 旧桌面路径在窗口里建原生子 WebView；NSH 路径直接使用内建 `web_view`，缺省返回 `None`。
    fn html_view(
        &self,
        url: &str,
        prefix: &str,
        open_external: Box<dyn Fn(String) + Send + Sync>,
        window: &dyn HasWindowHandle,
    ) -> Option<Result<Rc<dyn HtmlView>, String>> {
        let _ = (url, prefix, open_external, window);
        None
    }

    // ---------------- 交给别的应用 ----------------

    /// 把自定义协议的链接（`zed://`、`vscode://`）交给认领它的应用。
    /// - `Some(false)`：确定没有应用认领这个协议（如 macOS LaunchServices 查询为空）；
    /// - `Some(true)`：已由平台直接打开；
    /// - `None`：交给上层 UI 框架自己的 `open_url` 打开。
    fn open_app_link(&self, url: &str) -> Option<bool> {
        let _ = url;
        None
    }

    // ---------------- 应用图标 ----------------

    /// 换应用图标（macOS 的 Dock；PNG，已套好平台版式）。没有对应物的平台忽略
    fn set_app_icon(&self, _png: &[u8]) {}

    /// 读回当前的应用图标（TIFF）。只给自动化验证用
    fn current_app_icon(&self) -> Option<Vec<u8>> {
        None
    }

    // ---------------- 提示音 ----------------

    /// 响一声「跑完了」（程序经 OSC 7501 报告 done / error，ADR 0020 决定六）
    fn play_chime(&self) {}

    // ---------------- 应用更新（ADR 0026）----------------

    /// 检查并安装本机应用。没有这一能力的平台返回 None
    fn app_updater(&self) -> Option<Arc<dyn AppUpdater>> {
        None
    }

    /// 本进程退出之后打开 `bundle`
    fn schedule_relaunch(&self, bundle: &Path) -> Result<(), String> {
        let _ = bundle;
        Err("这个平台不会在退出后重新打开应用".into())
    }

    /// 同时运行多份客户端时的短区分标记（例如 `"v0.3.20 09:52"`）；只有一份时返回空串
    fn instance_mark(&self) -> String {
        String::new()
    }
}

/// 跨线程共享的 `Platform` 标记（供 NSH 引擎线程与宿主线程共享）。
pub trait SyncPlatform: Platform + Send + Sync {}
impl<T: Platform + Send + Sync + ?Sized> SyncPlatform for T {}

/// 一份可以安装的 GitHub Release
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub asset_name: String,
    pub download_url: String,
    /// macOS 安装包里带着服务端，装完按原来的参数执行 `service install`
    pub installs_local_service: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateCheck {
    UpToDate { version: String },
    Available(AvailableUpdate),
    Failed(UpdateFailure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledUpdate {
    pub version: String,
    pub backup_path: Option<String>,
    pub relaunch: bool,
    pub relaunch_bundle: Option<String>,
    pub staged_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateFailure {
    NoRelease,
    Network(String),
    NoPackage { platform: String },
    BadDownloadUrl,
    Install(String),
    Service(String),
}

/// 阻塞。界面把它放到后台执行器上调
pub trait AppUpdater: Send + Sync {
    fn check(&self, current_version: &str) -> UpdateCheck;
    fn install(&self, update: &AvailableUpdate) -> Result<InstalledUpdate, UpdateFailure>;
}

// ---------------- 默认跨端文件/内存存储实现 ----------------

const DEFAULT_LOCAL_URL: &str = "http://127.0.0.1:4923";

/// 开箱即用的跨平台默认宿主实现：
/// - 原生桌面 / TUI / Android：读写 `<数据目录>/{prefs.json, profiles.json, servers/<id>/workspace.json}`（原子 `.tmp` + `rename`），支持 `FALCON_NATIVE_DATA_DIR` 与 `FALCON_LOCAL_URL` 覆盖；
/// - `wasm32` / 纯内存测试：使用进程内内存字典，同源基址缺省回退 `DEFAULT_LOCAL_URL`。
pub struct DefaultPlatform {
    #[cfg(not(target_arch = "wasm32"))]
    data_dir: PathBuf,
    mem_prefs: RwLock<BTreeMap<String, String>>,
    mem_profiles: RwLock<Option<String>>,
    mem_workspaces: RwLock<BTreeMap<String, String>>,
    mem_passwords: RwLock<BTreeMap<String, String>>,
    local_url_override: Option<String>,
}

impl Default for DefaultPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultPlatform {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            data_dir: resolve_data_dir(),
            mem_prefs: RwLock::new(BTreeMap::new()),
            mem_profiles: RwLock::new(None),
            mem_workspaces: RwLock::new(BTreeMap::new()),
            mem_passwords: RwLock::new(BTreeMap::new()),
            local_url_override: None,
        }
    }

    /// 纯内存隔离平台（供 `nsh_testing::Headless` 与单测使用，绝不触碰用户真实数据目录）。
    pub fn in_memory(local_url: impl Into<String>) -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            data_dir: PathBuf::new(),
            mem_prefs: RwLock::new(BTreeMap::new()),
            mem_profiles: RwLock::new(None),
            mem_workspaces: RwLock::new(BTreeMap::new()),
            mem_passwords: RwLock::new(BTreeMap::new()),
            local_url_override: Some(local_url.into()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_data_dir(data_dir: PathBuf, local_url: Option<String>) -> Self {
        Self {
            data_dir,
            mem_prefs: RwLock::new(BTreeMap::new()),
            mem_profiles: RwLock::new(None),
            mem_workspaces: RwLock::new(BTreeMap::new()),
            mem_passwords: RwLock::new(BTreeMap::new()),
            local_url_override: local_url,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn disk_enabled(&self) -> bool {
        !self.data_dir.as_os_str().is_empty()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FALCON_NATIVE_DATA_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("com", "falcon", "Falcon")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("falcon-native"))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Platform for DefaultPlatform {
    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            mac: cfg!(target_os = "macos"),
            windows: cfg!(windows),
        }
    }

    fn load_prefs(&self) -> BTreeMap<String, String> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            return std::fs::read_to_string(self.data_dir.join("prefs.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        }
        self.mem_prefs.read().clone()
    }

    fn store_pref(&self, key: &str, value: &str, all: &BTreeMap<String, String>) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            let path = self.data_dir.join("prefs.json");
            if let Err(err) = write_atomic(&path, &serde_json::to_vec_pretty(all).unwrap_or_default()) {
                log::warn!("偏好写盘失败（{}）：{err}", path.display());
            }
            return;
        }
        self.mem_prefs.write().insert(key.to_owned(), value.to_owned());
    }

    fn load_workspace(&self, profile_id: &str) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            let path = self.data_dir.join("servers").join(profile_id).join("workspace.json");
            return std::fs::read_to_string(path).ok();
        }
        self.mem_workspaces.read().get(profile_id).cloned()
    }

    fn store_workspace(&self, profile_id: &str, json: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            let path = self.data_dir.join("servers").join(profile_id).join("workspace.json");
            if let Err(err) = write_atomic(&path, json.as_bytes()) {
                log::warn!("工作区写盘失败：{err}");
            }
            return;
        }
        self.mem_workspaces.write().insert(profile_id.to_owned(), json.to_owned());
    }

    fn local_server_url(&self) -> String {
        if let Some(url) = &self.local_url_override {
            return url.clone();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(url) = std::env::var("FALCON_LOCAL_URL")
            && !url.trim().is_empty()
        {
            return url;
        }
        DEFAULT_LOCAL_URL.to_owned()
    }

    fn load_profiles(&self) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            return std::fs::read_to_string(self.data_dir.join("profiles.json")).ok();
        }
        self.mem_profiles.read().clone()
    }

    fn store_profiles(&self, json: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            let path = self.data_dir.join("profiles.json");
            if let Err(err) = write_atomic(&path, json.as_bytes()) {
                log::warn!("服务端配置写盘失败：{err}");
            }
            return;
        }
        *self.mem_profiles.write() = Some(json.to_owned());
    }

    fn password(&self, profile_id: &str) -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            let map: BTreeMap<String, String> = std::fs::read_to_string(self.data_dir.join("passwords.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            return map.get(profile_id).cloned();
        }
        self.mem_passwords.read().get(profile_id).cloned()
    }

    fn store_password(&self, profile_id: &str, password: Option<&str>) {
        #[cfg(not(target_arch = "wasm32"))]
        if self.disk_enabled() {
            let path = self.data_dir.join("passwords.json");
            let mut map: BTreeMap<String, String> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            match password {
                Some(pw) => {
                    map.insert(profile_id.to_owned(), pw.to_owned());
                }
                None => {
                    map.remove(profile_id);
                }
            }
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let tmp = path.with_extension("tmp");
            let bytes = serde_json::to_vec_pretty(&map).unwrap_or_default();
            #[cfg(unix)]
            {
                use std::io::Write as _;
                use std::os::unix::fs::OpenOptionsExt as _;
                let _ = std::fs::remove_file(&tmp);
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&tmp)
                    && f.write_all(&bytes).is_ok()
                {
                    let _ = std::fs::rename(&tmp, &path);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = write_atomic(&path, &bytes);
            }
            return;
        }
        let mut map = self.mem_passwords.write();
        match password {
            Some(pw) => {
                map.insert(profile_id.to_owned(), pw.to_owned());
            }
            None => {
                map.remove(profile_id);
            }
        }
    }

    fn downloads_dir(&self) -> PathBuf {
        #[cfg(not(target_arch = "wasm32"))]
        {
            directories::UserDirs::new()
                .and_then(|u| {
                    u.download_dir()
                        .map(Path::to_path_buf)
                        .or_else(|| Some(u.home_dir().to_path_buf()))
                })
                .unwrap_or_else(std::env::temp_dir)
        }
        #[cfg(target_arch = "wasm32")]
        {
            PathBuf::from("/downloads")
        }
    }
}

// ---------------- 注册与获取 ----------------

static SHARED_PLATFORM: OnceLock<RwLock<Arc<dyn SyncPlatform>>> = OnceLock::new();

fn shared_slot() -> &'static RwLock<Arc<dyn SyncPlatform>> {
    SHARED_PLATFORM.get_or_init(|| RwLock::new(Arc::new(DefaultPlatform::new())))
}

/// 注册跨线程共享的平台实现（供 NSH 引擎线程与宿主入口使用）。
pub fn install_shared(platform: Arc<dyn SyncPlatform>) {
    *shared_slot().write() = platform;
}

/// 获取当前跨线程共享的平台实现；未显式注册时自动使用 [`DefaultPlatform`]。
pub fn shared() -> Arc<dyn SyncPlatform> {
    shared_slot().read().clone()
}

thread_local! {
    static THREAD_PLATFORM: RefCell<Option<Rc<dyn Platform>>> = const { RefCell::new(None) };
}

/// 兼容单线程 UI 入口（如过渡期 `falcon-ui`）挂接平台实现，不依赖特定 UI 框架的上下文类型。
pub fn install<C>(platform: Rc<dyn Platform>, _cx: &mut C) {
    THREAD_PLATFORM.with(|slot| {
        *slot.borrow_mut() = Some(platform);
    });
}

/// 兼容单线程 UI 入口读取平台实现。
pub fn get<C>(_cx: &C) -> Rc<dyn Platform> {
    THREAD_PLATFORM.with(|slot| {
        slot.borrow()
            .clone()
            .expect("falcon_platform::install 尚未在当前线程调用")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_platform_roundtrips_prefs_profiles_and_workspaces() {
        let p = DefaultPlatform::in_memory("http://127.0.0.1:4999");
        assert_eq!(p.local_server_url(), "http://127.0.0.1:4999");

        let mut prefs = p.load_prefs();
        assert!(prefs.is_empty());
        prefs.insert("falcon.native.zoom".into(), "1.25".into());
        p.store_pref("falcon.native.zoom", "1.25", &prefs);
        assert_eq!(p.load_prefs().get("falcon.native.zoom").map(String::as_str), Some("1.25"));

        p.store_profiles(r#"{"profiles":[]}"#);
        assert_eq!(p.load_profiles().as_deref(), Some(r#"{"profiles":[]}"#));

        p.store_workspace("local", r#"{"tabs":["s1"]}"#);
        assert_eq!(p.load_workspace("local").as_deref(), Some(r#"{"tabs":["s1"]}"#));

        p.store_password("srv-1", Some("sec"));
        assert_eq!(p.password("srv-1").as_deref(), Some("sec"));
        p.store_password("srv-1", None);
        assert_eq!(p.password("srv-1"), None);
    }
}
