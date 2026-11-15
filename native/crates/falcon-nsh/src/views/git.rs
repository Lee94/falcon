//! Git 工作区修改与统一差异窗口。
//!
//! - 右侧栏 [`GitPanel`]：勾选本次提交的文件，固定底部提交表单，提交与推送经服务端执行；
//! - 工作区 [`DiffPaneView`]：统一差异视图，新增行与删除行严格取自 `ui.success` 与 `ui.destructive`。

use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::shell::widgets::check_indicator;
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

/// 与 GPUI 的「修改」面板一致：文件列表占满余下空间，提交表单固定在底部。
#[component]
pub fn GitPanel() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let amend = signal(false);
    view().size_full().flex_col().child(dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let changes = store.git_changes.get();
        let count = changes.iter().filter(|c| c.staged).count();
        let reload = store.clone();
        let header = view()
            .h(zpx(40.0))
            .flex_none()
            .flex_row()
            .items_center()
            .justify_between()
            .px(zpx(12.0))
            .border_b_1()
            .border_color(rgba_to_hsla(ui.border))
            .child(
                text(t!("changes.panel"))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)),
            )
            .child(crate::shell::widgets::icon_button(
                "git-refresh",
                t!("git.refresh"),
                crate::shell::widgets::Glyph::Refresh,
                false,
                &resolved,
                move || reload.refresh_project_data(),
            ));
        let select_all = store.clone();
        let selection = view()
            .h(zpx(30.0))
            .flex_none()
            .flex_row()
            .items_center()
            .justify_between()
            .px(zpx(12.0))
            .child(
                text(t!(
                    "changes.count",
                    n = count.to_string().as_str(),
                    total = changes.len().to_string().as_str()
                ))
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
            .child(
                view()
                    .cursor_pointer()
                    .on_press(move || {
                        select_all.git_changes.update(|files| {
                            for file in files {
                                file.staged = count != changes.len();
                            }
                        })
                    })
                    .child(check_indicator(count > 0, &resolved)),
            );
        let mut list = view().flex_col().p(zpx(8.0)).gap(zpx(2.0));
        if store.git_loading.get() {
            list = list.child(
                text(t!("git.loading"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
        if let Some(error) = store.git_error.get() {
            list = list.child(
                text(error)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
        for (index, item) in store.git_changes.get().into_iter().enumerate() {
            let select = store.clone();
            let toggle = store.clone();
            let path = item.path.clone();
            let (parent, name) = item
                .path
                .rsplit_once('/')
                .unwrap_or(("", item.path.as_str()));
            list = list.child(
                view()
                    .h(zpx(28.0))
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.0))
                    .px(zpx(4.0))
                    .rounded(zpx(8.0))
                    .child(
                        text(item.status)
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.warning)),
                    )
                    .child(
                        view()
                            .flex_1()
                            .min_w_0()
                            .flex_row()
                            .gap(zpx(6.0))
                            .cursor_pointer()
                            .on_press(move || {
                                select.preview_git_path(&path);
                            })
                            .child(
                                text(name.to_string())
                                    .text_xs()
                                    .number_of_lines(1)
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            )
                            .child(
                                text(parent.to_string())
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .number_of_lines(1)
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            ),
                    )
                    .child(
                        text(format!("+{} −{}", item.additions, item.deletions))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.success)),
                    )
                    .child(
                        view()
                            .cursor_pointer()
                            .on_press(move || {
                                toggle.git_changes.update(|files| {
                                    if let Some(file) = files.get_mut(index) {
                                        file.staged = !file.staged;
                                    }
                                })
                            })
                            .child(check_indicator(item.staged, &resolved)),
                    ),
            );
        }
        let draft = store.commit_draft;
        let commit = store.clone();
        let push = store.clone();
        let footer = view()
            .flex_none()
            .flex_col()
            .gap(zpx(8.0))
            .p(zpx(8.0))
            .border_t_1()
            .border_color(rgba_to_hsla(ui.border))
            .child(
                input()
                    .multiline(true)
                    .h(zpx(64.0))
                    .w_full()
                    .px(zpx(10.0))
                    .py(zpx(8.0))
                    .rounded(zpx(8.0))
                    .border_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .bg(rgb_to_hsla(ui.app))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground))
                    .placeholder(t!("changes.messagePlaceholder"))
                    .value(draft.get())
                    .on_change(move |value: &str| draft.set(value.to_string())),
            )
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.0))
                    .cursor_pointer()
                    .on_press(move || amend.update(|v| *v = !*v))
                    .child(check_indicator(amend.get(), &resolved))
                    .child(
                        text(t!("changes.amend"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    ),
            )
            .child(
                view()
                    .flex_row()
                    .gap(zpx(6.0))
                    .child(
                        view()
                            .flex_1()
                            .items_center()
                            .py(zpx(5.0))
                            .rounded(zpx(8.0))
                            .bg(rgb_to_hsla(ui.secondary))
                            .cursor_pointer()
                            .on_press(move || {
                                if !commit.commit_draft.get().trim().is_empty() || amend.get() {
                                    commit.commit_git_changes(amend.get(), false);
                                }
                            })
                            .child(
                                text(t!("changes.commit"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            ),
                    )
                    .child(
                        view()
                            .flex_1()
                            .items_center()
                            .py(zpx(5.0))
                            .rounded(zpx(8.0))
                            .bg(rgb_to_hsla(ui.secondary))
                            .cursor_pointer()
                            .on_press(move || {
                                if !push.commit_draft.get().trim().is_empty() || amend.get() {
                                    push.commit_git_changes(amend.get(), true);
                                }
                            })
                            .child(
                                text(t!("changes.commitAndPush"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            ),
                    ),
            );
        view()
            .size_full()
            .flex_col()
            .child(header)
            .child(selection)
            .child(
                view()
                    .flex_1()
                    .min_h_0()
                    .flex_col()
                    .child(scroll().flex_1().min_h_0().child(list))
                    .when(store.selected_git_path.get().is_some(), |v| {
                        v.child(
                            view()
                                .flex_1()
                                .min_h_0()
                                .border_t_1()
                                .border_color(rgba_to_hsla(ui.border))
                                .child(diff_pane_view()),
                        )
                    }),
            )
            .child(footer)
            .into_element()
    }))
}

/// 工作区统一 Diff 审查窗口（对应 `DIFF_KEY`）。
#[component]
pub fn DiffPaneView() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();

    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let changes = store.git_changes.get();
            let selected = store.selected_git_path.get();
            let current = selected
                .as_deref()
                .and_then(|p| changes.iter().find(|c| c.path == p))
                .or_else(|| changes.first())
                .cloned();

            let Some(item) = current else {
                return view()
                    .p(zpx(16.0))
                    .child(
                        text(t!("git.diffEmpty"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .into_element();
            };

            let mut lines_view = view().flex_col().p(zpx(10.0)).gap(zpx(2.0));
            if let Some(diff) = store.git_diff.get() {
                for line in diff.lines() {
                    let color = if line.starts_with('+') {
                        ui.success
                    } else if line.starts_with('-') {
                        ui.destructive
                    } else if line.starts_with("@@") {
                        ui.tint_strong
                    } else {
                        ui.foreground
                    };
                    lines_view = lines_view.child(
                        text(line.to_string())
                            .text_xs()
                            .font_family(FONT_FAMILY_MONO)
                            .text_color(rgb_to_hsla(color)),
                    );
                }
            } else {
                for line in item.old_text.lines() {
                    lines_view = lines_view.child(
                        text(format!("− {line}"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.destructive)),
                    );
                }
                for line in item.new_text.lines() {
                    lines_view = lines_view.child(
                        text(format!("+ {line}"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.success)),
                    );
                }
            }

            let expand = store.clone();
            view()
                .size_full()
                .flex_col()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px(zpx(10.0))
                        .py(zpx(6.0))
                        .border_b_1()
                        .border_color(rgba_to_hsla(ui.border))
                        .child(
                            text(item.path)
                                .text_xs()
                                .font_family(FONT_FAMILY_MONO)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            text(format!("+{} -{}", item.additions, item.deletions))
                                .text_xs()
                                .font_family(FONT_FAMILY_MONO)
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                        .child(crate::shell::widgets::icon_button(
                            "expand-diff",
                            t!("files.expandPreview"),
                            crate::shell::widgets::Glyph::Maximize,
                            false,
                            &resolved,
                            move || expand.open_diff_pane(),
                        )),
                )
                .child(scroll().flex_1().child(lines_view))
                .into_element()
        }))
}
