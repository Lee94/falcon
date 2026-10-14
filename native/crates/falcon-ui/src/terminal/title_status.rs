//! 终端窗口标题栏上的程序状态（OSC 7501，ADR 0020 决定六）。
//!
//! 原先窗口底部有一条常驻状态栏：为了不让终端行数跟着状态一出一没（PTY 一 resize，正在干活的
//! TUI 就整屏重画），没状态时也白占一行。现在状态并进标题栏——标题栏本来就在，挂什么终端都不多
//! 不少，resize 的顾虑跟着没了。标题栏里依次是：名字 → 状态胶囊 → 子任务 → 程序那半句 → 工作目录，
//! 标题栏底边还有一条细线随状态变：
//!
//! - working：胶囊里图标转圈；报了进度时底边是进度线（宽度用弹簧追过去，不跳），没报时一段光
//!   来回扫；
//! - blocked：警示色实底胶囊外一圈光晕在呼吸，底边警示线跟着呼吸——在等人，得够醒目；图标进场时
//!   摇一下；
//! - done / error：换到这个状态时图标弹一下（done 放大回弹、error 左右抖），底边闪一下就淡掉。
//!
//! 一次性的动画（进场、弹、抖、闪）靠**换元素 id** 重新开始：id 里带着状态，状态一变就是新元素、
//! 新的动画状态。窗口重新出现（切项目、翻画布）时会再播一次，算是"刚看到"，不另外防。
//!
//! 动画走 `crate::motion`（不看系统的「减少动态效果」，为什么见那里），持续的动画（转圈、扫光、
//! 呼吸）限帧到 `motion::FPS`。会动的两块（胶囊、底边线）各是一个小视图（[`StatusSlot`]），
//! 每帧只重画它们自己，不连累整块画布。
//!
//! 胶囊悬停是完整的一段：短标签 + 程序那半句、计时、程序名、子任务。程序给的文本（title /
//! message / app）一律是 falcon-core 消过毒的；会话名不用写——胶囊就挂在那扇窗口的标题栏上，
//! 位置本身交代了来源。

use std::f32::consts::PI;

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnimationExt, AnyElement, App, Context, Entity, FontWeight, Hsla, Render, SharedString, SpringAnimation,
    SpringConfig, Subscription, Task, Transformation, Window, div, ease_in_out, linear_color_stop, linear_gradient, point, px,
    relative, size,
};
use rust_i18n::t;

use falcon_core::program_status::{self, BadgeTone, ClockReading, ProgramBadge};

use crate::motion::{Motion, MotionExt, breathe};
use crate::theme::Ui;
use crate::ui::{icon, program_color, program_icon, spinner};
use crate::workspace::Workspace;
use crate::zoom::zpx;

/// 这扇终端的标题栏上要显示的程序状态。会话不在、不是 active（标题栏的记号已经说了会话的情况）
/// 或程序没报状态时没有。画布据此决定挂不挂 [`StatusSlot`]、程序那半句（`detail`）写不写
pub fn badge(session_id: &str, ws: &Workspace) -> Option<ProgramBadge> {
    let session = ws.session(session_id)?;
    if session.state != falcon_proto::SessionState::Active {
        return None;
    }
    program_status::badge(session.program_status.as_ref()?)
}

/// 标题栏里会动的哪一块
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// 跟在名字后面：状态胶囊 + 子任务数
    Chip,
    /// 标题栏底边那条线（absolute，贴着标题栏底边；视图的根元素就是它，直接挂在标题栏上）
    Edge,
}

/// 标题栏里会动的两块各是一个小视图。持续动画重画的是发起它的视图：直接画在画布里，每一帧就是
/// 整块画布重排重画（实测 release 每次 1.2–1.5ms，30 帧约占一个核的 4%）；自己一个视图，每帧只
/// 重画这一小块。数据直接读工作区、跟着工作区的通知重画，画布只管挂不挂
pub struct StatusSlot {
    ws: Entity<Workspace>,
    session_id: String,
    part: Part,
    /// 胶囊上的计时（已运行 / 已等待）在涨时，一秒后再画一次
    clock_tick: Option<Task<()>>,
    _observe: Subscription,
}

impl StatusSlot {
    pub fn new(ws: Entity<Workspace>, session_id: String, part: Part, cx: &mut Context<Self>) -> Self {
        let _observe = cx.observe(&ws, |_, _, cx| cx.notify());
        Self { ws, session_id, part, clock_tick: None, _observe }
    }

    /// 计时在涨就一秒后再画一次。只在需要时排，不常驻。眼下 working / blocked 都有持续动画在
    /// 重画，这里是兜底——计时不该指望动画常驻
    fn schedule_clock_tick(&mut self, cx: &mut Context<Self>) {
        if self.part != Part::Chip || self.clock_tick.is_some() {
            return;
        }
        if !self.ws.read(cx).program_clock(&self.session_id).is_some_and(ClockReading::ticking) {
            return;
        }
        self.clock_tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
            this.update(cx, |this, cx| {
                this.clock_tick = None;
                cx.notify();
            })
            .ok();
        }));
    }
}

impl Render for StatusSlot {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.schedule_clock_tick(cx);
        let ws = self.ws.read(cx);
        let Some(badge) = badge(&self.session_id, ws) else {
            return div().into_any_element();
        };
        let color = program_color(badge.tone, cx);
        // 状态（blocked 再加上要的是什么）进 id：换了就是新元素，一次性的动画从头播
        let tag = format!("{}-{:?}-{:?}", self.session_id, badge.tone, badge.kind);
        match self.part {
            Part::Chip => chip(&self.session_id, &badge, color, &tag, ws, cx),
            Part::Edge => edge(&badge, color, &tag),
        }
    }
}

/// 状态胶囊 + 子任务数
fn chip(session_id: &str, badge: &ProgramBadge, color: Hsla, tag: &str, ws: &Workspace, cx: &App) -> AnyElement {
    let clock = ws.program_clock(session_id);
    let tasks = ws
        .terminals
        .get(session_id)
        .and_then(|v| v.read(cx).program_records())
        .map(program_status::subtasks)
        .unwrap_or_default();
    let app = ws.session(session_id).and_then(|s| s.program_status.as_ref()).and_then(program_status::app_text);
    let tip = tooltip_text(badge, clock, app, &tasks);
    let mut chip = div().flex().flex_none().items_center().gap(zpx(6.)).child(pill(badge, color, clock, tag, tip, cx));
    if !tasks.is_empty() {
        // 子任务里有在等人的，图标跟着变警示色——主状态可能是 working（根记录还在跑）
        let waiting = tasks.iter().any(|task| task.tone == BadgeTone::Blocked);
        let ui = Ui::global(cx);
        chip = chip.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(zpx(3.))
                .text_size(zpx(11.))
                .text_color(if waiting { ui.warning } else { ui.muted_foreground })
                .child(icon(IconName::ListTree).size(zpx(12.)))
                .child(tasks.len().to_string()),
        );
    }
    chip.into_any_element()
}

/// 悬停提示：短标签 · 程序那半句，下面依次是计时、程序名、子任务
fn tooltip_text(
    badge: &ProgramBadge,
    clock: Option<ClockReading>,
    app: Option<String>,
    tasks: &[program_status::SubTask],
) -> SharedString {
    let label = t!(badge.label_key).to_string();
    let mut lines = vec![match &badge.detail {
        Some(detail) => format!("{label} · {detail}"),
        None => label,
    }];
    if let Some(reading) = clock {
        let (key, ms) = clock_key(reading);
        lines.push(t!(key, t = program_status::format_duration(ms)).to_string());
    }
    lines.extend(app);
    if !tasks.is_empty() {
        lines.push(t!("programStatus.subtasks", n = tasks.len()).to_string());
        lines.extend(tasks.iter().map(|task| match &task.detail {
            Some(d) => format!("  {} · {d}", t!(task.label_key)),
            None => format!("  {}", t!(task.label_key)),
        }));
    }
    lines.join("\n").into()
}

fn clock_key(reading: ClockReading) -> (&'static str, u64) {
    match reading {
        ClockReading::Running(ms) => ("programStatus.clockRunning", ms),
        ClockReading::Waiting(ms) => ("programStatus.clockWaiting", ms),
        ClockReading::Took(ms) => ("programStatus.clockTook", ms),
    }
}

/// 状态胶囊：图标 + 两个字 + 进度 + 计时。blocked 是警示色实底（外加一圈呼吸的光晕），其余是
/// 语义色的淡底
fn pill(badge: &ProgramBadge, color: Hsla, clock: Option<ClockReading>, tag: &str, tip: SharedString, cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let blocked = badge.tone == BadgeTone::Blocked;
    let (bg, fg) = if blocked { (ui.warning, ui.warning_foreground) } else { (color.opacity(0.14), color) };
    let pill = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(zpx(4.))
        .h(zpx(18.))
        .px(zpx(7.))
        .rounded_full()
        .bg(bg)
        .text_color(fg)
        .text_size(zpx(11.))
        .font_weight(FontWeight::MEDIUM)
        .child(state_icon(badge, tag))
        .child(t!(badge.short_key).to_string())
        .when_some(badge.progress, |d, p| d.child(t!("programStatus.progress", n = p).to_string()))
        .when_some(clock, |d, reading| {
            d.child(
                div()
                    .font_family(crate::fonts::BERKELEY)
                    .font_weight(FontWeight::NORMAL)
                    .opacity(0.8)
                    .child(program_status::format_duration(clock_key(reading).1)),
            )
        });
    // 光晕占着位置（2px 一圈），不在 blocked 时也留着：换状态时胶囊不挪窝
    let warning = ui.warning;
    let halo = div()
        .id(SharedString::from(format!("pane-ps-{tag}")))
        .flex_none()
        .p(zpx(2.))
        .rounded_full()
        .child(pill)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
    let halo = if blocked {
        halo.motion(
            SharedString::from(format!("pane-ps-halo-{tag}")),
            Motion::repeat(1800).easing(breathe),
            move |d, t| d.bg(warning.opacity(0.45 * t)),
        )
        .into_any_element()
    } else {
        halo.into_any_element()
    };
    // 整颗胶囊进场时淡入
    div()
        .flex_none()
        .child(halo)
        .motion(SharedString::from(format!("pane-ps-in-{tag}")), Motion::once(220).easing(ease_in_out), |d, t| d.opacity(t))
        .into_any_element()
}

/// 胶囊里的图标：working 转圈；blocked 进场摇一下（像门铃）；done 放大回弹；error 左右抖
fn state_icon(badge: &ProgramBadge, tag: &str) -> AnyElement {
    let name = program_icon(badge.tone, badge.kind);
    let glyph = icon(name).size(zpx(12.));
    let id = SharedString::from(format!("pane-ps-icon-{tag}"));
    match badge.tone {
        BadgeTone::Working => spinner(id, name, zpx(12.)),
        BadgeTone::Blocked => glyph
            .motion(id, Motion::once(700), |i, t| {
                // 衰减的正弦：±16° 起摆，摆三下停
                let angle = (t * 6. * PI).sin() * (1. - t) * 16f32.to_radians();
                i.transform(Transformation::rotate(gpui_kit::radians(angle)))
            })
            .into_any_element(),
        BadgeTone::Done => glyph
            .motion(id, Motion::once(450).easing(back_out), |i, t| {
                let s = 0.3 + 0.7 * t;
                i.transform(Transformation::scale(size(s, s)))
            })
            .into_any_element(),
        BadgeTone::Error => glyph
            .motion(id, Motion::once(500), |i, t| {
                let dx = (t * 6. * PI).sin() * (1. - t) * 3.;
                i.transform(Transformation::translate(point(px(dx), px(0.))))
            })
            .into_any_element(),
    }
}

/// 冲过头再回来的缓出（CSS 的 easeOutBack）：t=1 时正好是 1，中途最多冲到 1.1 左右
fn back_out(t: f32) -> f32 {
    const C1: f32 = 1.70158;
    const C3: f32 = C1 + 1.;
    let x = t - 1.;
    1. + C3 * x * x * x + C1 * x * x
}

/// 标题栏底边的线：working 有进度是进度线、没进度是来回扫的一段光；blocked 整条呼吸；
/// done / error 闪一下淡掉
fn edge(badge: &ProgramBadge, color: Hsla, tag: &str) -> AnyElement {
    // 压在标题栏的下边框上（下移 1px），比它粗一点
    let track = div().absolute().left_0().right_0().bottom(px(-1.)).h(zpx(2.)).overflow_hidden();
    let id = SharedString::from(format!("pane-ps-edge-{tag}"));
    let line = match (badge.tone, badge.progress) {
        (BadgeTone::Working, Some(p)) => div()
            .h_full()
            .rounded_full()
            .bg(color)
            // 弹簧追进度：程序一格一格地报，线是滑过去的；第一次出现从 0 长出来。这一处照 GPUI 的
            // 规矩跟「减少动态效果」走（开着时直接跳到位）：长度本身就是状态，滑不滑过去无所谓
            .with_spring(
                id,
                SpringAnimation::new(SpringConfig::new(120., 22., 1.)).to(p.min(100) as f32 / 100.).from(0.),
                |d, v| d.w(relative(v.clamp(0., 1.))),
            )
            .into_any_element(),
        (BadgeTone::Working, None) => {
            // 一段两头透明、中间实的光：左右各一半渐变拼起来
            let clear = color.opacity(0.);
            div()
                .absolute()
                .top_0()
                .h_full()
                .w(relative(0.3))
                .flex()
                .child(div().flex_1().bg(linear_gradient(90., linear_color_stop(clear, 0.), linear_color_stop(color, 1.))))
                .child(div().flex_1().bg(linear_gradient(90., linear_color_stop(color, 0.), linear_color_stop(clear, 1.))))
                .motion(id, Motion::repeat(1600).easing(ease_in_out), |d, t| d.left(relative(-0.3 + 1.3 * t)))
                .into_any_element()
        }
        (BadgeTone::Blocked, _) => div()
            .size_full()
            .bg(color)
            .motion(id, Motion::repeat(1800).easing(breathe), |d, t| d.opacity(0.35 + 0.65 * t))
            .into_any_element(),
        (BadgeTone::Done | BadgeTone::Error, _) => div()
            .size_full()
            .bg(color)
            .motion(id, Motion::once(1400), |d, t| d.opacity((1. - t).powi(2)))
            .into_any_element(),
    };
    track.child(line).into_any_element()
}
