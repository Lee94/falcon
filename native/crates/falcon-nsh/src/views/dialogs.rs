//! 顶层模态对话框层（命令面板、服务端配置、设置 6 页签、新建项目、SSH Askpass、终止忙会话确认）。
//!
//! 严格遵循 `CONTEXT.md` 术语与 `falcon-theme` 语义取色，零硬编码中文与颜色常量。

use falcon_core::profiles::{LOCAL_PROFILE_ID, ServerProfile, new_profile_id};
use falcon_core::term::{TermCursorStyle, TermFontId};
use falcon_proto::PluginKind;
use falcon_theme::{Rgba as ThemeRgba, ThemeMode, ThemePref};
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{
    ActiveDialog, ProjectRecord, RightPanelKind, SettingsTab, WorkspaceStore, use_workspace,
};
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::{ZoomController, zpx};

/// 顶层模态框渲染组件。
#[component]
pub fn ModalLayer() -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let zoom_ctrl = use_context::<ZoomController>();

    let palette_query = signal(String::new());
    let profile_name = signal(String::new());
    let profile_url = signal(String::new());
    let new_proj_name = signal(String::new());
    let new_proj_path = signal(String::new());
    let askpass_input = signal(String::new());
    let theme_filter = signal(String::new());

    view().child(dynamic(move || {
        let dialog = store.active_dialog.get();
        if dialog == ActiveDialog::None {
            return view().into_element();
        }

        let resolved = theme_ctrl.resolved();
        let ui = &resolved.ui;

        let card_body = match dialog {
            ActiveDialog::None => return view().into_element(),
            ActiveDialog::Palette => {
                render_palette_modal(&store, &resolved, palette_query).into_element()
            }
            ActiveDialog::Profiles => render_profiles_modal(
                &store,
                &resolved,
                profile_name,
                profile_url,
            )
            .into_element(),
            ActiveDialog::NewProject => render_new_project_modal(
                &store,
                &resolved,
                new_proj_name,
                new_proj_path,
            )
            .into_element(),
            ActiveDialog::Settings(tab) => render_settings_modal(
                &store,
                theme_ctrl,
                zoom_ctrl,
                &resolved,
                tab,
                theme_filter,
            )
            .into_element(),
            ActiveDialog::Askpass { id, prompt } => {
                render_askpass_modal(&store, &resolved, &id, &prompt, askpass_input).into_element()
            }
            ActiveDialog::ConfirmTerminate {
                session_id,
                foreground_cmd,
            } => render_confirm_terminate_modal(&store, &resolved, &session_id, &foreground_cmd)
                .into_element(),
        };

        let scrim = rgba_to_hsla(ThemeRgba {
            r: ui.app.r,
            g: ui.app.g,
            b: ui.app.b,
            a: 148,
        });

        view()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .p(zpx(16.0))
            .bg(scrim)
            .child(
                view()
                    .w(zpx(580.0))
                    .max_w(relative(1.0))
                    .max_h(relative(0.86))
                    .flex_col()
                    .overflow_hidden()
                    .rounded(zpx(12.0))
                    .border_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .bg(rgb_to_hsla(ui.popover))
                    .child(card_body),
            )
            .into_element()
    }))
}

fn modal_header(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    title: String,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let st = store.clone();
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
        .child(
            view()
                .px(zpx(8.0))
                .py(zpx(3.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.secondary))
                .on_press(move || st.active_dialog.set(ActiveDialog::None))
                .child(
                    text(t!("common.close"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                ),
        )
}

fn render_palette_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    query: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let q = query.get().trim().to_ascii_lowercase();
    let sessions = store.sessions.get();
    let projects = store.projects.get();

    let mut items = view().flex_col().p(zpx(10.0)).gap(zpx(4.0));

    // 会话快速跳转与后台持久会话接回
    items = items.child(
        text(t!("palette.groupSessions"))
            .text_xs()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb_to_hsla(ui.muted_foreground)),
    );
    for s in sessions {
        let title = s.display_title();
        if !q.is_empty() && !title.to_ascii_lowercase().contains(&q) {
            continue;
        }
        let attached = store.is_session_attached(&s.id);
        let st = store.clone();
        let sid = s.id.clone();
        let right_label = if attached {
            t!("session.open")
        } else {
            t!("native.nsh.reattachSession")
        };
        items = items.child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(10.0))
                .py(zpx(6.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.card))
                .on_press(move || {
                    st.focus_session(&sid);
                    st.active_dialog.set(ActiveDialog::None);
                })
                .child(
                    text(title)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(right_label)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.tint_strong)),
                ),
        );
    }

    // 项目切换
    items = items.child(
        view()
            .pt(zpx(6.0))
            .child(
                text(t!("palette.groupProjects"))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            ),
    );
    for p in projects {
        if !q.is_empty() && !p.name.to_ascii_lowercase().contains(&q) {
            continue;
        }
        let st = store.clone();
        let pid = p.id.clone();
        items = items.child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(10.0))
                .py(zpx(6.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.card))
                .on_press(move || {
                    st.active_project_id.set(pid.clone());
                    st.active_dialog.set(ActiveDialog::None);
                })
                .child(
                    text(p.name)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(p.path)
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                ),
        );
    }

    // 全局操作
    items = items.child(
        view()
            .pt(zpx(6.0))
            .child(
                text(t!("palette.groupActions"))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            ),
    );

    let quick_actions: Vec<(String, Box<dyn Fn()>)> = vec![
        (t!("sidebar.newTerminal"), {
            let st = store.clone();
            Box::new(move || {
                st.create_terminal_in_active_project();
                st.active_dialog.set(ActiveDialog::None);
            })
        }),
        (t!("palette.openSettings"), {
            let st = store.clone();
            Box::new(move || {
                st.active_dialog
                    .set(ActiveDialog::Settings(SettingsTab::Appearance));
            })
        }),
        (t!("palette.openRelays"), {
            let st = store.clone();
            Box::new(move || {
                st.active_dialog
                    .set(ActiveDialog::Settings(SettingsTab::Relays));
            })
        }),
        (t!("native.menu.files"), {
            let st = store.clone();
            Box::new(move || {
                st.right_panel_open.set(true);
                st.right_panel_kind.set(RightPanelKind::Files);
                st.active_dialog.set(ActiveDialog::None);
            })
        }),
        (t!("native.menu.changes"), {
            let st = store.clone();
            Box::new(move || {
                st.right_panel_open.set(true);
                st.right_panel_kind.set(RightPanelKind::Git);
                st.active_dialog.set(ActiveDialog::None);
            })
        }),
    ];

    for (label, cb) in quick_actions {
        if !q.is_empty() && !label.to_ascii_lowercase().contains(&q) {
            continue;
        }
        items = items.child(
            view()
                .px(zpx(10.0))
                .py(zpx(6.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(rgb_to_hsla(ui.card))
                .on_press(cb)
                .child(
                    text(label)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                ),
        );
    }

    view()
        .flex_col()
        .max_h(zpx(460.0))
        .child(modal_header(store, resolved, t!("palette.title")))
        .child(
            view().p(zpx(10.0)).child(
                input()
                    .w_full()
                    .px(zpx(10.0))
                    .py(zpx(6.0))
                    .rounded(zpx(6.0))
                    .bg(rgba_to_hsla(ui.input))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.foreground))
                    .placeholder(t!("palette.placeholder"))
                    .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                    .value(query.get())
                    .on_change(move |v: &str| query.set(v.to_string())),
            ),
        )
        .child(scroll().flex_1().child(items))
}

fn render_profiles_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    name_draft: Signal<String>,
    url_draft: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let profiles_file = store.profiles.get();
    let current_url = store.server_url.get();
    let draft_url = url_draft.get();
    let draft_insecure = ServerProfile {
        id: "draft".into(),
        name: String::new(),
        url: draft_url.clone(),
        plaintext_ok: false,
    }
    .is_insecure();

    let st_add = store.clone();
    view()
        .flex_col()
        .child(modal_header(store, resolved, t!("native.connect.add")))
        .child(
            view()
                .flex_col()
                .p(zpx(14.0))
                .gap(zpx(10.0))
                .children(profiles_file.profiles.into_iter().map(|prof| {
                    let active = prof.url == current_url;
                    let title = if prof.id == LOCAL_PROFILE_ID && prof.name.is_empty() {
                        t!("native.server.local")
                    } else if prof.name.is_empty() {
                        prof.url.clone()
                    } else {
                        prof.name.clone()
                    };
                    let st_switch = store.clone();
                    let next_url = prof.url.clone();
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .p(zpx(8.0))
                        .rounded(zpx(8.0))
                        .border_1()
                        .border_color(rgba_to_hsla(ui.border))
                        .bg(if active {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.card)
                        })
                        .cursor_pointer()
                        .on_press(move || {
                            st_switch.connect_live_server(&next_url);
                            st_switch.active_dialog.set(ActiveDialog::None);
                        })
                        .child(
                            view()
                                .flex_col()
                                .child(
                                    text(title)
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                                .child(
                                    text(prof.url)
                                        .text_xs()
                                        .font_family(FONT_FAMILY_MONO)
                                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                                ),
                        )
                }))
                .child(
                    text(t!("native.connect.hint"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .bg(rgba_to_hsla(ui.input))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder(t!("native.connect.namePlaceholder"))
                        .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                        .value(name_draft.get())
                        .on_change(move |v: &str| name_draft.set(v.to_string())),
                )
                .child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .bg(rgba_to_hsla(ui.input))
                        .font_family(FONT_FAMILY_MONO)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder("https://falcon.example.com")
                        .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                        .value(draft_url.clone())
                        .on_change(move |v: &str| url_draft.set(v.to_string())),
                )
                .when(draft_insecure, |el| {
                    el.child(
                        view()
                            .p(zpx(8.0))
                            .rounded(zpx(6.0))
                            .bg(rgb_to_hsla(ui.muted))
                            .child(
                                text(t!("native.server.plaintext"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.warning)),
                            ),
                    )
                })
                .child(
                    view()
                        .items_center()
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint_strong))
                        .on_press(move || {
                            let u = url_draft.get().trim().to_string();
                            if u.starts_with("http://") || u.starts_with("https://") {
                                let p = ServerProfile {
                                    id: new_profile_id(),
                                    name: name_draft.get().trim().to_string(),
                                    url: u.clone(),
                                    plaintext_ok: true,
                                };
                                st_add.profiles.update(|f| f.upsert(p));
                                st_add.connect_live_server(&u);
                                name_draft.set(String::new());
                                url_draft.set(String::new());
                                st_add.active_dialog.set(ActiveDialog::None);
                            }
                        })
                        .child(
                            text(t!("native.connect.connect"))
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.primary_foreground)),
                        ),
                ),
        )
}

fn render_new_project_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    name_draft: Signal<String>,
    path_draft: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let st_create = store.clone();

    view()
        .flex_col()
        .child(modal_header(store, resolved, t!("project.createTitle")))
        .child(
            view()
                .flex_col()
                .p(zpx(14.0))
                .gap(zpx(10.0))
                .child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .bg(rgba_to_hsla(ui.input))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder(t!("project.name"))
                        .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                        .value(name_draft.get())
                        .on_change(move |v: &str| name_draft.set(v.to_string())),
                )
                .child(
                    input()
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .bg(rgba_to_hsla(ui.input))
                        .font_family(FONT_FAMILY_MONO)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder(t!("project.workingDir"))
                        .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                        .value(path_draft.get())
                        .on_change(move |v: &str| path_draft.set(v.to_string())),
                )
                .child(
                    view()
                        .items_center()
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint_strong))
                        .on_press(move || {
                            let name = name_draft.get().trim().to_string();
                            let path = path_draft.get().trim().to_string();
                            if !name.is_empty() {
                                let id = format!("proj-{}", st_create.projects.get().len() + 1);
                                let workdir = if path.is_empty() {
                                    format!("/workspace/{name}")
                                } else {
                                    path
                                };
                                st_create.projects.update(|list| {
                                    list.push(ProjectRecord {
                                        id: id.clone(),
                                        name,
                                        path: workdir,
                                        host_id: None,
                                        host_name: None,
                                        derived_from: None,
                                        branch: Some("main".into()),
                                    });
                                });
                                st_create.active_project_id.set(id);
                                name_draft.set(String::new());
                                path_draft.set(String::new());
                                st_create.active_dialog.set(ActiveDialog::None);
                            }
                        })
                        .child(
                            text(t!("project.create"))
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.primary_foreground)),
                        ),
                ),
        )
}

fn render_settings_modal(
    store: &WorkspaceStore,
    theme_ctrl: crate::theme::ThemeController,
    zoom_ctrl: Option<ZoomController>,
    resolved: &falcon_theme::ResolvedTheme,
    active_tab: SettingsTab,
    theme_filter: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;

    let tabs: &[(SettingsTab, String)] = &[
        (SettingsTab::Appearance, t!("settings.tab_appearance")),
        (SettingsTab::Terminal, t!("settings.terminalTitle")),
        (SettingsTab::Hosts, t!("settings.tab_hosts")),
        (SettingsTab::Relays, t!("settings.tab_relays")),
        (SettingsTab::Plugins, t!("settings.tab_plugins")),
        (SettingsTab::About, t!("settings.tab_about")),
    ];

    let nav = view()
        .flex_row()
        .flex_wrap()
        .gap(zpx(4.0))
        .px(zpx(12.0))
        .py(zpx(8.0))
        .border_b_1()
        .border_color(rgba_to_hsla(ui.border))
        .children(tabs.iter().map(|(tab, label)| {
            let selected = *tab == active_tab;
            let st = store.clone();
            let next_tab = *tab;
            view()
                .px(zpx(8.0))
                .py(zpx(4.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .bg(if selected {
                    rgb_to_hsla(ui.tint)
                } else {
                    rgb_to_hsla(ui.secondary)
                })
                .on_press(move || st.active_dialog.set(ActiveDialog::Settings(next_tab)))
                .child(
                    text(label.clone()).text_xs().text_color(if selected {
                        rgb_to_hsla(ui.tint_foreground)
                    } else {
                        rgb_to_hsla(ui.secondary_foreground)
                    }),
                )
        }));

    let body = match active_tab {
        SettingsTab::Appearance => {
            render_appearance_tab(theme_ctrl, zoom_ctrl, resolved, theme_filter).into_element()
        }
        SettingsTab::Terminal => render_terminal_tab(store, resolved).into_element(),
        SettingsTab::Hosts => render_hosts_tab(store, resolved).into_element(),
        SettingsTab::Relays => render_relays_tab(store, resolved).into_element(),
        SettingsTab::Plugins => render_plugins_tab(store, resolved).into_element(),
        SettingsTab::About => render_about_tab(store, resolved).into_element(),
    };

    view()
        .flex_col()
        .max_h(zpx(500.0))
        .child(modal_header(store, resolved, t!("sidebar.settings")))
        .child(nav)
        .child(scroll().flex_1().child(view().p(zpx(14.0)).child(body)))
}

fn render_appearance_tab(
    theme_ctrl: crate::theme::ThemeController,
    zoom_ctrl: Option<ZoomController>,
    resolved: &falcon_theme::ResolvedTheme,
    theme_filter: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let cur_settings = theme_ctrl.settings();
    let active_slot = theme_ctrl.active_slot();
    let catalog = theme_ctrl.catalog();
    let q = theme_filter.get().trim().to_ascii_lowercase();

    let modes: &[(ThemePref, String)] = &[
        (ThemePref::System, t!("native.nsh.themeModeSystem")),
        (ThemePref::Light, t!("native.nsh.themeModeLight")),
        (ThemePref::Dark, t!("native.nsh.themeModeDark")),
    ];

    let derived_label = if resolved.is_dark {
        t!("native.nsh.themeDerivedDark")
    } else {
        t!("native.nsh.themeDerivedLight")
    };

    let mut page = view()
        .flex_col()
        .gap(zpx(10.0))
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .child(
                    text(t!("theme.label"))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(derived_label)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.tint_strong)),
                ),
        )
        .child(
            view()
                .flex_row()
                .gap(zpx(6.0))
                .children(modes.iter().map(|(m, label)| {
                    let selected = cur_settings.mode == *m;
                    let pref = *m;
                    view()
                        .px(zpx(8.0))
                        .py(zpx(4.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(if selected {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.secondary)
                        })
                        .on_press(move || theme_ctrl.set_mode(pref))
                        .child(text(label.clone()).text_xs().text_color(if selected {
                            rgb_to_hsla(ui.tint_foreground)
                        } else {
                            rgb_to_hsla(ui.secondary_foreground)
                        }))
                })),
        )
        .child(
            text(t!(
                "native.nsh.themeCatalogCount",
                count = catalog.len().to_string().as_str()
            ))
            .text_xs()
            .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .child(
            input()
                .w_full()
                .px(zpx(8.0))
                .py(zpx(5.0))
                .rounded(zpx(6.0))
                .bg(rgba_to_hsla(ui.input))
                .text_xs()
                .text_color(rgb_to_hsla(ui.foreground))
                .placeholder(t!(
                    "theme.search",
                    n = catalog.len().to_string().as_str()
                ))
                .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                .value(theme_filter.get())
                .on_change(move |v: &str| theme_filter.set(v.to_string())),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(zpx(6.0))
                .children(
                    catalog
                        .iter()
                        .filter(|e| q.is_empty() || e.name.to_ascii_lowercase().contains(&q))
                        .take(24)
                        .map(|entry| {
                            let is_active = cur_settings.slot(active_slot).name == entry.name;
                            let entry_name = entry.name.to_string();
                            let entry_label = entry.name.to_string();
                            let slot_for_click: ThemeMode = active_slot;
                            view()
                                .px(zpx(8.0))
                                .py(zpx(4.0))
                                .rounded(zpx(6.0))
                                .cursor_pointer()
                                .bg(if is_active {
                                    rgb_to_hsla(ui.tint)
                                } else {
                                    rgb_to_hsla(ui.card)
                                })
                                .border_1()
                                .border_color(rgba_to_hsla(ui.border))
                                .on_press(move || {
                                    theme_ctrl.select_builtin_for_slot(slot_for_click, &entry_name);
                                })
                                .child(
                                    text(entry_label)
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                        }),
                ),
        );

    if let Some(zc) = zoom_ctrl {
        let pct_label = zc.label();
        page = page.child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .pt(zpx(6.0))
                .border_t_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    text(t!("native.nsh.ZoomLabel", pct = pct_label.as_str()))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    view()
                        .flex_row()
                        .gap(zpx(6.0))
                        .child(
                            view()
                                .px(zpx(8.0))
                                .py(zpx(3.0))
                                .rounded(zpx(5.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.secondary))
                                .on_press(move || zc.step_out())
                                .child(
                                    text(t!("native.settings.uiZoomOut"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                ),
                        )
                        .child(
                            view()
                                .px(zpx(8.0))
                                .py(zpx(3.0))
                                .rounded(zpx(5.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.secondary))
                                .on_press(move || zc.reset())
                                .child(
                                    text(t!("native.settings.uiZoomReset"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                ),
                        )
                        .child(
                            view()
                                .px(zpx(8.0))
                                .py(zpx(3.0))
                                .rounded(zpx(5.0))
                                .cursor_pointer()
                                .bg(rgb_to_hsla(ui.secondary))
                                .on_press(move || zc.step_in())
                                .child(
                                    text(t!("native.settings.uiZoomIn"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                ),
                        ),
                ),
        );
    }

    page
}

fn render_terminal_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let pref = store.term_prefs();

    let fonts: &[(TermFontId, String)] = &[
        (TermFontId::Berkeley, t!("term.font_berkeley")),
        (TermFontId::Ioskeley, t!("term.font_ioskeley")),
        (TermFontId::Maple, t!("term.font_maple")),
    ];

    let cursors: &[(TermCursorStyle, String)] = &[
        (TermCursorStyle::Block, t!("term.cursor_block")),
        (TermCursorStyle::Bar, t!("term.cursor_bar")),
        (TermCursorStyle::Underline, t!("term.cursor_underline")),
    ];

    view()
        .flex_col()
        .gap(zpx(10.0))
        .child(
            text(t!("settings.termFont"))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap(zpx(6.0))
                .children(fonts.iter().map(|(preset, label)| {
                    let selected = pref.font_id == *preset;
                    let st = store.clone();
                    let next_font = *preset;
                    view()
                        .px(zpx(8.0))
                        .py(zpx(4.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(if selected {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.secondary)
                        })
                        .on_press(move || {
                            st.term_prefs.update(|p| p.font_id = next_font);
                        })
                        .child(
                            text(label.clone())
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                })),
        )
        .child(
            text(t!("settings.termCursor"))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            view()
                .flex_row()
                .gap(zpx(6.0))
                .children(cursors.iter().map(|(cs, label)| {
                    let selected = pref.cursor_style == *cs;
                    let st = store.clone();
                    let next_cs = *cs;
                    view()
                        .px(zpx(8.0))
                        .py(zpx(4.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(if selected {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.secondary)
                        })
                        .on_press(move || {
                            st.term_prefs.update(|p| p.cursor_style = next_cs);
                        })
                        .child(
                            text(label.clone())
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                })),
        )
}

fn render_hosts_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let hosts = store.hosts.get();

    view()
        .flex_col()
        .gap(zpx(8.0))
        .children(hosts.into_iter().map(|h| {
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .p(zpx(8.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.card))
                .border_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    view()
                        .flex_col()
                        .child(
                            text(h.name)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            text(format!("{} · {}", h.destination, h.os_label))
                                .text_xs()
                                .font_family(FONT_FAMILY_MONO)
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                )
                .child(
                    text(if h.connected {
                        t!("native.nsh.hostConnected")
                    } else {
                        t!("native.nsh.hostDisconnected")
                    })
                    .text_xs()
                    .text_color(if h.connected {
                        rgb_to_hsla(ui.success)
                    } else {
                        rgb_to_hsla(ui.muted_foreground)
                    }),
                )
        }))
}

fn render_relays_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let relays = store.relays.get();

    view()
        .flex_col()
        .gap(zpx(8.0))
        .child(
            text(t!("native.nsh.relaysTitle"))
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .children(relays.into_iter().enumerate().map(|(idx, r)| {
            let st = store.clone();
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .p(zpx(8.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.card))
                .border_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    view()
                        .flex_col()
                        .gap(zpx(2.0))
                        .child(
                            text(format!(
                                "{} · :{} → {}",
                                r.machine_label, r.local_port, r.target
                            ))
                            .text_xs()
                            .font_family(FONT_FAMILY_MONO)
                            .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .when_some(r.public_url, |el, pub_url| {
                            el.child(
                                text(pub_url)
                                    .text_xs()
                                    .font_family(FONT_FAMILY_MONO)
                                    .text_color(rgb_to_hsla(ui.tint_strong)),
                            )
                        })
                        .when(r.same_port_conflict, |el| {
                            el.child(
                                text(t!("native.nsh.samePortConflict"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.warning)),
                            )
                        }),
                )
                .child(
                    view()
                        .px(zpx(8.0))
                        .py(zpx(3.0))
                        .rounded(zpx(5.0))
                        .cursor_pointer()
                        .bg(if r.enabled {
                            rgb_to_hsla(ui.success)
                        } else {
                            rgb_to_hsla(ui.secondary)
                        })
                        .on_press(move || {
                            st.relays.update(|list| {
                                if let Some(entry) = list.get_mut(idx) {
                                    entry.enabled = !entry.enabled;
                                }
                            });
                        })
                        .child(
                            text(if r.enabled {
                                t!("forward.state_active")
                            } else {
                                t!("forward.state_stopped")
                            })
                            .text_xs()
                            .text_color(if r.enabled {
                                rgb_to_hsla(ui.success_foreground)
                            } else {
                                rgb_to_hsla(ui.secondary_foreground)
                            }),
                        ),
                )
        }))
}

fn render_plugins_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let list = store.plugin_list.get();

    view()
        .flex_col()
        .gap(zpx(8.0))
        .child(
            text(format!("{}: {}", t!("plugin.dir"), list.dir))
                .text_xs()
                .font_family(FONT_FAMILY_MONO)
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .children(list.plugins.into_iter().map(|p| {
            let st = store.clone();
            let pid = p.id.clone();
            let kind_tag = match p.kind {
                PluginKind::Builtin => t!("plugin.kindBuiltin"),
                PluginKind::Command | PluginKind::Unknown => t!("plugin.kindCommand"),
                PluginKind::Package => t!("plugin.kindPackage"),
            };
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .p(zpx(8.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.card))
                .border_1()
                .border_color(rgba_to_hsla(ui.border))
                .child(
                    view()
                        .flex_col()
                        .child(
                            text(format!("{} · {kind_tag}", p.name))
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .when_some(p.description, |el, d| {
                            el.child(
                                text(d)
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                        }),
                )
                .child(
                    view()
                        .px(zpx(8.0))
                        .py(zpx(3.0))
                        .rounded(zpx(5.0))
                        .cursor_pointer()
                        .bg(if p.enabled {
                            rgb_to_hsla(ui.tint)
                        } else {
                            rgb_to_hsla(ui.secondary)
                        })
                        .on_press(move || {
                            st.plugin_list.update(|pl| {
                                if let Some(target) = pl.plugins.iter_mut().find(|x| x.id == pid) {
                                    target.enabled = !target.enabled;
                                }
                            });
                        })
                        .child(
                            text(if p.enabled {
                                t!("forward.enabled")
                            } else {
                                t!("forward.state_stopped")
                            })
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                )
        }))
}

fn render_about_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let url = store.server_url.get();
    view()
        .flex_col()
        .gap(zpx(6.0))
        .child(
            text(format!(
                "{}: v{} ({})",
                t!("native.settings.clientVersion"),
                env!("CARGO_PKG_VERSION"),
                t!("native.nsh.RendererBadge")
            ))
            .text_xs()
            .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            text(format!("{}: {url}", t!("native.settings.serverUrl")))
                .text_xs()
                .font_family(FONT_FAMILY_MONO)
                .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
}

fn render_askpass_modal(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    _id: &str,
    prompt: &str,
    secret: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let st_ok = store.clone();
    view()
        .flex_col()
        .child(modal_header(store, resolved, t!("askpass.title")))
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
                        .w_full()
                        .px(zpx(8.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .masked(true)
                        .bg(rgba_to_hsla(ui.input))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground))
                        .placeholder(t!("askpass.placeholder"))
                        .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                        .value(secret.get())
                        .on_change(move |v: &str| secret.set(v.to_string())),
                )
                .child(
                    view()
                        .items_center()
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.tint_strong))
                        .on_press(move || {
                            secret.set(String::new());
                            st_ok.active_dialog.set(ActiveDialog::None);
                        })
                        .child(
                            text(t!("askpass.submit"))
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
