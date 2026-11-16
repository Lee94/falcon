//! 调试截图：`FALCON_SNAPSHOT=<路径>.png` 时，窗口打开 `FALCON_SNAPSHOT_DELAY_MS`（默认 2500）
//! 毫秒后把画面存成 PNG；再设 `FALCON_SNAPSHOT_QUIT=1` 就截完退出。
//!
//! 走 `Window::render_to_image()`（Metal 回读当前帧的 scene），锁屏 / 远程时也能拿到真实画面，
//! 不依赖屏幕录制权限。只在 `--features snapshot` 下编进来（它要 gpui-kit 的 test-support）。

use gpui_kit::{AnyWindowHandle, App};

#[cfg(feature = "snapshot")]
pub fn schedule(window: AnyWindowHandle, cx: &mut App) {
    use std::time::Duration;

    use gpui_kit::AppContext as _;

    let Ok(path) = std::env::var("FALCON_SNAPSHOT") else {
        return;
    };
    let delay = std::env::var("FALCON_SNAPSHOT_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2500u64);
    let quit = std::env::var("FALCON_SNAPSHOT_QUIT").is_ok_and(|v| v == "1");
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(delay))
            .await;
        let result = cx.update_window(window, |_, window, _| window.render_to_image());
        match result {
            Ok(Ok(img)) => match img.save(&path) {
                Ok(()) => log::info!("截图已保存：{path}（{}x{}）", img.width(), img.height()),
                Err(e) => log::error!("截图保存失败：{e}"),
            },
            Ok(Err(e)) => log::error!("render_to_image 失败：{e:#}"),
            Err(e) => log::error!("截图时窗口已不在：{e:#}"),
        }
        if quit {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

#[cfg(not(feature = "snapshot"))]
pub fn schedule(_window: AnyWindowHandle, _cx: &mut App) {}
