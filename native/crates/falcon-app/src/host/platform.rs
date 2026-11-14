//! `falcon-platform::Platform` 的原生全能宿主实现（macOS / Windows / Linux / Android / TUI，ADR 0028）。
//!
//! 数据目录（`FALCON_NATIVE_DATA_DIR` 可改指临时目录，测试 / 自动化验证时绝不触碰用户真实的偏好与工作区）：
//! - macOS：`~/Library/Application Support/com.falcon.Falcon/`
//! - Windows：`%APPDATA%\falcon\Falcon\data\`
//! - Linux：`~/.local/share/falcon/`
//! - Android：`/data/user/<uid/100000>/com.falcon.android/files/falcon-native/`（首次启动自动导入旧版 `shared_prefs/falcon_android_prefs.xml`）
//!
//! 写入一律走"临时文件 + 改名"，写到一半崩了不会留下半截 JSON。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use falcon_platform::{AppUpdater, LocalService, Platform, PlatformInfo};

/// "本机"配置的缺省基址：没装过服务、也没用 `FALCON_LOCAL_URL` 改指别处时
const LOCAL_URL: &str = "http://127.0.0.1:4923";

pub struct HostPlatform {
    data_dir: PathBuf,
}

impl Default for HostPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl HostPlatform {
    pub fn new() -> Self {
        let data_dir = resolve_host_data_dir();
        #[cfg(target_os = "android")]
        if let Some(app_root) = android_app_root() {
            let xml = app_root.join("shared_prefs").join("falcon_android_prefs.xml");
            let _ = super::android_migration::migrate_if_needed(&xml, &data_dir);
        }
        Self { data_dir }
    }

    fn workspace_path(&self, profile_id: &str) -> PathBuf {
        self.data_dir.join("servers").join(profile_id).join("workspace.json")
    }
}

fn resolve_host_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FALCON_NATIVE_DATA_DIR")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    #[cfg(target_os = "android")]
    if let Some(root) = android_app_root() {
        return root.join("files").join("falcon-native");
    }
    directories::ProjectDirs::from("com", "falcon", "Falcon")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("falcon-native"))
}

#[cfg(target_os = "android")]
fn android_app_root() -> Option<PathBuf> {
    let pkg = std::fs::read("/proc/self/cmdline")
        .ok()
        .and_then(|bytes| {
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            std::str::from_utf8(&bytes[..end])
                .ok()
                .map(|s| s.split(':').next().unwrap_or(s).trim().to_string())
        })
        .filter(|s| !s.is_empty() && s.contains('.'))
        .unwrap_or_else(|| "com.falcon.android".to_string());

    let uid = unsafe { libc::getuid() };
    let user_id = uid / 100_000;
    for candidate in [
        PathBuf::from(format!("/data/user/{user_id}/{pkg}")),
        PathBuf::from(format!("/data/data/{pkg}")),
    ] {
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Platform for HostPlatform {
    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            mac: cfg!(target_os = "macos"),
            windows: cfg!(windows),
        }
    }

    fn load_prefs(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.data_dir.join("prefs.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn store_pref(&self, _key: &str, _value: &str, all: &BTreeMap<String, String>) {
        let path = self.data_dir.join("prefs.json");
        if let Err(err) = write_atomic(&path, &serde_json::to_vec_pretty(all).unwrap_or_default()) {
            log::warn!("偏好写盘失败（{}）：{err}", path.display());
        }
    }

    fn load_workspace(&self, profile_id: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace_path(profile_id)).ok()
    }

    fn store_workspace(&self, profile_id: &str, json: &str) {
        if let Err(err) = write_atomic(&self.workspace_path(profile_id), json.as_bytes()) {
            log::warn!("工作区写盘失败：{err}");
        }
    }

    fn local_server_url(&self) -> String {
        if let Ok(url) = std::env::var("FALCON_LOCAL_URL")
            && !url.trim().is_empty()
        {
            return url;
        }
        super::local_service::installed_service_args()
            .map(|a| a.local_url())
            .unwrap_or_else(|| LOCAL_URL.to_string())
    }

    fn load_profiles(&self) -> Option<String> {
        std::fs::read_to_string(self.data_dir.join("profiles.json")).ok()
    }

    fn store_profiles(&self, json: &str) {
        if let Err(err) = write_atomic(&self.data_dir.join("profiles.json"), json.as_bytes()) {
            log::warn!("服务端配置写盘失败：{err}");
        }
    }

    fn password(&self, profile_id: &str) -> Option<String> {
        super::passwords::get(&self.data_dir, profile_id)
    }

    fn store_password(&self, profile_id: &str, password: Option<&str>) {
        super::passwords::set(&self.data_dir, profile_id, password);
    }

    fn local_service(&self) -> Option<Arc<dyn LocalService>> {
        #[cfg(target_os = "android")]
        {
            None
        }
        #[cfg(not(target_os = "android"))]
        {
            Some(Arc::new(super::local_service::BundledService))
        }
    }

    fn downloads_dir(&self) -> PathBuf {
        directories::UserDirs::new()
            .and_then(|u| {
                u.download_dir()
                    .map(Path::to_path_buf)
                    .or_else(|| Some(u.home_dir().to_path_buf()))
            })
            .unwrap_or_else(std::env::temp_dir)
    }

    fn open_app_link(&self, url: &str) -> Option<bool> {
        super::app_link::open(url)
    }

    fn set_app_icon(&self, png: &[u8]) {
        super::dock::set_icon(png);
    }

    fn current_app_icon(&self) -> Option<Vec<u8>> {
        super::dock::current_icon_tiff()
    }

    fn play_chime(&self) {
        super::chime::play();
    }

    fn app_updater(&self) -> Option<Arc<dyn AppUpdater>> {
        #[cfg(any(target_os = "macos", windows))]
        {
            Some(Arc::new(super::app_update::GithubUpdater))
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            None
        }
    }

    fn schedule_relaunch(&self, bundle: &Path) -> Result<(), String> {
        super::app_update::schedule_relaunch(bundle)
    }

    fn instance_mark(&self) -> String {
        super::menu_identity::current_instance_mark()
    }
}
