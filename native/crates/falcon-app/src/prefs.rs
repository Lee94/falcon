//! 本地偏好：web 存在 localStorage 里的那些键（`falcon.themes` / `falcon.term` /
//! `falcon.workspace` …），原生存成一个 JSON 文件里的同名键值，形状逐字一致——两边的数据
//! 可以直接对照，移植过来的纯函数（falcon-theme 的 sanitize、falcon-core 的读入清洗）也不用改。
//!
//! 位置：`~/Library/Application Support/Falcon/prefs.json`（按服务端配置分开的工作区另存，
//! 见 profiles.rs）。写入走"临时文件 + 改名"，写到一半崩了不会留下半截 JSON。
//!
//! 浏览器版直接读写 localStorage，键就是 web 用的那些——现有 web 用户的主题、终端设置原样继承。

use std::collections::BTreeMap;
#[cfg(not(target_family = "wasm"))]
use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};

#[cfg(not(target_family = "wasm"))]
pub fn data_dir() -> PathBuf {
    // 测试 / 自动化验证时指到临时目录，别动用户真实的偏好与工作区
    if let Ok(dir) = std::env::var("FALCON_NATIVE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("com", "falcon", "Falcon")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("falcon-native"))
}

/// 浏览器的 localStorage。拿不到（隐私模式、被禁用）时返回 `None`，读写都安静地跳过
#[cfg(target_family = "wasm")]
pub fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// web 的 localStorage 键都以它开头；浏览器版启动时把这些键一次读进来
#[cfg(target_family = "wasm")]
const KEY_PREFIX: &str = "falcon.";

#[derive(Default)]
pub struct Prefs {
    #[cfg(not(target_family = "wasm"))]
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl Global for Prefs {}

impl Prefs {
    #[cfg(not(target_family = "wasm"))]
    pub fn load(path: PathBuf) -> Self {
        let values = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<BTreeMap<String, String>>(&s).ok())
            .unwrap_or_default();
        Self { path, values }
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn init(cx: &mut App) {
        let prefs = Self::load(data_dir().join("prefs.json"));
        cx.set_global(prefs);
    }

    #[cfg(target_family = "wasm")]
    pub fn init(cx: &mut App) {
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
        cx.set_global(Self { values });
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// 值没变就不写盘（与 web 的"内容相同跳过"同一个节制）。
    pub fn set(&mut self, key: &str, value: String) {
        if self.values.get(key) == Some(&value) {
            return;
        }
        #[cfg(target_family = "wasm")]
        if let Some(storage) = local_storage()
            && let Err(err) = storage.set_item(key, &value)
        {
            log::warn!("偏好写入 localStorage 失败（{key}）：{err:?}");
        }
        self.values.insert(key.to_string(), value);
        #[cfg(not(target_family = "wasm"))]
        if let Err(err) = write_atomic(&self.path, &serde_json::to_vec_pretty(&self.values).unwrap_or_default()) {
            log::warn!("偏好写盘失败（{}）：{err}", self.path.display());
        }
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

impl falcon_theme::StorageLike for Prefs {
    fn get_item(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.get(key).map(str::to_string))
    }

    fn set_item(&mut self, key: &str, value: &str) -> anyhow::Result<()> {
        self.set(key, value.to_string());
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
