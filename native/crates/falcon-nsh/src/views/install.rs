//! 会话守护进程的授权 / 部署对话框（对照 GPUI 基线 `falcon-ui/src/dialogs/install.rs`）。
//!
//! 三个阶段：授权询问（Ask）→ 部署进度（Installing，阶段 + 自动重试计数 + 远端命令日志）
//! → 失败（Failed，原因 + 详细 + 重试 / 跳过）。装完 / 跳过 / 取消都经
//! `InstallOperations::finish` 回到用户本来想干的事（续建会话）。

use falcon_proto::{InstallStage, NonDurableReason, can_retry_install};
use falcon_theme::Rgba as ThemeRgba;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::install_operations::{InstallOperations, InstallPhase, InstallState};
use crate::shell::widgets::{Glyph, glyph, icon_button_with_shortcut};
use crate::store::{WorkspaceStore, use_workspace};
use crate::terminal::bind_safe_key_when;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone, PartialEq, Props)]
pub struct InstallDialogProps {}

/// 授权 / 部署模态框主体（模态层卡片内容；遮罩与 Escape 走 `ModalLayer` 通用逻辑）。
#[component]
pub fn InstallDialog(_props: InstallDialogProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let operations = store.install_operations.clone();

    // 回车 = 当前阶段的主按钮（Ask → 启用并上传；Failed 且可重试 → 重试）
    let primary_ops = operations.clone();
    let primary_store = store.clone();
    bind_safe_key_when("enter", false, nsh_core::keymap::When::Always, move || {
        primary_ops.primary(&primary_store);
    });

    view()
        .test_id("install-dialog")
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme.resolved();
            let Some(state) = store.install_operations.state() else {
                return view().into_element();
            };
            let operations = store.install_operations.clone();
            render_frame(&store, &operations, &resolved, &state).into_element()
        }))
}

fn render_frame(
    store: &WorkspaceStore,
    operations: &InstallOperations,
    resolved: &falcon_theme::ResolvedTheme,
    state: &InstallState,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let installing = state.phase == InstallPhase::Installing;
    let title = if state.phase == InstallPhase::Failed {
        t!("durable.failTitle", host = state.host.clone())
    } else {
        t!("durable.enableTitle", host = state.host.clone())
    };
    // 安装进行中不画关闭按钮（GPUI 的 close_button(!installing)）；Escape 仍按取消走 finish
    let close = if installing {
        view().w(zpx(28.0)).into_element()
    } else {
        let ops = operations.clone();
        let st = store.clone();
        icon_button_with_shortcut(
            "install-close",
            t!("common.close"),
            Glyph::Close,
            false,
            Some("escape"),
            resolved,
            move || ops.finish(&st),
        )
        .into_element()
    };
    let body = match state.phase {
        InstallPhase::Ask => render_ask(store, operations, resolved, state).into_element(),
        InstallPhase::Installing => {
            render_installing(store, operations, resolved, state).into_element()
        }
        InstallPhase::Failed => render_failed(store, operations, resolved, state).into_element(),
    };
    view()
        .flex_col()
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(14.0))
                .py(zpx(10.0))
                .border_b_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    text(title)
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.popover_foreground)),
                )
                .child(close),
        )
        .child(scroll().min_h(zpx(0.0)).flex_shrink(1.0).child(body))
}

fn render_ask(
    store: &WorkspaceStore,
    operations: &InstallOperations,
    resolved: &falcon_theme::ResolvedTheme,
    state: &InstallState,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let deny_ops = operations.clone();
    let allow_ops = operations.clone();
    let deny_store = store.clone();
    let allow_store = store.clone();
    view()
        .flex_col()
        .p(zpx(14.0))
        .gap(zpx(12.0))
        .child(
            text(t!(
                "durable.enableBody",
                host = state.host.clone(),
                version = state.required_version.clone()
            ))
            .text_xs()
            .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .child(
            text(t!("durable.authNote"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .when_some(state.error.clone(), |v, error| {
            v.child(
                text(error)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            )
        })
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(zpx(8.0))
                .child(dialog_button(
                    "install-deny",
                    t!("durable.authDeny"),
                    false,
                    resolved,
                    move || deny_ops.deny(&deny_store),
                ))
                .child(dialog_button(
                    "install-allow",
                    t!("durable.authAllow"),
                    true,
                    resolved,
                    move || allow_ops.allow(&allow_store),
                )),
        )
}

fn render_installing(
    store: &WorkspaceStore,
    operations: &InstallOperations,
    resolved: &falcon_theme::ResolvedTheme,
    state: &InstallState,
) -> impl IntoElement {
    let ui = &resolved.ui;
    // 二进制只有几 MB，经 SSH 推过去也就几秒，后端不报字节数，只有阶段没有百分比
    let stage = match state.stage {
        InstallStage::Unknown => t!("native.forms.installing"),
        stage => t!(format!("durable.stage_{}", stage.as_str()).as_str()),
    };
    let mut body = view()
        .flex_col()
        .p(zpx(14.0))
        .gap(zpx(12.0))
        .child(
            text(stage)
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    if state.attempt > 1 {
        body = body.child(
            text(t!("durable.autoRetrying", n = state.attempt))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }
    if !state.commands.is_empty() {
        body = body.child(command_log(&state.commands, true, resolved));
    }
    let ops = operations.clone();
    let st = store.clone();
    body.child(
        view().flex_row().justify_end().child(dialog_button(
            "install-cancel",
            t!("durable.installCancel"),
            false,
            resolved,
            move || ops.finish(&st),
        )),
    )
}

fn render_failed(
    store: &WorkspaceStore,
    operations: &InstallOperations,
    resolved: &falcon_theme::ResolvedTheme,
    state: &InstallState,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let mut reason = view().flex_col().gap(zpx(2.0)).child(
        text(reason_text(state.failure))
            .text_size(zpx(13.0))
            .text_color(rgb_to_hsla(ui.foreground)),
    );
    if state.attempt > 1 {
        reason = reason.child(
            text(t!("durable.autoRetried", n = state.attempt))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }
    let mut body = view().flex_col().p(zpx(14.0)).gap(zpx(12.0)).child(
        view()
            .flex_row()
            .gap(zpx(10.0))
            .rounded(zpx(8.0))
            .bg(rgba_to_hsla(ThemeRgba {
                r: ui.warning.r,
                g: ui.warning.g,
                b: ui.warning.b,
                a: 26,
            }))
            .px(zpx(12.0))
            .py(zpx(10.0))
            .child(
                view()
                    .flex_none()
                    .text_color(rgb_to_hsla(ui.warning))
                    .child(glyph(Glyph::Warning)),
            )
            .child(reason),
    );
    if !state.commands.is_empty() {
        body = body.child(command_log(&state.commands, false, resolved));
    }
    if let Some(detail) = state.detail.clone() {
        let ops = operations.clone();
        let open = state.show_detail;
        body = body.child(
            view()
                .flex_col()
                .gap(zpx(6.0))
                .child(
                    view()
                        .test_id("install-detail-toggle")
                        .flex_row()
                        .items_center()
                        .gap(zpx(6.0))
                        .cursor_pointer()
                        .on_press(move || ops.toggle_detail())
                        .child(
                            view()
                                .flex_none()
                                .text_color(rgb_to_hsla(ui.muted_foreground))
                                .child(glyph(if open {
                                    Glyph::ChevronDown
                                } else {
                                    Glyph::ChevronRight
                                })),
                        )
                        .child(
                            text(t!("durable.detailToggle"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                )
                .when(open, |v| {
                    v.child(
                        view()
                            .rounded(zpx(8.0))
                            .bg(rgb_to_hsla(ui.app))
                            .px(zpx(12.0))
                            .py(zpx(10.0))
                            .child(
                                text(detail)
                                    .text_size(zpx(11.5))
                                    .font_family(FONT_FAMILY_MONO)
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            ),
                    )
                }),
        );
    }
    if let Some(error) = state.error.clone() {
        body = body.child(
            text(error)
                .text_xs()
                .text_color(rgb_to_hsla(ui.destructive)),
        );
    }
    let skip_ops = operations.clone();
    let skip_store = store.clone();
    let mut footer = view()
        .flex_row()
        .items_center()
        .gap(zpx(8.0))
        .child(dialog_button(
            "install-skip",
            t!("durable.installSkip"),
            false,
            resolved,
            move || skip_ops.finish(&skip_store),
        ))
        .child(view().flex_1());
    if can_retry_install(state.failure) {
        let retry_ops = operations.clone();
        let retry_store = store.clone();
        footer = footer.child(dialog_button(
            "install-retry",
            t!("durable.installRetry"),
            true,
            resolved,
            move || retry_ops.retry(&retry_store),
        ));
    }
    body.child(footer)
}

/// 安装过程中远端实际跑过的命令。最新一条是正在执行或刚失败的那条
fn command_log(
    commands: &[String],
    running: bool,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let log_ref = element_ref();
    let n = commands.len();
    let mut lines = view().flex_col().gap(zpx(8.0));
    for (i, cmd) in commands.iter().enumerate() {
        let last = i + 1 == n;
        let mut line = format!("$ {cmd}");
        if last && running {
            line.push_str(" …");
        }
        lines = lines.child(
            text(line)
                .text_size(zpx(11.5))
                .font_family(FONT_FAMILY_MONO)
                .text_color(rgb_to_hsla(if last {
                    ui.foreground
                } else {
                    ui.muted_foreground
                })),
        );
    }
    view()
        .flex_col()
        .gap(zpx(6.0))
        .child(
            text(t!("durable.commandLog"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .child(
            scroll()
                .test_id("install-command-log")
                .element_ref(&log_ref)
                .max_h(zpx(208.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.app))
                .px(zpx(12.0))
                .py(zpx(8.0))
                .child(
                    lines.on_layout(move |_: &Bounds<Points>| {
                        log_ref.scroll_to_end(false);
                    }),
                ),
        )
}

fn dialog_button(
    id: &str,
    label: String,
    primary: bool,
    resolved: &falcon_theme::ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    let ui = &resolved.ui;
    view()
        .test_id(id)
        .px(zpx(10.0))
        .py(zpx(6.0))
        .rounded(zpx(6.0))
        .cursor_pointer()
        .bg(rgb_to_hsla(if primary { ui.tint } else { ui.secondary }))
        .on_press(action)
        .child(
            text(label).text_xs().font_weight(
                if primary {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                },
            ).text_color(rgb_to_hsla(if primary {
                ui.tint_foreground
            } else {
                ui.secondary_foreground
            })),
        )
}

/// `NonDurableReason` → 给人看的一句话；key 约定与 GPUI 一致（`durable.reason_*`）。
fn reason_text(reason: Option<NonDurableReason>) -> String {
    falcon_core::reason::reason_text(
        &|key: &str, _params: &[(&str, &str)]| nsh_core::i18n::translate(key, None, &[]),
        reason,
    )
}
