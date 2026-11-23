//! 字体注册与字体栈：界面字体（UI）与终端字体（Terminal）完全分离。
//!
//! 内嵌字体在启动时通过 [`register`] 一次性注册进 GPUI 文本系统：
//! - `TX-02`（Berkeley Mono，Regular / Bold / Oblique / BoldOblique）：终端默认正文字体与界面等宽体；
//! - `IoskeleyMonoTerm Nerd Font Mono`：终端拉丁与盒线回退；
//! - `Maple Mono NL NF CN`：终端等宽中文回退；
//! - `Symbols Nerd Font Mono`：终端 Nerd Font 图标托底字体。
//!
//! 两条独立字体栈：
//! - **界面字体**（[`ui_fallbacks`] / [`resolve_ui_family`]）：默认跟随平台系统界面字体（macOS 为
//!   SF Pro、Windows 为 Segoe UI / 微软雅黑 UI、Linux 为系统无衬线字体），并挂上按平台排序的
//!   简体中文优先回退链（PingFang SC / Microsoft YaHei UI / Noto Sans CJK SC），避免英文系统下中文
//!   退化为日文字形或宋体；用户也可以为界面单独指定任意字体。
//! - **终端字体**（[`term_fallbacks`] / [`resolve_term_family`] / [`should_use_nerd_icon_fallback`]）：
//!   用户可自由指定一种终端字体。回退链以 `Symbols Nerd Font Mono` 托底，且渲染层对主字体缺失的
//!   Nerd Font 图标码位显式切到内嵌图标字体绘制，确保 Windows（DirectWrite 的 `IDWriteFontFallbackBuilder`
//!   只检索系统字体集合，看不到 `add_fonts` 加载的内嵌字体）、macOS 与 Linux 上图标均可正常显示。

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use falcon_core::term::{
    BERKELEY_FONT_FAMILY, IOSKELEY_FONT_FAMILY, MAPLE_FONT_FAMILY, NERD_SYMBOLS_FONT_FAMILY,
    TermFontId, TermPref, is_nerd_icon_char, primary_family,
};
use falcon_core::ui_font::{UiFontPref, primary_ui_family};
use falcon_platform::PlatformInfo;
use gpui_kit::{
    App, Font, FontFallbacks, FontFeatures, FontId, FontStyle, FontWeight, SharedString, TextRun,
    WindowTextSystem, black, px,
};

/// GPUI 在各平台上映射为系统界面字体的虚拟名称
pub const SYSTEM_UI_FONT: &str = ".SystemUIFont";

/// 内嵌字体的 family 名（取自字体文件的 name 表，修改内嵌字体前先确认）。
pub const BERKELEY: &str = BERKELEY_FONT_FAMILY;
pub const IOSKELEY: &str = IOSKELEY_FONT_FAMILY;
pub const MAPLE: &str = MAPLE_FONT_FAMILY;
pub const NERD_SYMBOLS: &str = NERD_SYMBOLS_FONT_FAMILY;

/// build.rs 解压放在 OUT_DIR 里的 TTF
macro_rules! embedded {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name)).as_slice()
    };
}

/// 注册全部内嵌字体
pub fn register(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = [
        embedded!("TX-02-Regular.ttf"),
        embedded!("TX-02-Bold.ttf"),
        embedded!("TX-02-Oblique.ttf"),
        embedded!("TX-02-BoldOblique.ttf"),
        embedded!("IoskeleyMonoTerm.ttf"),
        embedded!("MapleMonoNL-NF-CN.ttf"),
        embedded!("SymbolsNerdFontMono.ttf"),
    ]
    .into_iter()
    .map(Cow::Borrowed)
    .collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        log::error!("注册内嵌字体失败：{err:#}");
    }
}

/// 是否为随二进制内嵌、必定可用的字体 family
fn is_embedded_family(family: &str) -> bool {
    matches!(family, BERKELEY | IOSKELEY | MAPLE | NERD_SYMBOLS)
}

/// 本机已安装与内嵌的字体 family 列表（进程内只枚举一次；macOS 上枚举约 100ms）。
/// 调用须发生在 [`register`] 之后，以包含内嵌字体。
pub fn installed_font_names(cx: &App) -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| cx.text_system().all_font_names())
}

/// 快速检查某 family 是否在已枚举字体表中（不触发缺失字体告警，用于筛选下拉菜单可选项）。
pub fn is_family_listed(family: &str, cx: &App) -> bool {
    family == SYSTEM_UI_FONT
        || is_embedded_family(family)
        || installed_font_names(cx).iter().any(|n| n == family)
}

/// 检查某 family 是否能被当前平台的文本系统直接解析。
///
/// 先查内嵌常量与已枚举列表（零告警）；未命中时再用 GPUI 的 `resolve_font` 探测一次，
/// 以兼容 Windows 上中文字体在 `all_font_names` 中返回本地化名称（如「微软雅黑 UI」）但按英文名
/// （`"Microsoft YaHei UI"`）仍可被 DirectWrite 直接打开的情况，以及进程启动后新安装的字体。
pub fn is_family_available(family: &str, cx: &App) -> bool {
    if family.is_empty() {
        return false;
    }
    if is_family_listed(family, cx) {
        return true;
    }
    let probe = Font {
        family: SharedString::from(family.to_string()),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let text_system = cx.text_system();
    let id = text_system.resolve_font(&probe);
    text_system.get_font_for_id(id).is_some_and(|f| f.family.as_ref() == family)
}

// ---------------------------------------------------------------------------
// 界面字体（UI Font）
// ---------------------------------------------------------------------------

/// 界面字体的中文优先回退链，按运行平台排列，保证混合中英文界面排版清晰且不退化为日文字形或宋体。
pub fn ui_fallbacks(info: PlatformInfo, primary: &str) -> FontFallbacks {
    let list: &[&str] = if info.mac {
        &["PingFang SC", "Hiragino Sans GB", "Heiti SC", "STHeiti", "Apple Color Emoji"]
    } else if info.windows {
        &["Microsoft YaHei UI", "Microsoft YaHei", "DengXian", "SimHei", "Segoe UI Emoji"]
    } else {
        &[
            "Noto Sans CJK SC",
            "Source Han Sans SC",
            "Noto Sans SC",
            "WenQuanYi Micro Hei",
            "PingFang SC",
            "Microsoft YaHei UI",
            "Noto Color Emoji",
        ]
    };
    let families = list
        .iter()
        .copied()
        .filter(|name| *name != primary)
        .map(str::to_string)
        .collect();
    FontFallbacks::from_fonts(families)
}

/// 把界面字体偏好解析为实际生效的 family，以及用户请求的字体是否在本机可用。
///
/// 请求的字体不存在时回退到 `system_ui_family`，避免 GPUI 走内置 `fallback_font_stack` 时丢弃
/// 我们的中文优化回退链。
pub fn resolve_ui_family(
    pref: &UiFontPref,
    system_ui_family: &SharedString,
    cx: &App,
) -> (SharedString, bool) {
    let Some(requested) = primary_ui_family(pref) else {
        return (system_ui_family.clone(), true);
    };
    if is_family_available(&requested, cx) {
        (SharedString::from(requested), true)
    } else {
        (system_ui_family.clone(), false)
    }
}

// ---------------------------------------------------------------------------
// 终端字体（Terminal Font）与图标托底
// ---------------------------------------------------------------------------

/// 终端主字体之后的回退链。
///
/// 顺序约定：
/// 1. `Symbols Nerd Font Mono`：排在最前，确保任意非 Nerd 字体缺失图标时先命中单格宽度的标准图标；
/// 2. 用户自定义的回退列表（`falcon.term.fallbackFamilies`）；
/// 3. 内嵌 `IoskeleyMonoTerm`（拉丁 / 盒线）与 `Maple Mono NL NF CN`（等宽中文）；
/// 4. 平台系统中文与 Emoji 字体，兜底 Maple 未覆盖的生僻字、假名、谚文与彩色表情。
pub fn term_fallbacks(custom: &[String], info: PlatformInfo) -> FontFallbacks {
    let mut families = vec![NERD_SYMBOLS.to_string()];
    families.extend(custom.iter().cloned());
    families.extend([IOSKELEY.to_string(), MAPLE.to_string()]);
    let platform_cjk: &[&str] = if info.mac {
        &["PingFang SC", "Hiragino Sans GB", "Apple Color Emoji"]
    } else if info.windows {
        &["Microsoft YaHei UI", "Microsoft YaHei", "Segoe UI Emoji"]
    } else {
        &["Noto Sans Mono CJK SC", "Noto Sans CJK SC", "Noto Color Emoji"]
    };
    families.extend(platform_cjk.iter().map(|s| (*s).to_string()));
    FontFallbacks::from_fonts(families)
}

/// 终端使用的等宽字体。`family` 须是已验证可用的 family，避免 GPUI 落回默认字体栈而丢失 `fallbacks`。
pub fn mono(family: impl Into<SharedString>, custom_fallbacks: &[String], info: PlatformInfo) -> Font {
    Font {
        family: family.into(),
        features: FontFeatures::disable_ligatures(),
        fallbacks: Some(term_fallbacks(custom_fallbacks, info)),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// 平台默认系统等宽字体候选列表
fn system_mono_candidates(info: PlatformInfo) -> &'static [&'static str] {
    if info.mac {
        &["Menlo", "Monaco"]
    } else if info.windows {
        &["Cascadia Mono", "Consolas", "Courier New"]
    } else {
        &["DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono"]
    }
}

/// 把终端字体偏好解析为实际生效的 family，以及用户请求的字体是否在本机可用。
///
/// - `System`：按当前平台选择首个已安装的系统等宽字体，都没有时退回内嵌 `TX-02`；
/// - 其他预设 / `Custom`：若本机未安装，退回内嵌 `TX-02`（同时保留完整终端回退链与图标托底），
///   并把 `available` 记为 `false` 供设置页提示。
pub fn resolve_term_family(
    pref: &TermPref,
    info: PlatformInfo,
    cx: &App,
) -> (SharedString, bool) {
    if pref.font_id == TermFontId::System {
        let family = system_mono_candidates(info)
            .iter()
            .copied()
            .find(|name| is_family_available(name, cx))
            .unwrap_or(BERKELEY);
        return (SharedString::from(family), true);
    }
    let Some(requested) = primary_family(pref) else {
        return (SharedString::from(BERKELEY), true);
    };
    if is_family_available(&requested, cx) {
        (SharedString::from(requested), true)
    } else {
        (SharedString::from(BERKELEY), false)
    }
}

/// 终端图标托底用的内嵌 Nerd Font 纯图标字体。
///
/// GPUI Fast 支持加载不含 `m` 字形的字体（见 gpui-fast-apple 的 `fonts_without_an_m_load_and_lay_out_their_script`），
/// 直接作为 `TextRun` 主字体在 macOS、Windows（DirectWrite `custom_font_collection`）与 Linux 上均生效。
pub fn nerd_icon_font() -> Font {
    Font {
        family: SharedString::from(NERD_SYMBOLS),
        features: FontFeatures::disable_ligatures(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

/// 判断终端主字体在绘制 `ch` 时是否需要切到内嵌 `Symbols Nerd Font Mono` 托底。
///
/// 仅当同时满足三个条件时返回 `true`：
/// 1. `ch` 位于 Nerd Font 图标码位范围（[`is_nerd_icon_char`]，普通文本直接短路返回）；
/// 2. 主字体自身（不含回退链）没有 `ch` 的原生字形（若用户选用的是自带图标的 Nerd Font，优先用主字体）；
/// 3. 内嵌 `Symbols Nerd Font Mono` 包含 `ch` 的有效字形。
///
/// 探测结果按 `(FontId, char)` 缓存在主线程 `thread_local` 中，首帧之后为 O(1) 查表。
pub fn should_use_nerd_icon_fallback(
    ch: char,
    primary_font: &Font,
    text_system: &WindowTextSystem,
) -> bool {
    if !is_nerd_icon_char(ch) {
        return false;
    }
    let bare_primary = Font {
        fallbacks: None,
        ..primary_font.clone()
    };
    if font_has_native_glyph(&bare_primary, 'm', ch, text_system) {
        return false;
    }
    let icon_font = nerd_icon_font();
    font_has_native_glyph(&icon_font, '\u{e0b0}', ch, text_system)
}

/// 检测 `bare_font`（必须 `fallbacks == None`）自身是否包含 `ch` 的非 `.notdef` 字形。
///
/// 通过对比单字符 `shape_line` 产出的 `ShapedRun.font_id` 与基准字符 `ref_char` 的 `font_id`，
/// 同时检查 `glyph.id.0 != 0`：
/// - macOS 上 CoreText 回退到其他字体或 `.LastResort` 时 `run.font_id` 与基准不同；
/// - Windows 上 DirectWrite 的 `glyph_for_char` 对缺字会返回 `Some(GlyphId(0))`，这里检查
///   `glyph.id.0 != 0` 与 `run.font_id == ref_font_id` 可准确判缺。
fn font_has_native_glyph(
    bare_font: &Font,
    ref_char: char,
    ch: char,
    text_system: &WindowTextSystem,
) -> bool {
    thread_local! {
        static CACHE: RefCell<HashMap<(FontId, char), bool>> = RefCell::new(HashMap::new());
    }
    let resolved_id = text_system.resolve_font(bare_font);
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&(resolved_id, ch)).copied()) {
        return hit;
    }
    // 探测用固定字号，避免随终端缩放往字体尺寸缓存里写入多份探测条目
    let probe_size = px(16.);
    let shape_single = |c: char| {
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        let run = TextRun {
            len: s.len(),
            font: bare_font.clone(),
            color: black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        text_system.shape_line(SharedString::from(s.to_string()), probe_size, &[run], None)
    };
    let ref_line = shape_single(ref_char);
    let ref_font_id = ref_line
        .runs
        .first()
        .map(|r| r.font_id)
        .unwrap_or(resolved_id);
    let target_line = shape_single(ch);
    let present = !target_line.runs.is_empty()
        && target_line.runs.iter().all(|r| {
            r.font_id == ref_font_id
                && !r.glyphs.is_empty()
                && r.glyphs.iter().all(|g| g.id.0 != 0)
        });
    CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if map.len() > 8192 {
            map.clear();
        }
        map.insert((resolved_id, ch), present);
    });
    present
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: PlatformInfo = PlatformInfo { mac: true, windows: false };
    const WIN: PlatformInfo = PlatformInfo { mac: false, windows: true };
    const LINUX: PlatformInfo = PlatformInfo { mac: false, windows: false };

    #[test]
    fn custom_fallbacks_follow_icons_and_precede_builtins() {
        let custom = vec!["Sarasa Term SC".to_string(), "My Icons".to_string()];
        assert_eq!(
            term_fallbacks(&custom, MAC).fallback_list(),
            [
                NERD_SYMBOLS,
                "Sarasa Term SC",
                "My Icons",
                IOSKELEY,
                MAPLE,
                "PingFang SC",
                "Hiragino Sans GB",
                "Apple Color Emoji",
            ]
        );
        assert_eq!(term_fallbacks(&[], MAC).fallback_list()[..3], [NERD_SYMBOLS, IOSKELEY, MAPLE]);
        assert_eq!(
            term_fallbacks(&[], WIN).fallback_list(),
            [
                NERD_SYMBOLS,
                IOSKELEY,
                MAPLE,
                "Microsoft YaHei UI",
                "Microsoft YaHei",
                "Segoe UI Emoji",
            ]
        );
        assert_eq!(
            term_fallbacks(&[], LINUX).fallback_list(),
            [
                NERD_SYMBOLS,
                IOSKELEY,
                MAPLE,
                "Noto Sans Mono CJK SC",
                "Noto Sans CJK SC",
                "Noto Color Emoji",
            ]
        );
    }

    #[test]
    fn ui_fallbacks_prioritize_platform_cjk_and_deduplicate_primary() {
        assert_eq!(
            ui_fallbacks(MAC, SYSTEM_UI_FONT).fallback_list(),
            ["PingFang SC", "Hiragino Sans GB", "Heiti SC", "STHeiti", "Apple Color Emoji"]
        );
        assert_eq!(
            ui_fallbacks(MAC, "PingFang SC").fallback_list(),
            ["Hiragino Sans GB", "Heiti SC", "STHeiti", "Apple Color Emoji"]
        );
        assert_eq!(
            ui_fallbacks(WIN, SYSTEM_UI_FONT).fallback_list(),
            ["Microsoft YaHei UI", "Microsoft YaHei", "DengXian", "SimHei", "Segoe UI Emoji"]
        );
        assert_eq!(
            ui_fallbacks(WIN, "Microsoft YaHei UI").fallback_list(),
            ["Microsoft YaHei", "DengXian", "SimHei", "Segoe UI Emoji"]
        );
        assert_eq!(
            ui_fallbacks(LINUX, SYSTEM_UI_FONT).fallback_list()[0],
            "Noto Sans CJK SC"
        );
    }

    #[test]
    fn every_ui_and_term_font_locale_key_exists() {
        let locale: serde_json::Value =
            serde_json::from_str(include_str!("../locales/zh-CN.json")).unwrap();
        let has = |key: &str| {
            key.split('.')
                .try_fold(&locale, |v, part| v.get(part))
                .is_some_and(|v| v.is_string())
        };
        for id in falcon_core::ui_font::UiFontId::ALL {
            let key = format!("uiFont.font_{}", id.as_str().replace('-', "_"));
            assert!(has(&key), "缺少界面字体文案键 {key}");
        }
        for id in TermFontId::ALL {
            let key = format!("term.font_{}", id.as_str().replace('-', "_"));
            assert!(has(&key), "缺少终端字体文案键 {key}");
        }
        for key in [
            "settings.uiFont",
            "settings.uiFontHint",
            "settings.uiCustomFont",
            "settings.uiCustomFontPlaceholder",
            "settings.fontNotFound",
            "settings.termFont",
            "settings.termFontHint",
            "settings.termCustomFont",
            "settings.termFallbackFont",
        ] {
            assert!(has(key), "缺少设置文案键 {key}");
        }
    }
}
