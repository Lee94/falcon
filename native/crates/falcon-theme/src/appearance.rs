//! 主题的深浅：`@falcon/shared` 的 `appearanceFromHex`（`packages/shared/src/termEnv.ts`）。
//!
//! **按底色亮度判，不按明暗模式**（ADR 0006）：给浅色槽位选一套深底主题，界面就按
//! 深色算。这个判定在 web 有三处必须一致——shared 的 `appearanceFromHex`、web
//! `index.html` 的内联首帧脚本、catalog 给选择器分深浅——原生这边是第四处，公式
//! 照抄 shared（WCAG 相对亮度 > 0.5 为浅）。
//!
//! 和 falcon-proto 的 `TermAppearance` 是同一个概念（线上字面量都是 `"light"` /
//! `"dark"`）。这里不依赖 falcon-proto，免得主题层被协议层的改动牵着走，app 层
//! 两边互转就是一个 match。

use serde::{Deserialize, Serialize};

use crate::color::Rgb;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Light,
    Dark,
}

impl Appearance {
    pub fn is_dark(self) -> bool {
        self == Appearance::Dark
    }

    /// `"light"` / `"dark"`，与 TS 的 `TermAppearance` 字面量相同
    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }
}

/// shared 的 `hexLuminance`。
///
/// 注意线性化阈值是旧版 WCAG 的 0.03928，而 `color::luminance` 用的是 sRGB 规范的
/// 0.04045——照抄两边各自的写法。对 8 位分量两者没有区别：10/255 = 0.0392 在两个
/// 阈值之下，11/255 = 0.0431 在两个阈值之上，没有哪个整数分量落在两者之间。
fn hex_luminance(c: Rgb) -> f64 {
    let lin = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// `appearanceFromHex`：相对亮度 > 0.5 为浅色
pub fn appearance_from_rgb(background: Rgb) -> Appearance {
    if hex_luminance(background) > 0.5 { Appearance::Light } else { Appearance::Dark }
}

/// `appearanceFromHex(hex | undefined)`：缺省当 `#000000`，解析失败亮度当 0——都是深色
pub fn appearance_from_hex(hex: Option<&str>) -> Appearance {
    match hex.and_then(Rgb::parse_hex) {
        Some(c) => appearance_from_rgb(c),
        None => Appearance::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_luminance_not_by_mode() {
        assert_eq!(appearance_from_hex(Some("#ffffff")), Appearance::Light);
        assert_eq!(appearance_from_hex(Some("#0a0a0a")), Appearance::Dark);
        assert_eq!(appearance_from_hex(Some("#fdf6e3")), Appearance::Light);
        assert_eq!(appearance_from_hex(None), Appearance::Dark);
        assert_eq!(appearance_from_hex(Some("junk")), Appearance::Dark);
    }

    /// 两个线性化阈值在 8 位分量上等价（见 `hex_luminance` 的注释）
    #[test]
    fn thresholds_agree_on_bytes() {
        for v in 0..=255u8 {
            let c = Rgb::new(v, v, v);
            assert_eq!(hex_luminance(c), crate::color::luminance(c), "{v}");
        }
    }
}
