//! Git 工作区修改与统一 / 分栏差异审查。

use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::shell::widgets::{Glyph, check_indicator, glyph, icon_button};
use crate::store::{GitConfirmedOperation, git_change_is_conflicted, use_workspace};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[component]
pub fn GitPanel() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let amend = signal(false);

    let draft = store.commit_draft;
    let draft_store = store.clone();
    let commit_input = input()
        .test_id("git-commit-message")
        .multiline(true)
        .h(zpx(64.0))
        .w_full()
        .px(zpx(10.0))
        .py(zpx(8.0))
        .rounded(zpx(8.0))
        .border_1()
        .border_color(rgba_to_hsla(theme.resolved().ui.border))
        .bg(rgb_to_hsla(theme.resolved().ui.app))
        .text_xs()
        .text_color(rgb_to_hsla(theme.resolved().ui.foreground))
        .placeholder(t!("changes.messagePlaceholder"))
        .value(move || draft.get())
        .on_change(move |value: &str| draft_store.set_commit_draft(value.to_string()));

    let footer_store = store.clone();
    let footer_theme = theme.clone();
    let footer = dynamic(move || {
        let resolved = footer_theme.resolved();
        let ui = &resolved.ui;
        let busy = footer_store.git_operation_busy.get().is_some_and(|context| {
            footer_store.active_project_id.get() == context.project_id
                && footer_store.selected_git_repo.get() == context.repo
                && footer_store.server_url.get() == context.server_url
        });
        let has_selection = footer_store.git_changes.get().iter().any(|change| change.staged);
        let draft_present = !footer_store.commit_draft.get().trim().is_empty();
        let amend_enabled = amend.get();
        let can_commit = (draft_present || amend_enabled) && (has_selection || amend_enabled);
        let mut controls = view()
            .flex_none()
            .flex_col()
            .gap(zpx(8.0))
            .p(zpx(8.0))
            .border_t_1()
            .border_color(rgba_to_hsla(ui.border));
        if let Some(error) = footer_store.git_operation_error.get() {
            controls = controls.child(
                text(error).text_xs().text_color(rgb_to_hsla(ui.destructive)),
            );
        }
        let amend_toggle = amend.clone();
        let amend_state = amend.clone();
        controls = controls.child(
            view()
                .test_id("git-amend")
                .flex_row()
                .items_center()
                .gap(zpx(6.0))
                .cursor_pointer()
                .on_press(move || amend_toggle.update(|value| *value = !*value))
                .child(check_indicator(amend_state.get(), &resolved))
                .child(text(t!("changes.amend")).text_xs().text_color(rgb_to_hsla(ui.foreground))),
        );
        let commit = footer_store.clone();
        let push = footer_store.clone();
        let amend_commit = amend.clone();
        let amend_push = amend.clone();
        controls = controls.child(
            view().flex_row().gap(zpx(6.0))
                .child(action_button("git-commit", t!("changes.commit"), ui, busy || !can_commit, move || {
                    if !busy && can_commit {
                        commit.commit_git_changes(amend_commit.get(), false);
                    }
                }))
                .child(action_button("git-commit-push", t!("changes.commitAndPush"), ui, busy || !can_commit, move || {
                    if !busy && can_commit {
                        push.commit_git_changes(amend_push.get(), true);
                    }
                })),
        );
        controls
    });

    let body_store = store.clone();
    view()
        .size_full()
        .flex_col()
        .child(view().flex_1().min_h_0().child(dynamic(move || {
            let resolved = theme.resolved();
            let ui = &resolved.ui;
            let changes = body_store.git_changes.get();
            let total = changes.len();
            let has_changes = total > 0;
            let count = changes.iter().filter(|change| change.staged).count();
            let mutation_busy = body_store.git_operation_busy.get().is_some_and(|context| {
                body_store.active_project_id.get() == context.project_id
                    && body_store.selected_git_repo.get() == context.repo
                    && body_store.server_url.get() == context.server_url
            });
            let conflict = body_store.git_conflict.get();
            let repos = body_store.git_repos_for_active_project();
            let selected_repo = body_store.selected_git_repo.get();

            let refresh = body_store.clone();
            let header = view()
                .h(zpx(40.0))
                .flex_none()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(12.0))
                .border_b_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(text(t!("changes.panel")).text_xs().font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)))
                .child(action_button("git-refresh", t!("git.refresh"), ui, false, move || refresh.refresh_git_data()));

            let mut content = view().size_full().flex_col().child(header);
            if !repos.is_empty() {
                let selected = selected_repo.clone();
                let select_store = body_store.clone();
                let selected_repo_value = selected_repo.clone().unwrap_or_default();
                let trigger_title = selected_repo.as_deref().and_then(|value| value.rsplit('/').next())
                    .unwrap_or("").to_string();
                let popup_bg = rgb_to_hsla(ui.popover);
                let popup_foreground = rgb_to_hsla(ui.popover_foreground);
                let popup_border = rgba_to_hsla(ui.border);
                let action_owner = crate::shell::action_owner::ActionOwner::current();
                let popup_token = action_owner.popup_token();
                let select_guard = crate::shell::popups::ServerUrlGuard::capture(&body_store);
                let repo_select = nsh_base::select()
                    .items(repos.iter().map(|repo| {
                        nsh_base::SelectItem::new(
                            repo.clone(),
                            repo.rsplit('/').next().unwrap_or(repo).to_string(),
                        )
                    }).collect::<Vec<_>>())
                    .value(selected.unwrap_or_default())
                    .offset(zpx(4.0))
                    .margin(zpx(8.0))
                    .item_class("group focus:bg-tint focus:text-tint-foreground selected:bg-tint selected:text-tint-foreground disabled:opacity-50")
                    .popup_style(move |style| style.bg(popup_bg).text_color(popup_foreground)
                        .rounded(zpx(8.0)).border_1().border_color(popup_border).p(zpx(4.0))
                        .shadow([crate::shell::popups::popup_shadow(popup_border)]))
                    .trigger(view().test_id("git-repo-select").w_full().px(zpx(10.0)).py(zpx(6.0))
                        .focusable(true).flex_row().items_center().justify_between()
                        .border_1().border_color(rgba_to_hsla(ui.border)).rounded(zpx(8.0))
                        .bg(rgb_to_hsla(ui.app))
                        .child(text(trigger_title).text_xs().text_color(rgb_to_hsla(ui.foreground)))
                        .child(glyph(Glyph::ChevronDown)))
                    .render_item(move |entry: &nsh_base::SelectEntry| {
                        let is_selected = entry.value.as_ref() == selected_repo_value.as_str();
                        let mut item = view().flex_row().items_center().gap(zpx(6.0))
                            .px(zpx(10.0)).py(zpx(6.0)).rounded(zpx(6.0));
                        if is_selected { item = item.child(glyph(Glyph::Check)); }
                        item.child(text(entry.title.clone()).text_xs())
                    })
                    .on_open_change(move |event: &nsh_base::SelectEvent| {
                        if let nsh_base::SelectEvent::OpenChange(open, _) = event {
                            popup_token.set_open(*open);
                        }
                    })
                    .on_change(move |value: &Option<SharedString>| {
                        if let Some(repo) = value.as_deref() {
                            let repo = repo.to_string();
                            let store = select_store.clone();
                            let guard = select_guard.clone();
                            action_owner.run(|| {
                                if guard.is_current() { store.select_git_repo(&repo); }
                            });
                        }
                    });
                content = content.child(view().p(zpx(8.0)).child(repo_select));
            }

            if let Some(error) = body_store.git_error.get() {
                content = content.child(view().px(zpx(10.0)).py(zpx(4.0))
                    .child(text(error).text_xs().text_color(rgb_to_hsla(ui.destructive))));
            }
            if conflict.is_some() {
                let conflict_actions = body_store.clone();
                let abort = body_store.clone();
                let ours = body_store.clone();
                let theirs = body_store.clone();
                let can_take = changes.iter().any(|item| item.staged && git_change_is_conflicted(item));
                content = content.child(view().flex_none().flex_col().gap(zpx(4.0)).p(zpx(8.0))
                    .child(text(t!("git.conflictActive")).text_xs().text_color(rgb_to_hsla(ui.warning)))
                    .child(view().flex_row().gap(zpx(4.0))
                        .child(action_button("git-continue", t!("git.continue"), ui, mutation_busy, move || conflict_actions.continue_git_operation()))
                        .child(action_button("git-abort", t!("git.abort"), ui, mutation_busy, move || abort.ask_git_abort()))
                        .child(action_button("git-take-ours", t!("git.takeOurs"), ui, mutation_busy || !can_take, move || {
                            if can_take { ours.ask_git_take(falcon_proto::GitTakeSide::Ours); }
                        }))
                        .child(action_button("git-take-theirs", t!("git.takeTheirs"), ui, mutation_busy || !can_take, move || {
                            if can_take { theirs.ask_git_take(falcon_proto::GitTakeSide::Theirs); }
                        }))));
            }

            let select_all = body_store.clone();
            content = content.child(view().h(zpx(30.0)).flex_none().flex_row().items_center()
                .justify_between().px(zpx(12.0))
                .child(text(t!("changes.count", n = count.to_string().as_str(), total = changes.len().to_string().as_str()))
                    .text_xs().text_color(rgb_to_hsla(ui.muted_foreground)))
                .child(view().test_id("git-select-all").cursor_pointer().on_press(move || {
                    let select = count != total;
                    select_all.git_changes.update(|files| files.iter_mut().for_each(|file| file.staged = select));
                }).child(check_indicator(count > 0 && count == changes.len(), &resolved))));
            let discard_selected = body_store.clone();
            let discard_all = body_store.clone();
            content = content.child(view().flex_none().flex_row().gap(zpx(6.0)).px(zpx(8.0)).pb(zpx(4.0))
                .child(action_button("git-discard-selected", t!("changes.discard"), ui, mutation_busy || count == 0, move || {
                    if count > 0 { discard_selected.ask_discard_git_changes(false); }
                }))
                .child(action_button("git-discard-all", t!("git.discardAll"), ui, mutation_busy || !has_changes, move || {
                    if has_changes { discard_all.ask_discard_git_changes(true); }
                })));
            content = content.child(view().px(zpx(10.0)).pb(zpx(4.0))
                .child(text(t!("git.selectionMeaning")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground))));

            if let Some(confirmation) = body_store.git_confirmation.get() {
                let confirm = body_store.clone();
                let cancel = body_store.clone();
                let mut prompt = view().flex_none().flex_col().gap(zpx(5.0)).p(zpx(10.0))
                    .border_t_1().border_color(rgba_to_hsla(ui.border))
                    .child(text(match confirmation.operation {
                        GitConfirmedOperation::Restore { .. } => t!("git.confirmDiscard"),
                        GitConfirmedOperation::Abort => t!("git.confirmAbort"),
                        GitConfirmedOperation::Take { .. } => t!("git.confirmTake"),
                    }).text_xs().font_weight(FontWeight::SEMIBOLD).text_color(rgb_to_hsla(ui.foreground)))
                    .child(text(format!("{} · {}", confirmation.project_name,
                        confirmation.context.repo.as_deref().unwrap_or(&confirmation.context.project_id)))
                        .text_xs().text_color(rgb_to_hsla(ui.muted_foreground)));
                let mut path_list = view().flex_col().gap(zpx(3.0));
                for path in &confirmation.paths {
                    path_list = path_list.child(text(path.clone()).text_xs().font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.foreground)));
                }
                if !confirmation.paths.is_empty() {
                    prompt = prompt.child(scroll().h(zpx(96.0)).child(path_list));
                }
                if !confirmation.untracked.is_empty() {
                    prompt = prompt.child(text(t!("git.untrackedWillDelete"))
                        .text_xs().text_color(rgb_to_hsla(ui.destructive)));
                }
                let confirmation_busy = body_store.git_operation_busy.get().as_ref() == Some(&confirmation.context);
                prompt = prompt.child(view().flex_row().gap(zpx(6.0))
                    .child(action_button("git-cancel-confirm", t!("common.cancel"), ui, false, move || cancel.cancel_git_confirmation()))
                    .child(action_button("git-confirm-action", t!("common.confirm"), ui, confirmation_busy, move || confirm.confirm_git_operation())));
                content = content.child(prompt);
            }

            let mut list = view().flex_col().p(zpx(8.0)).gap(zpx(2.0));
            if body_store.git_loading.get() {
                list = list.child(text(t!("git.loading")).test_id("git-loading").text_xs().text_color(rgb_to_hsla(ui.muted_foreground)));
            }
            for (index, item) in changes.iter().cloned().enumerate() {
                let select = body_store.clone();
                let toggle = body_store.clone();
                let path = item.path.clone();
                let (parent, name) = item.path.rsplit_once('/').unwrap_or(("", item.path.as_str()));
                let file_test_id = format!("git-file-{index}");
                let select_test_id = format!("git-select-file-{index}");
                let row = view().h(zpx(28.0)).flex_row().items_center().gap(zpx(6.0)).px(zpx(4.0))
                    .child(text(item.status.clone()).text_xs().text_color(rgb_to_hsla(if git_change_is_conflicted(&item) { ui.destructive } else { ui.warning })))
                    .child(view().test_id(&file_test_id).flex_1().min_w_0().flex_row().gap(zpx(6.0))
                        .cursor_pointer().on_press(move || select.preview_git_path(&path))
                        .child(text(name.to_string()).text_xs().number_of_lines(1).text_color(rgb_to_hsla(ui.foreground)))
                        .child(text(parent.to_string()).flex_1().min_w_0().text_xs().number_of_lines(1).text_color(rgb_to_hsla(ui.muted_foreground))))
                    .child(text(format!("+{} −{}", item.additions, item.deletions)).text_xs().text_color(rgb_to_hsla(ui.success)))
                    .child(view().test_id(&select_test_id).cursor_pointer().on_press(move || {
                        toggle.git_changes.update(|files| if let Some(file) = files.get_mut(index) { file.staged = !file.staged; });
                    }).child(check_indicator(item.staged, &resolved)));
                list = list.child(row);
            }
            content = content.child(view().flex_1().min_h_0().flex_col()
                .child(scroll().flex_1().min_h_0().child(list))
                .when(body_store.selected_git_path.get().is_some(), |container| {
                    container.child(view().h(zpx(210.0)).flex_none().border_t_1().border_color(rgba_to_hsla(ui.border))
                        .child(diff_pane_view()))
                }));

            content.into_element()
        })))
        .child(view().flex_none().flex_col().p(zpx(8.0)).gap(zpx(8.0))
            .child(commit_input)
            .child(footer))
}

fn action_button(
    id: &'static str,
    label: impl Into<SharedString>,
    ui: &falcon_theme::UiTokens,
    disabled: bool,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    let label: SharedString = label.into();
    let background = rgb_to_hsla(if disabled { ui.app } else { ui.secondary });
    let foreground = rgb_to_hsla(if disabled { ui.muted_foreground } else { ui.foreground });
    view().test_id(id).disabled(disabled).flex_1().items_center().py(zpx(5.0)).rounded(zpx(8.0))
        .bg(background).when(!disabled, |button| button.cursor_pointer())
        .on_press(move || if !disabled { action(); })
        .child(text(label).text_xs().text_color(foreground))
}

#[component]
pub fn DiffPaneView() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    view().size_full().flex_col().overflow_hidden().child(dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let selected = store.selected_git_path.get().unwrap_or_default();
        let changes = store.git_changes.get();
        let item = changes.iter().find(|change| change.path == selected).cloned();
        let split = store.git_diff_split.get();
        let toggle = store.clone();
        let expand = store.clone();
        let mut header = view().flex_none().flex_row().items_center().justify_between()
            .px(zpx(10.0)).py(zpx(5.0)).border_b_1().border_color(rgba_to_hsla(ui.border))
            .child(text(item.as_ref().map(|change| change.path.clone()).unwrap_or(selected))
                .flex_1().min_w_0().number_of_lines(1)
                .text_xs().font_family(FONT_FAMILY_MONO).text_color(rgb_to_hsla(ui.foreground)))
                    .child(view().test_id("git-diff-mode").cursor_pointer().on_press(move || toggle.git_diff_split.set(!split))
                .child(text(if split { t!("git.viewUnified") } else { t!("git.viewSplit") }).text_xs().text_color(rgb_to_hsla(ui.tint_strong))));
        if store.active_dialog.get() != crate::store::ActiveDialog::PreviewPopup {
            header = header.child(icon_button(
                "git-diff-expand", t!("native.preview.expandDiff"), Glyph::Maximize, false,
                &resolved, move || crate::views::preview_popup::open_diff(&expand),
            ));
        }
        if let Some(item) = item {
            header = header.child(text(format!("+{} -{}", item.additions, item.deletions))
                .text_xs().font_family(FONT_FAMILY_MONO).text_color(rgb_to_hsla(ui.muted_foreground)));
        }
        let mut body = view().flex_1().min_h_0().flex_col();
        if store.git_diff_loading.get() {
            body = body.child(text(t!("git.diffLoading")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)));
        } else if let Some(error) = store.git_diff_error.get() {
            body = body.child(text(error).text_xs().text_color(rgb_to_hsla(ui.destructive)));
        } else if let Some(diff) = store.git_diff.get() {
            if store.git_diff_truncated.get() {
                body = body.child(text(t!("git.diffTruncated")).text_xs().text_color(rgb_to_hsla(ui.warning)));
            }
            let rows = falcon_core::agent_output::aligned_unified_diff(&diff);
            if diff.is_empty() || (split && rows.is_empty()) {
                body = body.child(text(t!("git.diffEmpty")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)));
            } else if split {
                let mut table = view().flex_col().p(zpx(6.0));
                for row in rows {
                    use falcon_core::agent_output::AlignedDiffKind as Kind;
                    if matches!(row.kind, Kind::FileHeader | Kind::HunkHeader | Kind::Marker) {
                        table = table.child(text(row.old_text).text_xs().font_family(FONT_FAMILY_MONO)
                            .text_color(rgb_to_hsla(if row.kind == Kind::HunkHeader { ui.tint_strong } else { ui.muted_foreground })));
                        continue;
                    }
                    let left = format!("{:>4}  {}", row.old_line.map(|n| n.to_string()).unwrap_or_default(), row.old_text);
                    let right = format!("{:>4}  {}", row.new_line.map(|n| n.to_string()).unwrap_or_default(), row.new_text);
                    let left_color = if matches!(row.kind, Kind::Replacement | Kind::Deletion) { ui.destructive } else { ui.foreground };
                    let right_color = if matches!(row.kind, Kind::Replacement | Kind::Insertion) { ui.success } else { ui.foreground };
                    let mut left_cell = view().flex_1().min_w_0().child(text(left).text_xs().font_family(FONT_FAMILY_MONO).text_color(rgb_to_hsla(left_color)));
                    let mut right_cell = view().flex_1().min_w_0().child(text(right).text_xs().font_family(FONT_FAMILY_MONO).text_color(rgb_to_hsla(right_color)));
                    if row.old_no_newline { left_cell = left_cell.child(text("\\ No newline at end of file").text_xs().text_color(rgb_to_hsla(ui.muted_foreground))); }
                    if row.new_no_newline { right_cell = right_cell.child(text("\\ No newline at end of file").text_xs().text_color(rgb_to_hsla(ui.muted_foreground))); }
                    table = table.child(view().flex_row().child(left_cell).child(right_cell));
                }
                body = body.child(scroll().flex_1().child(table));
            } else {
                let mut lines = view().flex_col().p(zpx(8.0)).gap(zpx(1.0));
                for row in falcon_core::agent_output::unified_diff_display_lines(&diff) {
                    use falcon_core::agent_output::UnifiedDiffLineKind as Kind;
                    let color = match row.kind {
                        Kind::FileHeader | Kind::Marker => ui.muted_foreground,
                        Kind::HunkHeader => ui.tint_strong,
                        Kind::Deletion => ui.destructive,
                        Kind::Insertion => ui.success,
                        Kind::Context => ui.foreground,
                    };
                    lines = lines.child(text(row.text).text_xs().font_family(FONT_FAMILY_MONO).text_color(rgb_to_hsla(color)));
                }
                body = body.child(scroll().flex_1().child(lines));
            }
        } else {
            body = body.child(text(t!("git.diffEmpty")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)));
        }
        view().size_full().flex_col().child(header).child(body)
    }))
}
