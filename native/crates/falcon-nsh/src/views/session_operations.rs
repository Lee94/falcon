use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::shell::widgets::{Glyph, button_caption, icon_button_with_shortcut};
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone, PartialEq, Props)]
pub struct RenameSessionProps {
    pub session_id: String,
    pub form_id: u64,
}

#[component]
pub fn RenameSession(props: RenameSessionProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let resolved = theme.resolved();
    let ui = &resolved.ui;
    let session_id = props.session_id;
    let form_id = props.form_id;
    let operations = store.session_operations.clone();
    let initial_name = operations.initial_name(form_id);
    let draft = signal(initial_name.clone());

    let input_draft = draft;
    let input_operations = operations.clone();
    let readonly_operations = operations.clone();
    let readonly_session_id = session_id.clone();
    let enter_draft = draft;
    let enter_operations = operations.clone();
    let enter_store = store.clone();
    let enter_session_id = session_id.clone();
    let button_draft = draft;
    let button_operations = operations.clone();
    let button_disabled_operations = operations.clone();
    let button_store = store.clone();
    let button_session_id = session_id.clone();
    let button_disabled_session_id = session_id.clone();
    let cancel_operations = operations.clone();
    let cancel_store = store.clone();
    let cancel_session_id = session_id.clone();
    let close_operations = operations.clone();
    let close_store = store.clone();
    let close_session_id = session_id.clone();
    let busy_operations = operations.clone();
    let busy_session_id = session_id.clone();
    let error_operations = operations.clone();
    let border = rgba_to_hsla(ui.border);
    let input_background = rgba_to_hsla(ui.input);
    let foreground = rgb_to_hsla(ui.foreground);
    let muted_foreground = rgb_to_hsla(ui.muted_foreground);
    let destructive = rgb_to_hsla(ui.destructive);
    let secondary = rgb_to_hsla(ui.secondary);
    let secondary_foreground = rgb_to_hsla(ui.secondary_foreground);
    let tint = rgb_to_hsla(ui.tint);
    let tint_foreground = rgb_to_hsla(ui.tint_foreground);
    let popover_foreground = rgb_to_hsla(ui.popover_foreground);

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
                .border_color(border)
                .child(
                    text(t!("rename.title"))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(popover_foreground),
                )
                .child(icon_button_with_shortcut(
                    "rename-session-close",
                    t!("common.close"),
                    Glyph::Close,
                    false,
                    Some("escape"),
                    &resolved,
                    move || {
                        close_operations.close_rename(
                            &close_store,
                            &close_session_id,
                            form_id,
                        )
                    },
                )),
        )
        .child(
            view()
                .flex_col()
                .p(zpx(16.0))
                .gap(zpx(12.0))
                .child(
                    input()
                        .test_id("rename-session-input")
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(7.0))
                        .rounded(zpx(7.0))
                        .border_1()
                        .border_color(border)
                        .bg(input_background)
                        .text_sm()
                        .text_color(foreground)
                        .placeholder(t!("rename.placeholder"))
                        .placeholder_color(muted_foreground)
                        .default_value(initial_name)
                        .auto_focus(true)
                        .readonly(move || readonly_operations.is_busy(&readonly_session_id))
                        .on_change(move |value: &str| {
                            input_draft.set(value.to_string());
                            input_operations.clear_form_error(form_id);
                        })
                        .on_submit(move |_: &str| {
                            enter_operations.submit_rename(
                                enter_store.clone(),
                                enter_session_id.clone(),
                                form_id,
                                enter_draft.get_untracked(),
                            )
                        }),
                )
                .child(dynamic(move || {
                    if let Some(error) = error_operations.form_error(form_id) {
                        text(error)
                            .text_xs()
                            .text_color(destructive)
                            .into_element()
                    } else {
                        view().into_element()
                    }
                }))
                .child(
                    view()
                        .flex_row()
                        .justify_end()
                        .gap(zpx(8.0))
                        .child(
                            view()
                                .test_id("rename-session-cancel")
                                .px(zpx(10.0))
                                .py(zpx(6.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .bg(secondary)
                                .on_press(move || {
                                    cancel_operations.close_rename(
                                        &cancel_store,
                                        &cancel_session_id,
                                        form_id,
                                    )
                                })
                                .child(
                                    view().text_color(secondary_foreground)
                                        .child(button_caption(t!("common.cancel"), "escape", &resolved)),
                                ),
                        )
                        .child(
                            view()
                                .test_id("rename-session-submit")
                                .disabled(move || {
                                    button_disabled_operations
                                        .is_busy(&button_disabled_session_id)
                                })
                                .px(zpx(10.0))
                                .py(zpx(6.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .bg(tint)
                                .on_press(move || {
                                    button_operations.submit_rename(
                                        button_store.clone(),
                                        button_session_id.clone(),
                                        form_id,
                                        button_draft.get_untracked(),
                                    )
                                })
                                .child(dynamic(move || {
                                    let label = if busy_operations.is_busy(&busy_session_id) {
                                        t!("session.renaming")
                                    } else {
                                        t!("rename.submit")
                                    };
                                    view()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(tint_foreground)
                                    .child(button_caption(label, "enter", &resolved))
                                })),
                        ),
                ),
        )
}
