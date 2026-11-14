//! 多份 Falcon 客户端同时运行时的菜单栏区分标记（macOS 状态项旁与左侧应用菜单标题）。
//!
//! 本地更新不强制杀死旧进程（保护尚未保存的文件修改），新进程再挂一个状态项。
//! 同时存在 ≥2 份实例时，按「版本号 → 可执行文件修改时间（时:分 → 时:分:秒）→ PID」逐级消歧；
//! 只剩一份时返回空串，菜单栏恢复为单图标。

#[cfg(any(target_os = "macos", test))]
const APP_MENU_NAME: &str = "Falcon";

/// 一份正在跑的 Falcon。`built` 是可执行文件修改时间的当地时分秒
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug)]
pub(crate) struct RunningCopy {
    pub version: String,
    pub built: Option<BuiltAt>,
    pub pid: i32,
    pub is_self: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BuiltAt {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// 查询当前系统上并存的 Falcon 实例并返回本进程的区分标记。
pub fn current_instance_mark() -> String {
    #[cfg(target_os = "macos")]
    {
        status_item_title(&mac::running_copies())
    }
    #[cfg(not(target_os = "macos"))]
    {
        String::new()
    }
}

/// 同时只有一份时返回空串。多份时返回能互相区分的短标记
#[cfg(any(target_os = "macos", test))]
pub(crate) fn status_item_title(copies: &[RunningCopy]) -> String {
    let Some(me) = copies.iter().find(|copy| copy.is_self) else {
        return String::new();
    };
    if copies.len() < 2 {
        return String::new();
    }
    let version = version_label(me.version.trim());
    // 读不到的版本号不能当成「另一版」：否则两份都只显示自己的版本，标记又一样
    let same_version: Vec<_> = copies
        .iter()
        .filter(|copy| {
            let v = copy.version.trim();
            v.is_empty() || v == me.version.trim()
        })
        .collect();
    if same_version.len() == 1 {
        return version;
    }
    if let Some(built) = me.built {
        let minute = |stamp: BuiltAt| (stamp.hour, stamp.minute);
        let same_minute = same_version
            .iter()
            .filter(|copy| copy.built.is_some_and(|stamp| minute(stamp) == minute(built)))
            .count();
        if same_minute == 1 {
            return format!("{version} {:02}:{:02}", built.hour, built.minute);
        }
        let same_second = same_version.iter().filter(|copy| copy.built == Some(built)).count();
        if same_second == 1 {
            return format!("{version} {:02}:{:02}:{:02}", built.hour, built.minute, built.second);
        }
    }
    if me.pid >= 0 {
        return format!("{version} · {}", me.pid);
    }
    version
}

#[cfg(any(target_os = "macos", test))]
fn version_label(version: &str) -> String {
    if version.is_empty() {
        APP_MENU_NAME.to_string()
    } else {
        format!("v{version}")
    }
}

#[cfg(test)]
pub(crate) fn app_menu_title(mark: &str) -> String {
    if mark.is_empty() {
        APP_MENU_NAME.to_string()
    } else {
        format!("{APP_MENU_NAME} {mark}")
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use nsh_platform_macos::objc2::rc::Retained;
    use nsh_platform_macos::objc2_app_kit::{NSRunningApplication, NSWorkspace};
    use nsh_platform_macos::objc2_foundation::{NSBundle, NSString};

    use super::{BuiltAt, RunningCopy};

    pub fn running_copies() -> Vec<RunningCopy> {
        let current = NSRunningApplication::currentApplication();
        let mut found_self = false;
        let mut copies = Vec::new();
        for app in peer_apps(&current) {
            let is_self = same_process(&app, &current);
            found_self |= is_self;
            copies.push(copy_of(&app, is_self));
        }
        if !found_self {
            copies.push(copy_of(&current, true));
        }
        copies
    }

    fn peer_apps(current: &NSRunningApplication) -> Vec<Retained<NSRunningApplication>> {
        if let Some(bundle_id) = nonempty_bundle_id(current) {
            let apps =
                NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(&bundle_id));
            return (0..apps.count()).map(|index| apps.objectAtIndex(index)).collect();
        }
        // `cargo run` 没有 bundle id，按可执行文件名认同一份程序
        let name = executable_name(current);
        let apps = NSWorkspace::sharedWorkspace().runningApplications();
        (0..apps.count())
            .map(|index| apps.objectAtIndex(index))
            .filter(|app| !name.is_empty() && executable_name(app) == name)
            .collect()
    }

    fn copy_of(app: &NSRunningApplication, is_self: bool) -> RunningCopy {
        let version = bundle_short_version(app).unwrap_or_else(|| {
            if is_self {
                env!("CARGO_PKG_VERSION").to_string()
            } else {
                String::new()
            }
        });
        RunningCopy {
            version,
            built: executable_built(app),
            pid: app.processIdentifier() as i32,
            is_self,
        }
    }

    fn bundle_short_version(app: &NSRunningApplication) -> Option<String> {
        let url = app.bundleURL()?;
        let bundle = NSBundle::bundleWithURL(&url)?;
        let value = bundle.objectForInfoDictionaryKey(&NSString::from_str("CFBundleShortVersionString"))?;
        let text = value.downcast_ref::<NSString>()?.to_string();
        let text = text.trim();
        if text.is_empty() { None } else { Some(text.to_string()) }
    }

    fn executable_built(app: &NSRunningApplication) -> Option<BuiltAt> {
        let url = app.executableURL()?;
        let path = url.path()?.to_string();
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        let secs = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(built_at_local(i64::try_from(secs).ok()?))
    }

    fn built_at_local(secs: i64) -> BuiltAt {
        let t = secs as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return BuiltAt {
                hour: 0,
                minute: 0,
                second: 0,
            };
        }
        BuiltAt {
            hour: tm.tm_hour.clamp(0, 23) as u8,
            minute: tm.tm_min.clamp(0, 59) as u8,
            second: tm.tm_sec.clamp(0, 60) as u8,
        }
    }

    fn executable_name(app: &NSRunningApplication) -> String {
        app.executableURL()
            .and_then(|url| url.path())
            .map(|path| path.to_string())
            .and_then(|path| path.rsplit('/').next().map(str::to_string))
            .unwrap_or_default()
    }

    fn nonempty_bundle_id(app: &NSRunningApplication) -> Option<String> {
        let id = app.bundleIdentifier()?.to_string();
        let id = id.trim();
        if id.is_empty() { None } else { Some(id.to_string()) }
    }

    fn same_process(a: &NSRunningApplication, b: &NSRunningApplication) -> bool {
        let left = a.processIdentifier();
        let right = b.processIdentifier();
        left >= 0 && left == right
    }
}

#[cfg(test)]
mod tests {
    use super::{BuiltAt, RunningCopy, app_menu_title, status_item_title};

    fn copy(version: &str, built: Option<(u8, u8, u8)>, pid: i32, is_self: bool) -> RunningCopy {
        RunningCopy {
            version: version.to_string(),
            built: built.map(|(hour, minute, second)| BuiltAt { hour, minute, second }),
            pid,
            is_self,
        }
    }

    #[test]
    fn single_copy_has_no_title() {
        assert_eq!(status_item_title(&[copy("0.3.19", Some((9, 52, 1)), 1, true)]), "");
    }

    #[test]
    fn different_versions_show_the_version() {
        let copies = [
            copy("0.3.18", Some((9, 0, 0)), 1, true),
            copy("0.3.19", Some((10, 0, 0)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.18");
    }

    #[test]
    fn same_version_shows_build_clock() {
        let copies = [
            copy("0.3.19", Some((9, 52, 1)), 1, true),
            copy("0.3.19", Some((16, 40, 2)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.19 09:52");
        let flipped = [
            copy("0.3.19", Some((9, 52, 1)), 1, false),
            copy("0.3.19", Some((16, 40, 2)), 2, true),
        ];
        assert_eq!(status_item_title(&flipped), "v0.3.19 16:40");
    }

    #[test]
    fn unreadable_peer_version_uses_the_clock() {
        let copies = [
            copy("0.3.19", Some((9, 52, 1)), 1, true),
            copy("", Some((16, 40, 2)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.19 09:52");
    }

    #[test]
    fn same_minute_shows_seconds() {
        let copies = [
            copy("0.3.19", Some((9, 52, 1)), 1, true),
            copy("0.3.19", Some((9, 52, 40)), 2, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.19 09:52:01");
    }

    #[test]
    fn same_build_shows_pid() {
        let copies = [
            copy("0.3.19", Some((9, 52, 1)), 11, true),
            copy("0.3.19", Some((9, 52, 1)), 22, false),
        ];
        assert_eq!(status_item_title(&copies), "v0.3.19 · 11");
    }

    #[test]
    fn missing_build_time_uses_pid() {
        let copies = [copy("0.3.19", None, 11, true), copy("0.3.19", None, 22, false)];
        assert_eq!(status_item_title(&copies), "v0.3.19 · 11");
    }

    #[test]
    fn no_self_is_blank() {
        assert_eq!(status_item_title(&[copy("0.3.19", None, 11, false)]), "");
    }

    #[test]
    fn app_menu_uses_the_same_mark() {
        assert_eq!(app_menu_title(""), "Falcon");
        assert_eq!(app_menu_title("v0.3.19 09:52"), "Falcon v0.3.19 09:52");
    }
}
