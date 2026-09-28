//! 主题偏好：明暗模式 + 浅色槽位 + 深色槽位（`lib/theme/pref.ts`）。
//!
//! 模型照 Ghostty 的 `theme = light:X,dark:Y`：两个槽位各放一套主题，明暗模式
//! （跟随系统 / 浅色 / 深色）决定此刻用哪个槽位。槽位里存的是主题**完整颜色的
//! 副本**而不只是名字——启动时不用解析内置目录就能画对界面，内置目录升级改了颜色
//! 也不会在用户没动过设置时悄悄换脸。
//!
//! 一个槽位放什么样的主题不设限：给"浅色"槽位选一套深底主题也行，界面按底色
//! 亮度判深浅（derive.rs），不按槽位。代码里 `ThemeMode` 是槽位、
//! `ResolvedTheme::appearance` 是深浅，别混用（CONTEXT.md）。
//!
//! 持久化形状与 web 存在 localStorage `falcon.themes` 里的 JSON 逐键相同
//! （`{"mode":…,"light":{"name","kind","colors":{…}},"dark":{…}}`），两边的偏好可以
//! 直接对照；`tests/fixtures/pref-default.json` 是 web `JSON.stringify` 出来的默认值，
//! 测试钉住 serde 输出与它逐字节相同。读入一律走 `sanitize_*`：坏一项回退一项，
//! 永远不因为偏好文件坏了起不来。
//!
//! 存取走注入的 [`StorageLike`]，纯函数可测；读不到 / 抛错就用默认。

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::catalog::{CatalogEntry, FALCON_DARK, FALCON_DARK_NAME, FALCON_LIGHT, FALCON_LIGHT_NAME};
use crate::color::Rgb;
use crate::ghostty::ThemeColors;
use crate::js::{js_object_entries, js_slice_utf16, js_string_to_number, js_trim, js_truthy};

/// 槽位
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    Dark,
}

/// 明暗模式偏好
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

/// builtin = 目录里的（Falcon / Ghostty 内置），custom = 用户贴的 Ghostty 文本
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    Builtin,
    Custom,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct ThemeChoice {
    pub name: String,
    pub kind: ThemeKind,
    pub colors: ThemeColors,
}

/// `falcon.themes` 的内容。反序列化走 [`sanitize_theme_settings`]（永不失败，坏项回默认）。
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct ThemeSettings {
    pub mode: ThemePref,
    pub light: ThemeChoice,
    pub dark: ThemeChoice,
}

impl ThemeSettings {
    /// 槽位里的那套主题（TS 里写作 `settings[mode]`）
    pub fn slot(&self, mode: ThemeMode) -> &ThemeChoice {
        match mode {
            ThemeMode::Light => &self.light,
            ThemeMode::Dark => &self.dark,
        }
    }

    pub fn slot_mut(&mut self, mode: ThemeMode) -> &mut ThemeChoice {
        match mode {
            ThemeMode::Light => &mut self.light,
            ThemeMode::Dark => &mut self.dark,
        }
    }
}

impl Default for ThemeSettings {
    /// `DEFAULT_THEME_SETTINGS`：跟随系统，Falcon Light / Falcon Dark
    fn default() -> Self {
        Self { mode: ThemePref::System, light: choice_of(&FALCON_LIGHT), dark: choice_of(&FALCON_DARK) }
    }
}

impl<'de> Deserialize<'de> for ThemeSettings {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        Ok(sanitize_theme_settings(&v))
    }
}

pub const THEMES_KEY: &str = "falcon.themes";
/// 旧版只存明暗模式的字符串
const LEGACY_MODE_KEYS: [&str; 2] = ["falcon.theme", "mojito.theme"];
/// 旧版终端偏好，themeId 字段是旧的终端配色 id
const LEGACY_TERM_KEYS: [&str; 2] = ["falcon.term", "mojito.term"];

pub fn choice_of(entry: &CatalogEntry) -> ThemeChoice {
    ThemeChoice { name: entry.name.to_string(), kind: ThemeKind::Builtin, colors: entry.colors.clone() }
}

/// 旧版终端配色能对上的 Ghostty 主题：给哪个槽位、叫什么名字
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LegacyTermTheme {
    pub slot: ThemeMode,
    pub name: &'static str,
}

/// 旧版终端配色 id → Ghostty 内置主题名。Campbell 与 Light+ 在 Ghostty 里没有
/// 对应，落回 Falcon 默认（Light+ 本来就是 Falcon Light 的 ANSI）。
pub const LEGACY_TERM_THEME_NAMES: [(&str, LegacyTermTheme); 14] = {
    const fn t(slot: ThemeMode, name: &'static str) -> LegacyTermTheme {
        LegacyTermTheme { slot, name }
    }
    use ThemeMode::{Dark, Light};
    [
        ("one-dark", t(Dark, "Atom One Dark")),
        ("dracula", t(Dark, "Dracula")),
        ("nord", t(Dark, "Nord")),
        ("tokyo-night", t(Dark, "TokyoNight")),
        ("catppuccin-mocha", t(Dark, "Catppuccin Mocha")),
        ("gruvbox-dark", t(Dark, "Gruvbox Dark")),
        ("solarized-dark", t(Dark, "iTerm2 Solarized Dark")),
        ("github-dark", t(Dark, "GitHub Dark Default")),
        ("monokai", t(Dark, "Monokai Classic")),
        ("solarized-light", t(Light, "iTerm2 Solarized Light")),
        ("catppuccin-latte", t(Light, "Catppuccin Latte")),
        ("github-light", t(Light, "GitHub Light Default")),
        ("gruvbox-light", t(Light, "Gruvbox Light")),
        ("one-light", t(Light, "Atom One Light")),
    ]
};

/// 查旧版终端配色 id。
///
/// TS 用普通对象查表，`"constructor"` 这类 id 会查到继承属性、被当成命中——那是
/// TS 的偶然行为（结果也没法用），这边只认表里的 14 个。
pub fn legacy_term_theme(id: &str) -> Option<LegacyTermTheme> {
    LEGACY_TERM_THEME_NAMES.iter().find(|(k, _)| *k == id).map(|(_, v)| *v)
}

/// localStorage 的抽象（原生这边是偏好文件）。读写都可能失败——web 的隐私模式 /
/// 沙箱 iframe 连读 localStorage 都会抛 SecurityError，原生这边是 IO 错误。
pub trait StorageLike {
    fn get_item(&self, key: &str) -> anyhow::Result<Option<String>>;
    fn set_item(&mut self, key: &str, value: &str) -> anyhow::Result<()>;
}

/// `hexOrNull`：字符串且是 #rgb / #rrggbb / #rrggbbaa 才收，规范成 #rrggbb
fn hex_or_null(v: Option<&Value>) -> Option<Rgb> {
    v.and_then(Value::as_str).and_then(Rgb::parse_hex)
}

/// 颜色副本必须整套合法，缺一个就整个不认——半套主题画出来比默认更糟。
///
/// 例外照抄 TS 的实际行为：`selectionForeground` 缺失或非法时**不**整套作废，而是
/// 当 `null`（cell-foreground，选中文字保留原色）。TS 那段 `=== undefined` 判断永远
/// 不成立（`hexOrNull` 只会给字符串或 null），看起来像想拒收，实际是放行。
pub fn sanitize_theme_colors(raw: &Value) -> Option<ThemeColors> {
    // 数组在 JS 里也是 object，但它身上取不到 background，结果同样是不认
    let r = raw.as_object()?;
    let background = hex_or_null(r.get("background"))?;
    let foreground = hex_or_null(r.get("foreground"))?;
    let cursor_color = hex_or_null(r.get("cursorColor"))?;
    let cursor_text = hex_or_null(r.get("cursorText"))?;
    let selection_background = hex_or_null(r.get("selectionBackground"))?;
    let selection_foreground = hex_or_null(r.get("selectionForeground"));
    let list = r.get("palette")?.as_array()?;
    if list.len() != 16 {
        return None;
    }
    let mut palette = [Rgb::default(); 16];
    for (slot, c) in palette.iter_mut().zip(list) {
        *slot = hex_or_null(Some(c))?;
    }
    let mut colors = ThemeColors {
        background,
        foreground,
        cursor_color,
        cursor_text,
        selection_background,
        selection_foreground,
        palette,
        extended: Default::default(),
    };
    // `r.extended && typeof r.extended === "object"`：对象与数组都算
    if let Some(ext) = r.get("extended").filter(|v| js_truthy(v) && (v.is_object() || v.is_array())) {
        for (k, v) in js_object_entries(ext) {
            // TS 用 Number(k) 转下标，"0x10"、"16.0" 也是 16
            let idx = js_string_to_number(&k);
            if idx.fract() == 0.0
                && (16.0..=255.0).contains(&idx)
                && let Some(h) = hex_or_null(Some(v))
            {
                colors.extended.insert(idx as u8, h);
            }
        }
    }
    Some(colors)
}

pub fn sanitize_theme_choice(raw: &Value, fallback: &ThemeChoice) -> ThemeChoice {
    let Some(r) = raw.as_object() else { return fallback.clone() };
    let Some(colors) = r.get("colors").and_then(sanitize_theme_colors) else { return fallback.clone() };
    let name = match r.get("name").and_then(Value::as_str).map(js_trim) {
        // 名字截 80（UTF-16 码元，与 JS 的 slice 同一把尺子）
        Some(n) if !n.is_empty() => js_slice_utf16(n, 80).to_string(),
        _ => fallback.name.clone(),
    };
    let kind = if r.get("kind").and_then(Value::as_str) == Some("custom") { ThemeKind::Custom } else { ThemeKind::Builtin };
    ThemeChoice { name, kind, colors }
}

pub fn sanitize_theme_pref(raw: &Value) -> ThemePref {
    match raw.as_str() {
        Some("light") => ThemePref::Light,
        Some("dark") => ThemePref::Dark,
        _ => ThemePref::System,
    }
}

pub fn sanitize_theme_settings(raw: &Value) -> ThemeSettings {
    let defaults = ThemeSettings::default();
    let Some(r) = raw.as_object() else { return defaults };
    let null = Value::Null;
    ThemeSettings {
        mode: sanitize_theme_pref(r.get("mode").unwrap_or(&null)),
        light: sanitize_theme_choice(r.get("light").unwrap_or(&null), &defaults.light),
        dark: sanitize_theme_choice(r.get("dark").unwrap_or(&null), &defaults.dark),
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LoadedThemeSettings {
    pub settings: ThemeSettings,
    /// 旧版终端配色能对上 Ghostty 主题时给出，由调用方从目录取颜色后补进槽位
    /// （`catalog::find_builtin` + `choice_of`）
    pub legacy_term: Option<LegacyTermTheme>,
}

/// 读一个 JSON 值：没有 / 空串 / 解析失败都是 `None`；读本身失败才是 `Err`
fn read_json(storage: &dyn StorageLike, key: &str) -> anyhow::Result<Option<Value>> {
    Ok(match storage.get_item(key)? {
        Some(raw) if !raw.is_empty() => serde_json::from_str(&raw).ok(),
        _ => None,
    })
}

/// 读偏好；没有新格式时从旧的两个 key 迁移（明暗模式直接带过来，终端配色
/// 需要目录里的颜色，只返回线索）。存储读失败就整份用默认。
pub fn load_theme_settings(storage: Option<&dyn StorageLike>) -> LoadedThemeSettings {
    let defaults = || LoadedThemeSettings { settings: ThemeSettings::default(), legacy_term: None };
    let Some(storage) = storage else { return defaults() };
    try_load(storage).unwrap_or_else(|_| defaults())
}

fn try_load(storage: &dyn StorageLike) -> anyhow::Result<LoadedThemeSettings> {
    // `if (current)`：JSON 里的 0 / "" / false / null 算没有，走旧格式迁移
    if let Some(current) = read_json(storage, THEMES_KEY)?.filter(js_truthy) {
        return Ok(LoadedThemeSettings { settings: sanitize_theme_settings(&current), legacy_term: None });
    }
    let mut settings = ThemeSettings::default();
    for key in LEGACY_MODE_KEYS {
        // 旧版存的是裸字符串，不是 JSON
        let mode = match storage.get_item(key)?.as_deref() {
            Some("light") => ThemePref::Light,
            Some("dark") => ThemePref::Dark,
            Some("system") => ThemePref::System,
            _ => continue,
        };
        settings.mode = mode;
        break;
    }
    let mut legacy_term = None;
    for key in LEGACY_TERM_KEYS {
        let term = read_json(storage, key)?;
        let id = term.as_ref().and_then(Value::as_object).and_then(|o| o.get("themeId")).and_then(Value::as_str);
        if let Some(hit) = id.and_then(legacy_term_theme) {
            legacy_term = Some(hit);
            break;
        }
    }
    Ok(LoadedThemeSettings { settings, legacy_term })
}

/// 写偏好。写不进去就只在本次运行生效（与 web 一致：不报错、不重试）
pub fn save_theme_settings(storage: Option<&mut dyn StorageLike>, settings: &ThemeSettings) {
    let Some(storage) = storage else { return };
    if let Ok(json) = serde_json::to_string(settings) {
        let _ = storage.set_item(THEMES_KEY, &json);
    }
}

pub fn resolve_theme_mode(pref: ThemePref, system_dark: bool) -> ThemeMode {
    match pref {
        ThemePref::Light => ThemeMode::Light,
        ThemePref::Dark => ThemeMode::Dark,
        ThemePref::System if system_dark => ThemeMode::Dark,
        ThemePref::System => ThemeMode::Light,
    }
}

/// 两个槽位都是内置默认（不看明暗模式）
pub fn is_default_theme_settings(s: &ThemeSettings) -> bool {
    s.light.kind == ThemeKind::Builtin
        && s.light.name == FALCON_LIGHT_NAME
        && s.dark.kind == ThemeKind::Builtin
        && s.dark.name == FALCON_DARK_NAME
}

/// 移植自 `pref.test.ts`，用例与断言值一一对应。
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct MemStorage {
        data: RefCell<HashMap<String, String>>,
    }

    impl MemStorage {
        fn with(init: &[(&str, &str)]) -> Self {
            let s = Self::default();
            for (k, v) in init {
                s.data.borrow_mut().insert(k.to_string(), v.to_string());
            }
            s
        }
    }

    impl StorageLike for MemStorage {
        fn get_item(&self, key: &str) -> anyhow::Result<Option<String>> {
            Ok(self.data.borrow().get(key).cloned())
        }
        fn set_item(&mut self, key: &str, value: &str) -> anyhow::Result<()> {
            self.data.borrow_mut().insert(key.to_string(), value.to_string());
            Ok(())
        }
    }

    /// `{ ...FALCON_DARK.colors, <改几项> }` 的 JSON 版
    fn dark_colors_with(edit: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Value {
        let mut v = serde_json::to_value(&FALCON_DARK.colors).unwrap();
        edit(v.as_object_mut().unwrap());
        v
    }

    fn hex(s: &str) -> Rgb {
        Rgb::parse_hex(s).unwrap()
    }

    // describe("sanitizeThemeColors")

    /// 整套合法才认，规范成小写六位
    #[test]
    fn colors_valid_and_normalized() {
        let c = sanitize_theme_colors(&dark_colors_with(|m| {
            m.insert("background".into(), json!("#0A0A0A"));
        }));
        assert_eq!(c.as_ref().map(|c| c.background), Some(hex("#0a0a0a")));
        assert_eq!(c.map(|c| c.selection_foreground), Some(None));
    }

    /// 缺任何一项 / 调色板不是 16 个 / 非法色 → undefined
    #[test]
    fn colors_incomplete_rejected() {
        assert_eq!(sanitize_theme_colors(&dark_colors_with(|m| drop(m.remove("cursorText")))), None);
        assert_eq!(
            sanitize_theme_colors(&dark_colors_with(|m| drop(m.insert("palette".into(), json!(["#000"]))))),
            None
        );
        assert_eq!(
            sanitize_theme_colors(&dark_colors_with(|m| drop(m.insert("foreground".into(), json!("red"))))),
            None
        );
        assert_eq!(sanitize_theme_colors(&Value::Null), None);
    }

    /// extended 只收 16–255 的合法项，空了就不带
    #[test]
    fn colors_extended_filtered() {
        let c = sanitize_theme_colors(&dark_colors_with(|m| {
            m.insert("extended".into(), json!({"16": "#123456", "3": "#000000", "300": "#ffffff", "20": "x"}));
        }));
        assert_eq!(c.map(|c| c.extended), Some(BTreeMap::from([(16, hex("#123456"))])));
        let only_low = sanitize_theme_colors(&dark_colors_with(|m| {
            m.insert("extended".into(), json!({"3": "#000000"}));
        }))
        .unwrap();
        assert!(only_low.extended.is_empty());
        assert!(!serde_json::to_string(&only_low).unwrap().contains("extended"), "空了就不写");
    }

    // describe("sanitizeThemeChoice / sanitizeThemeSettings")

    /// 坏的 choice 回 fallback；名字截 80
    #[test]
    fn choice_fallback_and_name_limit() {
        let fb = choice_of(&FALCON_LIGHT);
        assert_eq!(sanitize_theme_choice(&json!({"name": "x", "colors": {}}), &fb), fb);
        let dark = serde_json::to_value(&FALCON_DARK.colors).unwrap();
        let long = sanitize_theme_choice(&json!({"name": "a".repeat(100), "kind": "custom", "colors": dark}), &fb);
        assert_eq!(long.name.len(), 80);
        assert_eq!(long.kind, ThemeKind::Custom);
        assert_eq!(sanitize_theme_choice(&json!({"name": "y", "kind": "weird", "colors": dark}), &fb).kind, ThemeKind::Builtin);
    }

    /// settings 缺字段逐项回默认
    #[test]
    fn settings_fields_fall_back_individually() {
        let s = sanitize_theme_settings(&json!({"mode": "dark", "light": null}));
        let d = ThemeSettings::default();
        assert_eq!(s.mode, ThemePref::Dark);
        assert_eq!(s.light.name, d.light.name);
        assert_eq!(s.dark.name, d.dark.name);
        assert_eq!(sanitize_theme_settings(&json!("junk")).mode, ThemePref::System);
    }

    // describe("loadThemeSettings")

    /// 没有 storage 用默认
    #[test]
    fn load_without_storage() {
        assert_eq!(load_theme_settings(None).settings, ThemeSettings::default());
    }

    /// 新格式直接读
    #[test]
    fn load_new_format() {
        let mut st = MemStorage::default();
        let settings = ThemeSettings {
            mode: ThemePref::Light,
            light: choice_of(&FALCON_LIGHT),
            dark: ThemeChoice { name: "X".into(), kind: ThemeKind::Custom, ..choice_of(&FALCON_DARK) },
        };
        save_theme_settings(Some(&mut st), &settings);
        let LoadedThemeSettings { settings, legacy_term } = load_theme_settings(Some(&st));
        assert_eq!(settings.mode, ThemePref::Light);
        assert_eq!(settings.dark.name, "X");
        assert_eq!(settings.dark.kind, ThemeKind::Custom);
        assert_eq!(legacy_term, None);
    }

    /// 旧格式：明暗模式带过来，终端配色给出目录线索
    #[test]
    fn load_legacy_format() {
        let term = json!({"fontId": "maple", "themeId": "catppuccin-mocha"}).to_string();
        let st = MemStorage::with(&[("falcon.theme", "dark"), ("falcon.term", &term)]);
        let LoadedThemeSettings { settings, legacy_term } = load_theme_settings(Some(&st));
        assert_eq!(settings.mode, ThemePref::Dark);
        assert_eq!(settings.dark.name, ThemeSettings::default().dark.name);
        assert_eq!(legacy_term, Some(LegacyTermTheme { slot: ThemeMode::Dark, name: "Catppuccin Mocha" }));
    }

    /// 旧格式 mojito.* 也认；跟随界面 / 没对应的 id 没有线索
    #[test]
    fn load_legacy_mojito_and_unmapped_ids() {
        let term = json!({"themeId": "match"}).to_string();
        let st = MemStorage::with(&[("mojito.theme", "light"), ("mojito.term", &term)]);
        let LoadedThemeSettings { settings, legacy_term } = load_theme_settings(Some(&st));
        assert_eq!(settings.mode, ThemePref::Light);
        assert_eq!(legacy_term, None);
        let campbell = json!({"themeId": "campbell"}).to_string();
        assert_eq!(load_theme_settings(Some(&MemStorage::with(&[("falcon.term", &campbell)]))).legacy_term, None);
    }

    /// 坏 JSON 用默认
    #[test]
    fn load_bad_json() {
        let st = MemStorage::with(&[(THEMES_KEY, "{oops")]);
        assert_eq!(load_theme_settings(Some(&st)).settings.mode, ThemePref::System);
    }

    /// storage 抛异常也不炸
    #[test]
    fn load_and_save_survive_storage_errors() {
        struct Broken;
        impl StorageLike for Broken {
            fn get_item(&self, _: &str) -> anyhow::Result<Option<String>> {
                anyhow::bail!("SecurityError")
            }
            fn set_item(&mut self, _: &str, _: &str) -> anyhow::Result<()> {
                anyhow::bail!("SecurityError")
            }
        }
        assert_eq!(load_theme_settings(Some(&Broken)).settings.mode, ThemePref::System);
        save_theme_settings(Some(&mut Broken), &ThemeSettings::default());
    }

    // describe("resolveThemeMode / isDefaultThemeSettings")

    /// system 跟系统，其余固定
    #[test]
    fn resolve_mode() {
        assert_eq!(resolve_theme_mode(ThemePref::System, true), ThemeMode::Dark);
        assert_eq!(resolve_theme_mode(ThemePref::System, false), ThemeMode::Light);
        assert_eq!(resolve_theme_mode(ThemePref::Light, true), ThemeMode::Light);
        assert_eq!(resolve_theme_mode(ThemePref::Dark, false), ThemeMode::Dark);
    }

    /// 默认判定只看两个槽位，不看明暗模式
    #[test]
    fn default_detection_ignores_mode() {
        assert!(is_default_theme_settings(&ThemeSettings { mode: ThemePref::Dark, ..ThemeSettings::default() }));
        assert!(!is_default_theme_settings(&ThemeSettings {
            dark: ThemeChoice { name: "Dracula".into(), ..choice_of(&FALCON_DARK) },
            ..ThemeSettings::default()
        }));
    }

    // 以下不是从 TS 移植的：照抄 JS 语义的边角

    /// `if (current)`：新 key 里存的是 JSON 假值时走旧格式迁移
    #[test]
    fn falsy_current_falls_back_to_legacy() {
        for falsy in ["0", "false", "null", "\"\""] {
            let st = MemStorage::with(&[(THEMES_KEY, falsy), ("falcon.theme", "dark")]);
            assert_eq!(load_theme_settings(Some(&st)).settings.mode, ThemePref::Dark, "{falsy}");
        }
        // 真值但不是对象：不迁移，整份默认
        let st = MemStorage::with(&[(THEMES_KEY, "1"), ("falcon.theme", "dark")]);
        assert_eq!(load_theme_settings(Some(&st)).settings.mode, ThemePref::System);
    }

    /// selectionForeground 缺失 / 非法不作废整套，当 null
    #[test]
    fn missing_selection_foreground_is_null() {
        let mut v = serde_json::to_value(&crate::catalog::FALCON_LIGHT_COLORS).unwrap();
        v.as_object_mut().unwrap().insert("selectionForeground".into(), json!("#123456"));
        assert_eq!(sanitize_theme_colors(&v).unwrap().selection_foreground, Some(hex("#123456")));
        v.as_object_mut().unwrap().remove("selectionForeground");
        assert_eq!(sanitize_theme_colors(&v).unwrap().selection_foreground, None);
        v.as_object_mut().unwrap().insert("selectionForeground".into(), json!("junk"));
        assert_eq!(sanitize_theme_colors(&v).unwrap().selection_foreground, None);
    }

    /// extended 的键照 Number() 解析；数组也当对象遍历
    #[test]
    fn extended_keys_js_number() {
        let c = sanitize_theme_colors(&dark_colors_with(|m| {
            m.insert("extended".into(), json!({"0x11": "#111111", " 18 ": "#121212", "19.0": "#131313", "20.5": "#141414"}));
        }))
        .unwrap();
        assert_eq!(c.extended, BTreeMap::from([(17, hex("#111111")), (18, hex("#121212")), (19, hex("#131313"))]));
        let arr: Vec<String> = (0..18).map(|i| format!("#{i:02x}{i:02x}{i:02x}")).collect();
        let c = sanitize_theme_colors(&dark_colors_with(|m| drop(m.insert("extended".into(), json!(arr))))).unwrap();
        assert_eq!(c.extended, BTreeMap::from([(16, hex("#101010")), (17, hex("#111111"))]));
    }

    /// serde 往返：写出去再读回来一模一样
    #[test]
    fn serde_roundtrip() {
        let mut s = ThemeSettings { mode: ThemePref::Dark, ..ThemeSettings::default() };
        s.light.kind = ThemeKind::Custom;
        s.light.colors.extended.insert(200, hex("#abcdef"));
        let back: ThemeSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
