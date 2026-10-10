//! Falcon 原生客户端（macOS / Windows）。界面全在 falcon-ui，这里是入口与平台能力的原生实现
//! （falcon-platform 的 trait）：偏好 / 配置 / 工作区存数据目录的 JSON 文件、访问密码进钥匙串、
//! App 托管本机服务、HTML 预览用 wry、Dock 图标走 AppKit。
//!
//! 会话活在 launchd 服务里，不在 App 进程里——关掉最后一个窗口就退出 App（= 全部 Detach）。

// Windows release：GUI 子系统，双击 / 开始菜单启动时不再带出一个黑色控制台窗口。代价是日志
// （eprintln）没处去了——要看日志用 debug 构建（`cargo run -p falcon-desktop`），它仍是控制台程序
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod dock;
#[cfg(feature = "webview")]
mod html_view;
mod http;
mod local_service;
mod platform;

use std::rc::Rc;

fn main() {
    init_logging();

    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(|cx| {
        http::install(cx);
        falcon_ui::init(Rc::new(platform::Desktop::new()), cx);

        // 关掉最后一个窗口就退出 App：会话活在 launchd 服务里，退出 = 全部 Detach
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        falcon_ui::open_startup_window(cx);
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
