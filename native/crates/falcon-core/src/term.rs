//! 终端画面偏好：字体、字号、行高、光标。对应旧 React 版的 `lib/term.ts`。
//!
//! 只影响终端，不改界面字体。配色不在这里——终端与整个界面共用一套主题
//! （falcon-theme），按明暗各选一套。
//!
//! 持久化：键沿用 React 版的 `falcon.term`（改名前的 `mojito.term` 不再认），浏览器版存在 localStorage，
//! 原生存在偏好文件的同名键下；JSON 形状也沿用（[`TermPref`] 的 serde，只去掉了 `engine`）。
//! 读入一律过 [`sanitize_term_pref`]：换机器不跟着走、文件被手改坏、旧版存过 `themeId`
//! （终端单独配色，迁移见 falcon-theme 的 pref）都只是回到默认 / 丢掉那一项。
//!
//! 不在这里的：React 版的 `termFontStack`（拼 CSS font-family 字符串：图标字体打头、内置字体
//! 垫底、再接一长串系统回退）。GPUI 客户端的字体回退链在 falcon-ui 的 `fonts.rs`，顺序与它
//! 刻意不同（见那里的注释），这里只留"字体 id → family 名"的映射与自定义回退列表的解析。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::{js_max, js_min, js_num, js_number, js_round, js_slice_utf16, js_trim};

pub const TERM_PREF_KEY: &str = "falcon.term";

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
            // `cargo xtask vendor-fonts berkeley` 内嵌，family 名跟 TTF name 表一致。
            // 覆盖比 Ioskeley 窄（几乎没有希腊 / 西里尔，盒线也不全），缺的码位顺着
            // 回退落到 Ioskeley，中文再落到 Maple。
            TermFontId::Berkeley => Some(BERKELEY_FONT_FAMILY),
            // 内置的 OFL 回退（子集见 xtask/src/fonts.rs 的 IOSKELEY_RANGES）
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

/// `falcon.term` 的形状
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TermPref {
    pub font_id: TermFontId,
    pub custom_family: String,
    /// 用户追加的回退字体，原文是逗号分隔的 family 列表；顺序就是回退顺序
    pub fallback_families: String,
    #[serde(with = "js_num")]
    pub font_size: f64,
    #[serde(with = "js_num")]
    pub line_height: f64,
    pub cursor_style: TermCursorStyle,
    pub cursor_blink: bool,
}

impl Default for TermPref {
    /// TS 的 `DEFAULT_TERM_PREF`
    fn default() -> Self {
        TermPref {
            font_id: TermFontId::Berkeley,
            custom_family: String::new(),
            fallback_families: String::new(),
            font_size: 13.0,
            line_height: 1.0,
            cursor_style: TermCursorStyle::Block,
            cursor_blink: true,
        }
    }
}

/// 内置默认正文字体的 family 名，必须与内嵌字体的 name 表一致（React 版的 berkeley-mono.css）
pub const BERKELEY_FONT_FAMILY: &str = "TX-02";
/// 内置的 OFL 回退（React 版的 ioskeley-mono.css）
pub const IOSKELEY_FONT_FAMILY: &str = "IoskeleyMonoTerm Nerd Font Mono";
/// 内置 Maple 的 family 名（React 版的 maple-mono.css）
pub const MAPLE_FONT_FAMILY: &str = "Maple Mono NL NF CN";
/// 内置 Nerd Font 纯图标字体的 family 名（用于终端图标托底）
pub const NERD_SYMBOLS_FONT_FAMILY: &str = "Symbols Nerd Font Mono";

/// 判断字符是否属于 Nerd Font 图标码位范围（对应内置 `SymbolsNerdFontMono-Regular` 覆盖的
/// Powerline / Devicons / Codicons / FontAwesome / Octicons / Material Design / IEC 电源符号等）。
///
/// 终端渲染层仅对命中的字符检查主字体是否缺失字形，对普通 ASCII 与 CJK 零额外开销；
/// 主字体缺字时切到内置图标字体绘制，保证 macOS / Windows / Linux 下任意终端字体均有图标托底。
pub fn is_nerd_icon_char(ch: char) -> bool {
    matches!(
        ch as u32,
        // IEC Power Symbols 与少量提示符/状态符号（SymbolsNerdFontMono v3.5.0 非 PUA 码位）
        0x23FB..=0x23FE
            | 0x2630
            | 0x2665
            | 0x26A1
            | 0x276C..=0x2771
            | 0x2B58
            // BMP 私用区：Powerline、Pomicons、Seti-UI、Devicons、Codicons、FontAwesome、Octicons 等
            | 0xE000..=0xF8FF
            // 补充私用区 A：Nerd Fonts v3 Material Design Icons
            | 0xF0000..=0xFFFFD
    )
}

/// 这份偏好的正文字体 family：命名字体给它的 family，`custom` 给去掉首尾空白的
/// `customFamily`（空的算没有），`system` 没有。
pub fn primary_family(pref: &TermPref) -> Option<String> {
    match pref.font_id {
        TermFontId::Custom => Some(js_trim(&pref.custom_family).to_string()).filter(|s| !s.is_empty()),
        id => id.family().map(str::to_string),
    }
}

/// 设置里的逗号分隔原文 → 有序 family 列表。空项忽略；成对引号去掉，方便直接粘贴 CSS
/// font-family 列表。引号内的逗号不拆开（family 名本身可以带逗号）。
pub fn fallback_families(pref: &TermPref) -> Vec<String> {
    let mut result = Vec::new();
    let mut item = String::new();
    let mut quote = None;
    let push = |item: &str, result: &mut Vec<String>| {
        let item = js_trim(item);
        let item = if item.len() >= 2
            && ((item.starts_with('"') && item.ends_with('"')) || (item.starts_with('\'') && item.ends_with('\'')))
        {
            &item[1..item.len() - 1]
        } else {
            item
        };
        let item = js_trim(item);
        if !item.is_empty() {
            result.push(item.to_string());
        }
    };
    for ch in pref.fallback_families.chars() {
        match ch {
            '"' | '\'' if quote == Some(ch) => {
                quote = None;
                item.push(ch);
            }
            '"' | '\'' if quote.is_none() => {
                quote = Some(ch);
                item.push(ch);
            }
            ',' if quote.is_none() => {
                push(&item, &mut result);
                item.clear();
            }
            _ => item.push(ch),
        }
    }
    push(&item, &mut result);
    result
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
/// 多余字段（旧版的 `themeId`、旧 React 版选终端引擎 xterm.js / rio 的 `engine`）直接丢。
/// 数字照 JS 的 `Number(x)` 转（`"16"` 也算 16）。
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
        fallback_families: get("fallbackFamilies").and_then(Value::as_str).map(|s| js_slice_utf16(s, 480).to_string()).unwrap_or_default(),
        font_size: clamp_font_size(js_number(get("fontSize"))),
        line_height: clamp_line_height(js_number(get("lineHeight"))),
        cursor_style,
        cursor_blink: get("cursorBlink") != Some(&Value::Bool(false)),
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

/// 写回存储的 JSON（沿用 React 版 `JSON.stringify(pref)` 的形状，去掉了 `engine`）
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
    fn legacy_fields_are_dropped_other_fields_kept() {
        let pref = sanitize_term_pref(&json!({ "fontSize": 16, "themeId": "dracula", "engine": "rio" }));
        assert_eq!(pref.font_size, 16.0);
        let out = serialize_term_pref(&pref);
        assert!(!out.contains("themeId") && !out.contains("engine"), "{out}");
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
    fn fallback_families_keep_order_and_accept_css_quotes() {
        let pref = sanitize_term_pref(&json!({
            "fallbackFamilies": " Sarasa Term SC, \"PingFang SC\", , 'Family, With Comma' "
        }));
        assert_eq!(fallback_families(&pref), vec!["Sarasa Term SC", "PingFang SC", "Family, With Comma"]);
        assert!(fallback_families(&TermPref::default()).is_empty());
        assert_eq!(sanitize_term_pref(&json!({ "fallbackFamilies": 42 })).fallback_families, "");
    }

    #[test]
    fn json_shape_matches_web() {
        // 以下是 Rust 侧补的：落盘形状、JS 的 Number() 转换、load 的兜底
        assert_eq!(
            serialize_term_pref(&TermPref::default()),
            r#"{"fontId":"berkeley","customFamily":"","fallbackFamilies":"","fontSize":13,"lineHeight":1,"cursorStyle":"block","cursorBlink":true}"#
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

    #[test]
    fn nerd_icon_char_classification() {
        // 典型 Nerd Font 图标（Powerline、Devicon、FontAwesome、Codicon、Material Design、IEC Power）
        for ch in ['\u{e0a0}', '\u{e0b0}', '\u{e718}', '\u{ea60}', '\u{f00c}', '\u{f111}', '\u{f0001}', '\u{23fb}'] {
            assert!(is_nerd_icon_char(ch), "expected icon: U+{:04X}", ch as u32);
        }
        // 普通 ASCII、中文、盒线、一般符号不走图标分支
        for ch in ['a', 'Z', '0', ' ', '中', '文', '─', '│', '█', '→'] {
            assert!(!is_nerd_icon_char(ch), "unexpected icon: U+{:04X}", ch as u32);
        }
    }
}
