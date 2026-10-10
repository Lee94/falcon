//! 应用图标（ADR 0018）：内置几套可选，也能上传一张自定义图片。移植自
//! `packages/shared/src/appIcon.ts`（选择的归一、地址）与
//! `packages/server/src/appIcon.ts`（PNG 头校验、`AppIcons` 的存取）。
//!
//! 选择是**服务端级**的：存在 settings 表里，连这台服务端的所有浏览器（标签页图标）与原生
//! 客户端（Dock）看到的是同一个图标。自定义图片落在 `<dataDir>/app-icon/custom.png`。
//! PWA（清单、maskable 与 iOS 主屏幕图标）2026-10-11 删掉了：浏览器版只做桌面浏览器的标签页。
//!
//! 内置图标的图形在 native/xtask/src/icons.rs（`cargo xtask icons` 出文件），产物按 id 分目录放在
//! native/web/icons/<id>/ 下（原生另有一份 macOS 版式的 PNG 嵌进二进制）。这里只管 id 与地址。
//!
//! 复用 falcon-core 的 `app_icon`：图标 id 表、默认图标、`custom` 字面量、自定义图边长、
//! `is_builtin`；线上形状 `AppIconState` 用 falcon-proto 的。
//!
//! `AppIcons` 读写 settings 走 [`SettingsStore`]（与 auth 共用同一个 trait，Db 实现它），
//! 文件操作照 TS 直接做（同步 std::fs；路由在阻塞线程池上调它们）。
//!
//! 路由（TS 的 `registerAppIconRoutes`）在 `api/app_icon.rs`；那边用到的纯判断（缓存头、
//! 拒收的状态码）在 [`custom_icon_cache_control`] / [`custom_icon_reject_status`]。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use falcon_core::app_icon::{APP_ICON_CUSTOM_SIZE, APP_ICON_IDS, CUSTOM, DEFAULT_APP_ICON};
pub use falcon_proto::AppIconState;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use crate::auth::SettingsStore;
use crate::term_env::js;

// ---------------- shared/appIcon.ts ----------------

/// `isBuiltinAppIcon`：TS 收 `unknown`，这里 None = undefined
pub fn is_builtin_app_icon(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(s)) if falcon_core::app_icon::is_builtin(s))
}

pub fn is_app_icon_choice(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(s)) if s == CUSTOM) || is_builtin_app_icon(value)
}

/// 存储值 → 生效的选择。认不出的 id（降级到没有这套图标的旧版本、手改过库）
/// 与选了自定义却没有图的，一律回默认——图标永远得有一个能用的。
///
/// custom 按 JS 真值看：空串（删过自定义图后库里存的就是空串）与没有一样。
pub fn resolve_app_icon(stored: Option<&str>, custom: Option<&str>) -> &'static str {
    if stored == Some(CUSTOM) {
        return if custom.is_some_and(|c| !c.is_empty()) { CUSTOM } else { DEFAULT_APP_ICON };
    }
    stored.and_then(|s| APP_ICON_IDS.iter().find(|id| **id == s)).copied().unwrap_or(DEFAULT_APP_ICON)
}

/// 内置图标的标签页图标：圆角方块的 192 PNG（不用 SVG：默认图标是栅格插画，做成 SVG 就是
/// 1MB 多的 favicon）
pub fn builtin_icon_url(id: &str) -> String {
    format!("/icons/{id}/icon-192.png")
}

pub fn custom_icon_url(version: &str) -> String {
    format!("/api/app-icon/custom.png?v={}", js::encode_uri_component(version))
}

/// 当前选择落到页面上的标签页图标地址。选了自定义且确实有图（JS 真值）就是那张，
/// 否则是内置的（选的是 custom 却没有图时回默认）
pub fn favicon_url(state: &AppIconState) -> String {
    if state.selected == CUSTOM
        && let Some(version) = state.custom.as_deref().filter(|c| !c.is_empty())
    {
        return custom_icon_url(version);
    }
    let id = if state.selected == CUSTOM { DEFAULT_APP_ICON } else { &state.selected };
    builtin_icon_url(id)
}

// ---------------- server/appIcon.ts ----------------

const SELECTED_KEY: &str = "app_icon";
const CUSTOM_KEY: &str = "app_icon_custom";

/// 客户端上传的是规整过的 512 PNG，几百 KB 到头；给足余量也挡住塞大文件
pub const CUSTOM_ICON_MAX_BYTES: usize = 2 * 1024 * 1024;
const MIN_EDGE: u32 = 64;
const MAX_EDGE: u32 = 2048;

const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngSize {
    pub width: u32,
    pub height: u32,
}

/// 读 PNG 的 IHDR 拿宽高；不是 PNG（签名不对、首块不是 IHDR、太短）返回 None
pub fn png_size(buf: &[u8]) -> Option<PngSize> {
    if buf.len() < 24 || buf[..8] != PNG_SIGNATURE || &buf[12..16] != b"IHDR" {
        return None;
    }
    let be = |at: usize| u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
    Some(PngSize { width: be(16), height: be(20) })
}

/// 服务端不解码图片（没有图像库，也不想为此装一个），只做形状检查：
/// 必须是正方形 PNG、边长在合理范围。缩放 / 裁切是客户端上传前做的。
pub fn check_custom_icon(buf: &[u8]) -> Option<String> {
    if buf.len() > CUSTOM_ICON_MAX_BYTES {
        return Some("图片太大（上限 2MB）".into());
    }
    let Some(size) = png_size(buf) else { return Some("只收 PNG".into()) };
    if size.width != size.height {
        return Some("图标必须是正方形".into());
    }
    if size.width < MIN_EDGE || size.width > MAX_EDGE {
        return Some(format!("边长要在 {MIN_EDGE}–{MAX_EDGE} 之间"));
    }
    None
}

/// 上传被 [`check_custom_icon`] 拒收时回的状态码：太大是 413，其余 400
pub fn custom_icon_reject_status(body_len: usize) -> u16 {
    if body_len > CUSTOM_ICON_MAX_BYTES { 413 } else { 400 }
}

/// `custom.png` 的缓存头。地址里的 v 是内容哈希：对上了就能永久缓存；对不上（旧页面拿着旧地址）
/// 给当前这张、不缓存
pub fn custom_icon_cache_control(v: Option<&str>, current: Option<&str>) -> &'static str {
    match v {
        Some(v) if !v.is_empty() && Some(v) == current => "public, max-age=31536000, immutable",
        _ => "no-cache",
    }
}

/// 应用图标的存取：选择在 settings 表，自定义图在 `<dataDir>/app-icon/custom.png`
pub struct AppIcons<S> {
    db: S,
    dir: PathBuf,
    file: PathBuf,
}

impl<S: SettingsStore> AppIcons<S> {
    pub fn new(db: S, data_dir: impl AsRef<Path>) -> Self {
        let dir = data_dir.as_ref().join("app-icon");
        let file = dir.join("custom.png");
        Self { db, dir, file }
    }

    /// 自定义图的版本；库里记着但文件没了（被手动删掉）也当没有
    fn custom_version(&self) -> Option<String> {
        self.db.get_setting(CUSTOM_KEY).filter(|v| !v.is_empty() && self.file.exists())
    }

    pub fn state(&self) -> AppIconState {
        let custom = self.custom_version();
        let stored = self.db.get_setting(SELECTED_KEY);
        AppIconState { selected: resolve_app_icon(stored.as_deref(), custom.as_deref()).to_string(), custom }
    }

    /// 返回 None = 这个选择不成立（未知 id，或选自定义但还没传图）
    pub fn select(&self, choice: Option<&Value>) -> Option<AppIconState> {
        let Some(Value::String(choice)) = choice.filter(|c| is_app_icon_choice(Some(c))) else {
            return None;
        };
        if choice == CUSTOM && self.custom_version().is_none() {
            return None;
        }
        self.db.set_setting(SELECTED_KEY, choice);
        Some(self.state())
    }

    /// 存下自定义图并选中它。先写临时文件再改名：中途失败不留半张图。
    ///
    /// 临时名带随机段：Node 版只带 pid（单线程，同一进程里两次保存不会交错），这里的路由跑在
    /// 多线程运行时上，两次并发上传会写同一个临时文件、互相截断
    pub fn save_custom(&self, buf: &[u8]) -> io::Result<AppIconState> {
        let digest = Sha256::digest(buf);
        let version: String = digest.iter().map(|b| format!("{b:02x}")).collect::<String>()[..16].to_string();
        fs::create_dir_all(&self.dir)?;
        let mut tag = [0u8; 4];
        rand::fill(&mut tag[..]);
        let mut tmp = self.file.as_os_str().to_owned();
        tmp.push(format!(".{}.{}.tmp", std::process::id(), hex::encode(tag)));
        let tmp = PathBuf::from(tmp);
        if let Err(e) = fs::write(&tmp, buf).and_then(|()| fs::rename(&tmp, &self.file)) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.db.set_setting(CUSTOM_KEY, &version);
        self.db.set_setting(SELECTED_KEY, CUSTOM);
        Ok(self.state())
    }

    /// 删掉自定义图；正选着它就回默认（state() 的 resolve 自己会回落，这里把库也改干净）
    pub fn remove_custom(&self) -> io::Result<AppIconState> {
        match fs::remove_file(&self.file) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.db.set_setting(CUSTOM_KEY, "");
        if self.db.get_setting(SELECTED_KEY).as_deref() == Some(CUSTOM) {
            self.db.set_setting(SELECTED_KEY, "");
        }
        Ok(self.state())
    }

    pub fn custom_file(&self) -> Option<PathBuf> {
        self.custom_version().map(|_| self.file.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn state(selected: &str, custom: Option<&str>) -> AppIconState {
        AppIconState { selected: selected.into(), custom: custom.map(Into::into) }
    }

    // ---- shared/appIcon.test.ts ----

    // describe("resolveAppIcon")

    #[test]
    fn resolve_app_icon_known_builtin_id_applies_as_is() {
        // 认得的内置 id 原样生效
        assert_eq!(resolve_app_icon(Some("flash"), None), "flash");
    }

    #[test]
    fn resolve_app_icon_unknown_or_unset_falls_back_to_default() {
        // 没存过、认不出的 id 回默认
        assert_eq!(resolve_app_icon(None, None), DEFAULT_APP_ICON);
        assert_eq!(resolve_app_icon(Some("falcon-classic"), None), DEFAULT_APP_ICON);
    }

    #[test]
    fn resolve_app_icon_custom_without_image_falls_back_to_default() {
        // 选了自定义但图没了，回默认
        assert_eq!(resolve_app_icon(Some("custom"), None), DEFAULT_APP_ICON);
        assert_eq!(resolve_app_icon(Some("custom"), Some("ab12")), "custom");
    }

    // describe("isAppIconChoice")

    #[test]
    fn is_app_icon_choice_accepts_only_builtin_ids_and_custom() {
        // 内置 id 与 custom 以外都不收
        for id in APP_ICON_IDS {
            assert!(is_app_icon_choice(Some(&json!(id))));
        }
        assert!(is_app_icon_choice(Some(&json!("custom"))));
        assert!(!is_app_icon_choice(Some(&json!("../etc"))));
        assert!(!is_app_icon_choice(Some(&json!(1))));
    }

    // describe("appIconLinks")（PWA 删掉后只剩标签页图标）

    #[test]
    fn favicon_builtin_uses_rounded_192_png() {
        // 内置图标：标签页用圆角的 192 png
        assert_eq!(favicon_url(&state("glyph", None)), "/icons/glyph/icon-192.png");
    }

    #[test]
    fn favicon_custom_url_carries_the_version() {
        // 自定义图标的地址带版本，换图就换地址
        assert_eq!(favicon_url(&state("custom", Some("0f3c"))), "/api/app-icon/custom.png?v=0f3c");
    }

    #[test]
    fn favicon_uploaded_custom_but_builtin_selected_uses_builtin() {
        // 上传过自定义但选的是内置，用内置的
        assert_eq!(favicon_url(&state("flash", Some("0f3c"))), "/icons/flash/icon-192.png");
    }

    #[test]
    fn js_compat_edges() {
        // 选了 custom 却没有图（含删过后存的空串）回默认
        assert_eq!(favicon_url(&state("custom", None)), "/icons/emberwing/icon-192.png");
        assert_eq!(favicon_url(&state("custom", Some(""))), "/icons/emberwing/icon-192.png");
        assert_eq!(custom_icon_url("a b/中"), "/api/app-icon/custom.png?v=a%20b%2F%E4%B8%AD");
        assert_eq!(resolve_app_icon(Some("custom"), Some("")), "emberwing");
        assert_eq!(resolve_app_icon(Some(""), None), "emberwing");
    }

    #[test]
    fn route_helpers() {
        assert_eq!(custom_icon_cache_control(Some("ab"), Some("ab")), "public, max-age=31536000, immutable");
        assert_eq!(custom_icon_cache_control(Some("old"), Some("ab")), "no-cache");
        assert_eq!(custom_icon_cache_control(None, Some("ab")), "no-cache");
        assert_eq!(custom_icon_cache_control(Some(""), None), "no-cache");
        assert_eq!(custom_icon_reject_status(CUSTOM_ICON_MAX_BYTES + 1), 413);
        assert_eq!(custom_icon_reject_status(10), 400);
    }

    // ---- server/appIcon.test.ts ----

    /// 只有签名 + IHDR 头的"PNG"：服务端只看这两样
    fn fake_png(width: u32, height: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 33];
        buf[..8].copy_from_slice(&PNG_SIGNATURE);
        buf[8..12].copy_from_slice(&13u32.to_be_bytes());
        buf[12..16].copy_from_slice(b"IHDR");
        buf[16..20].copy_from_slice(&width.to_be_bytes());
        buf[20..24].copy_from_slice(&height.to_be_bytes());
        buf
    }

    /// 测试用的 settings 表（TS 用的是真 Db）
    #[derive(Default)]
    struct MemSettings(Mutex<HashMap<String, String>>);

    impl SettingsStore for MemSettings {
        fn get_setting(&self, key: &str) -> Option<String> {
            self.0.lock().unwrap().get(key).cloned()
        }
        fn set_setting(&self, key: &str, value: &str) {
            self.0.lock().unwrap().insert(key.into(), value.into());
        }
    }

    fn tmp_icons() -> (AppIcons<MemSettings>, tempfile::TempDir) {
        let data_dir = tempfile::tempdir().unwrap();
        (AppIcons::new(MemSettings::default(), data_dir.path()), data_dir)
    }

    // describe("pngSize")

    #[test]
    fn png_size_reads_width_and_height_from_ihdr() {
        // 从 IHDR 读宽高
        assert_eq!(png_size(&fake_png(512, 256)), Some(PngSize { width: 512, height: 256 }));
    }

    #[test]
    fn png_size_returns_none_for_non_png() {
        // 不是 PNG 返回 null
        assert_eq!(png_size(b"GIF89a..........................."), None);
        assert_eq!(png_size(&fake_png(1, 1)[..20]), None);
    }

    // describe("checkCustomIcon")

    #[test]
    fn check_custom_icon_square_png_passes() {
        // 正方形 PNG 通过
        assert_eq!(check_custom_icon(&fake_png(512, 512)), None);
    }

    #[test]
    fn check_custom_icon_rejects_non_square_small_large_and_non_png() {
        // 非正方形、太小、太大、不是 PNG 都拒
        let msg = |buf: &[u8]| check_custom_icon(buf).unwrap_or_default();
        assert!(msg(&fake_png(512, 500)).contains("正方形"));
        assert!(msg(&fake_png(32, 32)).contains("边长"));
        assert!(msg(&fake_png(4096, 4096)).contains("边长"));
        assert!(msg(b"<svg/>").contains("PNG"));
        assert!(msg(&vec![0u8; CUSTOM_ICON_MAX_BYTES + 1]).contains("太大"));
    }

    // describe("AppIcons")

    #[test]
    fn app_icons_default_when_never_set() {
        // 没设置过是默认图标
        let (icons, _dir) = tmp_icons();
        assert_eq!(icons.state(), state("emberwing", None));
    }

    #[test]
    fn app_icons_select_builtin_and_reject_unknown_or_custom_without_image() {
        // 选内置图标；未知 id、还没传图时选自定义都不成立
        let (icons, _dir) = tmp_icons();
        assert_eq!(icons.select(Some(&json!("flash"))), Some(state("flash", None)));
        assert_eq!(icons.select(Some(&json!("nope"))), None);
        assert_eq!(icons.select(Some(&json!("custom"))), None);
        assert_eq!(icons.state().selected, "flash");
    }

    #[test]
    fn app_icons_upload_selects_with_content_hash_version_and_remove_restores_default() {
        // 上传即选中，版本是内容哈希；删掉后回默认
        let (icons, dir) = tmp_icons();
        let png = fake_png(512, 512);
        let saved = icons.save_custom(&png).unwrap();
        assert_eq!(saved.selected, "custom");
        let version = saved.custom.clone().unwrap_or_default();
        assert!(version.len() == 16 && version.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(fs::read(dir.path().join("app-icon").join("custom.png")).unwrap(), png);

        // 改选内置再选回自定义，图还在
        icons.select(Some(&json!("glyph")));
        assert_eq!(icons.select(Some(&json!("custom"))).map(|s| s.selected).as_deref(), Some("custom"));

        assert_eq!(icons.remove_custom().unwrap(), state("emberwing", None));
        assert_eq!(icons.custom_file(), None);
    }

    #[test]
    fn app_icons_treats_a_deleted_file_as_no_custom_icon() {
        // 库里记着但文件被删了，当没有
        let (icons, dir) = tmp_icons();
        icons.save_custom(&fake_png(256, 256)).unwrap();
        fs::remove_file(dir.path().join("app-icon").join("custom.png")).unwrap();
        assert_eq!(icons.state(), state("emberwing", None));
    }

    #[test]
    fn app_icons_version_is_sha256_prefix() {
        // 不在 TS 测试里：版本就是 sha256 十六进制的前 16 位（老数据目录里存的就是这个）
        let (icons, _dir) = tmp_icons();
        let saved = icons.save_custom(b"abc").unwrap();
        assert_eq!(saved.custom.as_deref(), Some("ba7816bf8f01cfea"));
    }
}
