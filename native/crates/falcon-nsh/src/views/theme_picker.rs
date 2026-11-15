//! Settings theme picker. Drafts live in ModalLayer so previewing the whole surface cannot discard them.
use std::rc::Rc;

use falcon_core::command_palette::move_selection;
use falcon_theme::{
    Appearance, ThemeChoice, ThemeColors, ThemeKind, ThemeMode, choice_of, derive_theme,
    resolve_custom_theme, serialize_ghostty_theme, theme_match_rank,
};
use nsh_core::element::{ListLayout, input, virtual_list};
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::store::use_workspace;
use crate::terminal::bind_safe_key_when;
use crate::theme::{ThemeController, rgb_to_hsla, rgba_to_hsla};
use crate::zoom::zpx;

// Imported themes may reuse a catalog name, so highlight by kind and name together.
type ChoiceKey = (ThemeKind, String);

fn choice_key(choice: &ThemeChoice) -> ChoiceKey {
    (choice.kind, choice.name.clone())
}

#[derive(Clone, Copy)]
pub(super) struct PickerState {
    filter: Signal<String>,
    editor: Signal<bool>,
    name: Signal<String>,
    source: Signal<String>,
    highlighted: Signal<Option<ChoiceKey>>,
}

impl PickerState {
    pub fn new() -> Self {
        Self {
            filter: signal(String::new()),
            editor: signal(false),
            name: signal(String::new()),
            source: signal(String::new()),
            highlighted: signal(None),
        }
    }

    pub fn reset(self) {
        self.filter.set(String::new());
        self.editor.set(false);
        self.name.set(String::new());
        self.source.set(String::new());
        self.highlighted.set(None);
    }

    fn edit(self, theme: ThemeController, slot: ThemeMode) {
        let choice = theme.settings().slot(slot).clone();
        self.name.set(if choice.kind == ThemeKind::Custom {
            choice.name.clone()
        } else {
            String::new()
        });
        let source = if choice.kind == ThemeKind::Custom {
            serialize_ghostty_theme(&choice.colors)
        } else {
            format!("theme = {}\n", choice.name)
        };
        self.source.set(source.clone());
        self.editor.set(true);
        theme.preview(resolve_custom_theme(&source, slot, theme.catalog()).colors);
    }
}

pub(super) fn swatch(colors: &ThemeColors) -> impl IntoElement {
    let resolved = derive_theme(colors);
    let ui = resolved.ui;
    view()
        .w(zpx(16.0))
        .h(zpx(16.0))
        .rounded(zpx(4.0))
        .overflow_hidden()
        .flex_col()
        .child(
            view()
                .flex_1()
                .flex_row()
                .child(view().flex_1().bg(rgb_to_hsla(ui.background)))
                .child(view().flex_1().bg(rgb_to_hsla(ui.foreground))),
        )
        .child(
            view()
                .flex_1()
                .flex_row()
                .child(view().flex_1().bg(rgb_to_hsla(ui.destructive)))
                .child(view().flex_1().bg(rgb_to_hsla(ui.primary))),
        )
}

fn button(
    id: &str,
    label: String,
    enabled: bool,
    action: impl Fn() + 'static,
    theme: ThemeController,
) -> impl IntoElement {
    view()
        .test_id(id)
        .px(zpx(10.0))
        .py(zpx(6.0))
        .rounded(zpx(8.0))
        .bg(move || rgb_to_hsla(theme.resolved().ui.app))
        .text_color(move || {
            let resolved = theme.resolved();
            rgb_to_hsla(if enabled {
                resolved.ui.foreground
            } else {
                resolved.ui.muted_foreground
            })
        })
        .when(enabled, |v| v.cursor_pointer().on_press(action))
        .child(text(label).text_xs())
}

pub(super) fn render(
    theme: ThemeController,
    slot: ThemeMode,
    open: Signal<bool>,
) -> impl IntoElement {
    let state = use_context::<PickerState>().expect("ModalLayer theme picker state");
    if state.editor.get() {
        return editor(theme, slot, open, state).into_element();
    }
    let current = theme.settings().slot(slot).clone();
    let want = if slot == ThemeMode::Light {
        Appearance::Light
    } else {
        Appearance::Dark
    };
    let input_ref = element_ref();
    let store = use_workspace();
    let copy_colors = current.colors;
    view()
        .test_id("theme-picker")
        .flex_col()
        .gap(zpx(8.0))
        .child(
            input()
                .test_id("theme-picker-filter")
                .element_ref(&input_ref)
                .auto_focus(true)
                .w_full()
                .px(zpx(8.0))
                .py(zpx(6.0))
                .rounded(zpx(8.0))
                .bg(move || rgb_to_hsla(theme.resolved().ui.app))
                .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground))
                .text_xs()
                .placeholder(t!(
                    "native.themePicker.search",
                    n = theme
                        .catalog()
                        .iter()
                        .filter(|e| e.appearance == want)
                        .count()
                        .to_string()
                        .as_str()
                ))
                .value(state.filter.get_untracked())
                .on_change(move |value: &str| {
                    state.filter.set(value.to_string());
                    state.highlighted.set(None);
                    theme.preview(None);
                }),
        )
        .child(dynamic(move || {
            results(theme, slot, open, state, input_ref)
        }))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(zpx(8.0))
                .child(button(
                    "theme-picker-custom",
                    t!("native.themePicker.customEntry"),
                    true,
                    move || state.edit(theme, slot),
                    theme,
                ))
                .child(button(
                    "theme-picker-copy",
                    t!("native.themePicker.copyGhostty"),
                    true,
                    move || {
                        set_clipboard(serialize_ghostty_theme(&copy_colors));
                        store.show_toast(t!("native.themePicker.copied"));
                    },
                    theme,
                ))
                .child(button(
                    "theme-picker-close",
                    t!("native.themePicker.close"),
                    true,
                    move || {
                        theme.preview(None);
                        open.set(false);
                    },
                    theme,
                )),
        )
        .into_element()
}

fn results(
    theme: ThemeController,
    slot: ThemeMode,
    open: Signal<bool>,
    state: PickerState,
    input_ref: nsh_core::element::ElementRef,
) -> impl IntoElement {
    let current = theme.settings().slot(slot).clone();
    let want = if slot == ThemeMode::Light {
        Appearance::Light
    } else {
        Appearance::Dark
    };
    let query = state.filter.get();
    let mut choices = Vec::new();
    let mut headings = Vec::new();
    if current.kind == ThemeKind::Custom
        && theme_match_rank(&query, &format!("custom:{}", current.name)).is_some()
    {
        headings.push((0, t!("native.themePicker.groupCurrent")));
        choices.push(current.clone());
    }
    for (falcon, label) in [
        (true, t!("native.themePicker.groupFalcon")),
        (
            false,
            if slot == ThemeMode::Light {
                t!("native.themePicker.groupLight")
            } else {
                t!("native.themePicker.groupDark")
            },
        ),
    ] {
        let mut group: Vec<_> = theme
            .catalog()
            .iter()
            .filter(|entry| entry.appearance == want && entry.name.starts_with("Falcon ") == falcon)
            .filter_map(|entry| theme_match_rank(&query, &entry.name).map(|rank| (rank, entry)))
            .collect();
        group.sort_by_key(|(rank, _)| *rank);
        if !group.is_empty() {
            headings.push((choices.len(), label));
            choices.extend(group.into_iter().map(|(_, entry)| choice_of(entry)));
        }
    }
    let choices = Rc::new(choices);
    let list_ref = element_ref();
    let highlighted = state
        .highlighted
        .get_untracked()
        .filter(|key| choices.iter().any(|choice| choice_key(choice) == *key))
        .or_else(|| {
            choices
                .iter()
                .find(|choice| choice_key(choice) == choice_key(&current))
                .map(choice_key)
        })
        .or_else(|| choices.first().map(choice_key));
    state.highlighted.set(highlighted.clone());
    untracked(|| {
        theme.preview(
            highlighted
                .as_ref()
                .and_then(|key| choices.iter().find(|choice| choice_key(choice) == *key))
                .map(|choice| choice.colors.clone()),
        )
    });
    for (key, delta) in [("down", 1), ("up", -1)] {
        let choices = choices.clone();
        bind_safe_key_when(key, false, nsh_core::keymap::When::Always, move || {
            if choices.is_empty() {
                return;
            }
            let at = state
                .highlighted
                .get_untracked()
                .or_else(|| Some(choice_key(theme.settings().slot(slot))));
            let index = at.and_then(|key| choices.iter().position(|c| choice_key(c) == key));
            let next = index.map_or(if delta > 0 { 0 } else { choices.len() - 1 }, |i| {
                move_selection(i, choices.len(), delta)
            });
            state.highlighted.set(Some(choice_key(&choices[next])));
            theme.preview(Some(choices[next].colors.clone()));
            list_ref.scroll_to_index(next as u32, Percentage(0.5), zpx(0.0), false);
            input_ref.focus();
        });
    }
    let confirm_choices = choices.clone();
    bind_safe_key_when("enter", false, nsh_core::keymap::When::Always, move || {
        let key = state
            .highlighted
            .get_untracked()
            .unwrap_or_else(|| choice_key(theme.settings().slot(slot)));
        if let Some(choice) = confirm_choices.iter().find(|c| choice_key(c) == key) {
            activate(theme, slot, open, state, choice.clone());
        }
    });
    if choices.is_empty() {
        view()
            .child(
                text(t!("native.themePicker.empty"))
                    .test_id("theme-picker-empty")
                    .text_xs()
                    .text_color(move || rgb_to_hsla(theme.resolved().ui.muted_foreground))
                    .py(zpx(16.0)),
            )
            .into_element()
    } else {
        let start = highlighted
            .as_ref()
            .and_then(|key| choices.iter().position(|c| choice_key(c) == *key))
            .unwrap_or(0);
        let current = current.clone();
        let choices = choices.clone();
        virtual_list(choices.len(), move |at| {
            let choices = choices.clone();
            let headings = headings.clone();
            let current = current.clone();
            dynamic(move || {
                let index = at.index();
                let choice = &choices[index];
                let resolved = theme.resolved();
                let ui = &resolved.ui;
                let mut cell = view().flex_col();
                if let Some((_, label)) = headings.iter().find(|(i, _)| *i == index) {
                    cell = cell.child(
                        text(label.clone())
                            .text_xs()
                            .h(zpx(30.0))
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    );
                }
                let hover = choice.clone();
                let select = choice.clone();
                let selected = current.kind == choice.kind && current.name == choice.name;
                cell.child(
                    view()
                        .test_id(&format!(
                            "theme-choice-{}{}",
                            if choice.kind == ThemeKind::Custom { "custom-" } else { "" },
                            choice.name,
                        ))
                        .h(zpx(30.0))
                        .px(zpx(8.0))
                        .py(zpx(4.0))
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .when(
                            state.highlighted.get().unwrap_or_else(|| choice_key(&current))
                                == choice_key(choice),
                            |v| v.bg(rgb_to_hsla(ui.tint)),
                        )
                        .on_mouse_move(move |_: &nsh_core::element::MouseEvent| {
                            if state.highlighted.get_untracked().as_ref()
                                != Some(&choice_key(&hover))
                            {
                                state.highlighted.set(Some(choice_key(&hover)));
                                theme.preview(Some(hover.colors.clone()));
                            }
                        })
                        .on_press(move || activate(theme, slot, open, state, select.clone()))
                        .child(swatch(&choice.colors))
                        .child(
                            text(choice.name.clone())
                                .flex_1()
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .when(selected, |v| {
                            v.child(
                                text(t!("native.themePicker.groupCurrent"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                        }),
                )
                .into_element()
            })
        })
        .element_ref(&list_ref)
        .h(zpx(320.0))
        .estimated_item_size(zpx(30.0))
        .initial_scroll_index(start)
        .into_element()
    }
}

fn activate(
    theme: ThemeController,
    slot: ThemeMode,
    open: Signal<bool>,
    state: PickerState,
    choice: ThemeChoice,
) {
    if choice.kind == ThemeKind::Custom {
        state.edit(theme, slot);
    } else {
        theme.set_slot_choice(slot, choice);
        open.set(false);
    }
}

fn editor(
    theme: ThemeController,
    slot: ThemeMode,
    open: Signal<bool>,
    state: PickerState,
) -> impl IntoElement {
    view()
        .test_id("theme-editor")
        .flex_col()
        .gap(zpx(8.0))
        .child(
            text(t!("native.themePicker.editorTitle"))
                .text_sm()
                .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground)),
        )
        .child(
            text(t!("native.themePicker.editorHint"))
                .text_xs()
                .text_color(move || rgb_to_hsla(theme.resolved().ui.muted_foreground)),
        )
        .child(
            text(t!("native.themePicker.editorName"))
                .text_xs()
                .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground)),
        )
        .child(
            input()
                .test_id("theme-editor-name")
                .value(state.name.get_untracked())
                .placeholder(t!("native.themePicker.editorNamePlaceholder"))
                .px(zpx(8.0))
                .py(zpx(6.0))
                .bg(move || rgba_to_hsla(theme.resolved().ui.input))
                .text_xs()
                .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground))
                .on_change(move |value: &str| state.name.set(value.to_string())),
        )
        .child(
            text(t!("native.themePicker.editorText"))
                .text_xs()
                .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground)),
        )
        .child(
            input()
                .test_id("theme-editor-source")
                .multiline(true)
                .auto_focus(true)
                .value(state.source.get_untracked())
                .w_full()
                .h(zpx(240.0))
                .px(zpx(8.0))
                .py(zpx(6.0))
                .bg(move || rgba_to_hsla(theme.resolved().ui.input))
                .text_xs()
                .font_family(crate::fonts::FONT_FAMILY_MONO)
                .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground))
                .on_change(move |value: &str| {
                    state.source.set(value.to_string());
                    theme.preview(resolve_custom_theme(value, slot, theme.catalog()).colors);
                }),
        )
        .child(dynamic(move || editor_controls(theme, slot, open, state)))
}

fn editor_controls(
    theme: ThemeController,
    slot: ThemeMode,
    open: Signal<bool>,
    state: PickerState,
) -> impl IntoElement {
    let resolved = theme.resolved();
    let ui = &resolved.ui;
    let parsed = resolve_custom_theme(&state.source.get(), slot, theme.catalog());
    let hint = if let Some(base) = &parsed.unknown_base {
        t!("native.themePicker.editorUnknownBase", name = base.as_str())
    } else if parsed.colors.is_some() {
        t!(
            "native.themePicker.editorRecognized",
            n = parsed.recognized.to_string().as_str()
        )
    } else {
        t!("native.themePicker.editorNothing")
    };
    let valid = parsed.colors.is_some();
    let preview = parsed
        .colors
        .clone()
        .map(|colors| swatch(&colors).into_element());
    let copy = parsed.colors.clone();
    view()
        .flex_col()
        .gap(zpx(8.0))
        .child(
            text(hint)
                .test_id("theme-editor-hint")
                .text_xs()
                .text_color(rgb_to_hsla(if valid {
                    ui.muted_foreground
                } else {
                    ui.destructive
                })),
        )
        .children(preview)
        .child(
            view()
                .flex_row()
                .gap(zpx(8.0))
                .child(button(
                    "theme-editor-copy",
                    t!("native.themePicker.copyGhostty"),
                    valid,
                    move || {
                        if let Some(colors) = &copy {
                            set_clipboard(serialize_ghostty_theme(colors));
                        }
                    },
                    theme,
                ))
                .child(button(
                    "theme-editor-cancel",
                    t!("native.themePicker.cancel"),
                    true,
                    move || {
                        theme.preview(None);
                        open.set(false);
                    },
                    theme,
                ))
                .child(button(
                    "theme-editor-apply",
                    t!("native.themePicker.editorApply"),
                    valid,
                    move || {
                        let parsed = resolve_custom_theme(
                            &state.source.get_untracked(),
                            slot,
                            theme.catalog(),
                        );
                        let Some(colors) = parsed.colors else {
                            return;
                        };
                        let typed = state.name.get_untracked();
                        let name = if typed.trim().is_empty() {
                            parsed
                                .base_name
                                .unwrap_or_else(|| t!("native.themePicker.editorTitle"))
                        } else {
                            typed.trim().to_string()
                        };
                        theme.set_slot_choice(
                            slot,
                            ThemeChoice {
                                name: name.chars().take(80).collect(),
                                kind: ThemeKind::Custom,
                                colors,
                            },
                        );
                        open.set(false);
                    },
                    theme,
                )),
        )
}
