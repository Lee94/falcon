//! 终端窗口底部的状态栏：终端里的程序此刻在干什么（OSC 7501 程序状态，ADR 0020 决定六）。
//!
//! 标题栏上已经有一枚徽标，但只放得下一个图标；状态栏把它展开成一行：
//! - 左边：状态（图标 + 完整短标签，blocked 时整条铺一层警示色）、进度条、程序给的那半句；
//!   程序没报状态时退成前台命令（服务端探的），shell 提示符下是「空闲」；
//! - 右边：子任务数（Claude Code 的后台子 agent 各报一条，悬停列出来）、计时（已运行 / 已等待 /
//!   用时，本客户端没亲眼看到这一轮开始时不显示，见 falcon-core 的 `StateClock`）、程序名。
//!
//! **常驻，不随状态出没**：它一出一没终端就少一行、多一行，PTY 跟着 resize，正在干活的 TUI
//! （偏偏就是报状态的那些）整屏重画。所以每扇终端窗口都有，没东西可说时也占着这一行。
//!
//! 程序给的文本（title / message / app）一律是 falcon-core 消过毒的；会话名不用写——状态栏就挂
//! 在那扇窗口上，位置本身交代了来源。

use gpui_kit::assets::IconName;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, FontWeight, Hsla, SharedString, div};
use rust_i18n::t;

use falcon_core::program_status::{self, BadgeTone, ClockReading};

use crate::theme::Ui;
use crate::ui::{icon, program_color, program_icon};
use crate::workspace::Workspace;
use crate::zoom::zpx;

/// 状态栏的高（逻辑像素，乘界面缩放）
pub const STATUS_BAR_H: f32 = 22.;

/// 这一扇终端窗口的状态栏。`ws` 是已经 read 出来的工作区（canvas 渲染时手里就有）
pub fn status_bar(session_id: &str, ws: &Workspace, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let bar = div()
        .id(SharedString::from(format!("term-status-{session_id}")))
        .h(zpx(STATUS_BAR_H))
        .flex_none()
        .flex()
        .items_center()
        .gap(zpx(8.))
        .px_2()
        .overflow_hidden()
        .text_size(zpx(11.))
        .text_color(ui.muted_foreground)
        .border_t_1()
        .border_color(ui.border);
    let Some(session) = ws.session(session_id) else {
        return bar.into_any_element();
    };
    // 待接回 / 已断开：程序状态要么陈旧要么没有，标题栏的记号已经说了会话的情况，这里空着
    if session.state != falcon_proto::SessionState::Active {
        return bar.into_any_element();
    }

    let summary = session.program_status.as_ref();
    let badge = summary.and_then(program_status::badge);
    let mut bar = bar;
    match &badge {
        Some(b) => {
            let color = program_color(b.tone, cx);
            if b.tone == BadgeTone::Blocked {
                bar = bar.bg(ui.warning.opacity(0.12));
            }
            bar = bar.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(zpx(4.))
                    .text_color(color)
                    .font_weight(FontWeight::MEDIUM)
                    .child(icon(program_icon(b.tone, b.kind)).size(zpx(12.)))
                    .child(t!(b.label_key).to_string()),
            );
            if let Some(p) = b.progress {
                bar = bar.child(progress(p, color, &ui));
            }
            bar = bar.child(div().flex_1().min_w_0().truncate().children(b.detail.clone()));
        }
        None => {
            // 前台命令是服务端探的（只在有 Viewer 时探、2.5s 节流），在 shell 提示符下为空
            let (name, text) = match session.title.as_deref().filter(|t| !t.is_empty()) {
                Some(cmd) => (IconName::SquareTerminal, cmd.to_string()),
                None => (IconName::Circle, t!("programStatus.idle").to_string()),
            };
            bar = bar
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .min_w_0()
                        .items_center()
                        .gap(zpx(4.))
                        .child(icon(name).size(zpx(12.)))
                        .child(div().truncate().child(text)),
                )
                .child(div().flex_1());
        }
    }

    // ---- 右边 ----
    let tasks = ws
        .terminals
        .get(session_id)
        .and_then(|v| v.read(cx).program_records())
        .map(program_status::subtasks)
        .unwrap_or_default();
    if !tasks.is_empty() {
        let tip: SharedString = tasks
            .iter()
            .map(|task| match &task.detail {
                Some(d) => format!("{} · {d}", t!(task.label_key)),
                None => t!(task.label_key).to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into();
        // 子任务里有在等人的，图标跟着变警示色——主状态可能是 working（根记录还在跑）
        let waiting = tasks.iter().any(|task| task.tone == BadgeTone::Blocked);
        bar = bar.child(
            div()
                .id(SharedString::from(format!("term-status-tasks-{session_id}")))
                .flex()
                .flex_none()
                .items_center()
                .gap(zpx(3.))
                .when(waiting, |d| d.text_color(ui.warning))
                .child(icon(IconName::ListTree).size(zpx(12.)))
                .child(t!("programStatus.subtasks", n = tasks.len()).to_string())
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)),
        );
    }
    if badge.is_some()
        && let Some(reading) = ws.program_clock(session_id)
    {
        let (key, ms) = match reading {
            ClockReading::Running(ms) => ("programStatus.clockRunning", ms),
            ClockReading::Waiting(ms) => ("programStatus.clockWaiting", ms),
            ClockReading::Took(ms) => ("programStatus.clockTook", ms),
        };
        bar = bar.child(
            div()
                .flex_none()
                .font_family(crate::fonts::BERKELEY)
                .child(t!(key, t = program_status::format_duration(ms)).to_string()),
        );
    }
    if let Some(app) = summary.filter(|_| badge.is_some()).and_then(program_status::app_text) {
        bar = bar.child(div().flex_none().max_w(zpx(140.)).truncate().text_color(ui.muted_foreground.opacity(0.8)).child(app));
    }
    bar.into_any_element()
}

/// 有进度时的一截细进度条 + 百分比
fn progress(p: u8, color: Hsla, ui: &Ui) -> AnyElement {
    const W: f32 = 56.;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(zpx(4.))
        .child(
            div()
                .w(zpx(W))
                .h(zpx(4.))
                .rounded_full()
                .bg(ui.border)
                .child(div().h_full().rounded_full().bg(color).w(zpx(W * p.min(100) as f32 / 100.))),
        )
        .child(t!("programStatus.progress", n = p).to_string())
        .into_any_element()
}

/// 计时还在涨（working / blocked）的会话：画布据此每秒重画一次
pub fn ticking(session_id: &str, ws: &Workspace) -> bool {
    ws.session(session_id).is_some_and(|s| s.program_status.is_some())
        && ws.program_clock(session_id).is_some_and(ClockReading::ticking)
}
