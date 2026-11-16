use falcon_core::app_icon::{APP_ICON_IDS, CUSTOM, DEFAULT_APP_ICON, label_key};
use falcon_core::term::{TermCursorStyle, TermFontId, clamp_font_size, clamp_line_height};
use falcon_core::window_background::AppBackground;
use falcon_theme::{
    FALCON_DARK, FALCON_LIGHT, ThemeMode, ThemePref, choice_of, is_default_theme_settings,
};
use nsh_core::element::input;
use nsh_core::platform::Idiom;
use nsh_core::prelude::*;

use crate::appearance::{effective_background, supports_background};
use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::shell::widgets::{Glyph, glyph, icon_button};
use crate::store::{WorkspaceStore, use_workspace};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::{ZoomController, zpx};

pub fn appearance_settings(
    theme_picker_open: Signal<bool>,
    theme_slot: Signal<ThemeMode>,
) -> impl IntoElement {
    let theme = use_theme();
    let zoom = use_context::<ZoomController>();

    view()
        .flex_col()
        .gap(zpx(24.0))
        .child(theme_settings(theme, theme_slot, theme_picker_open))
        .child(zoom_settings(zoom))
        .child(window_background_settings())
        .child(app_icon_settings())
}

#[component]
pub fn TerminalSettings() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let initial = store.term_prefs.get_untracked();
    let custom_draft = signal(initial.custom_family);
    let fallback_draft = signal(initial.fallback_families);
    let input_revision = signal(0_u64);
    let prefs = store.term_prefs;

    view()
        .flex_col()
        .child(font_selector(store.clone(), theme, prefs))
        .child(custom_font_field(
            store.clone(),
            theme,
            prefs,
            custom_draft,
            input_revision,
        ))
        .child(fallback_font_field(
            store.clone(),
            theme,
            fallback_draft,
            input_revision,
        ))
        .child(numeric_controls(store.clone(), theme, prefs, true))
        .child(numeric_controls(store.clone(), theme, prefs, false))
        .child(cursor_controls(store.clone(), theme, prefs))
        .child(preview_and_reset(
            store,
            theme,
            prefs,
            custom_draft,
            fallback_draft,
            input_revision,
        ))
}

fn theme_settings(
    theme: crate::theme::ThemeController,
    slot: Signal<ThemeMode>,
    picker_open: Signal<bool>,
) -> impl IntoElement {
    view()
        .flex_col()
        .child(dynamic(move || {
            let resolved = theme.resolved();
            let settings = theme.settings();
            let _selected_slot = slot.get();
            let ui = &resolved.ui;
            let mut modes = view()
                .flex_row()
                .gap(zpx(2.0))
                .p(zpx(2.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.app));
            for (pref, key) in [
                (ThemePref::System, "theme.system"),
                (ThemePref::Light, "theme.light"),
                (ThemePref::Dark, "theme.dark"),
            ] {
                let mode_theme = theme.clone();
                modes = modes.child(
                    view()
                        .px(zpx(10.0))
                        .py(zpx(5.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .when(settings.mode == pref, |v| v.bg(rgb_to_hsla(ui.background)))
                        .on_press(move || mode_theme.set_mode(pref))
                        .child(text(t!(key)).text_xs().text_color(rgb_to_hsla(ui.foreground))),
                );
            }
            let mut page = view()
                .flex_col()
                .child(setting_row(&resolved, t!("theme.label"), modes));
            for current_slot in [ThemeMode::Light, ThemeMode::Dark] {
                let choice = settings.slot(current_slot);
                let name = choice.name.clone();
                let selection = slot;
                let picker = picker_open;
                page = page.child(setting_row(
                    &resolved,
                    if current_slot == ThemeMode::Light {
                        t!("theme.lightTheme")
                    } else {
                        t!("theme.darkTheme")
                    },
                    view()
                        .label(format!("theme-slot-{current_slot:?}"))
                        .w(zpx(256.0))
                        .flex_row()
                        .items_center()
                        .gap(zpx(8.0))
                        .px(zpx(10.0))
                        .py(zpx(5.0))
                        .rounded(zpx(8.0))
                        .bg(rgb_to_hsla(ui.app))
                        .cursor_pointer()
                        .on_press(move || {
                            theme.preview(None);
                            if selection.get_untracked() == current_slot {
                                picker.update(|open| *open = !*open);
                            } else {
                                selection.set(current_slot);
                                picker.set(true);
                            }
                        })
                        .child(super::theme_picker::swatch(&choice.colors))
                        .child(
                            text(name)
                                .text_xs()
                                .number_of_lines(1)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                ));
            }
            // GPUI 的 theme-reset：两个槽位回 Falcon 默认，明暗模式不动；已是默认则禁用
            let reset_enabled = !is_default_theme_settings(&settings);
            page.child(
                text(if resolved.is_dark {
                    t!("native.nsh.themeDerivedDark")
                } else {
                    t!("native.nsh.themeDerivedLight")
                })
                .text_xs()
                .py(zpx(6.0))
                .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
            .child(
                view()
                    .flex_row()
                    .justify_end()
                    .pt(zpx(6.0))
                    .child(
                        view()
                            .test_id("theme-reset")
                            .px(zpx(10.0))
                            .py(zpx(6.0))
                            .rounded(zpx(8.0))
                            .bg(rgb_to_hsla(ui.app))
                            .text_color(rgb_to_hsla(if reset_enabled {
                                ui.foreground
                            } else {
                                ui.muted_foreground
                            }))
                            .when(reset_enabled, |v| {
                                v.cursor_pointer().on_press(move || {
                                    let mut next = theme.settings();
                                    *next.slot_mut(ThemeMode::Light) = choice_of(&FALCON_LIGHT);
                                    *next.slot_mut(ThemeMode::Dark) = choice_of(&FALCON_DARK);
                                    theme.apply_settings(next);
                                })
                            })
                            .child(text(t!("theme.resetThemes")).text_xs()),
                    ),
            )
            .into_element()
        }))
        .child(dynamic(move || {
            if !picker_open.get() {
                return view().into_element();
            }
            super::theme_picker::render(theme, slot.get(), picker_open).into_element()
        }))
}

fn zoom_settings(zoom: Option<ZoomController>) -> impl IntoElement {
    let theme = use_theme();
    let body = if let Some(zoom) = zoom {
        dynamic(move || {
            let resolved = theme.resolved();
            let ui = &resolved.ui;
            setting_row(
                &resolved,
                t!("native.settings.uiZoom"),
                view()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.0))
                    .child(icon_button(
                        "zoom-out",
                        t!("native.settings.uiZoomOut"),
                        Glyph::Minus,
                        false,
                        &resolved,
                        move || zoom.step_out(),
                    ))
                    .child(
                        text(zoom.label())
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(icon_button(
                        "zoom-in",
                        t!("native.settings.uiZoomIn"),
                        Glyph::Plus,
                        false,
                        &resolved,
                        move || zoom.step_in(),
                    )),
            )
            .into_element()
        })
        .into_element()
    } else {
        view().into_element()
    };
    view().child(body)
}

#[component]
fn WindowBackgroundSettings() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let requested = store.appearance.window_background;
    view().child(dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let desktop = idiom() == Idiom::Desktop;
        let selected = requested.get();
        let effective = effective_background(selected, desktop);
        let mut choices = view()
            .flex_row()
            .flex_wrap()
            .gap(zpx(4.0))
            .p(zpx(2.0))
            .rounded(zpx(8.0))
            .bg(rgb_to_hsla(ui.app));
        for (mode, key) in [
            (AppBackground::Opaque, "native.settings.backgroundOpaque"),
            (
                AppBackground::Transparent,
                "native.settings.backgroundTransparent",
            ),
            (AppBackground::Blurred, "native.settings.backgroundBlurred"),
        ] {
            let enabled = supports_background(mode, desktop);
            let store = store.clone();
            choices = choices.child(
                view()
                    .px(zpx(9.0))
                    .py(zpx(5.0))
                    .rounded(zpx(7.0))
                    .when(enabled, |v| v.cursor_pointer())
                    .when(selected == mode, |v| v.bg(rgb_to_hsla(ui.background)))
                    .text_color(rgb_to_hsla(if enabled {
                        ui.foreground
                    } else {
                        ui.muted_foreground
                    }))
                    .when(enabled, move |v| {
                        v.on_press(move || store.appearance.set_window_background(mode))
                    })
                    .child(text(t!(key)).text_xs()),
            );
        }
        let note = if effective != selected {
            t!("native.settings.backgroundUnavailable")
        } else {
            t!("native.settings.backgroundNote")
        };
        view()
            .flex_col()
            .gap(zpx(8.0))
            .child(setting_row(&resolved, t!("settings.windowBackground"), choices))
            .child(
                text(note)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
            .into_element()
    }))
}

#[component]
fn AppIconSettings() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let controller = store.appearance.app_icon.clone();
    view()
        .pt(zpx(16.0))
        .flex_col()
        .gap(zpx(12.0))
        .child(dynamic(move || {
            let resolved = theme.resolved();
            let ui = &resolved.ui;
            let selected = controller.selected.get();
            let custom_version = controller.custom_version.get();
            let custom = controller.custom_icon.get();
            let busy = controller.busy.get() || !controller.ready.get();
            let mut tiles = view().flex_row().flex_wrap().gap(zpx(8.0));
            for id in APP_ICON_IDS {
                let action = controller.clone();
                tiles = tiles.child(
                    view()
                        .w(zpx(96.0))
                        .h(zpx(112.0))
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(zpx(12.0))
                        .rounded(zpx(8.0))
                        .when(!busy, |v| v.cursor_pointer())
                        .when(selected == id, |v| v.bg(rgb_to_hsla(ui.tint)))
                        .when(!busy, move |v| v.on_press(move || action.request_select(id)))
                        .child(image(crate::appearance::app_icon::builtin_source(id)).w(zpx(52.0)).h(zpx(52.0)))
                        .child(
                            text(t!(label_key(id).as_str()))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                );
            }
            let custom_action = controller.clone();
            let custom_source = custom
                .as_ref()
                .map(crate::appearance::app_icon::custom_source)
                .unwrap_or_else(|| crate::appearance::app_icon::builtin_source(DEFAULT_APP_ICON));
            tiles = tiles.child(
                view()
                    .w(zpx(96.0))
                    .h(zpx(112.0))
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(zpx(12.0))
                    .rounded(zpx(8.0))
                    .when(!busy && custom_version.is_some(), |v| v.cursor_pointer())
                    .when(selected == CUSTOM, |v| v.bg(rgb_to_hsla(ui.tint)))
                    .when(!busy && custom_version.is_some(), move |v| {
                        v.on_press(move || custom_action.request_select(CUSTOM))
                    })
                    .child(image(custom_source).w(zpx(52.0)).h(zpx(52.0)))
                    .child(
                        text(t!("appIcon.custom"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    ),
            );
            let upload = controller.clone();
            let upload_label = if custom_version.is_some() {
                t!("appIcon.replace")
            } else {
                t!("appIcon.upload")
            };
            let mut actions = view().flex_row().flex_wrap().gap(zpx(8.0));
            actions = actions.child(action_button(upload_label, !busy, move || upload.request_upload(), &resolved));
            if custom_version.is_some() {
                let remove = controller.clone();
                actions = actions.child(action_button(
                    t!("appIcon.remove"),
                    !busy,
                    move || remove.request_remove(),
                    &resolved,
                ));
            }
            view()
                .flex_col()
                .gap(zpx(12.0))
                .child(
                    text(t!("appIcon.title"))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(tiles)
                .child(actions)
                .child(
                    text(t!("appIcon.customHint"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    text(t!("appIcon.platformNote"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    text(t!("native.settings.appIconDockNote"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .into_element()
        }))
}

fn action_button(
    label: String,
    enabled: bool,
    action: impl Fn() + 'static,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    view()
        .px(zpx(10.0))
        .py(zpx(6.0))
        .rounded(zpx(8.0))
        .bg(rgb_to_hsla(resolved.ui.app))
        .text_color(rgb_to_hsla(if enabled {
            resolved.ui.foreground
        } else {
            resolved.ui.muted_foreground
        }))
        .when(enabled, |v| v.cursor_pointer().on_press(action))
        .child(text(label).text_xs())
}

fn font_selector(
    store: WorkspaceStore,
    theme: crate::theme::ThemeController,
    prefs: Signal<falcon_core::term::TermPref>,
) -> impl IntoElement {
    view().child(dynamic(move || {
        let store = store.clone();
        let pref = prefs.get();
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let fg = rgb_to_hsla(ui.foreground);
        let bg = rgb_to_hsla(ui.app);
        let popup_bg = rgb_to_hsla(ui.popover);
        let popup_fg = rgb_to_hsla(ui.popover_foreground);
        let border = rgba_to_hsla(ui.border);
        let selected_font = pref.font_id.as_str().to_owned();
        let action_owner = crate::shell::action_owner::ActionOwner::current();
        let popup_token = action_owner.popup_token();
        let title = t!(format!("term.font_{}", pref.font_id.as_str().replace('-', "_")).as_str());
        let fonts = nsh_base::select()
            .items(
                TermFontId::ALL
                    .into_iter()
                    .map(|id| {
                        nsh_base::SelectItem::new(
                            id.as_str(),
                            t!(format!("term.font_{}", id.as_str().replace('-', "_")).as_str()),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .value(pref.font_id.as_str())
            .offset(zpx(4.0))
            .margin(zpx(8.0))
            .item_class("group focus:bg-tint focus:text-tint-foreground selected:bg-tint selected:text-tint-foreground disabled:opacity-50")
            .popup_style(move |v| {
                v.bg(popup_bg)
                    .text_color(popup_fg)
                    .rounded(zpx(8.0))
                    .border_1()
                    .border_color(border)
                    .p(zpx(4.0))
                    .shadow([crate::shell::popups::popup_shadow(border)])
            })
            .trigger(
                view()
                    .w(zpx(256.0))
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(zpx(10.0))
                    .py(zpx(5.0))
                    .rounded(zpx(8.0))
                    .border_1()
                    .border_color(border)
                    .bg(bg)
                    .text_color(fg)
                    .child(text(title).text_xs().number_of_lines(1))
                    .child(glyph(Glyph::ChevronDown)),
            )
            .render_item(move |entry: &nsh_base::SelectEntry| {
                let selected = entry.value.as_ref() == selected_font.as_str();
                let mut item = view()
                    .flex_row()
                    .items_center()
                    .gap(zpx(6.0))
                    .px(zpx(10.0))
                    .py(zpx(6.0))
                    .rounded(zpx(6.0));
                if selected {
                    item = item.child(glyph(Glyph::Check));
                }
                item.child(text(entry.title.clone()).text_xs())
            })
            .on_open_change(move |event: &nsh_base::SelectEvent| {
                if let nsh_base::SelectEvent::OpenChange(open, _) = event {
                    popup_token.set_open(*open);
                }
            })
            .on_change(move |value: &Option<SharedString>| {
                if let Some(id) = value.as_deref().and_then(TermFontId::from_wire) {
                    action_owner.run(|| store.term_prefs.update(|prefs| prefs.font_id = id));
                }
            });
        setting_row(&resolved, t!("settings.termFont"), fonts).into_element()
    }))
}

fn custom_font_field(
    store: WorkspaceStore,
    theme: crate::theme::ThemeController,
    prefs: Signal<falcon_core::term::TermPref>,
    draft: Signal<String>,
    revision: Signal<u64>,
) -> impl IntoElement {
    view().child(dynamic(move || {
        let pref = prefs.get();
        if pref.font_id != TermFontId::Custom {
            return view().into_element();
        }
        let resolved = theme.resolved();
        setting_row(
            &resolved,
            t!("settings.termCustomFont"),
            font_family_input(
                store.clone(),
                draft,
                revision,
                true,
                &resolved,
                t!("settings.termCustomFontPlaceholder"),
            ),
        )
        .into_element()
    }))
}

fn fallback_font_field(
    store: WorkspaceStore,
    theme: crate::theme::ThemeController,
    draft: Signal<String>,
    revision: Signal<u64>,
) -> impl IntoElement {
    view().child(dynamic(move || {
        let resolved = theme.resolved();
        setting_row(
            &resolved,
            t!("settings.termFallbackFont"),
            font_family_input(
                store.clone(),
                draft,
                revision,
                false,
                &resolved,
                t!("settings.termFallbackFontPlaceholder"),
            ),
        )
        .into_element()
    }))
}

fn font_family_input(
    store: WorkspaceStore,
    draft: Signal<String>,
    revision: Signal<u64>,
    custom: bool,
    resolved: &falcon_theme::ResolvedTheme,
    placeholder: String,
) -> impl IntoElement {
    let border = resolved.ui.border;
    let app = resolved.ui.app;
    let foreground = resolved.ui.foreground;
    view().child(dynamic(move || {
        let _revision = revision.get();
        let draft_change = draft;
        let blur_store = store.clone();
        let submit_store = store.clone();
        let blur_draft = draft;
        let submit_draft = draft;
        let blur_revision = revision;
        let submit_revision = revision;
        input()
            .w(zpx(256.0))
            .px(zpx(8.0))
            .py(zpx(5.0))
            .rounded(zpx(8.0))
            .border_1()
            .border_color(rgba_to_hsla(border))
            .bg(rgb_to_hsla(app))
            .text_xs()
            .text_color(rgb_to_hsla(foreground))
            .placeholder(placeholder.clone())
            .value(draft.get_untracked())
            .on_change(move |value: &str| draft_change.set(value.to_string()))
            .on_blur(move || commit_font_field(blur_store.clone(), blur_draft, blur_revision, custom))
            .on_submit(move |_: &str| {
                commit_font_field(submit_store.clone(), submit_draft, submit_revision, custom)
            })
            .into_element()
    }))
}

fn commit_font_field(
    store: WorkspaceStore,
    draft: Signal<String>,
    revision: Signal<u64>,
    custom: bool,
) {
    let raw_value = draft.get_untracked();
    let field = if custom {
        "customFamily"
    } else {
        "fallbackFamilies"
    };
    let mut raw = serde_json::Map::new();
    raw.insert(field.to_string(), serde_json::Value::String(raw_value.clone()));
    let pref = falcon_core::term::sanitize_term_pref(&serde_json::Value::Object(raw));
    let saved = if custom {
        pref.custom_family
    } else {
        pref.fallback_families
    };
    let current = store.term_prefs.get_untracked();
    let persisted_value = if custom {
        current.custom_family
    } else {
        current.fallback_families
    };
    if persisted_value != saved {
        store.term_prefs.update(|prefs| {
            if custom {
                prefs.custom_family = saved.clone();
            } else {
                prefs.fallback_families = saved.clone();
            }
        });
    }
    if raw_value != saved {
        draft.set(saved);
        revision.update(|value| *value = value.wrapping_add(1));
    }
}

fn numeric_controls(
    store: WorkspaceStore,
    theme: crate::theme::ThemeController,
    prefs: Signal<falcon_core::term::TermPref>,
    size: bool,
) -> impl IntoElement {
    let label = if size {
        "settings.termFontSize"
    } else {
        "settings.termLineHeight"
    };
    view().child(dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let fg = rgb_to_hsla(ui.foreground);
        let bg = rgb_to_hsla(ui.app);
        let border = rgba_to_hsla(ui.border);
        let min = if size { 10.0 } else { 1.0 };
        let max = if size { 24.0 } else { 1.6 };
        let step = if size { 1.0 } else { 0.05 };
        let slider_prefs = prefs;
        let store = store.clone();
        setting_row(
            &resolved,
            t!(label),
            view()
                .flex_row()
                .items_center()
                .gap(zpx(16.0))
                .child(
                    nsh_base::slider()
                        .min(min)
                        .max(max)
                        .step(step)
                        .value(move || {
                            let pref = slider_prefs.get();
                            let value = if size {
                                clamp_font_size(pref.font_size)
                            } else {
                                clamp_line_height(pref.line_height)
                            };
                            Some(nsh_base::SliderValue::Single(value))
                        })
                        .accessibility_label(t!(label))
                        .track_style(|v| v.w(zpx(112.0)).h(zpx(20.0)).justify_center())
                        .indicator_style(move |v| v.h(zpx(4.0)).rounded_full().bg(border))
                        .range_style(move |v| v.rounded_full().bg(fg))
                        .thumb_style(move |v| {
                            v.w(zpx(14.0))
                                .h(zpx(14.0))
                                .rounded_full()
                                .border_1()
                                .border_color(border)
                                .bg(bg)
                        })
                        .on_change(move |value: &nsh_base::SliderValue| {
                            if let nsh_base::SliderValue::Single(value) = value {
                                store.term_prefs.update(|prefs| {
                                    if size {
                                        prefs.font_size = clamp_font_size(*value);
                                    } else {
                                        prefs.line_height = clamp_line_height(*value);
                                    }
                                });
                            }
                        }),
                )
                .child(dynamic(move || {
                    let pref = prefs.get();
                    let value = if size {
                        clamp_font_size(pref.font_size)
                    } else {
                        clamp_line_height(pref.line_height)
                    };
                    text(if size {
                        format!("{value:.0}")
                    } else {
                        format!("{value:.2}")
                    })
                    .w(zpx(40.0))
                    .text_xs()
                    .text_color(fg)
                })),
        )
        .into_element()
    }))
}

fn cursor_controls(
    store: WorkspaceStore,
    theme: crate::theme::ThemeController,
    prefs: Signal<falcon_core::term::TermPref>,
) -> impl IntoElement {
    view().child(dynamic(move || {
        let pref = prefs.get();
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let fg = rgb_to_hsla(ui.foreground);
        let bg = rgb_to_hsla(ui.app);
        let mut cursors = view()
            .flex_row()
            .gap(zpx(4.0))
            .p(zpx(2.0))
            .rounded(zpx(8.0))
            .bg(bg);
        for (style, key) in [
            (TermCursorStyle::Block, "term.cursor_block"),
            (TermCursorStyle::Bar, "term.cursor_bar"),
            (TermCursorStyle::Underline, "term.cursor_underline"),
        ] {
            let store = store.clone();
            cursors = cursors.child(
                view()
                    .px(zpx(10.0))
                    .py(zpx(5.0))
                    .rounded(zpx(6.0))
                    .cursor_pointer()
                    .when(pref.cursor_style == style, |v| {
                        v.bg(rgb_to_hsla(ui.background))
                    })
                    .on_press(move || store.term_prefs.update(|prefs| prefs.cursor_style = style))
                    .child(text(t!(key)).text_xs().text_color(fg)),
            );
        }
        let store_blink = store.clone();
        view()
            .flex_col()
            .child(setting_row(&resolved, t!("settings.termCursor"), cursors))
            .child(setting_row(
                &resolved,
                t!("settings.termCursorBlink"),
                view()
                    .cursor_pointer()
                    .on_press(move || {
                        store_blink
                            .term_prefs
                            .update(|prefs| prefs.cursor_blink = !prefs.cursor_blink)
                    })
                    .child(crate::shell::widgets::check_indicator(pref.cursor_blink, &resolved)),
            ))
            .into_element()
    }))
}

fn preview_and_reset(
    store: WorkspaceStore,
    theme: crate::theme::ThemeController,
    prefs: Signal<falcon_core::term::TermPref>,
    custom_draft: Signal<String>,
    fallback_draft: Signal<String>,
    input_revision: Signal<u64>,
) -> impl IntoElement {
    view().child(dynamic(move || {
        let pref = prefs.get();
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let reset_store = store.clone();
        view()
            .pt(zpx(16.0))
            .flex_col()
            .gap(zpx(12.0))
            .child(
                view()
                    .flex_row()
                    .justify_between()
                    .items_center()
                    .child(
                        text(t!("settings.termPreview"))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .child(
                        view()
                            .cursor_pointer()
                            .on_press(move || {
                                let default_pref = falcon_core::term::TermPref::default();
                                if reset_store.term_prefs.get_untracked() != default_pref {
                                    reset_store.term_prefs.set(default_pref);
                                }
                                let custom_changed = !custom_draft.get_untracked().is_empty();
                                let fallback_changed = !fallback_draft.get_untracked().is_empty();
                                if custom_changed {
                                    custom_draft.set(String::new());
                                }
                                if fallback_changed {
                                    fallback_draft.set(String::new());
                                }
                                if custom_changed || fallback_changed {
                                    input_revision.update(|value| *value = value.wrapping_add(1));
                                }
                            })
                            .child(
                                text(t!("settings.termReset"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            ),
                    ),
            )
            .child(
                view()
                    .p(zpx(12.0))
                    .rounded(zpx(8.0))
                    .border_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .bg(rgb_to_hsla(resolved.terminal.background))
                    .font_family(
                        falcon_core::term::primary_family(&pref)
                            .unwrap_or_else(|| FONT_FAMILY_MONO.to_string()),
                    )
                    .child(
                        text("falcon@host ~/falcon")
                            .text_size(zpx(pref.font_size as f32))
                            .text_color(rgb_to_hsla(ui.success)),
                    )
                    .child(
                        text(t!("settings.termPreviewSample"))
                            .text_size(zpx(pref.font_size as f32))
                            .text_color(rgb_to_hsla(resolved.terminal.foreground)),
                    ),
            )
            .into_element()
    }))
}

fn setting_row(
    resolved: &falcon_theme::ResolvedTheme,
    label: String,
    control: impl IntoElement,
) -> impl IntoElement {
    view()
        .min_h(zpx(56.0))
        .flex_row()
        .items_center()
        .justify_between()
        .gap(zpx(16.0))
        .border_b_1()
        .border_color(rgba_to_hsla(resolved.ui.border))
        .child(
            text(label)
                .text_xs()
                .text_color(rgb_to_hsla(resolved.ui.foreground)),
        )
        .child(control)
}
