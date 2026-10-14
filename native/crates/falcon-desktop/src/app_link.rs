//! 把 `zed://`、`vscode://` 这类链接交给认领它的应用（「用 Zed / VS Code 打开」）。
//!
//! macOS 先问 Launch Services 有没有应用认领这个协议：GPUI 的 `open_url` 没人认领时只在
//! 日志里记一笔，用户点了毫无反应，分不清是没装还是没点上。Windows 没人认领时系统自己弹
//! 「选一个应用来打开」，不必再问

use gpui_kit::App;

/// false = 没有应用认领这个协议
pub fn open(url: &str, cx: &App) -> bool {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        mac::open(url)
    }
    #[cfg(not(target_os = "macos"))]
    {
        cx.open_url(url);
        true
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    pub fn open(url: &str) -> bool {
        // 链接由 falcon-core 拼、全是 ASCII；解析不了算没人认领，界面照样有提示
        let Some(ns_url) = NSURL::URLWithString(&NSString::from_str(url)) else {
            log::warn!("链接解析不了：{url}");
            return false;
        };
        let workspace = NSWorkspace::sharedWorkspace();
        if workspace.URLForApplicationToOpenURL(&ns_url).is_none() {
            return false;
        }
        if !workspace.openURL(&ns_url) {
            log::warn!("系统没打开 {url}");
        }
        true
    }
}
