//! falcon-platform 的原生实现。
//!
//! 数据目录：`~/Library/Application Support/com.falcon.Falcon/`（`FALCON_NATIVE_DATA_DIR` 可改指临时目录，
//! 测试 / 自动化验证时别动用户真实的偏好与工作区）：
//!
//! - `prefs.json`：旧 React 版（现在是浏览器版）存在 localStorage 里的那些键，形状逐字一致；
//! - `profiles.json`：服务端配置表与上次连上的那台；
//! - `servers/<配置 id>/workspace.json`：按服务端配置分开存的工作区。
//!
//! 写入一律走"临时文件 + 改名"，写到一半崩了不会留下半截 JSON。记住的访问密码按配置 id 分条，
//! macOS 上是数据目录里的 `passwords.json`（0600，为什么不用钥匙串见 passwords.rs），Windows 进凭据管理器。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use falcon_platform::{
    Downloads, FontSource, HtmlView, LocalService, Platform, PlatformInfo, ServerSource,
};
use gpui_kit::{App, Window};

/// "本机"配置的缺省基址：没装过服务、也没用 FALCON_LOCAL_URL 改指别处时
const LOCAL_URL: &str = "http://127.0.0.1:4923";

pub struct Desktop {
    data_dir: PathBuf,
}

impl Desktop {
    pub fn new() -> Self {
        Self { data_dir: data_dir() }
    }

    fn workspace_path(&self, profile_id: &str) -> PathBuf {
        self.data_dir.join("servers").join(profile_id).join("workspace.json")
    }
}

fn data_dir() -> PathBuf {
    // 测试 / 自动化验证时指到临时目录，别动用户真实的偏好与工作区
    if let Ok(dir) = std::env::var("FALCON_NATIVE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("com", "falcon", "Falcon")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("falcon-native"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl Platform for Desktop {
    fn info(&self) -> PlatformInfo {
        PlatformInfo { mac: cfg!(target_os = "macos"), windows: cfg!(windows), browser: false }
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

    fn server_source(&self) -> ServerSource {
        // "本机"连哪儿每次启动现算，不信存下来的：开发时 FALCON_LOCAL_URL 指向 cargo run -p falcon-server
        // 或临时实例；否则按已安装服务的端口（自定义过 --port 的也能连上）；都没有才是默认 4923
        let local_url = std::env::var("FALCON_LOCAL_URL").ok().unwrap_or_else(|| {
            crate::local_service::installed_service_args()
                .map(|a| a.local_url())
                .unwrap_or_else(|| LOCAL_URL.to_string())
        });
        ServerSource::Profiles { local_url }
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
        crate::passwords::get(&self.data_dir, profile_id)
    }

    fn has_password_store(&self) -> bool {
        true
    }

    fn redirect_after_login(&self) -> bool {
        false
    }

    fn on_resume(&self, _f: Box<dyn Fn()>) {}

    fn store_password(&self, profile_id: &str, password: Option<&str>) {
        crate::passwords::set(&self.data_dir, profile_id, password);
    }

    fn local_service(&self) -> Option<Arc<dyn LocalService>> {
        Some(Arc::new(crate::local_service::BundledService))
    }

    fn font_source(&self) -> FontSource {
        FontSource::Embedded(falcon_ui::embedded_fallback_fonts())
    }

    fn downloads(&self) -> Downloads {
        let dir = directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(Path::to_path_buf).or_else(|| Some(u.home_dir().to_path_buf())))
            .unwrap_or_else(std::env::temp_dir);
        Downloads::LocalDir(dir)
    }

    fn hand_off_download(&self, _url: &str) -> Result<(), String> {
        Err("原生客户端自己写盘，不交给浏览器".into())
    }

    #[cfg(feature = "webview")]
    fn html_view(
        &self,
        url: &str,
        prefix: &str,
        open_external: Box<dyn Fn(String) + Send + Sync>,
        window: &mut Window,
    ) -> Option<Result<Rc<dyn HtmlView>, String>> {
        Some(crate::html_view::create(url, prefix, open_external, window))
    }

    #[cfg(not(feature = "webview"))]
    fn html_view(
        &self,
        _url: &str,
        _prefix: &str,
        _open_external: Box<dyn Fn(String) + Send + Sync>,
        _window: &mut Window,
    ) -> Option<Result<Rc<dyn HtmlView>, String>> {
        None
    }

    fn open_app_link(&self, url: &str, cx: &App) -> bool {
        crate::app_link::open(url, cx)
    }

    #[cfg(feature = "webview")]
    fn remote_browser_view(
        &self,
        url: &str,
        prefix: &str,
        open_external: Box<dyn Fn(String) + Send + Sync>,
        window: &mut Window,
    ) -> Option<Result<Rc<dyn HtmlView>, String>> {
        Some(crate::html_view::create_remote_browser(url, prefix, open_external, window))
    }

    fn set_app_icon(&self, png: &[u8]) {
        crate::dock::set_icon(png);
    }

    fn current_app_icon(&self) -> Option<Vec<u8>> {
        crate::dock::current_icon_tiff()
    }

    fn play_chime(&self) {
        crate::chime::play();
    }
}
