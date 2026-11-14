//! 把 `zed://`、`vscode://` 这类链接交给认领它的应用（「用 Zed / VS Code 打开」）。
//!
//! macOS 先问 Launch Services 有没有应用认领这个协议：没人认领时返回 `Some(false)` 让界面弹提示；
//! 其它平台返回 `None` 交由上层 UI 框架自己的 `open_url` 打开（系统在没人认领时自行提示）。

/// `Some(false)` = 确定没有应用认领这个协议；`Some(true)` = 已直接打开；`None` = 交给上层 `open_url`
pub fn open(url: &str) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        Some(mac::open(url))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use nsh_platform_macos::objc2_app_kit::NSWorkspace;
    use nsh_platform_macos::objc2_foundation::{NSString, NSURL};

    pub fn open(url: &str) -> bool {
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
