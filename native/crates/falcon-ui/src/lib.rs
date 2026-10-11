//! Falcon 客户端界面（GPUI）。设计见 docs/design/gpui-client.md，浏览器版见
//! docs/design/rust-unification.md。
//!
//! 一套界面、两种产物：原生桌面（falcon-desktop）与浏览器 wasm（falcon-web）。这里不写
//! target cfg——"这台机器上有没有 / 怎么做"的事（偏好存哪儿、服务端从哪儿来、钥匙串、本机
//! 服务、字体、下载、HTML 预览、应用图标）都问 falcon-platform 的 [`Platform`]，由入口在
//! [`init`] 时交进来。一台 falcon 服务端一个窗口。界面文案一律走 `t!`（locales/zh-CN.json——
//! 原从 React 前端的 i18n.ts 导出，React 删除后它就是文案的真相来源，直接改它；原生 / 平台
//! 独有的 key 挂在 `native.<区域>` 下）。

mod actions;
mod app_icon;
#[cfg(feature = "automation")]
mod automation;
mod canvas;
mod dialogs;
mod fonts;
mod labels;
mod local_time;
mod login;
mod menus;
mod palette;
mod panels;
mod prefs;
mod profiles;
mod rightbar;
mod sidebar;
mod snapshot;
mod terminal;
mod theme;
mod toasts;
mod ui;
mod window;
mod window_controls;
mod workspace;
mod zoom;

use std::rc::Rc;

use falcon_platform::Platform;
use gpui_kit::App;

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

/// 嵌进二进制的回退字体（Ioskeley / Maple 中文 / Nerd 图标），原生平台交回
/// `FontSource::Embedded` 用。浏览器版不引用它，链接时整段丢掉
pub fn embedded_fallback_fonts() -> Vec<&'static [u8]> {
    fonts::embedded_fallbacks()
}

/// 开启动时那台服务端的窗口：上次连上的（配置还在的话），否则是本机 / 页面所在的源
pub fn open_startup_window(cx: &mut App) {
    let profile = profiles::Profiles::global(cx).startup_profile();
    window::open_server_window(profile, cx);
}
