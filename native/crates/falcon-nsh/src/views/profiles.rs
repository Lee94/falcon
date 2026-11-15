//! 服务端配置管理；危险操作先经顶层模态确认，再保存或连接。
use falcon_core::profiles::{ServerProfile, new_profile_id, normalize_connection_url};
use falcon_theme::ResolvedTheme;
use nsh_core::{element::input, prelude::*};

use crate::{
    fonts::FONT_FAMILY_MONO,
    i18n::t,
    shell::{popups::themed_tooltip, widgets::{Glyph, glyph, icon_button}},
    store::{ActiveDialog, WorkspaceStore},
    theme::{rgb_to_hsla, rgba_to_hsla},
    zoom::zpx,
};

#[derive(Clone, Copy)]
pub(super) struct ProfileDraft {
    pub name: Signal<String>,
    pub url: Signal<String>,
    pub bad_url: Signal<bool>,
}

impl ProfileDraft {
    pub fn new() -> Self {
        Self { name: signal(String::new()), url: signal(String::new()), bad_url: signal(false) }
    }

    fn submit(self, store: &WorkspaceStore) {
        if store.active_dialog.get_untracked() != ActiveDialog::Profiles {
            return;
        }
        let Some(url) = normalize_connection_url(&self.url.get_untracked()) else {
            self.bad_url.set(true);
            return;
        };
        self.bad_url.set(false);
        open_profile(store, self, ServerProfile {
            id: new_profile_id(),
            name: self.name.get_untracked().trim().to_string(),
            url,
            plaintext_ok: false,
        }, true);
    }
}

fn display_name(profile: &ServerProfile) -> String {
    if profile.name.is_empty() {
        if profile.is_local() { t!("native.server.local") } else { profile.url.clone() }
    } else {
        profile.name.clone()
    }
}

fn open_profile(store: &WorkspaceStore, draft: ProfileDraft, profile: ServerProfile, is_new: bool) {
    if profile.is_insecure() && !profile.plaintext_ok {
        store.active_dialog.set(ActiveDialog::ConfirmPlaintextProfile { profile, is_new });
    } else {
        save_and_open(store, draft, profile, is_new);
    }
}

fn save_and_open(store: &WorkspaceStore, draft: ProfileDraft, profile: ServerProfile, is_new: bool) {
    store.profiles.update(|profiles| profiles.upsert(profile.clone()));
    if is_new {
        draft.name.set(String::new());
        draft.url.set(String::new());
    }
    store.active_dialog.set(ActiveDialog::None);
    // connect_live_server 负责接续未保存文件的确认，不能在它之后关闭模态。
    store.connect_live_server(&profile.url);
}

pub(super) fn render_profiles_modal(
    store: &WorkspaceStore,
    resolved: &ResolvedTheme,
    draft: ProfileDraft,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let current_url = store.server_url.get();
    let connected = store.server_connected.get();
    let submit = store.clone();
    let submit_name = store.clone();
    let submit_url = store.clone();
    view().flex_col().min_h_0()
        .child(super::dialogs::modal_header(store, resolved, t!("native.menu.connect")))
        .child(scroll().min_h_0().child(
            view().flex_col().p(zpx(14.0)).gap(zpx(10.0))
                .children(store.profiles.get().profiles.into_iter().map(|profile| {
                    let active = connected && profile.url.trim_end_matches('/') == current_url.trim_end_matches('/');
                    let open = store.clone();
                    let delete = store.clone();
                    let to_open = profile.clone();
                    let to_delete = profile.clone();
                    view().test_id(format!("connect-profile-row-{}", profile.id))
                        .min_h(zpx(36.0)).flex_row().items_center().gap(zpx(8.0))
                        .p(zpx(8.0)).rounded(zpx(8.0))
                        .bg(rgb_to_hsla(if active { ui.tint } else { ui.card }))
                        .child(view().text_color(rgb_to_hsla(ui.muted_foreground))
                            .child(glyph(if profile.is_local() { Glyph::Monitor } else { Glyph::Server })))
                        .child(view().flex_1().min_w_0().flex_col()
                            .child(text(display_name(&profile)).text_xs().number_of_lines(1)
                                .font_weight(FontWeight::SEMIBOLD).text_color(rgb_to_hsla(ui.foreground)))
                            .child(text(profile.url.clone()).text_xs().number_of_lines(1)
                                .font_family(FONT_FAMILY_MONO).text_color(rgb_to_hsla(ui.muted_foreground))))
                        .when(active, |row| row.child(
                            view().test_id(format!("connect-profile-connected-{}", profile.id))
                                .tooltip(themed_tooltip(text(t!("native.connect.connected")), None, resolved))
                                .text_color(rgb_to_hsla(ui.success)).child(glyph(Glyph::Check))))
                        .child(view().test_id(format!("connect-server-profile-{}", profile.id))
                            .focusable(true).px(zpx(10.0)).py(zpx(5.0)).rounded(zpx(6.0))
                            .border_1().border_color(rgba_to_hsla(ui.border)).cursor_pointer()
                            .on_press(move || open_profile(&open, draft, to_open.clone(), false))
                            .child(text(t!("native.connect.open")).text_xs().text_color(rgb_to_hsla(ui.foreground))))
                        .when(!profile.is_local(), |row| row.child(icon_button(
                            format!("connect-profile-delete-{}", profile.id),
                            t!("native.connect.delete"), Glyph::Trash, false, resolved,
                            move || delete.active_dialog.set(ActiveDialog::ConfirmDeleteProfile { profile: to_delete.clone() }),
                        )))
                }))
                .child(text(t!("native.connect.add")).text_sm().font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb_to_hsla(ui.foreground)))
                .child(input().test_id("connect-profile-name").w_full()
                    .px(zpx(8.0)).py(zpx(6.0)).rounded(zpx(6.0)).bg(rgba_to_hsla(ui.input))
                    .text_xs().text_color(rgb_to_hsla(ui.foreground))
                    .placeholder(t!("native.connect.namePlaceholder")).placeholder_color(rgb_to_hsla(ui.muted_foreground))
                    .value(draft.name.get()).on_change(move |value: &str| draft.name.set(value.to_string()))
                    .on_submit(move |_: &str| draft.submit(&submit_name)))
                .child(input().test_id("connect-profile-url").w_full()
                    .px(zpx(8.0)).py(zpx(6.0)).rounded(zpx(6.0)).bg(rgba_to_hsla(ui.input))
                    .font_family(FONT_FAMILY_MONO).text_xs().text_color(rgb_to_hsla(ui.foreground))
                    .placeholder(t!("native.connect.urlPlaceholder")).placeholder_color(rgb_to_hsla(ui.muted_foreground))
                    .value(draft.url.get()).on_change(move |value: &str| draft.url.set(value.to_string()))
                    .on_submit(move |_: &str| draft.submit(&submit_url)))
                .child(text(t!("native.connect.hint")).text_xs().text_color(rgb_to_hsla(ui.muted_foreground)))
                .when(draft.bad_url.get(), |body| body.child(text(t!("native.connect.badUrl"))
                    .test_id("connect-profile-error").text_xs().text_color(rgb_to_hsla(ui.destructive))))
                .child(view().test_id("connect-profile-add").focusable(true).items_center()
                    .py(zpx(6.0)).rounded(zpx(6.0)).cursor_pointer().bg(rgb_to_hsla(ui.tint_strong))
                    .on_press(move || draft.submit(&submit))
                    .child(text(t!("native.connect.connect")).text_xs().font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.primary_foreground))))
        ))
}

pub(super) fn render_profile_confirmation(
    store: &WorkspaceStore,
    resolved: &ResolvedTheme,
    draft: ProfileDraft,
    dialog: ActiveDialog,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let (title, body, label) = match &dialog {
        ActiveDialog::ConfirmDeleteProfile { profile } => (
            t!("native.connect.deleteTitle", name = display_name(profile).as_str()),
            t!("native.connect.deleteBody"),
            t!("native.connect.delete"),
        ),
        ActiveDialog::ConfirmPlaintextProfile { profile, .. } => (
            t!("native.connect.plaintextTitle"),
            t!("native.connect.plaintextBody", url = profile.url.as_str()),
            t!("native.connect.plaintextConfirm"),
        ),
        _ => unreachable!("profile confirmation only"),
    };
    let cancel = store.clone();
    let confirm = store.clone();
    view().test_id("connect-profile-confirmation").flex_col().gap(zpx(12.0)).p(zpx(20.0))
        .child(text(title).text_sm().font_weight(FontWeight::SEMIBOLD).text_color(rgb_to_hsla(ui.foreground)))
        .child(text(body).text_xs().text_color(rgb_to_hsla(ui.foreground)))
        .child(view().flex_row().justify_end().gap(zpx(8.0))
            .child(view().test_id("connect-profile-cancel").focusable(true)
                .px(zpx(12.0)).py(zpx(7.0)).rounded(zpx(8.0)).cursor_pointer().bg(rgb_to_hsla(ui.secondary))
                .on_press(move || cancel.active_dialog.set(ActiveDialog::Profiles))
                .child(text(t!("native.connect.cancel")).text_xs().text_color(rgb_to_hsla(ui.secondary_foreground))))
            .child(view().test_id("connect-profile-confirm").focusable(true)
                .px(zpx(12.0)).py(zpx(7.0)).rounded(zpx(8.0)).cursor_pointer().bg(rgb_to_hsla(ui.destructive))
                .on_press(move || {
                    if confirm.active_dialog.get_untracked() != dialog { return; }
                    match &dialog {
                        ActiveDialog::ConfirmDeleteProfile { profile } => {
                            confirm.profiles.update(|profiles| { profiles.remove(&profile.id); });
                            confirm.active_dialog.set(ActiveDialog::Profiles);
                        }
                        ActiveDialog::ConfirmPlaintextProfile { profile, is_new } => {
                            let mut profile = profile.clone();
                            profile.plaintext_ok = true;
                            save_and_open(&confirm, draft, profile, *is_new);
                        }
                        _ => {}
                    }
                })
                .child(text(label).text_xs().text_color(rgb_to_hsla(ui.destructive_foreground)))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_core::profiles::ProfilesFile;
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;
    use std::time::Duration;

    const LOCAL_URL: &str = "http://127.0.0.1:59998";

    #[component]
    fn ProfilesTest() -> impl IntoElement {
        crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        let store = WorkspaceStore::install(&crate::store::FalconLaunchOptions {
            offline_demo: true,
            server_url: Some(LOCAL_URL.into()),
            ..Default::default()
        });
        store.profiles.update(|profiles| {
            for (id, url, plaintext_ok) in [
                ("unconfirmed", "http://192.0.2.1:4923", false),
                ("confirmed", "http://192.0.2.2:4923", true),
                ("secure", "https://example.com", false),
                ("active", LOCAL_URL, true),
            ] {
                profiles.upsert(ServerProfile { id: id.into(), name: id.into(), url: url.into(), plaintext_ok });
            }
            profiles.mark_connected("active");
        });
        // 保留真实的未保存文件护栏，确认连接后停在这里，测试绝不访问外部服务。
        let mut file = crate::store::OpenFileBuffer::from_disk_text("project", "dirty.txt", "clean", "hash", None);
        file.current_body = "dirty".into();
        store.open_files.update(|files| { files.insert("dirty-file".into(), file); });
        store.active_dialog.set(ActiveDialog::Profiles);
        let reopen = store.clone();
        let state = store.clone();
        view().size_full()
            .child(view().test_id("profiles-test-reopen").on_press(move || reopen.active_dialog.set(ActiveDialog::Profiles)).child(text("profiles")))
            .child(dynamic(move || view()
                .child(text(format!("profiles-state:{}", serde_json::to_string(&state.profiles.get()).unwrap())))
                .child(text(format!("connection-state:{}:{}", state.server_url.get(), state.server_connected.get())))))
            .child(super::super::dialogs::modal_layer())
    }

    fn app() -> Headless {
        let root = Root::of::<ProfilesTest>().translations(crate::i18n::translations());
        let mut config = EngineConfig::new(root);
        config.viewport = Size::new(Points(1440.0), Points(1000.0));
        config.ui_sets = vec![nsh_base::registry(), crate::registry()];
        let mut app = Headless::start(config).unwrap();
        app.first_render().unwrap();
        app.wait_for("id:connect-profile-url").unwrap();
        app
    }

    fn profiles(app: &Headless) -> ProfilesFile {
        let texts = app.screen().texts();
        let json = texts.iter().find_map(|text| text.strip_prefix("profiles-state:")).unwrap();
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn profiles_bad_url_reports_error_without_saving_and_enter_retries() {
        let mut app = app();
        let original = profiles(&app);
        for value in ["", "https://", "ftp://example.com", "http://example.com:bad", "example.com"] {
            app.clear("id:connect-profile-url").unwrap();
            app.type_text("id:connect-profile-url", value).unwrap();
            app.submit("id:connect-profile-url").unwrap();
            app.wait_for("id:connect-profile-error").unwrap();
            assert_eq!(profiles(&app), original);
        }
        app.clear("id:connect-profile-url").unwrap();
        app.type_text("id:connect-profile-name", "  Production  ").unwrap();
        app.type_text("id:connect-profile-url", "  https://example.com/falcon///  ").unwrap();
        app.submit("id:connect-profile-name").unwrap();
        app.wait_for("id:unsaved-cancel").unwrap();
        let file = profiles(&app);
        let added = file.profiles.last().unwrap();
        assert_eq!(added.name, "Production");
        assert_eq!(added.url, "https://example.com/falcon");
        assert!(!added.plaintext_ok);
    }

    #[test]
    fn profiles_plaintext_cancel_keeps_draft_and_confirmation_precedes_save() {
        let mut app = app();
        let original = profiles(&app);
        app.type_text("id:connect-profile-name", "remote draft").unwrap();
        app.type_text("id:connect-profile-url", "http://192.0.2.3:4923///").unwrap();
        app.press("id:connect-profile-add").unwrap();
        app.wait_for("id:connect-profile-confirmation").unwrap();
        assert_eq!(profiles(&app), original);
        app.press_key("escape").unwrap();
        app.wait_for("id:connect-profile-url").unwrap();
        assert_eq!(profiles(&app), original);
        app.press("id:connect-profile-add").unwrap();
        app.wait_for("id:connect-profile-confirmation").unwrap();
        app.press("id:connect-profile-cancel").unwrap();
        app.wait_for("id:connect-profile-url").unwrap();
        app.submit("id:connect-profile-url").unwrap();
        app.wait_for("id:connect-profile-confirmation").unwrap();
        app.press("id:connect-profile-confirm").unwrap();
        app.wait_for("id:unsaved-cancel").unwrap();
        let file = profiles(&app);
        assert_eq!(file.profiles.len(), original.profiles.len() + 1);
        let added = file.profiles.last().unwrap();
        assert_eq!(added.name, "remote draft");
        assert_eq!(added.url, "http://192.0.2.3:4923");
        assert!(added.plaintext_ok);
        app.press("id:unsaved-cancel").unwrap();
        app.press("id:profiles-test-reopen").unwrap();
        app.wait_for("id:connect-profile-url").unwrap();
        app.press("id:connect-profile-add").unwrap();
        app.wait_for("id:connect-profile-error").unwrap();
        assert_eq!(profiles(&app), file);
    }

    #[test]
    fn profiles_existing_http_remembers_consent_and_connected_indicator_is_truthful() {
        let mut app = app();
        app.wait_for("id:connect-profile-connected-active").unwrap();
        assert!(!app.screen().matches("id:connect-profile-connected-unconfirmed").unwrap());
        app.press("id:connect-server-profile-unconfirmed").unwrap();
        app.wait_for("id:connect-profile-confirmation").unwrap();
        assert!(!profiles(&app).get("unconfirmed").unwrap().plaintext_ok);
        app.press("id:connect-profile-confirm").unwrap();
        app.wait_for("id:unsaved-cancel").unwrap();
        assert!(profiles(&app).get("unconfirmed").unwrap().plaintext_ok);
        app.press("id:unsaved-cancel").unwrap();
        for id in ["unconfirmed", "confirmed", "secure"] {
            app.press("id:profiles-test-reopen").unwrap();
            app.wait_for("id:connect-profile-url").unwrap();
            app.press(&format!("id:connect-server-profile-{id}")).unwrap();
            app.wait_for("id:unsaved-cancel").unwrap();
            app.press("id:unsaved-cancel").unwrap();
        }
        app.wait_for(&format!("connection-state:{LOCAL_URL}:true")).unwrap();
    }

    #[test]
    fn profiles_delete_requires_confirmation_and_keeps_current_connection() {
        let mut app = app();
        let original = profiles(&app);
        assert!(!app.screen().matches("label:connect-profile-delete-local").unwrap());
        app.press("label:connect-profile-delete-active").unwrap();
        app.wait_for("id:connect-profile-confirmation").unwrap();
        assert_eq!(profiles(&app), original);
        app.press("id:connect-profile-cancel").unwrap();
        app.wait_for("id:connect-profile-url").unwrap();
        assert_eq!(profiles(&app), original);
        app.press("label:connect-profile-delete-active").unwrap();
        app.wait_for("id:connect-profile-confirmation").unwrap();
        app.press("id:connect-profile-confirm").unwrap();
        app.wait_for("id:connect-profile-url").unwrap();
        app.wait_for_gone("id:connect-profile-row-active", Duration::from_secs(1)).unwrap();
        let remaining = profiles(&app);
        assert_eq!(remaining.profiles.len(), original.profiles.len() - 1);
        assert!(remaining.get("local").is_some());
        assert!(remaining.last_connected.is_none());
        app.wait_for(&format!("connection-state:{LOCAL_URL}:true")).unwrap();
    }

    #[test]
    fn profiles_loopback_http_does_not_require_plaintext_consent() {
        for url in ["http://localhost:59999", "http://127.0.0.1:59999", "http://[::1]:59999"] {
            let mut app = app();
            app.type_text("id:connect-profile-url", url).unwrap();
            app.press("id:connect-profile-add").unwrap();
            app.wait_for("id:unsaved-cancel").unwrap();
            let file = profiles(&app);
            assert_eq!(file.profiles.last().unwrap().url, url);
            assert!(!file.profiles.last().unwrap().plaintext_ok);
        }
    }
}
