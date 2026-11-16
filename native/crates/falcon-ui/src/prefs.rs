//! 本地偏好：沿用旧 React 版存在 localStorage 里的那些键（`falcon.themes` / `falcon.term` /
//! `falcon.workspace` …），形状逐字一致——它留下的数据直接能读，移植过来的纯函数
//! （falcon-theme 的 sanitize、falcon-core 的读入清洗）也不用改。
//!
//! 存哪儿归平台（[`falcon_platform::Platform::load_prefs`]）：原生是数据目录里的一个 JSON 文件
//! （写入走"临时文件 + 改名"），浏览器直接是 localStorage——React 版老用户的主题、终端设置原样继承。
//! 这里只管内存里的那份与"值没变就不写"。

use std::collections::BTreeMap;

use gpui_kit::{App, Global};

pub struct Prefs {
    values: BTreeMap<String, String>,
    platform: std::rc::Rc<dyn falcon_platform::Platform>,
}

impl Global for Prefs {}

impl Prefs {
    pub fn init(cx: &mut App) {
        let platform = falcon_platform::get(cx);
        let values = platform.load_prefs();
        cx.set_global(Self { values, platform });
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// 值没变就不写盘（沿用 React 版"内容相同跳过"的节制）。
    pub fn set(&mut self, key: &str, value: String) {
        if self.values.get(key) == Some(&value) {
            return;
        }
        self.values.insert(key.to_string(), value);
        self.platform.store_pref(key, &self.values[key], &self.values);
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
