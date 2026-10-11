//! falcon-platform 的浏览器实现。
//!
//! 偏好与工作区直接读写 localStorage，键沿用旧 React 版的那些（`falcon.themes` / `falcon.workspace` …）——
//! React 版老用户的主题、终端设置与排布原样继承。localStorage 天然按源分，一个页面只连一台服务端，
//! 所以不分配置 id。

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use falcon_platform::{Downloads, FontSource, HtmlView, LocalService, Platform, PlatformInfo, ServerSource};
use gpui_kit::Window;

use crate::dom::{self, local_storage};

/// Falcon 的 localStorage 键都以它开头（沿用 React 版）；启动时把这些键一次读进来
const KEY_PREFIX: &str = "falcon.";
const WORKSPACE_KEY: &str = "falcon.workspace";

pub struct Browser;

fn set_item(key: &str, value: &str) {
    if let Some(storage) = local_storage()
        && let Err(err) = storage.set_item(key, value)
    {
        log::warn!("写入 localStorage 失败（{key}）：{err:?}");
    }
}

impl Platform for Browser {
    fn info(&self) -> PlatformInfo {
        let (mac, windows) = dom::os();
        PlatformInfo { mac, windows, browser: true }
    }

    fn load_prefs(&self) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        if let Some(storage) = local_storage() {
            let len = storage.length().unwrap_or(0);
            for i in 0..len {
                if let Ok(Some(key)) = storage.key(i)
                    && key.starts_with(KEY_PREFIX)
                    && let Ok(Some(value)) = storage.get_item(&key)
                {
                    values.insert(key, value);
                }
            }
        }
        values
    }

    fn store_pref(&self, key: &str, value: &str, _all: &BTreeMap<String, String>) {
        set_item(key, value);
    }

    fn load_workspace(&self, _profile_id: &str) -> Option<String> {
        local_storage()?.get_item(WORKSPACE_KEY).ok().flatten()
    }

    fn store_workspace(&self, _profile_id: &str, json: &str) {
        set_item(WORKSPACE_KEY, json);
    }

    fn server_source(&self) -> ServerSource {
        ServerSource::PageOrigin { origin: dom::origin(), host: dom::host() }
    }

    fn load_profiles(&self) -> Option<String> {
        None
    }

    fn store_profiles(&self, _json: &str) {}

    // 浏览器里没有钥匙串，登录态在 cookie 里
    fn password(&self, _profile_id: &str) -> Option<String> {
        None
    }

    fn store_password(&self, _profile_id: &str, _password: Option<&str>) {}

    fn has_password_store(&self) -> bool {
        false
    }

    fn on_resume(&self, f: Box<dyn Fn()>) {
        dom::on_resume(f);
    }

    fn redirect_after_login(&self) -> bool {
        match falcon_core::px0::login_next(&dom::location_search()) {
            Some(next) => dom::location_replace(&next),
            None => false,
        }
    }

    fn local_service(&self) -> Option<Arc<dyn LocalService>> {
        None
    }

    fn font_source(&self) -> FontSource {
        // 字体与页面同源（构建脚本拷进产物的 /fonts/）
        FontSource::Fetch { base_url: dom::origin() }
    }

    fn downloads(&self) -> Downloads {
        Downloads::Browser
    }

    fn hand_off_download(&self, url: &str) -> Result<(), String> {
        dom::trigger_download(url)
    }

    // HTML 预览的 `<iframe sandbox>` 叠层在 C3 补（附录 B）；在那之前界面给"在浏览器中打开"
    fn html_view(
        &self,
        _url: &str,
        _prefix: &str,
        _open_external: Box<dyn Fn(String) + Send + Sync>,
        _window: &mut Window,
    ) -> Option<Result<Rc<dyn HtmlView>, String>> {
        None
    }

    // 标签页图标跟着换（`<link rel=icon>`）在 C3 补（附录 B）
    fn set_app_icon(&self, _png: &[u8]) {}
}
