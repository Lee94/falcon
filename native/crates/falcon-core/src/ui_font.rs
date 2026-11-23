//! 界面字体偏好（与终端字体 `falcon.term` 相互独立）。
//!
//! 仅影响软件界面（侧栏、标题条、面板、对话框、菜单、设置等），不改终端字体。
//! 默认使用 `system`：主字体交由平台系统界面字体（macOS 为 SF Pro、Windows 为 Segoe UI / 微软雅黑 UI、
//! Linux 为系统无衬线字体），并在界面层统一挂载中文优先回退链（PingFang SC / Microsoft YaHei UI /
//! Noto Sans CJK SC 等），保证英文与数字比例协调、中文清晰锐利且不因系统语言偏好退化为日文字形或宋体。
//!
//! 持久化键为 [`UI_FONT_PREF_KEY`]（`falcon.uiFont`），存在客户端本地偏好里。读入一律经过
//! [`sanitize_ui_font_pref`]：遇到损坏 JSON、未知枚举或多余字段时仅回退坏项，不影响启动。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::js::{js_slice_utf16, js_trim};
use crate::term::{BERKELEY_FONT_FAMILY, MAPLE_FONT_FAMILY};

pub const UI_FONT_PREF_KEY: &str = "falcon.uiFont";

/// 常见中文字体与界面字体的 family 名
pub const PINGFANG_SC_FONT_FAMILY: &str = "PingFang SC";
pub const HIRAGINO_SANS_GB_FONT_FAMILY: &str = "Hiragino Sans GB";
pub const MICROSOFT_YAHEI_UI_FONT_FAMILY: &str = "Microsoft YaHei UI";
pub const NOTO_SANS_CJK_SC_FONT_FAMILY: &str = "Noto Sans CJK SC";
pub const INTER_FONT_FAMILY: &str = "Inter";

/// 软件界面字体选项
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UiFontId {
    /// 默认：平台系统界面字体 + 中文优先回退链
    #[default]
    #[serde(rename = "system")]
    System,
    /// macOS 苹方-简
    #[serde(rename = "pingfang")]
    PingFang,
    /// macOS 冬青黑体简体中文
    #[serde(rename = "hiragino")]
    Hiragino,
    /// Windows 微软雅黑 UI
    #[serde(rename = "microsoft-yahei")]
    MicrosoftYaHei,
    /// 思源黑体 / Noto Sans CJK SC
    #[serde(rename = "noto-sans-sc")]
    NotoSansSc,
    /// Inter（现代英文无衬线，中文自动走中文优先回退链）
    #[serde(rename = "inter")]
    Inter,
    /// 内置 Berkeley Mono TX-02（保留给偏好等宽界面的用户）
    #[serde(rename = "berkeley")]
    Berkeley,
    /// 内置 Maple Mono NL NF CN（等宽中英混排）
    #[serde(rename = "maple")]
    Maple,
    /// 自定义本机已安装字体
    #[serde(rename = "custom")]
    Custom,
}

impl UiFontId {
    /// 全部选项顺序
    pub const ALL: [UiFontId; 9] = [
        UiFontId::System,
        UiFontId::PingFang,
        UiFontId::Hiragino,
        UiFontId::MicrosoftYaHei,
        UiFontId::NotoSansSc,
        UiFontId::Inter,
        UiFontId::Berkeley,
        UiFontId::Maple,
        UiFontId::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            UiFontId::System => "system",
            UiFontId::PingFang => "pingfang",
            UiFontId::Hiragino => "hiragino",
            UiFontId::MicrosoftYaHei => "microsoft-yahei",
            UiFontId::NotoSansSc => "noto-sans-sc",
            UiFontId::Inter => "inter",
            UiFontId::Berkeley => "berkeley",
            UiFontId::Maple => "maple",
            UiFontId::Custom => "custom",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == s)
    }

    /// 字体 id 对应的 family 名。`system`（交给平台默认界面字体）与 `custom`（使用 `customFamily`）
    /// 返回 `None`。
    pub fn family(self) -> Option<&'static str> {
        match self {
            UiFontId::PingFang => Some(PINGFANG_SC_FONT_FAMILY),
            UiFontId::Hiragino => Some(HIRAGINO_SANS_GB_FONT_FAMILY),
            UiFontId::MicrosoftYaHei => Some(MICROSOFT_YAHEI_UI_FONT_FAMILY),
            UiFontId::NotoSansSc => Some(NOTO_SANS_CJK_SC_FONT_FAMILY),
            UiFontId::Inter => Some(INTER_FONT_FAMILY),
            UiFontId::Berkeley => Some(BERKELEY_FONT_FAMILY),
            UiFontId::Maple => Some(MAPLE_FONT_FAMILY),
            UiFontId::System | UiFontId::Custom => None,
        }
    }
}

/// `falcon.uiFont` 的持久化结构
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct UiFontPref {
    pub font_id: UiFontId,
    pub custom_family: String,
}

/// 这份偏好的界面主字体 family：命名字体返回其 family，`custom` 返回去首尾空白后的
/// `customFamily`（空串视为未指定），`system` 返回 `None`（由界面层使用平台系统界面字体）。
pub fn primary_ui_family(pref: &UiFontPref) -> Option<String> {
    match pref.font_id {
        UiFontId::Custom => Some(js_trim(&pref.custom_family).to_string()).filter(|s| !s.is_empty()),
        id => id.family().map(str::to_string),
    }
}

/// 读入清洗：任意 JSON → 合法的界面字体偏好。无法识别的字段值回到默认，多余字段丢弃。
pub fn sanitize_ui_font_pref(raw: &Value) -> UiFontPref {
    let d = UiFontPref::default();
    let get = |k: &str| raw.as_object().and_then(|o| o.get(k));
    let font_id = get("fontId")
        .and_then(Value::as_str)
        .and_then(UiFontId::from_wire)
        .unwrap_or(d.font_id);
    let custom_family = get("customFamily")
        .and_then(Value::as_str)
        .map(|s| js_slice_utf16(s, 120).to_string())
        .unwrap_or_default();
    UiFontPref { font_id, custom_family }
}

/// 从存储原文解析界面字体偏好；缺失、空串、非法 JSON 或 `null` 均返回默认。
pub fn load_ui_font_pref(raw: Option<&str>) -> UiFontPref {
    match raw.filter(|s| !s.is_empty()).map(serde_json::from_str::<Value>) {
        Some(Ok(v)) if !v.is_null() => sanitize_ui_font_pref(&v),
        _ => UiFontPref::default(),
    }
}

/// 写回存储的 JSON
pub fn serialize_ui_font_pref(pref: &UiFontPref) -> String {
    serde_json::to_string(pref).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_is_system_optimized_for_cjk() {
        let d = UiFontPref::default();
        assert_eq!(d.font_id, UiFontId::System);
        assert!(d.custom_family.is_empty());
        assert_eq!(primary_ui_family(&d), None);
        assert_eq!(
            serialize_ui_font_pref(&d),
            r#"{"fontId":"system","customFamily":""}"#
        );
    }

    #[test]
    fn sanitize_recovers_from_invalid_fields_and_drops_extras() {
        assert_eq!(sanitize_ui_font_pref(&json!({})), UiFontPref::default());
        assert_eq!(
            sanitize_ui_font_pref(&json!({ "fontId": "unknown", "customFamily": 123, "extra": true })),
            UiFontPref::default()
        );
        let pref = sanitize_ui_font_pref(&json!({ "fontId": "pingfang", "extra": "drop" }));
        assert_eq!(pref.font_id, UiFontId::PingFang);
        assert_eq!(primary_ui_family(&pref).as_deref(), Some("PingFang SC"));
        let out = serialize_ui_font_pref(&pref);
        assert!(!out.contains("extra"), "{out}");
    }

    #[test]
    fn custom_family_trims_and_truncates() {
        let pref = sanitize_ui_font_pref(&json!({ "fontId": "custom", "customFamily": "  MiSans  " }));
        assert_eq!(primary_ui_family(&pref).as_deref(), Some("MiSans"));

        let blank = sanitize_ui_font_pref(&json!({ "fontId": "custom", "customFamily": "   " }));
        assert_eq!(primary_ui_family(&blank), None);

        let long = "A".repeat(200);
        let truncated = sanitize_ui_font_pref(&json!({ "fontId": "custom", "customFamily": long }));
        assert_eq!(truncated.custom_family.len(), 120);
    }

    #[test]
    fn load_handles_missing_and_corrupt_json() {
        assert_eq!(load_ui_font_pref(None), UiFontPref::default());
        assert_eq!(load_ui_font_pref(Some("")), UiFontPref::default());
        assert_eq!(load_ui_font_pref(Some("null")), UiFontPref::default());
        assert_eq!(load_ui_font_pref(Some("{bad")), UiFontPref::default());

        let saved = UiFontPref { font_id: UiFontId::Berkeley, custom_family: String::new() };
        let raw = serialize_ui_font_pref(&saved);
        assert_eq!(load_ui_font_pref(Some(&raw)), saved);
    }
}
