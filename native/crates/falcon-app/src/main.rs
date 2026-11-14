//! `falcon-app` 二进制入口（ADR 0028）。

// Windows release：GUI 子系统，双击 / 开始菜单启动时不再带出一个黑色控制台窗口。代价是日志
// （eprintln）没处去了——要看日志用 debug 构建（`cargo run -p falcon-app`），它仍是控制台程序
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    init_logging();
    if let Err(err) = falcon_app::run_cli() {
        eprintln!("falcon-app error: {err:#}");
        std::process::exit(1);
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
