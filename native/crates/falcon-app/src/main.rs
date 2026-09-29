//! Falcon 原生客户端（GPUI）。设计见 docs/design/gpui-client.md。
//!
//! 一台 falcon 服务端一个窗口；本机服务由 App 托管（local_service.rs）。界面文案一律走
//! `t!`（locales/zh-CN.json，由 native/scripts/export-i18n.mjs 从 web 的 i18n.ts 导出）。

mod actions;
mod app_icon;
#[cfg(feature = "automation")]
mod automation;
mod canvas;
mod dialogs;
mod fonts;
mod http;
mod labels;
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
mod ui;
mod window;
mod workspace;
mod zoom;


rust_i18n::i18n!("locales", fallback = "zh-CN");

fn main() {
    init_logging();
    rust_i18n::set_locale("zh-CN");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx| {
            gpui_kit::init(cx);
            fonts::register(cx);
            http::install(cx);
            prefs::Prefs::init(cx);
            zoom::init(cx);
            profiles::Profiles::init(cx);
            theme::init(cx);
            actions::init(cx);

            // 关掉最后一个窗口就退出 App：会话活在 launchd 服务里，退出 = 全部 Detach
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            let open = profiles::Profiles::global(cx).last_open();
            for profile in open {
                window::open_server_window(profile, cx);
            }
            cx.activate(true);
        });
}

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
