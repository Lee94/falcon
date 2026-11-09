//! Falcon 客户端界面（GPUI）。设计见 docs/design/gpui-client.md。
//!
//! 平台能力（偏好存哪儿、服务端从哪儿来、本机服务、下载、HTML 预览、应用图标）
//! 都问 falcon-platform 的 [`Platform`]，由入口在 [`init`] 时交进来。一台 falcon 服务端一个窗口。
//! 界面文案一律走 `t!`（locales/zh-CN.json——原从 React 前端的 i18n.ts 导出，React 删除后它就是
//! 文案的真相来源，直接改它；平台独有的 key 挂在 `native.<区域>` 下）。

mod actions;
mod app_icon;
mod app_update;
#[cfg(feature = "automation")]
mod automation;
mod canvas;
mod chime;
mod dialogs;
mod fonts;
mod labels;
mod local_time;
mod menus;
mod motion;
mod palette;
mod panels;
mod prefs;
mod profiles;
mod rightbar;
mod session_icon;
mod sidebar;
mod snapshot;
mod terminal;
mod theme;
mod toasts;
mod ui;
mod window;
mod window_background;
mod window_controls;
mod workspace;
mod zoom;

use std::rc::Rc;

use falcon_platform::Platform;
use gpui_kit::App;
use rust_i18n::t;

rust_i18n::i18n!("locales", fallback = "zh-CN");

/// 初始化界面：挂上平台实现，再是字体、主题、偏好、动作表……开窗口之前跑完。
/// 平台自带的东西（原生的 HTTP 客户端、日志）由入口在这之前挂好
pub fn init(platform: Rc<dyn Platform>, cx: &mut App) {
    rust_i18n::set_locale("zh-CN");
    falcon_platform::install(platform, cx);
    gpui_kit::init(cx);
    toasts::init(cx);
    fonts::register(cx);
    prefs::Prefs::init(cx);
    zoom::init(cx);
    profiles::Profiles::init(cx);
    theme::init(cx);
    actions::init(cx);
}

/// 开启动时那台服务端的窗口：上次连上的（配置还在的话），否则是本机
pub fn open_startup_window(cx: &mut App) {
    let profile = profiles::Profiles::global(cx).startup_profile();
    window::open_server_window(profile, cx);
}

/// 菜单栏在窗口都关着的时候把界面叫回来：有窗口就带到前面，没有就开启动时那一台
pub fn present_window(cx: &mut App) {
    if !window::present(cx) {
        open_startup_window(cx);
    }
}

/// 菜单栏状态项上的固定文案。带 pid 或错误文本的状态行另走 [`menu_bar_running`] / [`menu_bar_failed`]
pub struct MenuBarText {
    pub open: String,
    pub querying: String,
    pub missing: String,
    pub stopped: String,
    pub running: String,
    pub busy_start: String,
    pub busy_stop: String,
    pub busy_restart: String,
    pub no_program: String,
    pub start: String,
    pub stop: String,
    pub restart: String,
    pub quit: String,
}

pub fn menu_bar_text() -> MenuBarText {
    MenuBarText {
        open: t!("native.menuBar.open").to_string(),
        querying: t!("native.menuBar.querying").to_string(),
        missing: t!("native.menuBar.missing").to_string(),
        stopped: t!("native.menuBar.stopped").to_string(),
        running: t!("native.menuBar.running").to_string(),
        busy_start: t!("native.menuBar.busyStart").to_string(),
        busy_stop: t!("native.menuBar.busyStop").to_string(),
        busy_restart: t!("native.menuBar.busyRestart").to_string(),
        no_program: t!("native.menuBar.noProgram").to_string(),
        start: t!("native.menuBar.start").to_string(),
        stop: t!("native.menuBar.stop").to_string(),
        restart: t!("native.menuBar.restart").to_string(),
        quit: t!("native.menu.quit").to_string(),
    }
}

pub fn menu_bar_running(pid: u32) -> String {
    t!("native.menuBar.runningPid", pid = pid).to_string()
}

pub fn menu_bar_failed(error: &str) -> String {
    t!("native.menuBar.failed", error = error).to_string()
}

/// 菜单栏「检查更新」的文案，以及这一项现在能不能点
pub fn app_update_menu_state() -> (String, bool) {
    app_update::app_update_menu_state()
}

/// 菜单栏点了「检查更新」
pub fn check_for_app_update(cx: &mut App) {
    app_update::check_for_app_update(cx);
}

/// ⌘Q、应用菜单和菜单栏状态项上的「退出」都走这里：先问没存的修改，再退出进程
pub fn quit(cx: &mut App) {
    window::quit(cx);
}
