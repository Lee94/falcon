//! Falcon 客户端（GPUI）。设计见 docs/design/gpui-client.md，浏览器版见
//! docs/design/rust-unification.md。
//!
//! 一套代码两种产物：原生桌面（`src/main.rs` → [`run_desktop`]）与浏览器 wasm
//! （`falcon-web` crate → [`run_web`]）。一台 falcon 服务端一个窗口；原生上本机服务由 App
//! 托管（local_service.rs），浏览器里服务端就是页面的源。界面文案一律走 `t!`
//! （locales/zh-CN.json——原从 React 前端的 i18n.ts 导出，React 删除后它就是文案的真相来源，
//! 直接改它；原生独有的 key 在 i18n-native/）。

mod actions;
mod app_icon;
#[cfg(feature = "automation")]
mod automation;
mod canvas;
mod dialogs;
mod fonts;
#[cfg(not(target_family = "wasm"))]
mod http;
mod labels;
#[cfg(not(target_family = "wasm"))]
mod local_service;
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
#[cfg(target_family = "wasm")]
mod web;
mod window;
mod window_controls;
mod workspace;
mod zoom;

rust_i18n::i18n!("locales", fallback = "zh-CN");

/// 各平台共用的初始化：字体、主题、偏好、动作表……开窗口之前跑完。
fn init_app(cx: &mut gpui_kit::App) {
    gpui_kit::init(cx);
    toasts::init(cx);
    fonts::register(cx);
    // 浏览器里 gpui-web 自带 fetch 版的 HTTP 客户端（single_threaded_web 已经挂上了）
    #[cfg(not(target_family = "wasm"))]
    http::install(cx);
    prefs::Prefs::init(cx);
    zoom::init(cx);
    profiles::Profiles::init(cx);
    theme::init(cx);
    actions::init(cx);
}

/// 原生桌面入口。
#[cfg(not(target_family = "wasm"))]
pub fn run_desktop() {
    init_logging();
    rust_i18n::set_locale("zh-CN");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            init_app(cx);

            // 关掉最后一个窗口就退出 App：会话活在 launchd 服务里，退出 = 全部 Detach
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let profile = profiles::Profiles::global(cx).startup_profile();
            window::open_server_window(profile, cx);
            cx.activate(true);
        });
}

/// 浏览器入口：单线程 gpui-web（不要 SharedArrayBuffer / COOP·COEP，见设计文档决定七），
/// 整个文档一块 canvas、一个窗口，连的就是页面所在的 falcon 服务端。
#[cfg(target_family = "wasm")]
pub fn run_web() {
    gpui_kit::platform::web_init();
    rust_i18n::set_locale("zh-CN");

    // 图标 SVG 按需从 `<页面源>/assets/icons/*.svg` 取（gpui-kit-assets 的 wasm 实现），
    // 构建脚本把 gpui-kit-assets 的 icons 拷进产物。它拿 reqwest 去取，相对地址不认，得给全
    let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
    gpui_kit::platform::single_threaded_web()
        .with_assets(gpui_kit::assets::Assets::new(origin))
        .run(|cx| {
            init_app(cx);
            let profile = profiles::Profiles::global(cx).startup_profile();
            window::open_server_window(profile, cx);
        });
}

#[cfg(not(target_family = "wasm"))]
fn init_logging() {
    struct Logger;
    impl log::Log for Logger {
        fn enabled(&self, m: &log::Metadata) -> bool {
            let max = if std::env::var("FALCON_LOG").is_ok_and(|v| v == "debug") {
                log::Level::Debug
            } else {
                log::Level::Info
            };
            m.level() <= max && !m.target().starts_with("naga") && !m.target().starts_with("wgpu")
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_logger(&Logger).map(|()| log::set_max_level(log::LevelFilter::Debug));
}
