//! 字体注册：正文字体与回退字体全部嵌进二进制、启动时注册（见 [`register`]）。
//!
//! 字体栈照旧 React 版的 `lib/term.ts` / `styles.css`：界面与终端默认都是 Berkeley Mono（TX-02），
//! 缺的拉丁 / 盒线落到 Ioskeley，中文落到 Maple，图标落到 Symbols Nerd Font Mono。
//!
//! 与旧 React 版的一处差别：它的 CSS 字体栈把 Symbols Nerd Font Mono 排在**主字体前面**——Maple
//! 自带的 NF 图标是宽形，xterm 把 PUA 当 1 格且拒绝缩放，图标会被裁掉。GPUI 永远先查主字体，
//! 而且拒绝把没有 `m` 字形的字体当主字体（实测日志 "has no 'm' character and was not loaded"，
//! Symbols 正是这种纯图标字体），所以把它放在**回退链第一位**：主字体缺的码位先查图标字体，
//! 效果与 React 版的"图标排最前"相同（Berkeley 本身不含 Nerd 图标）。

use std::borrow::Cow;

use gpui_kit::{App, Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, SharedString};

/// 内嵌字体的 family 名（取自字体文件的 name 表，改字体前先确认）。
pub const BERKELEY: &str = "TX-02";
pub const IOSKELEY: &str = "IoskeleyMonoTerm Nerd Font Mono";
pub const MAPLE: &str = "Maple Mono NL NF CN";
pub const NERD_SYMBOLS: &str = "Symbols Nerd Font Mono";

/// build.rs 解好放在 OUT_DIR 里的 TTF
macro_rules! embedded {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/", $name)).as_slice()
    };
}

/// 注册内嵌字体
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

/// 主字体之后的回退链。用户配的 family 插在图标字体之后、内置回退之前：这样既保住 Nerd
/// 图标的宽度口径，又能让自定义中文 / 拉丁字体先于 Maple / Ioskeley 命中。系统 CJK / emoji
/// 兜 Maple 没覆盖的生僻字、假名、谚文、emoji——与 React 版的 FALLBACK_STACK 同一个理由
/// （Maple CN 不是全集）。
pub fn fallbacks(custom: &[String]) -> FontFallbacks {
    let mut families = vec![NERD_SYMBOLS.to_string()];
    families.extend(custom.iter().cloned());
    families.extend([
        IOSKELEY.to_string(),
        MAPLE.to_string(),
        "PingFang SC".to_string(),
        "Hiragino Sans GB".to_string(),
        "Apple Color Emoji".to_string(),
    ]);
    FontFallbacks::from_fonts(families)
}

/// 终端 / 界面用的等宽字体。`family` 是用户在设置里选的（默认 Berkeley），
/// `custom_fallbacks` 是设置里追加的有序回退列表。
pub fn mono(family: impl Into<SharedString>, custom_fallbacks: &[String]) -> Font {
    Font {
        family: family.into(),
        features: FontFeatures::disable_ligatures(),
        fallbacks: Some(fallbacks(custom_fallbacks)),
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_fallbacks_follow_icons_and_precede_builtins() {
        let custom = vec!["Sarasa Term SC".to_string(), "My Icons".to_string()];
        assert_eq!(
            fallbacks(&custom).fallback_list(),
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
        assert_eq!(fallbacks(&[]).fallback_list()[..3], [NERD_SYMBOLS, IOSKELEY, MAPLE]);
    }
}
