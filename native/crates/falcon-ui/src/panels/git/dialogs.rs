//! 历史面板的三个表单：从此新建分支、重置当前分支到此处、改提交说明（web GitPanel 的
//! `NewBranchDialog` / `ResetDialog` / `RewordDialog`）。
//!
//! 与 web 一致：点确认后对话框**不关**，操作成功才关——失败时（分支名已存在、改写被拒）
//! 用户要在原处改了再试，而 toast 里有 git 的原话。点遮罩不关（web 的 lockOverlay），
//! Esc / 取消照常关。

use std::cell::Cell;
use std::rc::Rc;

use falcon_proto::{GitOpInput, GitResetMode};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{Disableable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, SharedString, Task, Window, div};
use rust_i18n::t;

use super::GitPanel;
use super::common::{Check, checkbox};
use crate::theme::{Ui, radius};
use crate::zoom::zpx;

/// 发请求；成功才关掉当前对话框
fn submit(panel: &Entity<GitPanel>, op: GitOpInput, title: String, window: &mut Window, cx: &mut App) {
    let task: Task<bool> = panel.update(cx, |p, cx| p.run_op(op, title, cx));
    window
        .spawn(cx, async move |cx| {
            if task.await {
                cx.update(|window, cx| window.close_dialog(cx)).ok();
            }
        })
        .detach();
}

fn panel_busy(panel: &Entity<GitPanel>, cx: &App) -> bool {
    panel.read(cx).is_busy()
}

fn label(text: String, cx: &App) -> gpui_kit::Div {
    div()
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(Ui::global(cx).foreground)
        .child(text)
}

pub fn new_branch(panel: Entity<GitPanel>, start_point: String, hint: String, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| InputState::new(window, cx));
    let checkout = Rc::new(Cell::new(true));
    let focus = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let ui = Ui::global(cx).clone();
        let name = input.read(cx).value().trim().to_string();
        let busy = panel_busy(&panel, cx);
        let on = checkout.get();
        let toggle = checkout.clone();
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(hint.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(label(t!("git.newBranchName").to_string(), cx))
                    .child(Input::new(&input)),
            )
            .child(
                div()
                    .id("git-branch-checkout")
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .on_click({
                        let toggle = toggle.clone();
                        move |_, window, _| {
                            toggle.set(!toggle.get());
                            window.refresh();
                        }
                    })
                    .child(checkbox(
                        "git-branch-checkout-box",
                        if on { Check::On } else { Check::Off },
                        false,
                        None,
                        cx,
                        move |_, window, _| {
                            toggle.set(!toggle.get());
                            window.refresh();
                        },
                    ))
                    .child(t!("git.checkoutAfterCreate").to_string()),
            );
        let (panel, input, checkout, start_point) =
            (panel.clone(), input.clone(), checkout.clone(), start_point.clone());
        dialog
            .title(t!("git.newBranchTitle").to_string())
            .w(zpx(400.))
            .overlay_closable(false)
            .child(body)
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                    .child(
                        DialogAction::new().child(
                            Button::new("git-branch-ok")
                                .primary()
                                .label(t!("git.createBranch").to_string())
                                .disabled(busy || name.is_empty()),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let name = input.read(cx).value().trim().to_string();
                if name.is_empty() || panel_busy(&panel, cx) {
                    return false;
                }
                let op = GitOpInput::BranchCreate {
                    name,
                    start_point: start_point.clone(),
                    checkout: Some(checkout.get()),
                };
                submit(&panel, op, t!("git.newBranch").to_string(), window, cx);
                false
            })
    });
    // open_dialog 会把焦点交给对话框自己的 handle，所以输入框要在它之后再聚焦
    focus.update(cx, |i, cx| i.focus(window, cx));
}

pub fn reset(panel: Entity<GitPanel>, rev: String, hint: String, window: &mut Window, cx: &mut App) {
    let mode = Rc::new(Cell::new(GitResetMode::Mixed));
    window.open_dialog(cx, move |dialog, _, cx| {
        let ui = Ui::global(cx).clone();
        let current = mode.get();
        let busy = panel_busy(&panel, cx);
        let mut options = div().flex().flex_col().gap_1();
        for (value, key) in [
            (GitResetMode::Soft, "git.resetSoft"),
            (GitResetMode::Mixed, "git.resetMixed"),
            (GitResetMode::Hard, "git.resetHard"),
        ] {
            let on = current == value;
            let m1 = mode.clone();
            let m2 = mode.clone();
            options = options.child(
                div()
                    .id(SharedString::from(format!("git-reset-{}", value.as_str())))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py(zpx(6.))
                    .rounded(radius::MD)
                    .border_1()
                    .border_color(ui.border)
                    .text_xs()
                    .cursor_pointer()
                    .when(on, |d| d.bg(ui.tint))
                    .when(!on, |d| d.hover(|s| s.bg(ui.muted)))
                    .on_click(move |_, window, _| {
                        m1.set(value);
                        window.refresh();
                    })
                    .child(
                        Radio::new(SharedString::from(format!("git-reset-radio-{}", value.as_str())))
                            .checked(on)
                            .on_click(move |_, window, _| {
                                m2.set(value);
                                window.refresh();
                            }),
                    )
                    .child(t!(key).to_string()),
            );
        }
        let ok = Button::new("git-reset-ok")
            .label(t!("git.resetConfirm").to_string())
            .disabled(busy);
        let ok = if current == GitResetMode::Hard {
            ok.danger()
        } else {
            ok.primary()
        };
        let (panel, mode, rev) = (panel.clone(), mode.clone(), rev.clone());
        dialog
            .title(t!("git.resetTitle", short = hint).to_string())
            .w(zpx(420.))
            .overlay_closable(false)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .line_height(zpx(19.5))
                            .text_color(ui.muted_foreground)
                            .child(t!("git.resetBody").to_string()),
                    )
                    .child(options),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                    .child(DialogAction::new().child(ok)),
            )
            .on_ok(move |_, window, cx| {
                if panel_busy(&panel, cx) {
                    return false;
                }
                let op = GitOpInput::Reset {
                    rev: rev.clone(),
                    mode: mode.get(),
                };
                submit(&panel, op, t!("git.reset").to_string(), window, cx);
                false
            })
    });
}

pub fn reword(panel: Entity<GitPanel>, sha: String, hint: String, initial: String, window: &mut Window, cx: &mut App) {
    let input = cx.new(|cx| TextareaState::new(window, cx).default_value(initial));
    let focus = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let message = input.read(cx).value().trim().to_string();
        let busy = panel_busy(&panel, cx);
        let (panel, input2, sha) = (panel.clone(), input.clone(), sha.clone());
        dialog
            .title(t!("git.rewordTitle", short = hint).to_string())
            .w(zpx(460.))
            .overlay_closable(false)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .child(label(t!("git.rewordLabel").to_string(), cx))
                    .child(Textarea::new(&input).h(zpx(94.)).text_xs()),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                    .child(
                        DialogAction::new().child(
                            Button::new("git-reword-ok")
                                .primary()
                                .label(t!("git.rewordSave").to_string())
                                .disabled(busy || message.is_empty()),
                        ),
                    ),
            )
            .on_ok(move |_, window, cx| {
                let message = input2.read(cx).value().trim().to_string();
                if message.is_empty() || panel_busy(&panel, cx) {
                    return false;
                }
                let op = GitOpInput::Reword {
                    sha: sha.clone(),
                    message,
                };
                submit(&panel, op, t!("git.reword").to_string(), window, cx);
                false
            })
    });
    // open_dialog 会把焦点交给对话框自己的 handle，所以输入框要在它之后再聚焦
    focus.update(cx, |i, cx| i.focus(window, cx));
}
