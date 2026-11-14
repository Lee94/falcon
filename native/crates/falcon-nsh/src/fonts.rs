//! 内嵌等宽与图标字体注册。
//!
//! `nsh-core` 原生内置 `wuff` WOFF2 解压器，`Root::font` 直接接收仓库 `native/assets/fonts/`
//! 里的 `.woff2` 字节，无需 `build.rs` 预解压成 TTF：
//! - 正文等宽：Berkeley Mono `TX-02`（Regular / Bold / Oblique / BoldOblique）
//! - 盒线与拉丁补全：`IoskeleyMonoTerm Nerd Font Mono`
//! - 中文等宽回退：`Maple Mono NL NF CN`
//! - 终端图标：`Symbols Nerd Font Mono`

use std::sync::{Arc, LazyLock};

use nsh_core::element::Root;

/// 终端与代码区域的默认等宽字体族名。
pub const FONT_FAMILY_MONO: &str = "TX-02";

/// 界面正文与等宽后备字体栈。
pub const FONT_STACK_MONO: &str =
    "\"TX-02\", \"IoskeleyMonoTerm Nerd Font Mono\", \"Maple Mono NL NF CN\", \"Symbols Nerd Font Mono\", monospace";

static EMBEDDED_FONTS: LazyLock<[Arc<[u8]>; 7]> = LazyLock::new(|| {
    [
        Arc::from(&include_bytes!("../../../assets/fonts/berkeley-mono/TX-02-Regular.woff2")[..]),
        Arc::from(&include_bytes!("../../../assets/fonts/berkeley-mono/TX-02-Bold.woff2")[..]),
        Arc::from(&include_bytes!("../../../assets/fonts/berkeley-mono/TX-02-Oblique.woff2")[..]),
        Arc::from(
            &include_bytes!("../../../assets/fonts/berkeley-mono/TX-02-BoldOblique.woff2")[..],
        ),
        Arc::from(
            &include_bytes!("../../../assets/fonts/ioskeley-mono/IoskeleyMonoTermNFM-Latin.woff2")
                [..],
        ),
        Arc::from(
            &include_bytes!("../../../assets/fonts/maple-mono/MapleMonoNL-NF-CN-Regular.woff2")[..],
        ),
        Arc::from(
            &include_bytes!("../../../assets/fonts/nerd-symbols/SymbolsNerdFontMono-Regular.woff2")
                [..],
        ),
    ]
});

/// 将全部内嵌 WOFF2 字体挂载到 NSH `Root`。
pub fn register_embedded_fonts(mut root: Root) -> Root {
    for font in EMBEDDED_FONTS.iter() {
        root = root.font(font.clone());
    }
    root
}
