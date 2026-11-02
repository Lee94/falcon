//! 平台能力接口。
//!
//! 界面层（falcon-ui）不写平台特定的系统调用，凡是"这台机器上有没有 / 怎么做"
//! 的事都问这里的 [`Platform`]：偏好与工作区存哪儿、服务端从哪儿来、访问密码、本机服务、
//! 下载目录、HTML 预览视图、交给别的应用的链接、应用图标。入口（falcon-desktop）在开窗口之前用
//! [`install`] 挂一份实现。
//!
//! 只放接口与少量数据类型，不放实现：文件、凭据库、wry、AppKit 在 falcon-desktop。
//! 所有方法都在 GPUI 的前台线程上调（实现可以是 `!Send` 的），
//! 唯一的例外是 [`LocalService`]——它阻塞，要放到后台执行器上跑。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Bounds, Global, Pixels, Window};

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

/// 平台能力。原生入口实现一份，界面层经 [`get`] 取
pub trait Platform {
    fn info(&self) -> PlatformInfo;

    // ---------------- 偏好（KvStore）----------------

    /// 启动时把偏好整份读进来
    fn load_prefs(&self) -> BTreeMap<String, String>;

    /// 一个键改了。`all` 是改完之后的整份
    fn store_pref(&self, key: &str, value: &str, all: &BTreeMap<String, String>);

    // ---------------- 工作区（按服务端配置各一份）----------------

    /// 落盘的工作区（形状沿用 React 版的 `falcon.workspace`）
    fn load_workspace(&self, profile_id: &str) -> Option<String>;
    fn store_workspace(&self, profile_id: &str, json: &str);

    // ---------------- 服务端 ----------------

    /// "本机"这条配置此刻该连的基址（每次启动现算）
    fn local_server_url(&self) -> String;

    /// 服务端配置表（JSON）
    fn load_profiles(&self) -> Option<String>;
    fn store_profiles(&self, json: &str);

    /// 记住的访问密码（按配置分条；原生在 macOS 上是数据目录里的 0600 文件、Windows 是凭据管理器，
    /// macOS 为什么不用钥匙串见 falcon-desktop 的 passwords.rs）
    fn password(&self, profile_id: &str) -> Option<String>;
    /// `None` = 删掉
    fn store_password(&self, profile_id: &str, password: Option<&str>);

    /// App 托管的本机服务
    fn local_service(&self) -> Option<Arc<dyn LocalService>>;

    // ---------------- 下载 ----------------

    /// 多个文件下载时缺省落在这个目录（"下载"文件夹）
    fn downloads_dir(&self) -> PathBuf;

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

    // ---------------- 交给别的应用 ----------------

    /// 把自定义协议的链接（`zed://`、`vscode://`，「用编辑器打开」）交给认领它的应用。
    /// false = 确定没有应用认领这个协议（编辑器没装）；问不到的平台一律 true，没人认领时由
    /// 系统自己提示
    fn open_app_link(&self, url: &str, cx: &App) -> bool;

    // ---------------- 应用图标 ----------------

    /// 换应用图标（macOS 的 Dock；PNG，已套好平台版式）。没有对应物的平台忽略
    fn set_app_icon(&self, png: &[u8]);

    /// 读回当前的应用图标（TIFF）。只给自动化验证用（锁屏时截不到屏幕）
    fn current_app_icon(&self) -> Option<Vec<u8>> {
        None
    }

    // ---------------- 提示音 ----------------

    /// 响一声「跑完了」（程序经 OSC 7501 报告 done / error，ADR 0020 决定六；设置里的试听）。
    /// 原生用系统自带的提示音；放不出来（没有声卡）就静默不响，不报错——这是锦上添花
    fn play_chime(&self);
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
