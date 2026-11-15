use std::rc::Rc;

use falcon_core::command_palette::move_selection;
use falcon_core::file_search::{FILE_SEARCH_LIMIT, basename, dirname, filter_files};
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::store::{ActiveDialog, WorkspaceStore};
use crate::shell::widgets::{Glyph, glyph};
use crate::terminal::bind_safe_key_when;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

pub fn render(
    store: &WorkspaceStore,
    query: Signal<String>,
    selected: Signal<usize>,
) -> impl IntoElement {
    let input_ref = element_ref();
    let row_refs = Rc::new(std::cell::RefCell::new(Vec::new()));
    let change_input = input_ref.clone();
    let content_input = input_ref.clone();
    let content_store = store.clone();
    let content_rows = row_refs.clone();
    let resolved = use_theme().resolved();
    let ui = &resolved.ui;
    view()
        .flex_col()
        .h(zpx(460.0))
        .child(
            view().p(zpx(16.0)).child(
                input()
                    .test_id("quick-open-input")
                    .element_ref(&input_ref)
                    .auto_focus(true)
                    .w_full()
                    .px(zpx(10.0))
                    .py(zpx(6.0))
                    .rounded(zpx(6.0))
                    .bg(rgba_to_hsla(ui.input))
                    .text_xs()
                    .placeholder(t!("quickOpen.placeholder"))
                    .on_change(move |value: &str| {
                        query.set(value.to_string());
                        selected.set(0);
                        change_input.focus();
                    }),
            ),
        )
        .child(dynamic(move || {
            render_results(
                content_store.clone(),
                query,
                selected,
                content_rows.clone(),
                content_input.clone(),
            )
        }))
}

fn render_results(
    store: WorkspaceStore,
    query: Signal<String>,
    selected: Signal<usize>,
    row_refs: Rc<std::cell::RefCell<Vec<nsh_core::element::ElementRef>>>,
    input_ref: nsh_core::element::ElementRef,
) -> impl IntoElement {
    let resolved = use_theme().resolved();
    let ui = &resolved.ui;
    let paths = store.quick_open_paths.get();
    let loading = store.quick_open_loading.get();
    let error = store.quick_open_error.get();
    let has_project = !store.quick_open_project.get().is_empty();
    let filtered = if has_project && !loading && error.is_none() {
        filter_files(&paths, &query.get(), FILE_SEARCH_LIMIT)
    } else { Vec::new() };
    let selected_index = selected.get().min(filtered.len().saturating_sub(1));
    selected.set(selected_index);
    let down_store = store.clone();
    let down_query = query;
    let down_refs = row_refs.clone();
    let down_input = input_ref.clone();
    bind_safe_key_when("down", false, nsh_core::keymap::When::Always, move || {
        let paths = down_store.quick_open_paths.get();
        let count = filter_files(&paths, &down_query.get(), FILE_SEARCH_LIMIT).len();
        selected.update(|v| *v = move_selection(*v, count, 1));
        let next = selected.get();
        if let Some(row) = down_refs.borrow().get(next).cloned() {
            row.reveal(Default::default());
        }
        down_input.focus();
    });
    let up_store = store.clone();
    let up_query = query;
    let up_refs = row_refs.clone();
    let up_input = input_ref.clone();
    bind_safe_key_when("up", false, nsh_core::keymap::When::Always, move || {
        let paths = up_store.quick_open_paths.get();
        let count = filter_files(&paths, &up_query.get(), FILE_SEARCH_LIMIT).len();
        selected.update(|v| *v = move_selection(*v, count, -1));
        let next = selected.get();
        if let Some(row) = up_refs.borrow().get(next).cloned() {
            row.reveal(Default::default());
        }
        up_input.focus();
    });
    let st = store.clone();
    let current = selected;
    let enter_query = query;
    bind_safe_key_when("enter", false, nsh_core::keymap::When::Always, move || {
        let paths = st.quick_open_paths.get();
        let filtered = filter_files(&paths, &enter_query.get(), FILE_SEARCH_LIMIT);
        let Some(path) = filtered.get(current.get()) else {
            return;
        };
        let project = st.quick_open_project.get();
        if project != st.active_project_id.get() {
            return;
        }
        let path = path.clone();
        st.active_dialog.set(ActiveDialog::None);
        st.open_file_pane(&project, &path);
    });
    let mut list = view().flex_col().gap(zpx(3.0)).p(zpx(10.0));
    if !has_project {
        list = list.child(text(t!("native.quickOpen.noProject")).test_id("quick-open-no-project")
            .text_xs().text_color(rgb_to_hsla(ui.muted_foreground)));
    } else if let Some(error) = error {
        list = list.child(view().test_id("quick-open-error").flex_col().gap(zpx(4.0))
            .child(text(t!("native.quickOpen.failed")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)))
            .child(text(error).text_xs().text_color(rgb_to_hsla(ui.destructive))));
    } else if loading {
        list = list.child(
            text(t!("quickOpen.loading"))
                .test_id("quick-open-loading")
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    } else if filtered.is_empty() {
        list = list.child(
            text(t!("native.quickOpen.empty"))
                .test_id("quick-open-empty")
                .text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        );
    }
    let mut visible_rows = Vec::with_capacity(filtered.len());
    for (index, path) in filtered.iter().enumerate() {
        let st = store.clone();
        let file = path.clone();
        let row_ref = element_ref();
        let test_id = if index == selected_index {
            format!("quick-open-file-{index}-selected")
        } else {
            format!("quick-open-file-{index}")
        };
        let row = view()
            .test_id(&test_id)
            .element_ref(&row_ref)
            .flex_row()
            .items_center()
            .px(zpx(10.0))
            .py(zpx(6.0))
            .rounded(zpx(6.0))
            .cursor_pointer()
            .gap(zpx(8.0))
            .on_press(move || {
                let project = st.quick_open_project.get();
                if project != st.active_project_id.get() {
                    return;
                }
                st.active_dialog.set(ActiveDialog::None);
                st.open_file_pane(&project, &file);
            })
            .child(view().text_color(rgb_to_hsla(ui.muted_foreground)).child(glyph(Glyph::File)))
            .child(
                text(basename(path).to_string())
                    .flex_1().min_w_0().number_of_lines(1)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground)),
            )
            .when(!dirname(path).is_empty(), |v| v.child(text(dirname(path).to_string())
                .max_w(zpx(200.0)).number_of_lines(1).text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground))));
        list = list.child(if index == selected_index {
            row.bg(rgb_to_hsla(ui.tint))
        } else {
            row
        });
        visible_rows.push(row_ref);
    }
    *row_refs.borrow_mut() = visible_rows;
    view().flex_1().min_h_0().flex_col()
        .child(scroll().flex_1().child(list))
        .when(has_project && !loading && store.quick_open_error.get().is_none() && store.quick_open_truncated.get(), |v| {
            v.child(text(t!("native.quickOpen.truncated", n = paths.len().to_string().as_str()))
                .test_id("quick-open-truncated-footer")
                .px(zpx(16.0)).py(zpx(6.0)).text_xs()
                .text_color(rgb_to_hsla(ui.muted_foreground)))
        })
}
