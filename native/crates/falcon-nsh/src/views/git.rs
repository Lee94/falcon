//! Git 工作区变更面板、提交图与统一差异窗口。
//!
//! - 右侧栏 [`GitPanel`]：展示已暂存 / 未暂存文件、暂存切换、提交表单、推送/拉取按钮，以及使用
//!   `ResolvedTheme::ui.graph[0..6]` 绘制泳道圆点的最近提交历史；
//! - 工作区 [`DiffPaneView`]：统一差异视图，新增行与删除行严格取自 `ui.success` 与 `ui.destructive`。

use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{GitCommitItem, use_workspace};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

/// 右侧栏修改与 Git 提交面板。
#[component]
pub fn GitPanel() -> impl IntoElement {
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
            let commits = store.git_commits.get();
            let draft = store.commit_draft.get();
            let staged_count = changes.iter().filter(|c| c.staged).count();

            let header = view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(10.0))
                .py(zpx(8.0))
                .border_b_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    text(t!(
                        "changes.count",
                        n = staged_count.to_string().as_str(),
                        total = changes.len().to_string().as_str()
                    ))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(4.0))
                        .child({
                            let st = store.clone();
                            view()
                                .px(zpx(6.0))
                                .py(zpx(3.0))
                                .rounded(zpx(5.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.secondary))
                                .on_press(move || {
                                    st.git_changes.update(|list| {
                                        for c in list {
                                            c.staged = true;
                                        }
                                    });
                                })
                                .child(
                                    text(t!("native.nsh.stageAll"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                )
                        })
                        .child({
                            let st = store.clone();
                            view()
                                .px(zpx(6.0))
                                .py(zpx(3.0))
                                .rounded(zpx(5.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.secondary))
                                .on_press(move || {
                                    st.git_changes.update(|list| {
                                        for c in list {
                                            c.staged = false;
                                        }
                                    });
                                })
                                .child(
                                    text(t!("native.nsh.unstageAll"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                )
                        }),
                );

            let changes_list = view()
                .flex_col()
                .gap(zpx(4.0))
                .children(changes.iter().enumerate().map(|(idx, item)| {
                    let st_toggle = store.clone();
                    let st_select = store.clone();
                    let path_select = item.path.clone();
                    let badge_text = if item.staged {
                        t!("native.nsh.stagedBadge")
                    } else {
                        t!("native.nsh.unstagedBadge")
                    };
                    let badge_color = if item.staged {
                        rgb_to_hsla(ui.success)
                    } else {
                        rgb_to_hsla(ui.muted_foreground)
                    };

                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap(zpx(6.0))
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .bg(rgb_to_hsla(ui.card))
                        .cursor_pointer()
                        .on_press(move || {
                            st_select.selected_git_path.set(Some(path_select.clone()));
                            st_select.open_diff_pane();
                        })
                        .child(
                            view()
                                .flex_col()
                                .flex_1()
                                .overflow_hidden()
                                .child(
                                    text(item.path.clone())
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .number_of_lines(1)
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                                .child(
                                    text(format!("+{} -{}", item.additions, item.deletions))
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                                ),
                        )
                        .child(
                            view()
                                .px(zpx(6.0))
                                .py(zpx(2.0))
                                .rounded(zpx(4.0))
                                .bg(rgb_to_hsla(ui.muted))
                                .cursor_pointer()
                                .on_press(move || {
                                    st_toggle.git_changes.update(|list| {
                                        if let Some(target) = list.get_mut(idx) {
                                            target.staged = !target.staged;
                                        }
                                    });
                                })
                                .child(text(badge_text).text_xs().text_color(badge_color)),
                        )
                }));

            let commit_box = {
                let st_draft = store.clone();
                let st_commit = store.clone();
                let st_diff = store.clone();
                view()
                    .flex_col()
                    .gap(zpx(6.0))
                    .p(zpx(8.0))
                    .rounded(zpx(8.0))
                    .bg(rgb_to_hsla(ui.card))
                    .border_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .child(
                        input()
                            .w_full()
                            .px(zpx(8.0))
                            .py(zpx(5.0))
                            .rounded(zpx(5.0))
                            .bg(rgba_to_hsla(ui.input))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground))
                            .placeholder(t!("native.nsh.commitPlaceholder"))
                            .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                            .value(draft)
                            .on_change(move |v: &str| st_draft.commit_draft.set(v.to_string())),
                    )
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap(zpx(6.0))
                            .child(
                                view()
                                    .flex_1()
                                    .items_center()
                                    .py(zpx(4.0))
                                    .rounded(zpx(5.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.tint_strong))
                                    .on_press(move || {
                                        let msg = st_commit.commit_draft.get();
                                        if !msg.trim().is_empty() {
                                            st_commit.git_commits.update(|log| {
                                                log.insert(
                                                    0,
                                                    GitCommitItem {
                                                        sha: "HEAD".into(),
                                                        lane: 0,
                                                        subject: msg.trim().to_string(),
                                                        author: "local".into(),
                                                        age: "0m".into(),
                                                    },
                                                );
                                            });
                                            st_commit.git_changes.update(|list| {
                                                list.retain(|c| !c.staged);
                                            });
                                            st_commit.commit_draft.set(String::new());
                                        }
                                    })
                                    .child(
                                        text(t!("native.nsh.commitAction"))
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb_to_hsla(ui.primary_foreground)),
                                    ),
                            )
                            .child(
                                view()
                                    .px(zpx(8.0))
                                    .py(zpx(4.0))
                                    .rounded(zpx(5.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.secondary))
                                    .on_press(move || st_diff.open_diff_pane())
                                    .child(
                                        text(t!("git.viewDiff"))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                    ),
                            ),
                    )
            };

            let commit_graph = view()
                .flex_col()
                .gap(zpx(4.0))
                .child(
                    text(t!("native.nsh.commitsTitle"))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .children(commits.into_iter().map(|c| {
                    let lane_color = rgb_to_hsla(ui.graph[c.lane % ui.graph.len()]);
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.0))
                        .py(zpx(4.0))
                        .child(
                            view()
                                .w(zpx(8.0))
                                .h(zpx(8.0))
                                .rounded_full()
                                .bg(lane_color),
                        )
                        .child(
                            view()
                                .flex_col()
                                .flex_1()
                                .overflow_hidden()
                                .child(
                                    text(c.subject)
                                        .text_xs()
                                        .number_of_lines(1)
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                                .child(
                                    text(format!("{} · {}", c.sha, c.age))
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                                ),
                        )
                }));

            view()
                .size_full()
                .flex_col()
                .child(header)
                .child(
                    scroll().flex_1().child(
                        view()
                            .flex_col()
                            .p(zpx(10.0))
                            .gap(zpx(12.0))
                            .child(changes_list)
                            .child(commit_box)
                            .child(commit_graph),
                    ),
                )
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
            for old_line in item.old_text.lines() {
                lines_view = lines_view.child(
                    text(format!("- {old_line}"))
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.destructive)),
                );
            }
            for new_line in item.new_text.lines() {
                lines_view = lines_view.child(
                    text(format!("+ {new_line}"))
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.success)),
                );
            }

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
                        ),
                )
                .child(scroll().flex_1().child(lines_view))
                .into_element()
        }))
}
