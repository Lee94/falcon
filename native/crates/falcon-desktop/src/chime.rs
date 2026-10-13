//! 「跑完了」的提示音（`Platform::play_chime`）。用系统自带的声音，不往二进制里塞音频文件、
//! 不为一声叮拖进一整套音频输出库（cpal / rodio）：
//!
//! - macOS：`NSSound` 的 Glass——/System/Library/Sounds 里的系统音，"跑完了"这类提醒的惯用声
//!   （Claude Code 文档里教人在 hook 里 `afplay` 的也是它）。`soundNamed:` 返回的是共享实例，
//!   上一声还没放完时 `play` 直接返回 false，几个会话同时跑完不会叠成一串；
//! - Windows：`MessageBeep(MB_ICONASTERISK)`，异步放用户在「声音」设置里给"星号"配的那个声音。
//!   **没在 Windows 真机上听过**，只过了交叉编译；
//! - 其它平台：不响。

pub fn play() {
    #[cfg(target_os = "macos")]
    mac::play();
    #[cfg(windows)]
    win::play();
}

#[cfg(target_os = "macos")]
mod mac {
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSString;

    pub fn play() {
        match NSSound::soundNamed(&NSString::from_str("Glass")) {
            Some(sound) => {
                sound.play();
            }
            None => log::warn!("系统提示音 Glass 找不到，没响"),
        }
    }
}

#[cfg(windows)]
mod win {
    // 只用到 user32 的一个函数，自己声明，不为它加 windows / windows-sys 依赖
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBeep(u_type: u32) -> i32;
    }

    const MB_ICONASTERISK: u32 = 0x40;

    pub fn play() {
        // 失败（没有声卡、声音方案是"无声"）只返回 0，不值得报
        unsafe { MessageBeep(MB_ICONASTERISK) };
    }
}
