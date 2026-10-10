//! Falcon 原生客户端的可执行入口；一切都在 lib 里（src/lib.rs）。

// Windows release：GUI 子系统，双击 / 开始菜单启动时不再带出一个黑色控制台窗口。代价是日志
// （eprintln）没处去了——要看日志用 debug 构建（`cargo run -p falcon-app`），它仍是控制台程序
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    falcon_app::run_desktop();
}
