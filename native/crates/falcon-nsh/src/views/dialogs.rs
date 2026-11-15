//! 顶层模态对话框层（命令面板、服务端配置、设置 6 页签、新建项目、SSH Askpass、终止忙会话确认）。
//!
//! 严格遵循 `CONTEXT.md` 术语与 `falcon-theme` 语义取色，零硬编码中文与颜色常量。

use falcon_core::profiles::{LOCAL_PROFILE_ID, ServerProfile, new_profile_id};
use falcon_core::term::{TermCursorStyle, TermFontId};
use falcon_theme::{Rgba as ThemeRgba, ThemeMode, ThemePref};
use nsh_core::element::input;
use nsh_core::platform::Idiom;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::shell::widgets::{Glyph, glyph, icon_button};
use crate::store::{ActiveDialog, RightPanelKind, SettingsTab, WorkspaceStore, use_workspace};
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
    let new_project_host = signal(None::<String>);
    let askpass_input = signal(String::new());
    let theme_filter = signal(String::new());
    let theme_slot = signal(theme_ctrl.active_slot());
    let theme_picker_open = signal(false);
    let settings_filter = signal(String::new());

    view().child(dynamic(move || {
        let dialog = store.active_dialog.get();
        if dialog == ActiveDialog::None {
            return view().into_element();
        }

        let resolved = theme_ctrl.resolved();
        let ui = &resolved.ui;

        let settings_dialog = matches!(dialog, ActiveDialog::Settings(_));
        let palette_dialog = matches!(dialog, ActiveDialog::Palette);
        let card_body = match dialog {
            ActiveDialog::None => return view().into_element(),
            ActiveDialog::Palette => {
                render_palette_modal(&store, &resolved, palette_query).into_element()
            }
            ActiveDialog::Profiles => {
                render_profiles_modal(&store, &resolved, profile_name, profile_url).into_element()
            }
            ActiveDialog::NewProject => render_new_project_modal(
                &store,
                &resolved,
                new_proj_name,
                new_proj_path,
                new_project_host,
            )
            .into_element(),
            ActiveDialog::Settings(tab) => render_settings_modal(
                &store,
                theme_ctrl,
                zoom_ctrl,
                &resolved,
                tab,
                theme_filter,
                theme_slot,
                theme_picker_open,
                settings_filter,
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
            .when(palette_dialog, |v| v.justify_start().pt(zpx(96.0)))
            .bg(scrim)
            .child(
                view()
                    .w(zpx(if settings_dialog { 1024.0 } else { 580.0 }))
                    .max_w(relative(1.0))
                    .max_h(relative(0.86))
                    .flex_col()
                    .overflow_hidden()
                    .rounded(zpx(16.0))
                    .border_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .bg(rgb_to_hsla(ui.background))
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
                .child(text(title).text_xs().text_color(rgb_to_hsla(ui.foreground)))
                .child(
                    text(right_label)
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.tint_strong)),
                ),
        );
    }

    // 项目切换
    items = items.child(
        view().pt(zpx(6.0)).child(
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
        view().pt(zpx(6.0)).child(
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
                .child(text(label).text_xs().text_color(rgb_to_hsla(ui.foreground))),
        );
    }

    view()
        .flex_col()
        .h(zpx(460.0))
        .child(
            view().p(zpx(16.0)).child(
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
    host: Signal<Option<String>>,
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
                    view()
                        .flex_row()
                        .gap(zpx(8.0))
                        .flex_wrap()
                        .child(
                            view()
                                .px(zpx(10.0))
                                .py(zpx(6.0))
                                .rounded(zpx(8.0))
                                .cursor_pointer()
                                .when(host.get().is_none(), |v| v.bg(rgb_to_hsla(ui.tint)))
                                .on_press(move || host.set(None))
                                .child(
                                    text(t!("project.typeLocal"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                ),
                        )
                        .children(store.hosts.get().into_iter().map(|h| {
                            let id = h.id.clone();
                            view()
                                .px(zpx(10.0))
                                .py(zpx(6.0))
                                .rounded(zpx(8.0))
                                .cursor_pointer()
                                .when(host.get().as_deref() == Some(h.id.as_str()), |v| {
                                    v.bg(rgb_to_hsla(ui.tint))
                                })
                                .on_press(move || host.set(Some(id.clone())))
                                .child(
                                    text(format!("{} · {}", t!("project.typeSsh"), h.name))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                        })),
                )
                .child(
                    text(t!("project.name"))
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
                                st_create.create_project_request(
                                    &name,
                                    &path,
                                    host.get().as_deref(),
                                );
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
    theme_slot: Signal<ThemeMode>,
    theme_picker_open: Signal<bool>,
    settings_filter: Signal<String>,
) -> impl IntoElement {
    let ui = &resolved.ui;

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
    let query = settings_filter.get().to_lowercase();
    let mut nav = view()
        .flex_col()
        .w(zpx(224.0))
        .flex_none()
        .when(idiom() != Idiom::Phone, |v| v.h(zpx(680.0)))
        .gap(zpx(2.0))
        .p(zpx(12.0))
        .bg(rgb_to_hsla(ui.app))
        .when(idiom() == Idiom::Phone, |v| {
            v.w_full().flex_row().flex_wrap()
        })
        .child(
            input()
                .w_full()
                .px(zpx(8.0))
                .py(zpx(5.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.muted))
                .text_xs()
                .text_color(rgb_to_hsla(ui.foreground))
                .placeholder(t!("settings.search"))
                .value(settings_filter.get())
                .on_change(move |v: &str| settings_filter.set(v.to_string())),
        );
    for (tab, label, icon) in tabs {
        if !query.is_empty() && !label.to_lowercase().contains(&query) {
            continue;
        }
        if query.is_empty() && (tab == SettingsTab::Appearance || tab == SettingsTab::Hosts) {
            nav = nav.child(
                text(if tab == SettingsTab::Hosts {
                    t!("settings.group_connection")
                } else {
                    t!("settings.group_general")
                })
                .text_xs()
                .pt(zpx(20.0))
                .pb(zpx(6.0))
                .px(zpx(8.0))
                .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
        let selected = tab == active_tab;
        let st = store.clone();
        nav = nav.child(
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
                    ui.foreground
                } else {
                    ui.muted_foreground
                }))
                .when(selected, |v| v.bg(rgb_to_hsla(ui.tint)))
                .hover(|s| s.bg(rgb_to_hsla(ui.muted)))
                .on_press(move || st.active_dialog.set(ActiveDialog::Settings(tab)))
                .child(glyph(icon))
                .child(text(label).text_xs()),
        );
    }
    let body = match active_tab {
        SettingsTab::Appearance => view()
            .flex_col()
            .gap(zpx(24.0))
            .child(render_appearance_tab(
                store,
                theme_ctrl,
                zoom_ctrl,
                resolved,
                theme_filter,
                theme_slot,
                theme_picker_open,
            ))
            .child(render_terminal_tab(store, resolved))
            .into_element(),
        SettingsTab::Terminal => render_terminal_tab(store, resolved).into_element(),
        SettingsTab::Notifications => render_notifications_tab(store, resolved).into_element(),
        SettingsTab::Hosts => render_hosts_tab(store, resolved).into_element(),
        SettingsTab::Relays => render_relays_tab(store, resolved).into_element(),
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
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .child(body),
            ),
        )
        .child(
            view()
                .absolute()
                .top(zpx(6.0))
                .right(zpx(6.0))
                .child(icon_button(
                    "settings-close",
                    t!("common.close"),
                    Glyph::Close,
                    false,
                    resolved,
                    move || close.active_dialog.set(ActiveDialog::None),
                )),
        )
}

fn render_appearance_tab(
    store: &WorkspaceStore,
    theme_ctrl: crate::theme::ThemeController,
    zoom_ctrl: Option<ZoomController>,
    resolved: &falcon_theme::ResolvedTheme,
    theme_filter: Signal<String>,
    theme_slot: Signal<ThemeMode>,
    theme_picker_open: Signal<bool>,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let settings = theme_ctrl.settings();
    let catalog = theme_ctrl.catalog();
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
        modes = modes.child(
            view()
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(8.0))
                .cursor_pointer()
                .when(settings.mode == pref, |v| v.bg(rgb_to_hsla(ui.background)))
                .on_press(move || theme_ctrl.set_mode(pref))
                .child(
                    text(t!(key))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                ),
        );
    }
    let mut page = view()
        .flex_col()
        .child(setting_row(resolved, t!("theme.label"), modes));
    for slot in [ThemeMode::Light, ThemeMode::Dark] {
        let choice = settings.slot(slot);
        let name = choice.name.clone();
        page = page.child(setting_row(
            resolved,
            if slot == ThemeMode::Light {
                t!("theme.lightTheme")
            } else {
                t!("theme.darkTheme")
            },
            view()
                .label(format!("theme-slot-{slot:?}"))
                .w(zpx(256.0))
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.app))
                .cursor_pointer()
                .on_press(move || {
                    if theme_slot.get() == slot {
                        theme_picker_open.update(|v| *v = !*v);
                    } else {
                        theme_slot.set(slot);
                        theme_picker_open.set(true);
                    }
                })
                .child(
                    text(name)
                        .text_xs()
                        .number_of_lines(1)
                        .text_color(rgb_to_hsla(ui.foreground)),
                ),
        ));
    }
    // 外观仍按当前主题底色判定；说明放在选择器附近，不占主侧栏。
    page = page.child(
        text(if resolved.is_dark {
            t!("native.nsh.themeDerivedDark")
        } else {
            t!("native.nsh.themeDerivedLight")
        })
        .text_xs()
        .py(zpx(6.0))
        .text_color(rgb_to_hsla(ui.muted_foreground)),
    );
    if theme_picker_open.get() {
        let query = theme_filter.get().to_lowercase();
        page = page.child(
            input()
                .w_full()
                .px(zpx(8.0))
                .py(zpx(6.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.app))
                .text_xs()
                .text_color(rgb_to_hsla(ui.foreground))
                .placeholder(t!("theme.search", n = catalog.len().to_string().as_str()))
                .value(theme_filter.get())
                .on_change(move |v: &str| theme_filter.set(v.to_string())),
        );
        page = page.child(
            view().flex_row().flex_wrap().gap(zpx(4.0)).children(
                catalog
                    .iter()
                    .filter(|e| query.is_empty() || e.name.to_lowercase().contains(&query))
                    .take(40)
                    .map(|entry| {
                        let name = entry.name.to_string();
                        let label = name.clone();
                        let slot = theme_slot.get();
                        view()
                            .px(zpx(8.0))
                            .py(zpx(5.0))
                            .rounded(zpx(8.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.app))
                            .on_press(move || {
                                theme_ctrl.select_builtin_for_slot(slot, &name);
                                theme_picker_open.set(false);
                            })
                            .child(text(label).text_xs().text_color(rgb_to_hsla(ui.foreground)))
                    }),
            ),
        );
    }
    if let Some(zoom) = zoom_ctrl {
        page = page.child(setting_row(
            resolved,
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
                    resolved,
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
                    resolved,
                    move || zoom.step_in(),
                )),
        ));
    }
    page.child(
        view()
            .pt(zpx(32.0))
            .flex_col()
            .gap(zpx(16.0))
            .child(
                text(t!("appIcon.title"))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)),
            )
            .child(view().flex_row().flex_wrap().gap(zpx(8.0)).children(
                falcon_core::app_icon::APP_ICON_IDS.into_iter().map(|id| {
                    let st = store.clone();
                    view()
                        .w(zpx(96.0))
                        .h(zpx(112.0))
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(zpx(12.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .when(store.selected_app_icon.get() == id, |v| {
                            v.bg(rgb_to_hsla(ui.tint))
                        })
                        .on_press(move || st.select_app_icon(id))
                        .child(
                            image(crate::shell::widgets::app_icon_source(id))
                                .w(zpx(52.0))
                                .h(zpx(52.0)),
                        )
                        .child(
                            text(t!(falcon_core::app_icon::label_key(id).as_str()))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                }),
            )),
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

fn render_terminal_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let pref = store.term_prefs();
    let fg = rgb_to_hsla(ui.foreground);
    let bg = rgb_to_hsla(ui.app);
    let border = rgba_to_hsla(ui.border);
    let font_store = store.clone();
    let font_title = t!(format!("term.font_{}", pref.font_id.as_str().replace('-', "_")).as_str());
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
        .popup_style(move |v| v.bg(bg).rounded(zpx(8.0)).border_1().border_color(border))
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
                .child(text(font_title).text_xs().number_of_lines(1))
                .child(glyph(Glyph::ChevronDown)),
        )
        .render_item(move |entry: &nsh_base::SelectEntry| {
            view()
                .px(zpx(10.0))
                .py(zpx(6.0))
                .bg(bg)
                .child(text(entry.title.clone()).text_xs().text_color(fg))
        })
        .on_change(move |value: &Option<SharedString>| {
            if let Some(id) = value.as_deref().and_then(TermFontId::from_wire) {
                font_store.term_prefs.update(|p| p.font_id = id);
            }
        });
    let mut page = view()
        .flex_col()
        .child(setting_row(resolved, t!("settings.termFont"), fonts));
    for (custom, label, hint, value) in [
        (
            true,
            "settings.termCustomFont",
            "settings.termCustomFontPlaceholder",
            pref.custom_family.clone(),
        ),
        (
            false,
            "settings.termFallbackFont",
            "settings.termFallbackFontPlaceholder",
            pref.fallback_families.clone(),
        ),
    ] {
        if custom && pref.font_id != TermFontId::Custom {
            continue;
        }
        let st = store.clone();
        page = page.child(setting_row(
            resolved,
            t!(label),
            input()
                .w(zpx(256.0))
                .px(zpx(8.0))
                .py(zpx(5.0))
                .rounded(zpx(8.0))
                .border_1()
                .border_color(border)
                .bg(bg)
                .text_xs()
                .text_color(fg)
                .placeholder(t!(hint))
                .value(value)
                .on_change(move |v: &str| {
                    st.term_prefs.update(|p| {
                        if custom {
                            p.custom_family = v.to_string();
                        } else {
                            p.fallback_families = v.to_string();
                        }
                    })
                }),
        ));
    }
    for (size, label, min, max, step, value) in [
        (
            true,
            "settings.termFontSize",
            10.0,
            24.0,
            1.0,
            pref.font_size,
        ),
        (
            false,
            "settings.termLineHeight",
            1.0,
            1.6,
            0.05,
            pref.line_height,
        ),
    ] {
        let st = store.clone();
        page = page.child(setting_row(
            resolved,
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
                        .value(Some(nsh_base::SliderValue::Single(value)))
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
                        .on_change(move |v: &nsh_base::SliderValue| {
                            if let nsh_base::SliderValue::Single(value) = v {
                                st.term_prefs.update(|p| {
                                    if size {
                                        p.font_size = *value;
                                    } else {
                                        p.line_height = *value;
                                    }
                                });
                            }
                        }),
                )
                .child(
                    text(if size {
                        format!("{value:.0}")
                    } else {
                        format!("{value:.2}")
                    })
                    .w(zpx(40.0))
                    .text_xs()
                    .text_color(fg),
                ),
        ));
    }
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
        let st = store.clone();
        cursors = cursors.child(
            view()
                .px(zpx(10.0))
                .py(zpx(5.0))
                .rounded(zpx(6.0))
                .cursor_pointer()
                .when(pref.cursor_style == style, |v| {
                    v.bg(rgb_to_hsla(ui.background))
                })
                .on_press(move || st.term_prefs.update(|p| p.cursor_style = style))
                .child(text(t!(key)).text_xs().text_color(fg)),
        );
    }
    let blink = store.clone();
    let reset = store.clone();
    page.child(setting_row(resolved, t!("settings.termCursor"), cursors))
        .child(setting_row(resolved, t!("settings.termCursorBlink"), view().cursor_pointer()
            .on_press(move || blink.term_prefs.update(|p| p.cursor_blink = !p.cursor_blink))
            .child(crate::shell::widgets::check_indicator(pref.cursor_blink, resolved))))
        .child(view().pt(zpx(16.0)).flex_col().gap(zpx(12.0))
            .child(view().flex_row().justify_between().items_center()
                .child(text(t!("settings.termPreview")).text_xs().text_color(fg))
                .child(view().cursor_pointer().on_press(move || reset.term_prefs.set(Default::default()))
                    .child(text(t!("settings.termReset")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)))))
            .child(view().p(zpx(12.0)).rounded(zpx(8.0)).border_1().border_color(border)
                .bg(rgb_to_hsla(resolved.terminal.background)).font_family(falcon_core::term::primary_family(&pref).unwrap_or_else(|| "monospace".into()))
                .child(text("falcon@host ~/falcon").text_size(zpx(pref.font_size as f32)).text_color(rgb_to_hsla(ui.success)))
                .child(text("$ git status\nOn branch main\nnothing to commit, working tree clean\n$ echo 你好 ▋").text_size(zpx(pref.font_size as f32)).text_color(rgb_to_hsla(resolved.terminal.foreground)))))
}

fn render_hosts_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let hosts = store.hosts.get();
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
        let auth = host
            .source
            .as_ref()
            .map(|h| h.auth_method.as_str())
            .unwrap_or("agent");
        table = table.child(
            view()
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
                    text(match auth {
                        "key" => t!("project.authKey"),
                        "password" => t!("project.authPassword"),
                        _ => t!("project.authAgent"),
                    })
                    .w(zpx(100.0))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(
                    view()
                        .px(zpx(8.0))
                        .py(zpx(4.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.background))
                        .on_press(move || st.test_host_connection(&id))
                        .child(
                            text(t!("host.test"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                ),
        );
    }
    view()
        .flex_col()
        .gap(zpx(12.0))
        .child(table)
        .child(super::host_form::host_form())
}

fn render_relays_tab(
    store: &WorkspaceStore,
    _resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let mut groups = view().flex_col().gap(zpx(24.0)).child(
        super::relays::relay_machine()
            .machine_id(String::new())
            .name(t!("forward.local"))
            .destination(t!("forward.localConn")),
    );
    for host in store.hosts.get() {
        groups = groups.child(
            super::relays::relay_machine()
                .key(host.id.clone())
                .machine_id(host.id)
                .name(host.name)
                .destination(host.destination),
        );
    }
    groups
}

fn render_plugins_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let list = store.plugin_list.get();
    let reload = store.clone();
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
            view()
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
                            text(plugin.name)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .when_some(plugin.description, |v, description| {
                            v.child(
                                text(description)
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                        }),
                )
                .child(
                    nsh_base::switch()
                        .w(zpx(36.0))
                        .h(zpx(20.0))
                        .rounded_full()
                        .bg(rgb_to_hsla(if plugin.enabled {
                            ui.primary
                        } else {
                            ui.muted
                        }))
                        .thumb_class("size-4 rounded-full bg-background checked:translate-x-4")
                        .checked(plugin.enabled)
                        .on_change(move |enabled: &bool| st.set_plugin_enabled(&id, *enabled)),
                )
        }))
}

fn render_about_tab(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let info = store.server_info.get();
    let st = store.clone();
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
        .child(
            view()
                .pt(zpx(32.0))
                .flex_col()
                .gap(zpx(10.0))
                .child(
                    text(t!("native.update.title"))
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(t!("native.update.intro"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                )
                .child(setting_row(
                    resolved,
                    t!("native.update.check"),
                    view()
                        .px(zpx(10.0))
                        .py(zpx(4.0))
                        .rounded(zpx(8.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.app))
                        .on_press(move || st.check_app_update())
                        .child(
                            text(t!("native.update.check"))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                )),
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
