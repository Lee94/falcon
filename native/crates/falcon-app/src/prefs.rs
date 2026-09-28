//! 本地偏好：web 存在 localStorage 里的那些键（`falcon.themes` / `falcon.term` /
//! `falcon.workspace` …），原生存成一个 JSON 文件里的同名键值，形状逐字一致——两边的数据
//! 可以直接对照，移植过来的纯函数（falcon-theme 的 sanitize、falcon-core 的读入清洗）也不用改。
//!
//! 位置：`~/Library/Application Support/Falcon/prefs.json`（按服务端配置分开的工作区另存，
//! 见 profiles.rs）。写入走"临时文件 + 改名"，写到一半崩了不会留下半截 JSON。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui_kit::{App, Global};

pub fn data_dir() -> PathBuf {
    // 测试 / 自动化验证时指到临时目录，别动用户真实的偏好与工作区
    if let Ok(dir) = std::env::var("FALCON_NATIVE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("com", "falcon", "Falcon")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("falcon-native"))
}

#[derive(Default)]
pub struct Prefs {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl Global for Prefs {}

impl Prefs {
    pub fn load(path: PathBuf) -> Self {
        let values = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<BTreeMap<String, String>>(&s).ok())
            .unwrap_or_default();
        Self { path, values }
    }

    pub fn init(cx: &mut App) {
        let prefs = Self::load(data_dir().join("prefs.json"));
        cx.set_global(prefs);
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// 值没变就不写盘（与 web 的"内容相同跳过"同一个节制）。
    pub fn set(&mut self, key: &str, value: String) {
        if self.values.get(key) == Some(&value) {
            return;
        }
        self.values.insert(key.to_string(), value);
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

pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}
