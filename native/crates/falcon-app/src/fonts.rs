//! 内嵌字体：启动时一次注册完，没有 web 那套"字到了再重建图集"的等待。
//!
//! 字体栈照 web 的 `lib/term.ts` / `styles.css`：界面与终端默认都是 Berkeley Mono（TX-02），
//! 缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
//!
//! 与 web 的一处差别：web 的 CSS 字体栈把 Symbols Nerd Font Mono 排在**主字体前面**——Maple
//! 自带的 NF 图标是宽形，xterm 把 PUA 当 1 格且拒绝缩放，图标会被裁掉。GPUI 永远先查主字体，
//! 而且拒绝把没有 `m` 字形的字体当主字体（实测日志 "has no 'm' character and was not loaded"，
//! Symbols 正是这种纯图标字体），所以把它放在**回退链第一位**：主字体缺的码位先查图标字体，
//! 效果与 web 的"图标排最前"相同（Berkeley 本身不含 Nerd 图标）。

use std::borrow::Cow;

use gpui_kit::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, SharedString};

/// 内嵌字体的 family 名（取自字体文件的 name 表，改字体前先确认）。
pub const BERKELEY: &str = "TX-02";
pub const IOSKELEY: &str = "IoskeleyMonoTerm Nerd Font Mono";
pub const MAPLE: &str = "Maple Mono NL NF CN";
pub const NERD_SYMBOLS: &str = "Symbols Nerd Font Mono";

macro_rules! embedded {
    ($name:literal) => {
        Cow::Borrowed(include_bytes!(concat!(env!("OUT_DIR"), "/", $name)).as_slice())
    };
}

pub fn register(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        embedded!("TX-02-Regular.ttf"),
        embedded!("TX-02-Bold.ttf"),
        embedded!("TX-02-Oblique.ttf"),
        embedded!("TX-02-BoldOblique.ttf"),
        embedded!("IoskeleyMonoTerm.ttf"),
        embedded!("MapleMonoNL-NF-CN.ttf"),
        embedded!("SymbolsNerdFontMono.ttf"),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        log::error!("注册内嵌字体失败：{err:#}");
    }
}

/// 主字体之后的回退链。系统 CJK / emoji 兜 Maple 没覆盖的生僻字、假名、谚文、emoji——与
/// web 的 FALLBACK_STACK 同一个理由（Maple CN 不是全集）。
pub fn fallbacks() -> FontFallbacks {
    FontFallbacks::from_fonts(vec![
        NERD_SYMBOLS.to_string(),
        IOSKELEY.to_string(),
        MAPLE.to_string(),
        "PingFang SC".to_string(),
        "Hiragino Sans GB".to_string(),
        "Apple Color Emoji".to_string(),
    ])
}

/// 终端 / 界面用的等宽字体。`family` 是用户在设置里选的（默认 Berkeley）。
pub fn mono(family: impl Into<SharedString>) -> Font {
    Font {
        family: family.into(),
        features: FontFeatures::disable_ligatures(),
        fallbacks: Some(fallbacks()),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}
