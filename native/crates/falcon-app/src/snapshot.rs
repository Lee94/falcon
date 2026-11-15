//! AppKit 离屏截图；仅在 snapshot feature 下启用，锁屏时也可核对原生界面。
use dispatch2::DispatchQueue;
use nsh_platform_macos::objc2_app_kit::{
    NSApplication, NSBitmapImageFileType, NSBitmapImageRepPropertyKey,
};
use nsh_platform_macos::objc2_foundation::NSDictionary;
use objc2::MainThreadMarker;
use std::time::Duration;

pub fn schedule() {
    let Ok(path) = std::env::var("FALCON_SNAPSHOT") else {
        return;
    };
    let delay = std::env::var("FALCON_SNAPSHOT_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8000);
    let _ = DispatchQueue::main().after(
        Duration::from_millis(delay)
            .try_into()
            .expect("valid screenshot delay"),
        move || {
            let result = (|| -> Result<(), String> {
                let mtm = MainThreadMarker::new().ok_or("not on main thread")?;
                let app = NSApplication::sharedApplication(mtm);
                let window = app
                    .windows()
                    .to_vec()
                    .into_iter()
                    .find(|w| w.contentView().is_some())
                    .ok_or("no window")?;
                let view = window.contentView().ok_or("no content view")?;
                view.layoutSubtreeIfNeeded();
                let bounds = view.bounds();
                let rep = view
                    .bitmapImageRepForCachingDisplayInRect(bounds)
                    .ok_or("cannot allocate bitmap")?;
                view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
                let properties =
                    NSDictionary::<NSBitmapImageRepPropertyKey, objc2::runtime::AnyObject>::new();
                let png = unsafe {
                    rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
                }
                .ok_or("cannot encode PNG")?;
                std::fs::write(&path, png.to_vec()).map_err(|e| e.to_string())?;
                Ok(())
            })();
            match result {
                Ok(()) => eprintln!("snapshot saved: {path}"),
                Err(ref e) => eprintln!("snapshot failed: {e}"),
            }
            if std::env::var("FALCON_SNAPSHOT_QUIT").is_ok_and(|v| v == "1") {
                std::process::exit(if result.is_ok() { 0 } else { 1 });
            }
        },
    );
}
