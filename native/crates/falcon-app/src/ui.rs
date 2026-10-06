//! 界面小件：岛、图标按钮、状态记号、主机色条。颜色一律取 [`crate::theme::Ui`] 的语义 token。

use falcon_proto::SessionState;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, IntoElement, Pixels, SharedString,
    Stateful, Window, div, hsla,
};

use crate::theme::{Ui, radius};
use crate::zoom::zpx;

/// 浮动岛（ADR 0011）：面板底一律 `background`，圆角 LG，不画边框
pub fn island(cx: &App) -> Div {
    let ui = Ui::global(cx);
    div()
        .bg(ui.background)
        .rounded(radius::LG)
        .overflow_hidden()
}

/// 岛里再嵌一块（统计卡、代码块、分段控件的槽）：借窗口底色，不用边框
pub fn sunken(cx: &App) -> Div {
    let ui = Ui::global(cx);
    div().bg(ui.app).rounded(radius::MD)
}

pub fn icon(name: IconName) -> Icon {
    Icon::new(name)
}

/// 小号图标按钮：hover 灰、按下 / 选中用 tint（"当前选中"用 tint，hover 用灰）
pub fn icon_button(
    id: impl Into<ElementId>,
    name: IconName,
    tooltip: impl Into<SharedString>,
    size: Pixels,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let ui = Ui::global(cx);
    let tooltip: SharedString = tooltip.into();
    div()
        .id(id.into())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(size)
        .rounded(zpx(6.))
        .text_color(ui.muted_foreground)
        .cursor_pointer()
        .hover(|s| s.bg(ui.muted).text_color(ui.foreground))
        .child(icon(name).size(size * 0.6))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .on_click(on_click)
}

/// 会话状态记号：形状 + 颜色双编码（web 的 StatusMark）。creating 是纯前端状态
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Active,
    Unverified,
    Dead,
    Creating,
}

impl From<SessionState> for Mark {
    fn from(s: SessionState) -> Self {
        match s {
            SessionState::Active => Mark::Active,
            SessionState::Unverified => Mark::Unverified,
            SessionState::Dead => Mark::Dead,
            _ => Mark::Active,
        }
    }
}

impl Mark {
    pub fn label(self) -> String {
        use rust_i18n::t;
        match self {
            Mark::Active => t!("session.state_active").to_string(),
            Mark::Unverified => t!("session.state_unverified").to_string(),
            Mark::Dead => t!("session.state_dead").to_string(),
            Mark::Creating => t!("session.state_creating").to_string(),
        }
    }
}

pub fn status_mark(mark: Mark, size: Pixels, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let (name, color) = match mark {
        Mark::Active => (IconName::CircleDot, ui.success),
        Mark::Unverified => (IconName::CircleAlert, ui.warning),
        Mark::Dead => (IconName::CircleX, ui.destructive),
        Mark::Creating => (IconName::LoaderCircle, ui.muted_foreground),
    };
    icon(name).size(size).text_color(color).into_any_element()
}

/// 主机身份色：色相由连接串散列而来，饱和度 / 亮度随主题（hostColor.ts 的 hsl(h, s%, l%)）
pub fn host_color(hue: u32, cx: &App) -> Hsla {
    let ui = Ui::global(cx);
    hsla(hue as f32 / 360.0, ui.host_s as f32 / 100.0, ui.host_l as f32 / 100.0, 1.0)
}

/// 3px 宽的主机色条
pub fn host_bar(bar: Option<falcon_core::host_color::HostBar>, height: Pixels, cx: &App) -> AnyElement {
    use falcon_core::host_color::HostBar;
    let color = match bar {
        Some(HostBar::Hue(h)) => host_color(h, cx),
        Some(HostBar::Neutral) => Ui::global(cx).border,
        None => gpui_kit::transparent_black(),
    };
    div()
        .flex_none()
        .w(zpx(3.))
        .h(height)
        .rounded_full()
        .bg(color)
        .into_any_element()
}

/// 键位提示（菜单 / tooltip 里显示的主键位）
pub fn chord(action: &str) -> &'static str {
    #[cfg(target_os = "macos")]
    return match action {
        "palette" => "⌘⇧P",
        "quickOpen" => "⌘P",
        "newTerminal" => "⌘T",
        "closeTab" => "⌘W",
        "toggleSidebar" => "⌘B",
        "toggleGitPanel" => "⌘⇧G",
        "toggleChangesPanel" => "⌘⇧U",
        "openRelays" => "⌘⇧F",
        "toggleFilesPanel" => "⌘⇧E",
        "toggleMeeglePanel" => "⌘⇧M",
        "reattach" => "⌘R",
        "overview" => "⌘0",
        "nextTab" => "⌘⇧]",
        "prevTab" => "⌘⇧[",
        "nextCanvas" => "⌘⌥]",
        "prevCanvas" => "⌘⌥[",
        _ => "",
    };
    #[cfg(not(target_os = "macos"))]
    return match action {
        "palette" => "Ctrl+Shift+P",
        "quickOpen" => "Alt+P",
        "newTerminal" => "Ctrl+Shift+T",
        "closeTab" => "Ctrl+Shift+W",
        "toggleSidebar" => "Ctrl+Shift+B",
        "toggleGitPanel" => "Ctrl+Shift+G",
        "toggleChangesPanel" => "Ctrl+Shift+U",
        "openRelays" => "Ctrl+Shift+F",
        "toggleFilesPanel" => "Ctrl+Shift+E",
        "toggleMeeglePanel" => "Ctrl+Shift+M",
        "reattach" => "Ctrl+Shift+R",
        "overview" => "Alt+0",
        "nextTab" => "Ctrl+Tab",
        "prevTab" => "Ctrl+Shift+Tab",
        "nextCanvas" => "Alt+Shift+]",
        "prevCanvas" => "Alt+Shift+[",
        _ => "",
    };
}
