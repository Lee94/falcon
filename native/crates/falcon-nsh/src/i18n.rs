//! 本地化字典加载：将嵌套的 `locales/zh-CN.json` 展平为点分键，注入 NSH `Translations`。
//!
//! 界面上不允许硬编码中文，统一经 `nsh_core::t!("...")` 读取。
//! NSH 的 `Translations::lookup` 在宿主语言不匹配时（例如无头测试默认 `en-US`）会回退到 `"en"`，
//! v1 仅有中文一份文案，因此同时填入 `"zh-CN"`、`"zh"` 与 `"en"`，保证任意宿主语言首帧即显示中文。

use std::collections::HashMap;
use std::sync::LazyLock;

use nsh_core::i18n::Translations;
use serde_json::Value;

pub use nsh_core::t;

const ZH_CN_JSON: &str = include_str!("../locales/zh-CN.json");

static FLAT_ZH_CN: LazyLock<HashMap<String, String>> = LazyLock::new(|| {
    let root: Value = serde_json::from_str(ZH_CN_JSON)
        .expect("falcon-nsh locales/zh-CN.json must be valid JSON");
    let mut out = HashMap::with_capacity(1024);
    flatten_json("", &root, &mut out);
    out
});

static TRANSLATIONS: LazyLock<Translations> = LazyLock::new(|| {
    let base = FLAT_ZH_CN.clone();
    let mut map = HashMap::with_capacity(3);
    map.insert("zh-CN".to_string(), base.clone());
    map.insert("zh".to_string(), base.clone());
    map.insert("en".to_string(), base);
    Translations::from_map(map)
});

fn flatten_json(prefix: &str, value: &Value, out: &mut HashMap<String, String>) {
    match value {
        Value::Object(obj) => {
            for (k, v) in obj {
                let next = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json(&next, v, out);
            }
        }
        Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        Value::Number(n) => {
            out.insert(prefix.to_string(), n.to_string());
        }
        Value::Bool(b) => {
            out.insert(prefix.to_string(), b.to_string());
        }
        Value::Null | Value::Array(_) => {}
    }
}

/// 返回内嵌完整文案的 `Translations` 实例（内部 `Arc` 共享，克隆零开销）。
pub fn translations() -> Translations {
    TRANSLATIONS.clone()
}

/// 非组件上下文（纯单元测试对账文案 key）直接按点分键取值。
pub fn lookup_static(key: &str) -> Option<&'static str> {
    FLAT_ZH_CN.get(key).map(String::as_str)
}
