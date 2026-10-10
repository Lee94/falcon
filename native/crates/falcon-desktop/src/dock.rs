//! 运行时换 Dock 图标（ADR 0018）。只有 macOS 有对应物；其它平台忽略（Windows 的窗口图标
//! 跟的是 build.rs 编进 exe 的默认图标）。

/// 换 Dock 图标（整个 App 一个；几台服务端各开一个窗口时，最后换的那台说了算）
pub fn set_icon(png: &[u8]) {
    #[cfg(target_os = "macos")]
    mac::set_icon(png);
    #[cfg(not(target_os = "macos"))]
    let _ = png;
}

/// 从 AppKit 读回当前的 Dock 图标（TIFF）。锁屏时截不到屏幕，自动化的 `dock:` 步骤靠它验证
pub fn current_icon_tiff() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    return mac::current_icon_tiff();
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2::AnyThread;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    pub fn set_icon(png: &[u8]) {
        // GPUI 的前台回调都在主线程上；拿不到标记说明被挪到了别处，宁可不换也别碰 AppKit
        let Some(mtm) = MainThreadMarker::new() else {
            log::warn!("不在主线程上，没换 Dock 图标");
            return;
        };
        let data = NSData::with_bytes(png);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            log::warn!("Dock 图标解不开（{} 字节）", png.len());
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }

    pub fn current_icon_tiff() -> Option<Vec<u8>> {
        let mtm = MainThreadMarker::new()?;
        let image = NSApplication::sharedApplication(mtm).applicationIconImage()?;
        Some(image.TIFFRepresentation()?.to_vec())
    }
}
