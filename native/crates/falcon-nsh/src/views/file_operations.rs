use nsh_core::element::input;
use nsh_core::keymap::When;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::shell::widgets::{Glyph, check_indicator, glyph};
use crate::store::{ActiveDialog, ConfirmationKind, FileJob, JobKind, use_workspace};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[component]
pub fn FileOperationsToolbar() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let draft = store.file_operations.name_draft;
    let form = store.file_operations.name_form;
    let form_store = store.clone();
    let job_store = store.clone();

    view()
        .flex_col()
        .gap(zpx(6.0))
        .px(zpx(8.0))
        .py(zpx(6.0))
        .border_b_1()
        .border_color(move || rgba_to_hsla(theme.resolved().ui.border))
        .child(dynamic({
            let store = store.clone();
            move || {
                let selected = store.file_operations.selected.get();
                let entries = store.file_entries.get();
                let files_selected = entries
                    .iter()
                    .filter(|entry| selected.contains(&entry.path) && !entry.is_dir)
                    .count();
                let ready = store.file_operations.can_operate(&store);
                let resolved = theme.resolved();
                let count_label = t!(
                    "files.operations.selectionCount",
                    count = selected.len().to_string()
                );

                let mut controls = view().flex_row().items_center().flex_wrap().gap(zpx(5.0));
                let st = store.clone();
                controls = controls.child(action_button(
                    t!("files.operations.newFolder").to_string(),
                    !ready,
                    &resolved,
                    false,
                    move || st.file_operations.begin_new_directory(&st),
                ));
                let st = store.clone();
                controls = controls.child(action_button(
                    t!("files.operations.rename").to_string(),
                    !ready || selected.len() != 1,
                    &resolved,
                    false,
                    move || st.file_operations.begin_rename(&st),
                ));
                let st = store.clone();
                controls = controls.child(action_button(
                    t!("files.operations.delete").to_string(),
                    !ready || selected.is_empty(),
                    &resolved,
                    true,
                    move || st.file_operations.delete_selected(&st),
                ));
                let st = store.clone();
                controls = controls.child(action_button(
                    t!("files.operations.uploadFiles").to_string(),
                    !ready,
                    &resolved,
                    false,
                    move || st.file_operations.upload_files(&st),
                ));
                let st = store.clone();
                controls = controls.child(action_button(
                    t!("files.operations.uploadFolder").to_string(),
                    !ready,
                    &resolved,
                    false,
                    move || st.file_operations.upload_folder(&st),
                ));
                let st = store.clone();
                controls = controls.child(action_button(
                    t!("files.operations.download").to_string(),
                    !ready || files_selected == 0,
                    &resolved,
                    false,
                    move || st.file_operations.download_selected(&st),
                ));

                view()
                    .flex_col()
                    .gap(zpx(5.0))
                    .child(
                        text(count_label)
                            .text_xs()
                            .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                    )
                    .child(controls)
            }
        }))
        .child(dynamic(move || {
            let Some(current_form) = form.get() else {
                return view().into_element();
            };
            let resolved = theme.resolved();
            let label = match current_form.action {
                crate::store::NameAction::CreateDirectory => t!("files.operations.folderName"),
                crate::store::NameAction::Rename { .. } => t!("files.operations.renameName"),
            };
            let st_submit = form_store.clone();
            let st_submit_input = form_store.clone();
            let st_cancel = form_store.clone();
            let escape_handler = if form_store.active_dialog.get() == ActiveDialog::None
                && form_store.file_operations.confirmation.get().is_none()
            {
                NameFormEscapeHandler().into_element()
            } else {
                view().into_element()
            };
            view()
                .flex_row()
                .items_center()
                .gap(zpx(5.0))
                .child(
                    input()
                        .flex_1()
                        .min_w_0()
                        .px(zpx(7.0))
                        .py(zpx(5.0))
                        .rounded(zpx(5.0))
                        .bg(rgba_to_hsla(resolved.ui.input))
                        .text_xs()
                        .text_color(rgb_to_hsla(resolved.ui.foreground))
                        .placeholder(move || label.clone())
                        .value(move || draft.get())
                        .on_change(move |value: &str| draft.set(value.to_owned()))
                        .on_submit(move |_: &str| {
                            st_submit_input
                                .file_operations
                                .submit_name_form(&st_submit_input)
                        }),
                )
                .child(action_button(
                    t!("files.operations.confirm").to_string(),
                    false,
                    &resolved,
                    false,
                    move || st_submit.file_operations.submit_name_form(&st_submit),
                ))
                .child(action_button(
                    t!("files.operations.cancel").to_string(),
                    false,
                    &resolved,
                    false,
                    move || st_cancel.file_operations.cancel_name_form(),
                ))
                .child(escape_handler)
                .into_element()
        }))
        .child(dynamic({
            let store = job_store.clone();
            move || {
                let Some(job) = store.file_operations.job.get() else {
                    return view().into_element();
                };
                let resolved = theme.resolved();
                let label = job_label(&job);
                let st = store.clone();
                let button = if job.active {
                    let cancel_locked = job.dispatched
                        && matches!(
                            job.kind,
                            JobKind::Rename | JobKind::CreateDirectory | JobKind::Delete
                        );
                    action_button(
                        if cancel_locked {
                            t!("files.operations.working").to_string()
                        } else {
                            t!("files.operations.cancel").to_string()
                        },
                        cancel_locked,
                        &resolved,
                        !cancel_locked,
                        move || st.file_operations.cancel_job(),
                    )
                    .into_element()
                } else {
                    let uncertain = job.uncertain
                        && matches!(
                            job.kind,
                            JobKind::Rename | JobKind::CreateDirectory | JobKind::Delete
                        );
                    action_button(
                        if uncertain {
                            t!("files.operations.refreshBeforeDismiss").to_string()
                        } else {
                            t!("files.operations.dismiss").to_string()
                        },
                        uncertain,
                        &resolved,
                        false,
                        move || st.file_operations.dismiss_job(),
                    )
                    .into_element()
                };
                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(zpx(6.0))
                    .child(
                        text(label)
                            .flex_1()
                            .text_xs()
                            .text_color(rgb_to_hsla(resolved.ui.muted_foreground)),
                    )
                    .child(button)
                    .into_element()
            }
        }))
}

#[component]
pub fn FileOperationLayer() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    view().child(dynamic(move || {
        let Some(confirmation) = store.file_operations.confirmation.get() else {
            return view().into_element();
        };
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let (title, body, details) = match confirmation.kind {
            ConfirmationKind::Delete { paths } => (
                t!(
                    "files.operations.deleteTitle",
                    count = paths.len().to_string()
                ),
                t!("files.operations.deleteBody"),
                paths.join("\n"),
            ),
            ConfirmationKind::Overwrite { path } => (
                t!("files.operations.overwriteTitle"),
                t!("files.operations.overwriteBody"),
                path,
            ),
        };
        let scrim = rgba_to_hsla(falcon_theme::Rgba {
            r: ui.app.r,
            g: ui.app.g,
            b: ui.app.b,
            a: 148,
        });
        let st_cancel = store.clone();
        let st_confirm = store.clone();
        let escape_handler = FileOperationEscapeHandler();
        view()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .p(zpx(16.0))
            .bg(scrim)
            .child(
                view()
                    .w(zpx(520.0))
                    .max_w(relative(1.0))
                    .flex_col()
                    .gap(zpx(12.0))
                    .p(zpx(18.0))
                    .rounded(zpx(12.0))
                    .border_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .bg(rgb_to_hsla(ui.popover))
                    .child(
                        text(title)
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb_to_hsla(ui.popover_foreground)),
                    )
                    .child(
                        text(body)
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(
                        scroll().max_h(zpx(180.0)).child(
                            text(details)
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                    )
                    .child(
                        view()
                            .flex_row()
                            .justify_end()
                            .gap(zpx(8.0))
                            .child(action_button(
                                t!("files.operations.cancel").to_string(),
                                false,
                                &resolved,
                                false,
                                move || st_cancel.file_operations.answer_confirmation(false),
                            ))
                            .child(action_button(
                                t!("files.operations.confirm").to_string(),
                                false,
                                &resolved,
                                true,
                                move || st_confirm.file_operations.answer_confirmation(true),
                            )),
                    ),
            )
            .child(escape_handler)
            .into_element()
    }))
}

#[component]
fn NameFormEscapeHandler() -> impl IntoElement {
    let store = use_workspace();
    on_key_with(
        "escape",
        KeyOptions {
            when: When::Always,
            repeat: false,
            pass_through: false,
        },
        move |_| {
            if store.active_dialog.get() == ActiveDialog::None
                && store.file_operations.confirmation.get().is_none()
                && store.file_operations.name_form.get().is_some()
            {
                store.file_operations.cancel_name_form();
            }
        },
    );
    view().w(zpx(0.0)).h(zpx(0.0))
}

#[component]
fn FileOperationEscapeHandler() -> impl IntoElement {
    let store = use_workspace();
    on_key_with(
        "escape",
        KeyOptions {
            when: When::Always,
            repeat: false,
            pass_through: false,
        },
        move |_| {
            if store.file_operations.confirmation.get().is_some() {
                store.file_operations.answer_confirmation(false);
            }
        },
    );
    view().w(zpx(0.0)).h(zpx(0.0))
}

fn action_button(
    label: String,
    disabled: bool,
    theme: &falcon_theme::ResolvedTheme,
    destructive: bool,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    let ui = &theme.ui;
    let (background, foreground) = if destructive {
        (
            rgb_to_hsla(ui.destructive),
            rgb_to_hsla(ui.destructive_foreground),
        )
    } else {
        (
            rgb_to_hsla(ui.secondary),
            rgb_to_hsla(ui.secondary_foreground),
        )
    };
    view()
        .px(zpx(7.0))
        .py(zpx(4.0))
        .rounded(zpx(5.0))
        .when(disabled, |view| view.opacity(0.45))
        .when(!disabled, |view| view.cursor_pointer())
        .bg(background)
        .on_press(move || {
            if !disabled {
                action();
            }
        })
        .child(text(label).text_xs().text_color(foreground))
}

pub(crate) fn selection_indicator(
    checked: bool,
    theme: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    check_indicator(checked, theme)
}

pub(crate) fn selection_icon(is_directory: bool) -> impl IntoElement {
    glyph(if is_directory {
        Glyph::Folder
    } else {
        Glyph::File
    })
}

fn job_label(job: &FileJob) -> String {
    let action = match job.kind {
        JobKind::Rename => t!("files.operations.rename"),
        JobKind::CreateDirectory => t!("files.operations.newFolder"),
        JobKind::Delete => t!("files.operations.delete"),
        JobKind::Upload if job.active => t!("files.operations.uploading"),
        JobKind::Upload => t!("files.operations.uploadAction"),
        JobKind::Download if job.active => t!("files.operations.downloading"),
        JobKind::Download => t!("files.operations.downloadAction"),
    };
    if matches!(job.kind, JobKind::Upload | JobKind::Download) && job.active {
        t!(
            "files.operations.progress",
            action = action,
            done = job.completed.to_string(),
            total = job.total.to_string(),
            bytesDone = job.bytes_done.to_string(),
            bytesTotal = job.bytes_total.to_string(),
            current = job.current_item.as_str()
        )
    } else if !job.active {
        let total = job.total.to_string();
        let completed = job.completed.to_string();
        let failed = job.failed.to_string();
        let details = if job.errors.is_empty() {
            String::new()
        } else {
            t!(
                "files.operations.jobErrorDetails",
                details = job.errors.join("\n")
            )
        };
        if job.cancelled {
            t!(
                "files.operations.jobCancelled",
                action = action,
                completed = completed,
                total = total,
                failed = failed,
                details = details,
                uncertain = if job.uncertain {
                    t!("files.operations.resultUncertain")
                } else {
                    String::new()
                }
            )
        } else if job.failed > 0 {
            t!(
                "files.operations.jobPartial",
                action = action,
                completed = completed,
                total = total,
                failed = failed,
                details = details,
                uncertain = if job.uncertain {
                    t!("files.operations.resultUncertain")
                } else {
                    String::new()
                }
            )
        } else {
            t!(
                "files.operations.jobDone",
                action = action,
                completed = completed,
                total = total
            )
        }
    } else {
        t!(
            "files.operations.jobProgress",
            action = action,
            done = job.completed.to_string(),
            total = job.total.to_string(),
            failed = job.failed.to_string()
        )
    }
}
