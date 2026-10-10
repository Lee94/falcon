//! 平台能力接口（docs/design/rust-unification.md 决定六）。
//!
//! 客户端一套界面代码（falcon-ui）、两种产物：原生桌面（falcon-desktop）与浏览器 wasm
//! （falcon-web）。界面层不写 `cfg(target_family = "wasm")`，凡是"这台机器上有没有 / 怎么做"
//! 的事都问这里的 [`Platform`]：偏好与工作区存哪儿、服务端从哪儿来、访问密码、本机服务、
//! 字体、下载、HTML 预览视图、应用图标。两个入口在开窗口之前用 [`install`] 挂一份实现。
//!
//! 只放接口与少量数据类型，不放实现：钥匙串、文件、wry、AppKit 在 falcon-desktop，
//! localStorage、DOM 在 falcon-web。所有方法都在 GPUI 的前台线程上调（实现可以是 `!Send` 的），
//! 唯一的例外是 [`LocalService`]——它阻塞，要放到后台执行器上跑。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Bounds, Global, Pixels, Window};

/// 跑在什么地方。`mac` / `windows` 说的是操作系统——浏览器里是浏览器所在的那台
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlatformInfo {
    /// 键位按 mac 习惯（⌘ 系列）还是 Ctrl+Shift 系列
    pub mac: bool,
    pub windows: bool,
    /// 跑在浏览器里：单窗口、服务端就是页面的源、没有本机文件系统、浏览器保留了 ⌘T / ⌘W 这类键
    pub browser: bool,
}

impl PlatformInfo {
    /// 窗口左上角有系统画的红绿灯（原生 macOS），标题条要给它让位
    pub fn traffic_lights(&self) -> bool {
        self.mac && !self.browser
    }

    /// 窗口按钮要自己画（原生 Windows：无边框窗口，标题条右边三颗按钮 + 拖动区）
    pub fn custom_window_controls(&self) -> bool {
        self.windows && !self.browser
    }
}

/// 服务端从哪儿来
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerSource {
    /// 浏览器：唯一的一台就是页面所在的源。`host` 用来在标题条上称呼它
    PageOrigin { origin: String, host: String },
    /// 原生：用户维护的配置表（[`Platform::load_profiles`]）+ App 托管的本机服务，
    /// `local_url` 是"本机"这条配置此刻该连的基址（每次启动现算）
    Profiles { local_url: String },
}

/// 回退字体（Ioskeley / Maple 中文 / Nerd 图标）从哪儿来。正文字体（Berkeley Mono TX-02）
/// 永远嵌在界面层里
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontSource {
    /// 嵌在二进制里：字节由平台交进来（原生调 `falcon_ui::embedded_fallback_fonts()`）。
    /// 不由界面层自己按分支选——那样两条路都会被链接进去，浏览器版平白多 24MB
    Embedded(Vec<&'static [u8]>),
    /// 启动后从 `<base_url>/fonts/*.ttf` 拉：wasm 要整个下载完才能启动，嵌全套会多出 24MB
    Fetch { base_url: String },
}

/// 下载落到哪儿
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Downloads {
    /// App 自己流式写盘；多个文件时缺省落在这个目录（"下载"文件夹）
    LocalDir(PathBuf),
    /// 交给浏览器的下载管理器（[`Platform::hand_off_download`]）
    Browser,
}

/// 本机服务托管的结果
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceStatus {
    /// 端口起来了
    Ready,
    /// 等了一轮还是连不上（服务装不上 / 起不来）
    Unreachable(String),
}

/// App 托管的本机服务（原生 macOS：Falcon.app 里捆着服务程序，launchd 托管）
pub trait LocalService: Send + Sync {
    /// 装好 / 拉起服务并等端口起来。**阻塞**，放在后台执行器上调
    fn ensure_running(&self, url: &str) -> ServiceStatus;
}

/// 压在 GPUI 画面**上面**的网页视图（HTML 预览，ADR 0007）。GPUI 的浮层盖不住它，
/// 显隐与位置由界面层每帧摆
pub trait HtmlView {
    fn set_visible(&self, visible: bool);
    /// 窗口内容区的逻辑像素
    fn set_bounds(&self, bounds: Bounds<Pixels>);
    /// 把键盘焦点还给 GPUI（点到网页外面时）
    fn focus_parent(&self);
}

/// 平台能力。原生与浏览器各实现一份，界面层经 [`get`] 取
pub trait Platform {
    fn info(&self) -> PlatformInfo;

    // ---------------- 偏好（KvStore）----------------

    /// 启动时把偏好整份读进来。键与 web 的 localStorage 一致（`falcon.themes` / `falcon.term` …），
    /// 两边的数据可以直接对照，老用户的主题 / 终端设置原样继承
    fn load_prefs(&self) -> BTreeMap<String, String>;

    /// 一个键改了。`all` 是改完之后的整份：存成一个文件的整份重写，localStorage 只写这一个键
    fn store_pref(&self, key: &str, value: &str, all: &BTreeMap<String, String>);

    // ---------------- 工作区（按服务端配置各一份）----------------

    /// 落盘的工作区（web 的 `falcon.workspace`，形状逐字一致）
    fn load_workspace(&self, profile_id: &str) -> Option<String>;
    fn store_workspace(&self, profile_id: &str, json: &str);

    // ---------------- 服务端 ----------------

    fn server_source(&self) -> ServerSource;

    /// 服务端配置表（JSON）。只有 [`ServerSource::Profiles`] 才有，另一种返回 `None`
    fn load_profiles(&self) -> Option<String>;
    fn store_profiles(&self, json: &str);

    /// 访问密码（按配置分条，原生存钥匙串）。浏览器里没有钥匙串，登录态在 cookie 里，恒为 `None`
    fn password(&self, profile_id: &str) -> Option<String>;
    /// `None` = 删掉
    fn store_password(&self, profile_id: &str, password: Option<&str>);

    /// App 托管的本机服务；没有的平台（浏览器）返回 `None`，"本机"配置就直接连
    fn local_service(&self) -> Option<Arc<dyn LocalService>>;

    // ---------------- 字体 ----------------

    fn font_source(&self) -> FontSource;

    // ---------------- 下载 ----------------

    fn downloads(&self) -> Downloads;

    /// 把一个同源下载地址交给浏览器（[`Downloads::Browser`] 时才调）。必须在用户手势的同步
    /// 调用栈里调，否则可能被当成弹窗拦掉
    fn hand_off_download(&self, url: &str) -> Result<(), String>;

    // ---------------- HTML 预览 ----------------

    /// 在窗口里建一个网页视图加载 `url`，只准在 `prefix`（这个项目的原始字节前缀）内导航，
    /// 要去别处的 http(s) 地址交给 `open_external`（网页视图的回调线程上调，可能不是前台线程）。平台没有这个能力时返回
    /// `None`，界面退成"在浏览器中打开"的提示；建失败是 `Some(Err(原因))`
    fn html_view(
        &self,
        url: &str,
        prefix: &str,
        open_external: Box<dyn Fn(String) + Send + Sync>,
        window: &mut Window,
    ) -> Option<Result<Rc<dyn HtmlView>, String>>;

    // ---------------- 应用图标 ----------------

    /// 换应用图标（macOS 的 Dock；PNG，已套好平台版式）。没有对应物的平台忽略
    fn set_app_icon(&self, png: &[u8]);

    /// 读回当前的应用图标（TIFF）。只给自动化验证用（锁屏时截不到屏幕）
    fn current_app_icon(&self) -> Option<Vec<u8>> {
        None
    }
}

struct Installed(Rc<dyn Platform>);

impl Global for Installed {}

/// 入口在初始化界面之前挂上平台实现
pub fn install(platform: Rc<dyn Platform>, cx: &mut App) {
    cx.set_global(Installed(platform));
}

/// 当前平台。没 [`install`] 就调是入口的 bug，直接 panic
pub fn get(cx: &App) -> Rc<dyn Platform> {
    cx.global::<Installed>().0.clone()
}
