//! 界面小件：岛、图标按钮、状态记号、主机色条。颜色一律取 [`crate::theme::Ui`] 的语义 token。

use std::sync::atomic::{AtomicBool, Ordering};

use falcon_core::shortcuts::{self, Command};
use falcon_proto::SessionState;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, Hsla, IntoElement, Pixels, SharedString, Stateful, Transformation,
    Window, div, hsla, percentage,
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

/// 会话状态记号：形状 + 颜色双编码（旧 React 版的 StatusMark）。creating 是纯前端状态
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

/// 转圈的图标（程序在跑）：一秒一圈，限帧到 `motion::FPS`，不看「减少动态效果」（转圈就是
/// "还在跑"这个状态本身，见 [`crate::motion`]）。`id` 只要在父元素里唯一
pub fn spinner(id: impl Into<ElementId>, name: IconName, size: Pixels) -> AnyElement {
    use crate::motion::{Motion, MotionExt};
    icon(name)
        .size(size)
        .motion(id, Motion::repeat(1000), |i, t| i.transform(Transformation::rotate(percentage(t))))
        .into_any_element()
}

/// 程序状态（OSC 7501，ADR 0020 决定六）的徽标。
///
/// - blocked 最醒目：警示色实底的小胶囊，图标 + 两个字直接写出要的是什么（授权 / 回答 / 凭据），
///   不用悬停就知道该去干嘛；
/// - error / done 只是一个上色的图标（红三角 / 绿勾），不和会话本身的状态记号（`Mark`，圆形）
///   撞形状；
/// - working 一个语义蓝的图标在转圈（[`spinner`]），程序报了进度就跟一个百分比。
///
/// 悬停提示是完整短标签 + 程序那半句（`ProgramBadge::detail` 已经消毒、截断过）。会话名不在这里
/// 写：徽标挂在侧栏那一行上，位置本身就交代了来源。终端标题栏上用的是展开的版本
/// （`terminal/title_status.rs`）。
pub fn program_badge(
    id: impl Into<ElementId>,
    badge: &falcon_core::program_status::ProgramBadge,
    size: Pixels,
    cx: &App,
) -> AnyElement {
    use falcon_core::program_status::BadgeTone;
    use rust_i18n::t;
    let ui = Ui::global(cx);
    let label = t!(badge.label_key).to_string();
    let tip: SharedString = match &badge.detail {
        Some(detail) => format!("{label} · {detail}"),
        None => label,
    }
    .into();
    let name = program_icon(badge.tone, badge.kind);
    let el = div().id(id.into()).flex().flex_none().items_center().gap(zpx(3.));
    let el = match badge.tone {
        BadgeTone::Blocked => el
            .h(size + zpx(4.))
            .px(zpx(5.))
            .rounded_full()
            .bg(ui.warning)
            .text_color(ui.warning_foreground)
            .text_size(zpx(11.))
            .child(icon(name).size(size * 0.85))
            .child(t!(badge.short_key).to_string()),
        BadgeTone::Working => el
            .text_color(ui.tint_strong)
            .text_size(zpx(11.))
            .child(spinner("ps-spin", name, size))
            .when_some(badge.progress, |d, p| {
                d.child(div().text_color(ui.muted_foreground).child(t!("programStatus.progress", n = p).to_string()))
            }),
        BadgeTone::Error => el.text_color(ui.destructive).child(icon(name).size(size)),
        BadgeTone::Done => el.text_color(ui.success).child(icon(name).size(size)),
    };
    el.tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
        .into_any_element()
}

/// 程序状态的图标（侧栏徽标、终端标题栏的状态胶囊共用）：blocked 按要的东西分（授权 / 回答 / 凭据），
/// 其余按状态
pub fn program_icon(
    tone: falcon_core::program_status::BadgeTone,
    kind: Option<falcon_proto::ProgramBlockKind>,
) -> IconName {
    use falcon_core::program_status::BadgeTone;
    use falcon_proto::ProgramBlockKind;
    match (tone, kind) {
        (BadgeTone::Blocked, Some(ProgramBlockKind::Permission)) => IconName::ShieldQuestionMark,
        (BadgeTone::Blocked, Some(ProgramBlockKind::Question)) => IconName::MessageCircleQuestionMark,
        (BadgeTone::Blocked, Some(ProgramBlockKind::Auth)) => IconName::KeyRound,
        (BadgeTone::Blocked, _) => IconName::Hand,
        (BadgeTone::Error, _) => IconName::TriangleAlert,
        (BadgeTone::Working, _) => IconName::LoaderCircle,
        (BadgeTone::Done, _) => IconName::CircleCheck,
    }
}

/// 程序状态的语义色（图标与文字）：blocked 警示、error 危险、working 强调蓝、done 成功绿
pub fn program_color(tone: falcon_core::program_status::BadgeTone, cx: &App) -> Hsla {
    use falcon_core::program_status::BadgeTone;
    let ui = Ui::global(cx);
    match tone {
        BadgeTone::Blocked => ui.warning,
        BadgeTone::Error => ui.destructive,
        BadgeTone::Working => ui.tint_strong,
        BadgeTone::Done => ui.success,
    }
}

/// 主机身份色：色相由连接串散列而来，饱和度 / 亮度随主题（React 版 hostColor.ts 的 hsl(h, s%, l%)）
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

/// 键位按 mac 习惯（⌘）还是 Ctrl+Shift——平台的事（浏览器里看浏览器所在的系统），
/// [`crate::actions::init`] 注册键位时设一次，之后不变
static MAC_KEYS: AtomicBool = AtomicBool::new(false);

pub(crate) fn set_mac_keys(mac: bool) {
    MAC_KEYS.store(mac, Ordering::Relaxed);
}

/// 键位提示（菜单 / tooltip 里显示的主键位），字面沿用 React 版 `chord()` 的写法
/// （falcon-core 的快捷键表）。不认识的动作 id 给空串
pub fn chord(action: &str) -> String {
    Command::from_id(action).map(|c| shortcuts::chord(c, MAC_KEYS.load(Ordering::Relaxed))).unwrap_or_default()
}

/// 变更面板提交框的提交键。不注册成全局绑定——只在提交框里生效，全局绑定会在终端里抢走 ⌘↵
pub fn commit_chord() -> &'static str {
    if MAC_KEYS.load(Ordering::Relaxed) { "⌘↵" } else { "Ctrl+↵" }
}
