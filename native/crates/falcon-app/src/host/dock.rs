//! 运行时换 Dock 图标（ADR 0018）。只有 macOS 有对应物；其它平台忽略（Windows 的窗口图标
//! 跟的是 build.rs 编进 exe 的默认图标）。

/// 换 Dock 图标（整个 App 一个；几台服务端各开一个窗口时，最后换的那台说了算）
pub fn set_icon(png: &[u8]) {
    #[cfg(target_os = "macos")]
    mac::set_icon(png);
    #[cfg(not(target_os = "macos"))]
    let _ = png;
}

/// 从 AppKit 读回当前的 Dock 图标（TIFF）。自动化验证步骤靠它核对
pub fn current_icon_tiff() -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    return mac::current_icon_tiff();
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(target_os = "macos")]
mod mac {
    use dispatch2::DispatchQueue;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    fn apply_on_main(mtm: MainThreadMarker, png: &[u8]) {
        let data = NSData::with_bytes(png);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            log::warn!("Dock 图标解不开（{} 字节）", png.len());
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }

    pub fn set_icon(png: &[u8]) {
        if let Some(mtm) = MainThreadMarker::new() {
            apply_on_main(mtm, png);
            return;
        }
        // NSH 组件在独立引擎线程运行，AppKit 图标更新派回主队列执行
        let bytes = png.to_vec();
        DispatchQueue::main().exec_async(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                apply_on_main(mtm, &bytes);
            }
        });
    }

    pub fn current_icon_tiff() -> Option<Vec<u8>> {
        let mtm = MainThreadMarker::new()?;
        let image = NSApplication::sharedApplication(mtm).applicationIconImage()?;
        Some(image.TIFFRepresentation()?.to_vec())
    }
}
