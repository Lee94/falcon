//! 桌面壳层共用的矢量图标与紧凑按钮。颜色继承语义 token，尺寸随界面缩放。
use crate::theme::rgb_to_hsla;
use crate::shell::popups::themed_tooltip;
use crate::zoom::zpx;
use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

#[derive(Clone, Copy)]
pub enum Glyph {
    Folder,
    Changes,
    Monitor,
    Server,
    Terminal,
    ChevronRight,
    ChevronDown,
    Plus,
    Minus,
    Settings,
    Sidebar,
    Search,
    Pin,
    Close,
    Check,
    Detach,
    Maximize,
    Restore,
    Activity,
    Box,
    Link,
    Branch,
    File,
    ArrowUp,
    Refresh,
}

pub fn glyph(kind: Glyph) -> impl IntoElement {
    let paths = match kind {
        Glyph::File => "<path d='M14 2H5v20h14V7Z M14 2v5h5'/>",
        Glyph::ArrowUp => "<path d='M12 20V4 m-7 7 7-7 7 7'/>",
        Glyph::Refresh => "<path d='M20 7a9 9 0 1 0 1 8 M20 2v6h-6'/>",
        Glyph::Folder => "<path d='M3 7V5h6l2 2h10v12H3Z'/>",
        Glyph::Changes => "<path d='M14 2H6v20h12V6Z M14 2v5h5 M8 12h8 M8 16h8 M12 9v6'/>",
        Glyph::Monitor => {
            "<rect x='3' y='3' width='18' height='13' rx='2'/><path d='M8 21h8 M12 16v5'/>"
        }
        Glyph::Server => {
            "<rect x='3' y='3' width='18' height='7' rx='2'/><rect x='3' y='14' width='18' height='7' rx='2'/><path d='M7 6.5h.01 M7 17.5h.01'/>"
        }
        Glyph::Terminal => "<path d='m4 5 6 6-6 6 M13 18h7'/>",
        Glyph::ChevronRight => "<path d='m9 5 7 7-7 7'/>",
        Glyph::ChevronDown => "<path d='m5 9 7 7 7-7'/>",
        Glyph::Minus => "<path d='M5 12h14'/>",
        Glyph::Plus => "<path d='M12 5v14 M5 12h14'/>",
        Glyph::Settings => {
            "<path d='m9 3-1 3-3 1-2 5 3 2 1 3 5 4 2-3 3-1 4-5-3-2-1-3-5-4Z'/><circle cx='12' cy='12' r='3'/>"
        }
        Glyph::Sidebar => "<rect x='3' y='4' width='18' height='16' rx='2'/><path d='M9 4v16'/>",
        Glyph::Search => "<circle cx='10.5' cy='10.5' r='6.5'/><path d='m16 16 5 5'/>",
        Glyph::Pin => "<path d='m16 3 5 5-4 1-4 6-4-4 6-4Z M9 15l-6 6'/>",
        Glyph::Check => "<path d='m5 12 4 4L19 6'/>",
        Glyph::Close => "<path d='m6 6 12 12 M6 18 18 6'/>",
        Glyph::Detach => "<path d='M4 7h16 M4 12h16 M4 17h16'/>",
        Glyph::Maximize => "<path d='M8 3H3v5 M16 3h5v5 M3 16v5h5 M21 16v5h-5'/>",
        Glyph::Restore => "<path d='M3 8h5V3 M21 8h-5V3 M3 16h5v5 M21 16h-5v5'/>",
        Glyph::Activity => "<path d='M2 12h4l3-9 6 18 3-9h4'/>",
        Glyph::Link => {
            "<path d='m10 13 4-4 M8 16l-1 1a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0 M16 8l1-1a4 4 0 1 1 6 6l-4 4a4 4 0 0 1-6 0'/>"
        }
        Glyph::Box => "<path d='m3 7 9-5 9 5v10l-9 5-9-5Z M3 7l9 5 9-5 M12 12v10'/>",
        Glyph::Branch => {
            "<circle cx='6' cy='4' r='2'/><circle cx='18' cy='6' r='2'/><circle cx='6' cy='20' r='2'/><path d='M6 6v12 M18 8c0 6-12 2-12 8'/>"
        }
    };
    let markup = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='1.75' stroke-linecap='round' stroke-linejoin='round'>{paths}</svg>"
    );
    svg(ImageSrc::svg_markup(&markup))
        .w(zpx(15.0))
        .h(zpx(15.0))
        .flex_none()
}

pub fn icon_button(
    id: impl Into<String>,
    title: String,
    icon: Glyph,
    active: bool,
    resolved: &falcon_theme::ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .label(id.into())
        .focusable(true)
        .tooltip(themed_tooltip(text(title), None, resolved))
        .w(zpx(28.0))
        .h(zpx(28.0))
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(zpx(8.0))
        .cursor_pointer()
        .text_color(rgb_to_hsla(if active {
            ui.tint_foreground
        } else {
            ui.muted_foreground
        }))
        .when(active, |v| v.bg(rgb_to_hsla(ui.tint)))
        .hover(|s| {
            s.bg(rgb_to_hsla(ui.muted))
                .text_color(rgb_to_hsla(ui.foreground))
        })
        .on_press(action)
        .child(glyph(icon))
}

/// 图标作为内嵌资源使用，离线首屏也能显示；不依赖服务端图片请求。
pub fn app_icon_source(id: &str) -> ImageSrc {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::sync::LazyLock;
    static SOURCES: LazyLock<Vec<(&'static str, String)>> = LazyLock::new(|| {
        let images: [(&str, &[u8]); 8] = [
            (
                "emberwing",
                include_bytes!("../../assets/app-icons/emberwing.png"),
            ),
            (
                "voltwing",
                include_bytes!("../../assets/app-icons/voltwing.png"),
            ),
            (
                "voltwing-spark",
                include_bytes!("../../assets/app-icons/voltwing-spark.png"),
            ),
            (
                "voltwing-night",
                include_bytes!("../../assets/app-icons/voltwing-night.png"),
            ),
            (
                "voltwing-ivory",
                include_bytes!("../../assets/app-icons/voltwing-ivory.png"),
            ),
            ("glyph", include_bytes!("../../assets/app-icons/glyph.png")),
            ("flash", include_bytes!("../../assets/app-icons/flash.png")),
            (
                "thunderbird",
                include_bytes!("../../assets/app-icons/thunderbird.png"),
            ),
        ];
        images
            .into_iter()
            .map(|(id, bytes)| {
                (
                    id,
                    format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
                )
            })
            .collect()
    });
    SOURCES
        .iter()
        .find(|(name, _)| *name == id)
        .unwrap_or(&SOURCES[0])
        .1
        .clone()
        .into()
}

/// 勾选状态用矢量绘制，避免等宽字体的 Unicode 方框在不同平台上显示成细小字形。
pub fn check_indicator(checked: bool, resolved: &falcon_theme::ResolvedTheme) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .w(zpx(14.0))
        .h(zpx(14.0))
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(zpx(4.0))
        .border_1()
        .border_color(crate::theme::rgba_to_hsla(ui.border))
        .when(checked, |v| {
            v.bg(rgb_to_hsla(ui.foreground))
                .text_color(rgb_to_hsla(ui.background))
                .child(glyph(Glyph::Check))
        })
}
