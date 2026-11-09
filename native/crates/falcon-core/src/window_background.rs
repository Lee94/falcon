//! 应用窗口底的透明效果。
//!
//! 浮动岛骨架（ADR 0011）里，`app` 是岛与岛之间露出的那一层窗口底。侧栏、主区、右面板用面板
//! 底色，不跟这层一起变。这里只决定窗口底怎么画，以及偏好里存的那一个词。
//!
//! 偏好键 [`APP_BACKGROUND_PREF_KEY`]，值是 `opaque` / `transparent` / `blurred`。认不出的值
//! （手改坏了、以后删掉的效果）回到实色，避免窗口底直接消失。系统窗口要不要真透明由界面层按这个
//! 枚举去设，这里不碰平台。

/// 偏好键。跟 `falcon.term` 一样存在客户端偏好里，不进服务端
pub const APP_BACKGROUND_PREF_KEY: &str = "falcon.windowBackground";

/// 窗口底的效果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppBackground {
    /// 实色，桌面不透出来
    #[default]
    Opaque,
    /// 带透明度，透出桌面
    Transparent,
    /// 带透明度，并请系统把窗口背后模糊
    Blurred,
}

impl AppBackground {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Transparent => "transparent",
            Self::Blurred => "blurred",
        }
    }

    /// 读偏好。`None`、空串、不认识的词都是实色
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("transparent") => Self::Transparent,
            Some("blurred") => Self::Blurred,
            _ => Self::Opaque,
        }
    }

    /// 画在窗口根上的 `app` 不透明度。
    ///
    /// 标题条和右侧活动栏直接落在这层上，完全镂空时字会贴在任意壁纸上，所以透明和模糊都留一层
    /// 主题色。模糊再薄一些：系统已经把背后糊开，色层太厚看起来仍是一块实色。
    pub fn fill_alpha(self) -> f32 {
        match self {
            Self::Opaque => 1.0,
            Self::Transparent => 0.72,
            Self::Blurred => 0.55,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_values_stay_opaque() {
        assert_eq!(AppBackground::parse(None), AppBackground::Opaque);
        assert_eq!(AppBackground::parse(Some("")), AppBackground::Opaque);
        assert_eq!(AppBackground::parse(Some("  ")), AppBackground::Opaque);
        assert_eq!(AppBackground::parse(Some("mica")), AppBackground::Opaque);
        assert_eq!(AppBackground::parse(Some("TRANSPARENT")), AppBackground::Opaque);
    }

    #[test]
    fn stored_word_roundtrips() {
        for value in [AppBackground::Opaque, AppBackground::Transparent, AppBackground::Blurred] {
            assert_eq!(AppBackground::parse(Some(value.as_str())), value);
        }
        assert_eq!(AppBackground::parse(Some("  blurred  ")), AppBackground::Blurred);
    }

    #[test]
    fn glass_keeps_a_tint_and_blur_is_thinner() {
        assert_eq!(AppBackground::Opaque.fill_alpha(), 1.0);
        let transparent = AppBackground::Transparent.fill_alpha();
        let blurred = AppBackground::Blurred.fill_alpha();
        assert!(transparent > 0.0 && transparent < 1.0);
        assert!(blurred > 0.0 && blurred < transparent);
    }
}
