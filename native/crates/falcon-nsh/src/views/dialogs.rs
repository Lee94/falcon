//! 顶层模态对话框层（命令面板、服务端配置、设置 6 页签、新建项目、SSH Askpass、终止忙会话确认）。
//!
//! 严格遵循 `CONTEXT.md` 术语与 `falcon-theme` 语义取色，零硬编码中文与颜色常量。

use falcon_theme::Rgba as ThemeRgba;
use nsh_core::element::input;
use nsh_core::platform::Idiom;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::shell::widgets::{Glyph, glyph, icon_button, icon_button_with_shortcut, button_caption};
use crate::store::{
    ActiveDialog, FileGuardAction, FileGuardChoice, SettingsTab, WorkspaceStore, use_workspace,
};
use crate::terminal::bind_safe_key_when;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::views::project_form::{project_form, worktree_form};
use crate::views::profiles::{ProfileDraft, render_profiles_modal, render_profile_confirmation};
use crate::zoom::{ZoomController, zpx};

/// 顶层模态框渲染组件。
#[derive(Clone, PartialEq, Props)]
pub struct ModalLayerProps {
    #[prop(default)]
    pub test_modal_controls: bool,
    #[prop(default)]
    pub test_switch_url: Option<String>,
}

#[component]
pub fn ModalLayer(props: ModalLayerProps) -> impl IntoElement {
    let store = use_workspace();
    let project_picker = signal(false);
    provide_context(crate::views::project_form::ProjectPickerNavigation(project_picker));
    let theme_ctrl = use_theme();
    let zoom_ctrl = use_context::<ZoomController>();

    let palette_query = signal(String::new());
    let palette_selection = signal(0usize);
    let palette_key_state = std::rc::Rc::new(std::cell::RefCell::new(
        crate::views::palette::PaletteKeyState::default(),
    ));
    let palette_was_open = signal(false);
    let quick_open_query = signal(String::new());
    let quick_open_selection = signal(0usize);
    let quick_open_was_open = signal(false);
    let profile_draft = ProfileDraft::new();
    let askpass_input = signal(String::new());
    let theme_slot = signal(theme_ctrl.active_slot());
    let theme_picker_open = signal(false);
    let theme_picker = super::theme_picker::PickerState::new();
    provide_context(theme_picker);
    let previous_theme_slot = stored(theme_slot.get_untracked());
    effect(move || {
        let selected = theme_slot.get();
        if previous_theme_slot.replace(selected) != selected {
            theme_ctrl.preview(None);
            theme_picker.reset();
        }
    });
    let preview_store = store.clone();
    effect(move || {
        let appearance = preview_store.active_dialog.get()
            == ActiveDialog::Settings(SettingsTab::Appearance);
        if !appearance || !theme_picker_open.get() {
            theme_ctrl.preview(None);
            theme_picker.reset();
        }
    });
    let settings_filter = signal(String::new());
    let appearance_was_open = signal(false);
    let host_settings_was_open = signal(false);
    let modal_focus = element_ref();
    let askpass_focus = element_ref();
    let focus_store = store.clone();
    let previous_dialog = stored(ActiveDialog::None);
    effect(move || {
        let dialog = focus_store.active_dialog.get();
        let previous = previous_dialog.replace(dialog.clone());
        if matches!(dialog, ActiveDialog::Askpass { .. }) && previous != dialog {
            after_answer(move || askpass_focus.focus());
        } else if dialog != ActiveDialog::None
            && previous != ActiveDialog::None
            && previous != dialog
            && !dialog_uses_input_autofocus(&dialog)
        {
            let focus = modal_focus;
            after_answer(move || focus.focus());
        }
    });

    view().child(dynamic(move || {
        let dialog = store.active_dialog.get();
        let appearance_open = dialog == ActiveDialog::Settings(SettingsTab::Appearance);
        if appearance_was_open.get() && !appearance_open {
            theme_picker_open.set(false);
        }
        appearance_was_open.set(appearance_open);
        let host_settings_open = dialog == ActiveDialog::Settings(SettingsTab::Hosts);
        if host_settings_open && !host_settings_was_open.get() {
            store.refresh_settings_hosts();
        }
        if host_settings_was_open.get() && !host_settings_open {
            store.invalidate_host_edit_generation();
        }
        host_settings_was_open.set(host_settings_open);
        if dialog == ActiveDialog::Palette && !palette_was_open.get() {
            palette_query.set(String::new());
            palette_selection.set(0);
        }
        if dialog == ActiveDialog::QuickOpen && !quick_open_was_open.get() {
            quick_open_query.set(String::new());
            quick_open_selection.set(0);
        }
        palette_was_open.set(dialog == ActiveDialog::Palette);
        quick_open_was_open.set(dialog == ActiveDialog::QuickOpen);
        if dialog == ActiveDialog::QuickOpen
            && store.server_connected.get()
            && store.quick_open_project.get() != store.active_project_id.get()
        {
            store.load_quick_open_index(&store.active_project_id.get());
        }
        if dialog == ActiveDialog::None {
            return view().into_element();
        }

        let st_escape = store.clone();
        let escape_dialog = dialog.clone();
        bind_safe_key_when("escape", false, nsh_core::keymap::When::Always, move || {
            close_modal_dialog(&st_escape, &escape_dialog, project_picker);
        });

        // Theme previews must restyle the surface without remounting the editor inputs.
        let resolved = if appearance_open {
            untracked(|| theme_ctrl.resolved())
        } else {
            theme_ctrl.resolved()
        };
        let ui = &resolved.ui;

        // 目录选择器开着时筛选框自己 auto_focus（GPUI 打开选择器时聚焦 filter），
        // 卡片别把焦点抢回去。project_picker 必须在 on_open 触发时再读：
        // 表单挂载后才打开选择器，渲染期读到的还是旧值。
        let focus_card_on_open = !dialog_uses_input_autofocus(&dialog);
        let cancel_store = store.clone();
        let cancel_dialog = dialog.clone();
        let settings_dialog = matches!(dialog, ActiveDialog::Settings(_));
        let palette_dialog = matches!(dialog, ActiveDialog::Palette);
        let rename_dialog = matches!(dialog, ActiveDialog::RenameSession { .. });
        let preview_dialog = dialog == ActiveDialog::PreviewPopup;
        let card_body = match dialog {
            ActiveDialog::None => return view().into_element(),
            ActiveDialog::Palette => crate::views::palette::render(
                &store,
                palette_query,
                palette_selection,
                palette_key_state.clone(),
                theme_ctrl,
                zoom_ctrl,
                theme_picker_open,
                theme_slot,
            )
            .into_element(),
            ActiveDialog::QuickOpen => {
                crate::views::quick_open::render(&store, quick_open_query, quick_open_selection)
                    .into_element()
            }
            ActiveDialog::PreviewPopup => crate::views::preview_popup::preview_popup().into_element(),
            ActiveDialog::Profiles => {
                render_profiles_modal(&store, &resolved, profile_draft).into_element()
            }
            dialog @ (ActiveDialog::ConfirmDeleteProfile { .. }
            | ActiveDialog::ConfirmPlaintextProfile { .. }) => {
                render_profile_confirmation(&store, &resolved, profile_draft, dialog).into_element()
            }
            ActiveDialog::NewProject => project_form().project_id(None).into_element(),
            ActiveDialog::NewProjectPreset(preset) => project_form().project_id(None)
                .preset_local(Some(preset.local)).preset_host_id(preset.host_id).preset_multi(preset.multi).into_element(),
            ActiveDialog::ProjectActionLoading { .. } => view().p(zpx(24.0))
                .child(text(t!("native.menus.checkingWorktree")).text_sm()).into_element(),
            ActiveDialog::ConfirmProjectAction(confirmation) => crate::menu_operations::render_confirmation(&store, &resolved, confirmation),
            ActiveDialog::EditProject(id) => project_form().project_id(Some(id)).into_element(),
            ActiveDialog::DeriveProject(id) => worktree_form().project_id(id).into_element(),
            ActiveDialog::ServerUpdate(key) => super::server_update::server_update()
                .gateway(key.gateway)
                .host_id(key.host_id)
                .into_element(),
            ActiveDialog::EditRelay { id, share, connection_generation, request_generation } => super::relays::relay_editor()
                .id(id).share(share).connection_generation(connection_generation).request_generation(request_generation).into_element(),
            ActiveDialog::Settings(tab) => render_settings_modal(
                &store,
                &resolved,
                tab,
                settings_filter,
                theme_picker_open,
                theme_slot,
            )
            .into_element(),
            ActiveDialog::Askpass { id, prompt } => {
                render_askpass_modal(&store, &resolved, &id, &prompt, askpass_focus).into_element()
            }
            ActiveDialog::ConfirmAppUpdate(update) => {
                render_confirm_app_update(&store, &resolved, update).into_element()
            }
            ActiveDialog::ConfirmTerminate {
                session_id,
                foreground_cmd,
            } => render_confirm_terminate_modal(&store, &resolved, &session_id, &foreground_cmd)
                .into_element(),
            batch @ (ActiveDialog::ConfirmTerminateMany { .. }
            | ActiveDialog::ConfirmClearAllDead { .. }) => {
                crate::shell::overview::render_batch_confirmation(&store, &resolved, batch)
            }
            ActiveDialog::RenameSession { session_id, form_id } => crate::views::rename_session()
                .session_id(session_id)
                .form_id(form_id)
                .into_element(),
            ActiveDialog::ConfirmDeleteHost {
                host_id,
                host_name,
                project_count,
            } => render_confirm_delete_host_modal(
                &store,
                &resolved,
                &host_id,
                &host_name,
                project_count,
            )
            .into_element(),
            ActiveDialog::ConfirmUnsavedFiles { action, file_keys } => {
                render_unsaved_files_modal(&store, &resolved, action, file_keys).into_element()
            }
        };

        let focus_on_open = modal_focus;
        let overlay = view()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .p(zpx(16.0))
            .when(palette_dialog, |v| v.justify_start().pt(zpx(96.0)))
            .bg(move || {
                let app = theme_ctrl.resolved().ui.app;
                rgba_to_hsla(ThemeRgba { r: app.r, g: app.g, b: app.b, a: 148 })
            })
            .child(
                view()
                    .test_id("modal-layer-panel")
                    .element_ref(&modal_focus)
                    .focusable(true)
                    .w(zpx(if preview_dialog {
                        1360.0
                    } else if settings_dialog {
                        1024.0
                    } else if rename_dialog {
                        380.0
                    } else {
                        580.0
                    }))
                    .max_w(relative(1.0))
                    .when(preview_dialog, |v| v.h(zpx(860.0)))
                    .max_h(relative(0.86))
                    .flex_col()
                    .overflow_hidden()
                    .rounded(zpx(16.0))
                    .border_1()
                    .border_color(move || rgba_to_hsla(theme_ctrl.resolved().ui.border))
                    .bg(move || rgb_to_hsla(theme_ctrl.resolved().ui.background))
                    .child(card_body),
            );
        #[cfg(test)]
        let overlay = overlay.when(props.test_modal_controls, |overlay| {
            overlay.child(crate::test_modal_workspace_controls(
                store.clone(),
                props.test_switch_url.clone(),
            ))
        });
        #[cfg(not(test))]
        let _ = (&props.test_modal_controls, &props.test_switch_url);

        nsh_base::dialog()
            .animation(nsh_core::ModalAnimation::None)
            .close_on_escape(false)
            .close_on_backdrop_press(false)
            .close_on_swipe(false)
            .on_open(move || {
                if focus_card_on_open && !project_picker.get_untracked() {
                    focus_on_open.focus();
                }
            })
            .on_cancel(move || close_modal_dialog(&cancel_store, &cancel_dialog, project_picker))
            .child(overlay)
            .into_element()
    }))
}

fn dialog_uses_input_autofocus(dialog: &ActiveDialog) -> bool {
    matches!(
        dialog,
        ActiveDialog::Palette
            | ActiveDialog::QuickOpen
            | ActiveDialog::PreviewPopup
            | ActiveDialog::RenameSession { .. }
            | ActiveDialog::Askpass { .. }
            | ActiveDialog::EditRelay { .. }
    )
}

fn close_modal_dialog(store: &WorkspaceStore, dialog: &ActiveDialog, project_picker: Signal<bool>) {
    if store.active_dialog.get_untracked() != dialog.clone() {
        return;
    }
    match dialog {
        ActiveDialog::Askpass { id, .. } => store.answer_askpass(id, true),
        ActiveDialog::PreviewPopup => crate::views::preview_popup::close(store),
        ActiveDialog::ConfirmAppUpdate(_) => {
            store.active_dialog.set(ActiveDialog::Settings(SettingsTab::About));
        }
        ActiveDialog::NewProject | ActiveDialog::NewProjectPreset(_) | ActiveDialog::EditProject(_)
            if project_picker.get_untracked() => project_picker.set(false),
        ActiveDialog::EditRelay { .. } => store.active_dialog.set(ActiveDialog::Settings(SettingsTab::Relays)),
        ActiveDialog::RenameSession { session_id, form_id } => {
            store.session_operations.close_rename(store, session_id, *form_id);
        }
        ActiveDialog::ConfirmDeleteHost { .. } => {
            store.host_delete_error.set(None);
            store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Hosts));
        }
        ActiveDialog::ConfirmDeleteProfile { .. } | ActiveDialog::ConfirmPlaintextProfile { .. } => {
            store.active_dialog.set(ActiveDialog::Profiles);
        }
        _ => store.active_dialog.set(ActiveDialog::None),
    }
}

pub(super) fn modal_header(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    title: String,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let st = store.clone();
    let dialog = store.active_dialog.get_untracked();
    let project_picker = use_context::<crate::views::project_form::ProjectPickerNavigation>()
        .map(|nav| nav.0)
        .unwrap_or_else(|| signal(false));
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
        .child(icon_button_with_shortcut(
            "modal-close",
            t!("common.close"),
            Glyph::Close,
            false,
            Some("escape"),
            resolved,
            move || close_modal_dialog(&st, &dialog, project_picker),
        ))
}

fn render_unsaved_files_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    action: FileGuardAction,
    file_keys: Vec<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let saving = file_keys.iter().any(|key| {
        store
            .open_files
            .get()
            .get(key)
            .is_some_and(|buffer| buffer.saving)
    });
    let reload = matches!(action, FileGuardAction::Reload(_));
    let st_save = store.clone();
    let st_discard = store.clone();
    let st_cancel = store.clone();
    let save_label = if file_keys.len() > 1 {
        t!("native.nsh.saveAllFiles")
    } else {
        t!("native.nsh.saveFile")
    };
    let file_list = file_keys
        .iter()
        .filter_map(|key| {
            store
                .open_files
                .get()
                .get(key)
                .map(|buffer| buffer.path.clone())
        })
        .collect::<Vec<_>>()
        .join(", ");

    let mut buttons = view().flex_row().justify_end().gap(zpx(8.0)).child(
        view()
            .test_id("unsaved-cancel")
            .px(zpx(10.0))
            .py(zpx(5.0))
            .rounded(zpx(6.0))
            .cursor_pointer()
            .bg(rgb_to_hsla(ui.secondary))
            .on_press(move || st_cancel.resolve_file_guard(FileGuardChoice::Cancel))
            .child(
                view().text_color(rgb_to_hsla(ui.secondary_foreground))
                    .child(button_caption(t!("common.cancel"), "escape", resolved)),
            ),
    );

    if !saving {
        buttons = buttons.child(
            view()
                .test_id("unsaved-discard")
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.destructive))
                .on_press(move || st_discard.resolve_file_guard(FileGuardChoice::Discard))
                .child(
                    text(if reload {
                        t!("native.nsh.reloadFile")
                    } else {
                        t!("native.nsh.discardFileChanges")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive_foreground)),
                ),
        );
    }

    if !reload {
        buttons = buttons.child(
            view()
                .test_id("unsaved-save")
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.tint))
                .on_press(move || st_save.resolve_file_guard(FileGuardChoice::Save))
                .child(
                    text(save_label)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.tint_foreground)),
                ),
        );
    }

    view()
        .flex_col()
        .child(modal_header(
            store,
            resolved,
            t!("native.nsh.unsavedFilesTitle"),
        ))
        .child(
            view()
                .flex_col()
                .p(zpx(14.0))
                .gap(zpx(12.0))
                .child(
                    text(if saving {
                        t!("native.nsh.fileSaveInProgress")
                    } else if reload {
                        t!("native.nsh.confirmReloadFile")
                    } else {
                        t!("native.nsh.unsavedFilesBody")
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground)),
                )
                .when(!file_list.is_empty(), |v| {
                    v.child(
                        text(file_list)
                            .text_xs()
                            .font_family(FONT_FAMILY_MONO)
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                })
                .child(buttons),
        )
}

fn render_settings_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    active_tab: SettingsTab,
    settings_filter: Signal<String>,
    theme_picker_open: Signal<bool>,
    theme_slot: Signal<falcon_theme::ThemeMode>,
) -> impl IntoElement {
    let theme = use_theme();
    let nav_store = (*store).clone();
    let search = input()
        .w_full()
        .px(zpx(8.0))
        .py(zpx(5.0))
        .rounded(zpx(8.0))
        .bg(move || rgb_to_hsla(theme.resolved().ui.muted))
        .text_xs()
        .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground))
        .placeholder(t!("settings.search"))
        .value(settings_filter.get_untracked())
        .on_change(move |value: &str| settings_filter.set(value.to_string()));
    let nav_rows = dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let (muted, foreground, muted_foreground, tint) =
            (ui.muted, ui.foreground, ui.muted_foreground, ui.tint);
        let query = settings_filter.get().to_lowercase();
        let tabs = [
            (
                SettingsTab::Appearance,
                t!("settings.tab_appearance"),
                Glyph::Settings,
            ),
            (
                SettingsTab::Notifications,
                t!("settings.tab_notifications"),
                Glyph::Activity,
            ),
            (SettingsTab::Plugins, t!("settings.tab_plugins"), Glyph::Box),
            (SettingsTab::About, t!("settings.tab_about"), Glyph::Monitor),
            (SettingsTab::Hosts, t!("settings.tab_hosts"), Glyph::Server),
            (
                SettingsTab::Relays,
                t!("settings.tab_relays"),
                Glyph::Terminal,
            ),
        ];
        let mut rows = view().flex_col().gap(zpx(2.0));
        for (tab, label, icon) in tabs {
            if !query.is_empty() && !label.to_lowercase().contains(&query) {
                continue;
            }
            if query.is_empty() && (tab == SettingsTab::Appearance || tab == SettingsTab::Hosts) {
                rows = rows.child(
                    text(if tab == SettingsTab::Hosts {
                        t!("settings.group_connection")
                    } else {
                        t!("settings.group_general")
                    })
                    .text_xs()
                    .pt(zpx(20.0))
                    .pb(zpx(6.0))
                    .px(zpx(8.0))
                    .text_color(rgb_to_hsla(muted_foreground)),
                );
            }
            let selected = tab == active_tab;
            let st = nav_store.clone();
            rows = rows.child(
                view()
                    .label(format!("settings-tab-{tab:?}"))
                    .h(zpx(32.0))
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.0))
                    .px(zpx(8.0))
                    .rounded(zpx(8.0))
                    .cursor_pointer()
                    .text_color(rgb_to_hsla(if selected {
                        foreground
                    } else {
                        muted_foreground
                    }))
                    .when(selected, |v| v.bg(rgb_to_hsla(tint)))
                    .hover(|style| style.bg(rgb_to_hsla(muted)))
                    .on_press(move || st.active_dialog.set(ActiveDialog::Settings(tab)))
                    .child(glyph(icon))
                    .child(text(label).text_xs()),
            );
        }
        rows.into_element()
    });
    let nav = view()
        .flex_col()
        .w(zpx(224.0))
        .flex_none()
        .when(idiom() != Idiom::Phone, |v| v.h(zpx(680.0)))
        .gap(zpx(2.0))
        .p(zpx(12.0))
        .bg(move || rgb_to_hsla(theme.resolved().ui.app))
        .when(idiom() == Idiom::Phone, |v| {
            v.w_full().flex_row().flex_wrap()
        })
        .child(search)
        .child(nav_rows);
    let body = match active_tab {
        SettingsTab::Appearance => view()
            .flex_col()
            .gap(zpx(24.0))
            .child(crate::views::appearance_settings(
                theme_picker_open,
                theme_slot,
            ))
            .child(crate::views::terminal_settings())
            .into_element(),
        SettingsTab::Terminal => crate::views::terminal_settings().into_element(),
        SettingsTab::Notifications => render_notifications_tab(store, resolved).into_element(),
        SettingsTab::Hosts => render_hosts_tab(store, resolved).into_element(),
        SettingsTab::Relays => super::relays::relays_pane().into_element(),
        SettingsTab::Plugins => render_plugins_tab(store, resolved).into_element(),
        SettingsTab::About => render_about_tab(store, resolved).into_element(),
    };

    let title = match active_tab {
        SettingsTab::Appearance => t!("settings.tab_appearance"),
        SettingsTab::Terminal => t!("settings.terminalTitle"),
        SettingsTab::Notifications => t!("settings.soundTitle"),
        SettingsTab::Hosts => t!("settings.tab_hosts"),
        SettingsTab::Relays => t!("settings.tab_relays"),
        SettingsTab::Plugins => t!("settings.tab_plugins"),
        SettingsTab::About => t!("settings.tab_about"),
    };
    let close = store.clone();
    view()
        .h(zpx(680.0))
        .max_h(relative(0.86))
        .flex_row()
        .relative()
        .when(idiom() == Idiom::Phone, |v| v.flex_col())
        .child(nav)
        .child(
            scroll().flex_1().min_w_0().child(
                view()
                    .p(zpx(32.0))
                    .flex_col()
                    .gap(zpx(24.0))
                    .child(
                        text(title)
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(move || rgb_to_hsla(theme.resolved().ui.foreground)),
                    )
                    .child(body),
            ),
        )
        .child(
            view()
                .absolute()
                .top(zpx(6.0))
                .right(zpx(6.0))
                .child(dynamic(move || icon_button(
                    "settings-close",
                    t!("common.close"),
                    Glyph::Close,
                    false,
                    &theme.resolved(),
                    {
                        let close = close.clone();
                        move || close.active_dialog.set(ActiveDialog::None)
                    },
                ).into_element())),
        )
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

fn render_hosts_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let row_store = store.clone();
    let row_theme = resolved.clone();
    let table = dynamic(move || {
        let store = row_store.clone();
        let resolved = row_theme.clone();
        let ui = &resolved.ui;
        let hosts = store.hosts.get();
        let testing = store.host_test_busy.get();
        if hosts.is_empty() {
            return view().py(zpx(24.0)).child(
                text(t!("native.settingsParity.noHosts")).text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            ).into_element();
        }
        let mut table = view()
            .flex_col()
            .p(zpx(8.0))
            .rounded(zpx(12.0))
            .bg(rgb_to_hsla(ui.app))
            .child(
                view()
                    .h(zpx(32.0))
                    .flex_row()
                    .gap(zpx(12.0))
                    .child(
                        text(t!("host.name"))
                            .w(zpx(136.0))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .child(
                        text(t!("native.nsh.hostConnection"))
                            .flex_1()
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .child(
                        text(t!("project.authMethod"))
                            .w(zpx(160.0))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                    ),
            );
        for host in hosts {
            let st = store.clone();
            let id = host.id.clone();
            let is_testing = testing.as_deref() == Some(id.as_str());
            let host_source = host.source.clone();
            let auth = host
                .source
                .as_ref()
                .map(|h| h.auth_method.as_str())
                .unwrap_or("agent");
            let mut row = view()
                .min_h(zpx(40.0))
                .flex_row()
                .items_center()
                .gap(zpx(12.0))
                .border_t_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    text(host.name)
                        .w(zpx(136.0))
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(host.destination)
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    view().w(zpx(100.0)).flex_col().gap(zpx(2.0))
                    .child(text(match auth {
                        "key" => t!("project.authKey"),
                        "password" => t!("project.authPassword"),
                        _ => t!("project.authAgent"),
                    })
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)))
                    .when_some(host_source.as_ref(), |v, source| v.child(
                        text(if source.project_count > 0 {
                            t!("native.settingsParity.usedBy", n = source.project_count.to_string().as_str())
                        } else { t!("native.settingsParity.unused") })
                        .text_xs().text_color(rgb_to_hsla(ui.muted_foreground)),
                    )),
                )
                .child(
                    view()
                        .label(format!("host-test-{id}"))
                        .px(zpx(8.0))
                        .py(zpx(4.0))
                        .rounded(zpx(8.0))
                        .when(testing.is_none(), |v| v.cursor_pointer())
                        .bg(rgb_to_hsla(ui.background))
                        .when(testing.is_none(), |v| v.on_press(move || st.test_host_connection(&id)))
                        .child(
                            text(if is_testing { t!("host.testing") } else { t!("host.test") })
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                );
            if let Some(source) = host_source {
                let update_store = store.clone();
                let update_id = source.id.clone();
                row = row.child(super::server_update::action(
                    &format!("host-update-{}", source.id),
                    t!("native.serverUpdate.node"),
                    false,
                    &resolved,
                    move || update_store.server_updates.open(&update_store, Some(update_id.clone())),
                ));
                let edit_store = store.clone();
                let delete_store = store.clone();
                let edit_source = source.clone();
                let delete_id = source.id.clone();
                let delete_name = source.name.clone();
                let project_count = source.project_count;
                row = row
                    .child(
                        view()
                            .label(format!("host-edit-{}", source.id))
                            .px(zpx(8.0))
                            .py(zpx(4.0))
                            .rounded(zpx(8.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.background))
                            .on_press(move || {
                                edit_store.begin_host_edit(edit_source.clone());
                                edit_store
                                    .active_dialog
                                    .set(ActiveDialog::Settings(SettingsTab::Hosts));
                            })
                            .child(
                                text(t!("host.edit"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            ),
                    )
                    .child(
                        view()
                            .label(format!("host-delete-{}", source.id))
                            .px(zpx(8.0))
                            .py(zpx(4.0))
                            .rounded(zpx(8.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.background))
                            .on_press(move || {
                                delete_store.host_delete_error.set(None);
                                delete_store
                                    .active_dialog
                                    .set(ActiveDialog::ConfirmDeleteHost {
                                        host_id: delete_id.clone(),
                                        host_name: delete_name.clone(),
                                        project_count,
                                    });
                            })
                            .child(
                                text(t!("host.delete"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.destructive)),
                            ),
                    );
            }
            let menu_id = host.id.clone();
            row = row.child(crate::shell::popups::overflow_menu(
                crate::shell::popups::menu_trigger(
                    format!("host-settings-menu-{menu_id}"),
                    t!("session.moreActions"),
                    &resolved,
                ),
                crate::shell::menus::host_menu_actions(&store, Some(&menu_id)),
                &resolved,
            ));
            table = table.child(crate::shell::popups::context_menu(
                row,
                crate::shell::menus::host_menu_actions(&store, Some(&menu_id)),
                &resolved,
            ));
        }
        table.into_element()
    });
    let editing = store.editing_host.get();
    let editing_generation = store.editing_host_generation.get();
    let editing_id = editing.as_ref().map(|host| host.id.clone());
    let form_key = editing_id.as_ref().map_or_else(
        || "host-form-create".to_string(),
        |id| format!("host-form-{id}-{editing_generation}"),
    );
    view().flex_col().gap(zpx(12.0)).child(table).child(
        super::host_form::host_form()
            .editing_id(editing_id)
            .key(form_key),
    )
}
fn render_confirm_delete_host_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    host_id: &str,
    host_name: &str,
    project_count: u32,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let busy_target = store.host_delete_busy.get();
    let busy = busy_target.is_some();
    let deleting = busy_target
        .as_ref()
        .is_some_and(|(_, id)| id == host_id);
    let error = store.host_delete_error.get();
    let cancel = store.clone();
    let delete = store.clone();
    let id = host_id.to_string();
    let title = t!("host.deleteTitle", name = host_name);
    let mut confirm = view()
        .test_id("host-delete-confirm")
        .px(zpx(12.0))
        .py(zpx(7.0))
        .rounded(zpx(8.0))
        .bg(rgb_to_hsla(ui.destructive));
    if !busy {
        confirm = confirm.cursor_pointer().on_press(move || delete.delete_ssh_host(&id));
    }
    view()
        .test_id("host-delete-confirmation")
        .flex_col()
        .gap(zpx(12.0))
        .p(zpx(20.0))
        .child(
            text(title)
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            text(t!("host.deleteBody"))
                .text_xs()
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .when(project_count > 0, |view| {
            view.child(
                text(t!("host.deleteInUse", n = project_count.to_string().as_str()))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.warning)),
            )
        })
        .when_some(error, |view, error| {
            view.child(
                text(error)
                    .test_id("host-delete-error")
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.destructive)),
            )
        })
        .child(
            view().flex_row().justify_end().gap(zpx(8.0))
                .child(
                    view()
                        .test_id("host-delete-cancel")
                        .px(zpx(12.0))
                        .py(zpx(7.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.secondary))
                        .on_press(move || {
                            cancel.host_delete_error.set(None);
                            cancel.active_dialog.set(ActiveDialog::Settings(SettingsTab::Hosts));
                        })
                        .child(
                            text(t!("common.cancel"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.secondary_foreground)),
                        ),
                )
                .child(confirm.child(
                    text(if deleting {
                        t!("host.deleting")
                    } else {
                        t!("host.delete")
                    })
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.destructive_foreground)),
                )),
        )
}

fn render_plugins_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let list = store.plugin_list.get();
    let reload = store.clone();
    let copy = store.clone();
    let dir = list.dir.clone();
    let no_custom = !list.plugins.iter().any(|p| p.kind != falcon_proto::PluginKind::Builtin);
    view()
        .flex_col()
        .gap(zpx(8.0))
        .child(setting_row(
            resolved,
            t!("plugin.dir"),
            view()
                .flex_row()
                .items_center()
                .gap(zpx(8.0))
                .child(
                    text(list.dir.clone())
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(icon_button(
                    "copy-plugins-dir", t!("native.settingsParity.copyDir"), Glyph::Copy,
                    false, resolved, move || {
                        if !dir.is_empty() {
                            set_clipboard(dir.clone());
                            copy.show_toast(t!("native.settingsParity.copied"));
                        }
                    },
                ))
                .child(icon_button(
                    "reload-plugins",
                    t!("plugin.rescan"),
                    Glyph::Refresh,
                    false,
                    resolved,
                    move || reload.reload_plugins(),
                )),
        ))
        .children(list.plugins.into_iter().map(|plugin| {
            let st = store.clone();
            let id = plugin.id.clone();
            let unavailable = plugin.error.is_some();
            let kind = match plugin.kind {
                falcon_proto::PluginKind::Builtin => t!("native.settingsParity.kindBuiltin"),
                falcon_proto::PluginKind::Package => t!("native.settingsParity.kindPackage"),
                _ => t!("native.settingsParity.kindCommand"),
            };
            view()
                .test_id(&format!("settings-plugin-{}", plugin.id))
                .min_h(zpx(68.0))
                .flex_row()
                .items_center()
                .gap(zpx(16.0))
                .border_b_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    view()
                        .flex_1()
                        .min_w_0()
                        .flex_col()
                        .gap(zpx(6.0))
                        .child(
                            view().flex_row().items_center().gap(zpx(8.0))
                            .child(view().text_color(rgb_to_hsla(ui.muted_foreground)).child(glyph(Glyph::Box)))
                            .child(text(plugin.name.clone())
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.foreground)))
                            .child(text(kind).text_xs().text_color(rgb_to_hsla(ui.muted_foreground))),
                        )
                        .when_some(plugin.description, |v, description| {
                            v.child(
                                text(description)
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                        })
                        .when_some(plugin.dir, |v, dir| v.child(text(dir).text_xs()
                            .font_family(crate::fonts::FONT_FAMILY_MONO)
                            .text_color(rgb_to_hsla(ui.muted_foreground))))
                        .child(text(match plugin.refresh {
                            Some(seconds) => t!("native.settingsParity.pluginRefresh", n = seconds.to_string().as_str()),
                            None => t!("native.settingsParity.pluginManualRefresh"),
                        }).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)))
                        .when_some(plugin.error, |v, error| v.child(text(error).text_xs()
                            .text_color(rgb_to_hsla(ui.destructive)))),
                )
                .child(
                    view().test_id(&format!("settings-plugin-toggle-{}", plugin.id)).child(nsh_base::switch()
                        .accessibility_label(plugin.name)
                        .disabled(unavailable)
                        .w(zpx(36.0))
                        .h(zpx(20.0))
                        .rounded_full()
                        .bg(rgb_to_hsla(if plugin.enabled && !unavailable {
                            ui.primary
                        } else {
                            ui.muted
                        }))
                        .thumb_class("size-4 rounded-full bg-background checked:translate-x-4")
                        .checked(plugin.enabled && !unavailable)
                        .on_change(move |enabled: &bool| st.set_plugin_enabled(&id, *enabled))),
                )
        }))
        .when(no_custom, |v| v.child(text(t!("native.settingsParity.noCustom"))
            .text_xs().text_color(rgb_to_hsla(ui.muted_foreground))))
}

fn render_about_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let info = store.server_info.get();
    let update_store = store.clone();
    let value = |value: String| {
        text(value)
            .text_xs()
            .number_of_lines(1)
            .text_color(rgb_to_hsla(ui.muted_foreground))
    };
    view()
        .flex_col()
        .child(setting_row(
            resolved,
            t!("overview.version"),
            value(
                info.as_ref()
                    .map(|i| format!("v{}", i.version))
                    .unwrap_or_else(|| "—".into()),
            ),
        ))
        .child(setting_row(
            resolved,
            t!("overview.platform"),
            value(info.map(|i| i.platform).unwrap_or_else(|| "—".into())),
        ))
        .child(setting_row(
            resolved,
            t!("native.settings.serverUrl"),
            value(store.server_url.get()),
        ))
        .child(setting_row(
            resolved,
            t!("native.settings.clientVersion"),
            value(format!("v{}", env!("CARGO_PKG_VERSION"))),
        ))
        .child(setting_row(
            resolved,
            t!("native.serverUpdate.title"),
            super::server_update::action(
                "server-update-open",
                t!("native.serverUpdate.open"),
                false,
                resolved,
                move || update_store.server_updates.open(&update_store, None),
            ),
        ))
        .child(crate::app_update::render(store, resolved))
}

fn render_confirm_app_update(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    update: falcon_platform::AvailableUpdate,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let st_cancel = store.clone();
    let st_install = store.clone();
    let st_close = store.clone();
    view().flex_col()
        .child(view().flex_row().items_center().justify_between().p(zpx(14.0))
            .child(text(t!("native.update.confirmTitle", version = update.version.as_str())).text_sm().text_color(rgb_to_hsla(ui.foreground)))
            .child(icon_button("app-update-close", t!("common.close"), Glyph::Close, false, resolved,
                move || st_close.active_dialog.set(ActiveDialog::Settings(SettingsTab::About)))))
        .child(view().flex_col().p(zpx(14.0)).gap(zpx(12.0))
            .child(text(if update.installs_local_service {
                t!("native.update.confirmBody", asset = update.asset_name.as_str())
            } else {
                t!("native.update.confirmBodyDownload", asset = update.asset_name.as_str())
            }).text_xs().text_color(rgb_to_hsla(ui.foreground)))
            .child(view().flex_row().justify_end().gap(zpx(8.0))
                .child(view().test_id("app-update-cancel").px(zpx(10.0)).py(zpx(5.0)).rounded(zpx(6.0))
                    .bg(rgb_to_hsla(ui.secondary)).cursor_pointer()
                    .on_press(move || st_cancel.active_dialog.set(ActiveDialog::Settings(SettingsTab::About)))
                    .child(text(t!("common.cancel")).text_xs().text_color(rgb_to_hsla(ui.secondary_foreground))))
                .child(view().test_id("app-update-install").px(zpx(10.0)).py(zpx(5.0)).rounded(zpx(6.0))
                    .bg(rgb_to_hsla(ui.tint)).cursor_pointer()
                    .on_press(move || st_install.install_app_update(update.clone()))
                    .child(text(t!("native.update.confirmOk")).text_xs().text_color(rgb_to_hsla(ui.tint_foreground))))))
}

fn render_askpass_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    id: &str,
    prompt: &str,
    focus: nsh_core::element::ElementRef,
) -> impl IntoElement {
    let secret = store.askpass.secret;
    let ui = &resolved.ui;
    let st_ok = store.clone();
    let st_enter = store.clone();
    let st_cancel = store.clone();
    let id_ok = id.to_string();
    let id_enter = id.to_string();
    let id_cancel = id.to_string();
    let busy = store.askpass.busy.get();
    view()
        .flex_col()
        .child(modal_header(store, resolved, t!("native.askpass.title")))
        .child(
            view()
                .flex_col()
                .p(zpx(14.0))
                .gap(zpx(10.0))
                .child(
                    text(prompt.to_string())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    input()
                        .key(id.to_string())
                        .test_id("askpass-input")
                        .element_ref(&focus)
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .masked(true)
                        .auto_focus(true)
                        .readonly(busy)
                        .bg(rgba_to_hsla(ui.input))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder(t!("native.askpass.placeholder"))
                        .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                        .value(move || secret.get())
                        .on_change(move |v: &str| secret.set(v.to_string()))
                        .on_submit(move |_: &str| st_enter.answer_askpass(&id_enter, false)),
                )
                .when_some(store.askpass.error.get(), |v, error| {
                    v.child(text(error).text_xs().text_color(rgb_to_hsla(ui.destructive)))
                })
                .child(
                    view()
                        .test_id("askpass-cancel")
                        .items_center()
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.secondary))
                        .on_press(move || st_cancel.answer_askpass(&id_cancel, true))
                        .child(
                            text(t!("native.askpass.cancel"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                )
                .child(
                    view()
                        .test_id("askpass-submit")
                        .items_center()
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint_strong))
                        .on_press(move || st_ok.answer_askpass(&id_ok, false))
                        .child(
                            text(t!(if busy {
                                "native.askpass.sending"
                            } else {
                                "native.askpass.submit"
                            }))
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.primary_foreground)),
                        ),
                ),
        )
}

/// 忙前台进程终止护栏（区分「仍然终止」销毁进程 vs 「改为收起窗口」Detach 保留会话）。
fn render_confirm_terminate_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    session_id: &str,
    foreground_cmd: &str,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let st_kill = store.clone();
    let sid_kill = session_id.to_string();
    let st_detach = store.clone();
    let sid_detach = session_id.to_string();
    let st_cancel = store.clone();

    view()
        .flex_col()
        .child(modal_header(
            store,
            resolved,
            t!("native.nsh.terminateBusyTitle"),
        ))
        .child(
            view()
                .flex_col()
                .p(zpx(14.0))
                .gap(zpx(12.0))
                .child(
                    text(t!("native.nsh.terminateBusyBody", cmd = foreground_cmd))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    view()
                        .flex_row()
                        .justify_end()
                        .gap(zpx(8.0))
                        .child(
                            view()
                                .px(zpx(10.0))
                                .py(zpx(5.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.secondary))
                                .on_press(move || st_cancel.active_dialog.set(ActiveDialog::None))
                                .child(
                                    text(t!("common.cancel"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                ),
                        )
                        .child(
                            view()
                                .px(zpx(10.0))
                                .py(zpx(5.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.tint))
                                .on_press(move || {
                                    let key = falcon_core::pane_key::term_key(&sid_detach);
                                    st_detach.detach_pane(&key);
                                    st_detach.active_dialog.set(ActiveDialog::None);
                                })
                                .child(
                                    text(t!("native.nsh.detachInstead"))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb_to_hsla(ui.tint_foreground)),
                                ),
                        )
                        .child(
                            view()
                                .px(zpx(10.0))
                                .py(zpx(5.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.destructive))
                                .on_press(move || {
                                    st_kill.force_terminate_session(&sid_kill);
                                })
                                .child(
                                    text(t!("native.nsh.terminateConfirm"))
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb_to_hsla(ui.destructive_foreground)),
                                ),
                        ),
                ),
        )
}

fn render_notifications_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let enabled = store.finish_sound.get();
    let toggle = store.finish_sound;
    view()
        .flex_col()
        .gap(zpx(16.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .gap(zpx(12.0))
                .child(
                    text(t!("settings.finishSound"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    nsh_base::switch()
                        .w(zpx(36.0))
                        .h(zpx(20.0))
                        .rounded_full()
                        .bg(rgb_to_hsla(if enabled { ui.primary } else { ui.muted }))
                        .thumb_class("size-4 rounded-full bg-background checked:translate-x-4")
                        .checked(enabled)
                        .on_change(move |on: &bool| {
                            let on = *on;
                            toggle.set(on);
                            crate::theme::save_pref_item(
                                "falcon.finishSound",
                                Some(if on { "on" } else { "off" }),
                            );
                        }),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text(t!("settings.finishSoundPreviewLabel"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    view()
                        .px(zpx(12.0))
                        .py(zpx(6.0))
                        .rounded(zpx(8.0))
                        .bg(rgb_to_hsla(ui.secondary))
                        .cursor_pointer()
                        .on_press(|| falcon_platform::shared().play_chime())
                        .child(
                            text(t!("settings.finishSoundPreview"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                ),
        )
}
