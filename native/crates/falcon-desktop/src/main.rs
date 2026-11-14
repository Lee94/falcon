//! Falcon 原生客户端（macOS / Windows）。界面全在 falcon-ui，这里是入口与平台能力的原生实现
//! （falcon-platform 的 trait）：偏好 / 配置 / 工作区存数据目录的 JSON 文件、记住的访问密码
//! （macOS 也是数据目录里的文件，Windows 进凭据管理器）、App 托管本机服务、HTML 预览用 wry、Dock 图标走 AppKit。
//!
//! 会话活在独立的本机服务里，不在 App 进程里（macOS 是 launchd，Windows 是登录自启的
//! 计划任务）。macOS 上关窗口只收起界面，菜单栏状态项留下，
//! 从那里启动 / 停止 / 重启本机服务，或把窗口再打开；退出（⌘Q）才是全部 Detach。
//! Windows 没有菜单栏，关掉最后一扇窗口就退出（服务在计划任务里继续跑）。

// Windows release：GUI 子系统，双击 / 开始菜单启动时不再带出一个黑色控制台窗口。代价是日志
// （eprintln）没处去了——要看日志用 debug 构建（`cargo run -p falcon-desktop`），它仍是控制台程序
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app_link;
mod app_update;
mod chime;
mod dock;
#[cfg(feature = "webview")]
mod html_view;
mod http;
mod local_service;
#[cfg(target_os = "macos")]
mod menu_bar;
mod passwords;
mod platform;

use std::rc::Rc;

fn main() {
    init_logging();

    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(|cx| {
        http::install(cx);
        falcon_ui::init(Rc::new(platform::Desktop::new()), cx);
        #[cfg(target_os = "macos")]
        menu_bar::install(cx);

        // macOS：最后一扇窗口关掉之后进程还在，菜单栏继续管 launchd 里的服务。
        // 激活策略改成 Accessory，Dock 图标一起收起，否则会剩一个没有窗口的普通应用。
        // 自动化脚本靠关窗结束进程，不走这条路。Windows 没有菜单栏，关窗即退出
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                if keep_menu_bar_after_close() {
                    cx.set_activation_policy(gpui_kit::ActivationPolicy::Accessory);
                } else {
                    cx.quit();
                }
            }
        })
        .detach();

        falcon_ui::open_startup_window(cx);
        cx.activate(true);
    });
}

fn keep_menu_bar_after_close() -> bool {
    #[cfg(target_os = "macos")]
    {
        menu_bar::installed() && std::env::var_os("FALCON_AUTOMATE").is_none()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
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
