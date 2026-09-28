//! 终端画面偏好：字体、字号、行高、光标、引擎。对应 web 的 `lib/term.ts`。
//!
//! 只影响终端，不改界面字体。配色不在这里——终端与整个界面共用一套主题
//! （falcon-theme），按明暗各选一套。
//!
//! 持久化：web 存在 localStorage 的 `falcon.term`（旧名 `mojito.term`），原生存在偏好
//! 文件的同名键下，JSON 形状逐字一致（[`TermPref`] 的 serde）。读入一律过
//! [`sanitize_term_pref`]：换机器不跟着走、文件被手改坏、旧版存过 `themeId`（终端单独
//! 配色，迁移见 falcon-theme 的 pref）都只是回到默认 / 丢掉那一项。
//!
//! 不在这里的：web 的 `termFontStack`（拼 CSS font-family 字符串，含一长串系统回退）。
//! 原生字体回退由平台做，这里只留"字体 id → family 名"的映射与内置字体的先后
//! （[`term_font_families`]）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::{js_max, js_min, js_num, js_number, js_round, js_slice_utf16, js_trim};

pub const TERM_PREF_KEY: &str = "falcon.term";
pub const TERM_PREF_KEY_LEGACY: &str = "mojito.term";

pub const TERM_FONT_SIZE_MIN: f64 = 10.0;
pub const TERM_FONT_SIZE_MAX: f64 = 24.0;
pub const TERM_LINE_HEIGHT_MIN: f64 = 1.0;
pub const TERM_LINE_HEIGHT_MAX: f64 = 1.6;

/// 终端正文字体
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TermFontId {
    #[serde(rename = "berkeley")]
    Berkeley,
    #[serde(rename = "ioskeley")]
    Ioskeley,
    #[serde(rename = "maple")]
    Maple,
    #[serde(rename = "system")]
    System,
    #[serde(rename = "jetbrains")]
    Jetbrains,
    #[serde(rename = "cascadia")]
    Cascadia,
    #[serde(rename = "fira-code")]
    FiraCode,
    #[serde(rename = "ibm-plex")]
    IbmPlex,
    #[serde(rename = "source-code-pro")]
    SourceCodePro,
    #[serde(rename = "custom")]
    Custom,
}

impl TermFontId {
    /// 设置里的顺序（TS 的 `TERM_FONT_IDS`）
    pub const ALL: [TermFontId; 10] = [
        TermFontId::Berkeley,
        TermFontId::Ioskeley,
        TermFontId::Maple,
        TermFontId::System,
        TermFontId::Jetbrains,
        TermFontId::Cascadia,
        TermFontId::FiraCode,
        TermFontId::IbmPlex,
        TermFontId::SourceCodePro,
        TermFontId::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TermFontId::Berkeley => "berkeley",
            TermFontId::Ioskeley => "ioskeley",
            TermFontId::Maple => "maple",
            TermFontId::System => "system",
            TermFontId::Jetbrains => "jetbrains",
            TermFontId::Cascadia => "cascadia",
            TermFontId::FiraCode => "fira-code",
            TermFontId::IbmPlex => "ibm-plex",
            TermFontId::SourceCodePro => "source-code-pro",
            TermFontId::Custom => "custom",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == s)
    }

    /// 字体 id → family 名。`system`（交给平台默认等宽字体）与 `custom`（用
    /// `customFamily`）没有固定的 family，给 `None`。
    pub fn family(self) -> Option<&'static str> {
        match self {
            // 默认字体。Berkeley Mono TX-02（U.S. Graphics 商业字体）经
            // scripts/vendor-berkeley-mono.mjs 内嵌，family 名跟 TTF name 表一致。
            // 覆盖比 Ioskeley 窄（几乎没有希腊 / 西里尔，盒线也不全），缺的码位顺着
            // 回退落到 Ioskeley，中文再落到 Maple。
            TermFontId::Berkeley => Some(BERKELEY_FONT_FAMILY),
            // 内置的 OFL 回退（子集见 scripts/vendor-ioskeley-mono.mjs）
            TermFontId::Ioskeley => Some(IOSKELEY_FONT_FAMILY),
            TermFontId::Maple => Some(MAPLE_FONT_FAMILY),
            TermFontId::Jetbrains => Some("JetBrains Mono"),
            TermFontId::Cascadia => Some("Cascadia Mono"),
            TermFontId::FiraCode => Some("Fira Code"),
            TermFontId::IbmPlex => Some("IBM Plex Mono"),
            TermFontId::SourceCodePro => Some("Source Code Pro"),
            TermFontId::System | TermFontId::Custom => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TermCursorStyle {
    Block,
    Bar,
    Underline,
}

/// 终端渲染引擎。web 专属（rio = 实验性的 rioterm），原生只有一套；保留这个字段是为了
/// 与 web 的偏好 JSON 形状一致、读写时不丢值。
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TermEngine {
    Xterm,
    Rio,
}

/// `falcon.term` 的形状
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TermPref {
    pub font_id: TermFontId,
    pub custom_family: String,
    #[serde(with = "js_num")]
    pub font_size: f64,
    #[serde(with = "js_num")]
    pub line_height: f64,
    pub cursor_style: TermCursorStyle,
    pub cursor_blink: bool,
    pub engine: TermEngine,
}

impl Default for TermPref {
    /// TS 的 `DEFAULT_TERM_PREF`
    fn default() -> Self {
        TermPref {
            font_id: TermFontId::Berkeley,
            custom_family: String::new(),
            font_size: 13.0,
            line_height: 1.0,
            cursor_style: TermCursorStyle::Block,
            cursor_blink: true,
            engine: TermEngine::Xterm,
        }
    }
}

/// 内置默认正文字体的 family 名，必须与内嵌字体的 name 表一致（web 的 berkeley-mono.css）
pub const BERKELEY_FONT_FAMILY: &str = "TX-02";
/// 内置的 OFL 回退（web 的 ioskeley-mono.css）
pub const IOSKELEY_FONT_FAMILY: &str = "IoskeleyMonoTerm Nerd Font Mono";
/// 内置 Maple 的 family 名（web 的 maple-mono.css）
pub const MAPLE_FONT_FAMILY: &str = "Maple Mono NL NF CN";
/// 内置图标字体（web 的 nerd-symbols.css）
pub const NERD_FONT_FAMILY: &str = "Symbols Nerd Font Mono";

/// 这份偏好的正文字体 family：命名字体给它的 family，`custom` 给去掉首尾空白的
/// `customFamily`（空的算没有），`system` 没有。
pub fn primary_family(pref: &TermPref) -> Option<String> {
    match pref.font_id {
        TermFontId::Custom => Some(js_trim(&pref.custom_family).to_string()).filter(|s| !s.is_empty()),
        id => id.family().map(str::to_string),
    }
}

/// 内置字体的先后（web `termFontStack` 里系统回退之前的那一段，去掉 CSS 引号）。
///
/// - 图标字体永远打头：换正文字体不该把 Powerline / Nerd 图标弄丢。web 那边的理由是
///   Maple 自带的 NF 是宽形，xterm 把 U+E000–F8FF 当成 1 格且拒绝 rescale；原生渲染
///   同样按 1 格排私用区，保持同一个顺序。
/// - 命名字体后面垫内置的 Ioskeley 再垫 Maple：选了本机没装的字体、或 Berkeley 缺字形
///   时，回退的是同为等宽骨架的内置字体而不是系统字体。fontId 本身就是 Ioskeley 时不重复列。
/// - `maple`：图标字体 + Maple；`custom`：图标字体 + 自定义（空则省略）+ Maple；
///   `system`：一个内置字体都不列（web 那边整条交给系统回退栈）。
pub fn term_font_families(pref: &TermPref) -> Vec<String> {
    let s = |x: &str| x.to_string();
    match pref.font_id {
        TermFontId::System => Vec::new(),
        TermFontId::Maple => vec![s(NERD_FONT_FAMILY), s(MAPLE_FONT_FAMILY)],
        TermFontId::Custom => {
            let mut out = vec![s(NERD_FONT_FAMILY)];
            if let Some(custom) = primary_family(pref) {
                out.push(custom);
            }
            out.push(s(MAPLE_FONT_FAMILY));
            out
        }
        TermFontId::Ioskeley => vec![s(NERD_FONT_FAMILY), s(IOSKELEY_FONT_FAMILY), s(MAPLE_FONT_FAMILY)],
        id => {
            let named = id.family().unwrap_or(BERKELEY_FONT_FAMILY);
            vec![s(NERD_FONT_FAMILY), s(named), s(IOSKELEY_FONT_FAMILY), s(MAPLE_FONT_FAMILY)]
        }
    }
}

/// 字号取整后夹到 [10, 24]；NaN / ±∞ 回默认
pub fn clamp_font_size(n: f64) -> f64 {
    if !n.is_finite() {
        return TermPref::default().font_size;
    }
    js_min(TERM_FONT_SIZE_MAX, js_max(TERM_FONT_SIZE_MIN, js_round(n)))
}

/// 行高吸到 0.05 的格点再夹到 [1, 1.6]；NaN / ±∞ 回默认
pub fn clamp_line_height(n: f64) -> f64 {
    if !n.is_finite() {
        return TermPref::default().line_height;
    }
    let snapped = js_round(n * 20.0) / 20.0;
    js_min(TERM_LINE_HEIGHT_MAX, js_max(TERM_LINE_HEIGHT_MIN, snapped))
}

/// 读入清洗：任意 JSON（旧版、手改过、别的类型）→ 合法偏好。认不出的字段回默认，
/// 多余字段（旧版的 `themeId`）直接丢。数字照 JS 的 `Number(x)` 转（`"16"` 也算 16）。
pub fn sanitize_term_pref(raw: &Value) -> TermPref {
    let d = TermPref::default();
    let get = |k: &str| raw.as_object().and_then(|o| o.get(k));
    let font_id = get("fontId").and_then(Value::as_str).and_then(TermFontId::from_wire).unwrap_or(d.font_id);
    let cursor_style = match get("cursorStyle").and_then(Value::as_str) {
        Some("bar") => TermCursorStyle::Bar,
        Some("underline") => TermCursorStyle::Underline,
        Some("block") => TermCursorStyle::Block,
        _ => d.cursor_style,
    };
    TermPref {
        font_id,
        custom_family: get("customFamily").and_then(Value::as_str).map(|s| js_slice_utf16(s, 120).to_string()).unwrap_or_default(),
        font_size: clamp_font_size(js_number(get("fontSize"))),
        line_height: clamp_line_height(js_number(get("lineHeight"))),
        cursor_style,
        cursor_blink: get("cursorBlink") != Some(&Value::Bool(false)),
        engine: if get("engine").and_then(Value::as_str) == Some("rio") { TermEngine::Rio } else { TermEngine::Xterm },
    }
}

/// 从存储里读出来的原文（先 `falcon.term`，没有再 `mojito.term`，由调用方取）→ 偏好。
/// 没有、不是合法 JSON、是 `null` 都给默认。
pub fn load_term_pref(raw: Option<&str>) -> TermPref {
    match raw.filter(|s| !s.is_empty()).map(serde_json::from_str::<Value>) {
        Some(Ok(v)) if !v.is_null() => sanitize_term_pref(&v),
        _ => TermPref::default(),
    }
}

/// 写回存储的 JSON（与 web 的 `JSON.stringify(pref)` 同形）
pub fn serialize_term_pref(pref: &TermPref) -> String {
    serde_json::to_string(pref).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_empty_object_is_the_default() {
        assert_eq!(sanitize_term_pref(&json!({})), TermPref::default());
    }

    #[test]
    fn legacy_theme_id_is_dropped_other_fields_kept() {
        let pref = sanitize_term_pref(&json!({ "fontSize": 16, "themeId": "dracula", "engine": "rio" }));
        assert_eq!(pref.font_size, 16.0);
        assert_eq!(pref.engine, TermEngine::Rio);
        assert!(!serialize_term_pref(&pref).contains("themeId"));
    }

    #[test]
    fn out_of_range_values_clamp_and_bad_enums_fall_back() {
        let pref = sanitize_term_pref(&json!({ "fontSize": 99, "lineHeight": 0.2, "cursorStyle": "weird", "fontId": "nope" }));
        assert_eq!(pref.font_size, 24.0);
        assert_eq!(pref.line_height, 1.0);
        assert_eq!(pref.cursor_style, TermCursorStyle::Block);
        assert_eq!(pref.font_id, TermFontId::Berkeley);
    }

    #[test]
    fn icon_font_leads_then_the_default_body_font_then_builtin_fallbacks() {
        assert_eq!(
            term_font_families(&TermPref::default()),
            [NERD_FONT_FAMILY, BERKELEY_FONT_FAMILY, IOSKELEY_FONT_FAMILY, MAPLE_FONT_FAMILY]
        );
    }

    #[test]
    fn ioskeley_is_not_listed_twice() {
        let f = term_font_families(&TermPref { font_id: TermFontId::Ioskeley, ..TermPref::default() });
        assert_eq!(f, [NERD_FONT_FAMILY, IOSKELEY_FONT_FAMILY, MAPLE_FONT_FAMILY]);
    }

    #[test]
    fn maple_follows_the_icon_font() {
        let f = term_font_families(&TermPref { font_id: TermFontId::Maple, ..TermPref::default() });
        assert_eq!(f, [NERD_FONT_FAMILY, MAPLE_FONT_FAMILY]);
    }

    #[test]
    fn custom_sits_between_the_icon_font_and_maple_empty_falls_back_to_maple() {
        let custom = |s: &str| TermPref { font_id: TermFontId::Custom, custom_family: s.into(), ..TermPref::default() };
        assert_eq!(term_font_families(&custom("Sarasa Term SC")), [NERD_FONT_FAMILY, "Sarasa Term SC", MAPLE_FONT_FAMILY]);
        assert_eq!(term_font_families(&custom("  ")), [NERD_FONT_FAMILY, MAPLE_FONT_FAMILY]);
    }

    #[test]
    fn json_shape_matches_web() {
        // 以下是 Rust 侧补的：落盘形状、JS 的 Number() 转换、load 的兜底
        assert_eq!(
            serialize_term_pref(&TermPref::default()),
            r#"{"fontId":"berkeley","customFamily":"","fontSize":13,"lineHeight":1,"cursorStyle":"block","cursorBlink":true,"engine":"xterm"}"#
        );
        let p = sanitize_term_pref(&json!({ "fontSize": "15", "lineHeight": 1.23, "cursorBlink": 0, "fontId": "fira-code" }));
        assert_eq!((p.font_size, p.line_height, p.cursor_blink, p.font_id), (15.0, 1.25, true, TermFontId::FiraCode));
        // Number(null) 是 0 → 夹到下限；缺字段是 NaN → 默认
        assert_eq!(sanitize_term_pref(&json!({ "fontSize": null })).font_size, 10.0);
        assert_eq!(load_term_pref(Some("{broken")), TermPref::default());
        assert_eq!(load_term_pref(Some("null")), TermPref::default());
        assert_eq!(load_term_pref(Some("5")), TermPref::default());
        assert_eq!(load_term_pref(None), TermPref::default());
        assert_eq!(primary_family(&TermPref::default()).as_deref(), Some("TX-02"));
        assert_eq!(TermFontId::System.family(), None);
    }
}
