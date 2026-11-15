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
    Trash,
    Shield,
    Question,
    Key,
    Warning,
    ListTree,
    Copy,
}

pub(crate) fn session_icon(agent: Option<falcon_proto::SessionAgent>) -> AnyElement {
    use falcon_proto::SessionAgent;
    let markup = match agent {
        None => return glyph(Glyph::Terminal).into_element(),
        Some(SessionAgent::Claude) => include_str!("../../assets/agents/claude.svg"),
        Some(SessionAgent::Codex) => include_str!("../../assets/agents/codex.svg"),
        Some(SessionAgent::Grok) => include_str!("../../assets/agents/grok.svg"),
        Some(SessionAgent::Devin) => include_str!("../../assets/agents/devin.svg"),
        Some(SessionAgent::Gemini) => include_str!("../../assets/agents/gemini.svg"),
        Some(SessionAgent::OpenCode) => include_str!("../../assets/agents/opencode.svg"),
        Some(SessionAgent::Cursor) => include_str!("../../assets/agents/cursor-agent.svg"),
        Some(SessionAgent::Copilot) => include_str!("../../assets/agents/copilot.svg"),
        Some(SessionAgent::Aider) => include_str!("../../assets/agents/aider.svg"),
        Some(SessionAgent::Amp) => include_str!("../../assets/agents/amp.svg"),
        Some(SessionAgent::Qwen) => include_str!("../../assets/agents/qwen.svg"),
        Some(SessionAgent::Droid) => include_str!("../../assets/agents/droid.svg"),
        Some(SessionAgent::Agy) => include_str!("../../assets/agents/agy.svg"),
        Some(SessionAgent::Omp) => include_str!("../../assets/agents/omp.svg"),
        Some(_) => return glyph(Glyph::Terminal).into_element(),
    };
    svg(ImageSrc::svg_markup(markup)).w(zpx(15.0)).h(zpx(15.0)).flex_none().into_element()
}

pub fn glyph(kind: Glyph) -> impl IntoElement {
    glyph_sized(kind, zpx(15.0))
}

pub fn glyph_sized(kind: Glyph, size: Points) -> impl IntoElement {
    let paths = match kind {
        Glyph::Shield => "<path d='m12 2 8 3v6c0 5-8 11-8 11S4 16 4 11V5Z M8 11l3 3 5-6'/>",
        Glyph::Question => "<circle cx='12' cy='12' r='9'/><path d='M9 9a3 3 0 0 1 6 0c0 2-3 2-3 5 M12 17h.01'/>",
        Glyph::Key => "<circle cx='8' cy='8' r='5'/><path d='m12 12 9 9 M16 16l3-3 M19 19l3-3'/>",
        Glyph::Warning => "<path d='m12 3 10 18H2Z M12 9v5 M12 17h.01'/>",
        Glyph::ListTree => "<path d='M4 3v14h4 M4 7h4 M12 7h9 M12 17h9'/>",
        Glyph::Copy => "<rect x='8' y='8' width='13' height='13' rx='2'/><path d='M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3'/>",
        Glyph::File => "<path d='M14 2H5v20h14V7Z M14 2v5h5'/>",
        Glyph::ArrowUp => "<path d='M12 20V4 m-7 7 7-7 7 7'/>",
        Glyph::Refresh => "<path d='M20 7a9 9 0 1 0 1 8 M20 2v6h-6'/>",
        Glyph::Trash => "<path d='M3 6h18 M9 6V3h6v3 M5 6l1 15h12l1-15 M10 10v7 M14 10v7'/>",
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
        .w(size)
        .h(size)
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
    icon_button_with_shortcut(id, title, icon, active, None, resolved, action)
}

pub fn icon_button_with_shortcut(
    id: impl Into<String>,
    title: String,
    icon: Glyph,
    active: bool,
    shortcut: Option<&str>,
    resolved: &falcon_theme::ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .label(id.into())
        .focusable(true)
        .tooltip(themed_tooltip(text(title), shortcut.map(shortcut_label).map(Into::into), resolved))
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

fn shortcut_label(spec: &str) -> String {
    nsh_core::keymap::Keystroke::parse_with(spec, apple_keys())
        .expect("static shortcut must parse")
        .label(os(), apple_keys())
}

/// Only show a key that the enclosing form actually handles (submit or modal dismissal).
pub fn button_caption(label: String, spec: &str, resolved: &falcon_theme::ResolvedTheme) -> impl IntoElement {
    view().flex_row().items_center().gap(zpx(8.0))
        .child(text(label).text_xs())
        .child(view().flex_none().px(zpx(4.0)).py(zpx(1.0)).rounded(zpx(4.0))
            .bg(rgb_to_hsla(resolved.ui.muted))
            .text_color(rgb_to_hsla(resolved.ui.muted_foreground))
            .child(text(shortcut_label(spec)).text_size(zpx(10.0))))
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
