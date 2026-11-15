//! 关于页与菜单栏共用的本机更新状态（ADR 0026）。

#[cfg(not(target_arch = "wasm32"))]
use falcon_platform::{AppUpdater, UpdateCheck, UpdateProgress};
use falcon_platform::{AvailableUpdate, InstalledUpdate, UpdateFailure};
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::store::{ActiveDialog, FileGuardAction, SettingsTab, WorkspaceStore};
use crate::theme::rgb_to_hsla;
use crate::zoom::zpx;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Checking,
    UpToDate {
        version: String,
    },
    Available(AvailableUpdate),
    Downloading {
        version: String,
        percent: Option<u8>,
    },
    Installing {
        version: String,
    },
    Installed(InstalledUpdate),
    Failed(UpdateFailure),
}

impl Phase {
    pub(crate) fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Downloading { .. } | Self::Installing { .. }
        )
    }

    fn label(&self) -> String {
        match self {
            Self::Checking => t!("native.update.checking"),
            Self::Downloading { .. } => t!("native.update.downloadingShort"),
            Self::Installing { .. } => t!("native.update.installingShort"),
            Self::Available(update) => {
                t!("native.update.install", version = update.version.as_str())
            }
            Self::Installed(update) if update.relaunch && update.relaunch_bundle.is_some() => {
                t!("native.update.restart")
            }
            _ => t!("native.update.check"),
        }
    }

    fn hint(&self) -> String {
        match self {
            Self::Idle => t!("native.update.intro"),
            Self::Checking => t!("native.update.checking"),
            Self::UpToDate { version } => {
                t!("native.update.upToDateBody", version = version.as_str())
            }
            Self::Available(update) => {
                t!("native.update.available", version = update.version.as_str())
            }
            Self::Downloading {
                version,
                percent: Some(percent),
            } => t!(
                "native.update.downloadProgress",
                version = version.as_str(),
                percent = percent.to_string()
            ),
            Self::Downloading {
                version,
                percent: None,
            } => t!("native.update.downloading", version = version.as_str()),
            Self::Installing { version } => {
                t!("native.update.installing", version = version.as_str())
            }
            Self::Installed(update) => installed_body(update),
            Self::Failed(err) => failure_body(err),
        }
    }
}

pub(crate) fn menu_label(phase: &Phase) -> String {
    match phase {
        Phase::Checking => t!("native.menuBar.checkingUpdate"),
        Phase::Downloading { .. } => t!("native.update.downloadingShort"),
        Phase::Installing { .. } => t!("native.menuBar.installingUpdate"),
        _ => t!("native.menuBar.checkUpdate"),
    }
}

impl WorkspaceStore {
    pub fn check_app_update(&self) {
        if self.app_update.get_untracked().busy() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(updater) = falcon_platform::shared().app_updater() {
            self.check_with_updater(updater);
            return;
        }
        self.app_update.set(Phase::Failed(UpdateFailure::NoPackage {
            platform: std::env::consts::OS.into(),
        }));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn check_with_updater(&self, updater: std::sync::Arc<dyn AppUpdater>) {
        if self.app_update.get_untracked().busy() {
            return;
        }
        self.app_update.set(Phase::Checking);
        let store = self.clone();
        self.spawn_root(async move {
            let result = blocking(move || updater.check(env!("CARGO_PKG_VERSION"))).await;
            let phase = match result {
                Ok(UpdateCheck::UpToDate { version }) => Phase::UpToDate { version },
                Ok(UpdateCheck::Available(update)) => Phase::Available(update),
                Ok(UpdateCheck::Failed(err)) => Phase::Failed(err),
                Err(err) => Phase::Failed(UpdateFailure::Network(err.to_string())),
            };
            store.show_toast(phase.hint());
            store.app_update.set(phase);
        });
    }

    pub(crate) fn on_app_update_action(&self) {
        match self.app_update.get_untracked() {
            Phase::Available(update) => self
                .active_dialog
                .set(ActiveDialog::ConfirmAppUpdate(update)),
            Phase::Installed(update) if update.relaunch && update.relaunch_bundle.is_some() => {
                if let Some(bundle) = update.relaunch_bundle {
                    let file_keys = self.dirty_file_keys();
                    if file_keys.is_empty() {
                        self.complete_update_relaunch(&bundle);
                    } else {
                        self.active_dialog.set(ActiveDialog::ConfirmUnsavedFiles {
                            action: FileGuardAction::RelaunchUpdate(bundle),
                            file_keys,
                        });
                    }
                }
            }
            phase if phase.busy() => {}
            _ => self.check_app_update(),
        }
    }

    pub(crate) fn install_app_update(&self, update: AvailableUpdate) {
        if self.app_update.get_untracked() != Phase::Available(update.clone()) {
            return;
        }
        self.active_dialog
            .set(ActiveDialog::Settings(SettingsTab::About));
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(updater) = falcon_platform::shared().app_updater() {
            self.install_with_updater(updater, update);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn install_with_updater(
        &self,
        updater: std::sync::Arc<dyn AppUpdater>,
        update: AvailableUpdate,
    ) {
        if self.app_update.get_untracked().busy() {
            return;
        }
        self.app_update.set(Phase::Downloading {
            version: update.version.clone(),
            percent: None,
        });
        let store = self.clone();
        self.spawn_root(async move {
            use futures::StreamExt;
            let (tx, mut rx) = futures::channel::mpsc::unbounded();
            let version = update.version.clone();
            let install = blocking(move || {
                updater.install_with_progress(&update, &mut |progress| {
                    let _ = tx.unbounded_send(progress);
                })
            });
            let progress = async {
                while let Some(progress) = rx.next().await {
                    store.app_update.set(match progress {
                        UpdateProgress::Downloading { percent } => Phase::Downloading {
                            version: version.clone(),
                            percent: percent.map(|p| p.min(100)),
                        },
                        UpdateProgress::Installing => Phase::Installing {
                            version: version.clone(),
                        },
                    });
                }
            };
            let (result, ()) = futures::join!(install, progress);
            let phase = match result {
                Ok(Ok(update)) => Phase::Installed(update),
                Ok(Err(err)) => Phase::Failed(err),
                Err(err) => Phase::Failed(UpdateFailure::Install(err.to_string())),
            };
            store.show_toast(phase.hint());
            store.app_update.set(phase);
        });
    }

    pub(crate) fn complete_update_relaunch(&self, bundle: &str) {
        match falcon_platform::shared().schedule_relaunch(std::path::Path::new(bundle)) {
            Ok(()) => {
                #[cfg(not(target_arch = "wasm32"))]
                nsh_core::element::desktop::quit();
            }
            Err(_) => self.show_toast(t!("native.update.relaunchFailed")),
        }
    }
}

fn installed_body(update: &InstalledUpdate) -> String {
    if let Some(path) = &update.staged_path {
        return t!("native.update.staged", path = path.as_str());
    }
    if update.relaunch {
        if let Some(backup) = update.backup_path.as_ref().filter(|s| !s.is_empty()) {
            return t!(
                "native.update.installedRelaunch",
                version = update.version.as_str(),
                backup = backup.as_str()
            );
        }
        return t!(
            "native.update.installedFresh",
            version = update.version.as_str()
        );
    }
    t!(
        "native.update.installedOther",
        version = update.version.as_str()
    )
}

fn failure_body(err: &UpdateFailure) -> String {
    match err {
        UpdateFailure::NoRelease => t!("native.update.noRelease"),
        UpdateFailure::Network(error) => t!("native.update.network", error = error.as_str()),
        UpdateFailure::NoPackage { platform } => {
            t!("native.update.noPackage", platform = platform.as_str())
        }
        UpdateFailure::BadDownloadUrl => t!("native.update.badUrl"),
        UpdateFailure::Install(error) | UpdateFailure::Service(error) => {
            t!("native.update.installFailed", error = error.as_str())
        }
    }
}

pub(crate) fn render(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
) -> impl IntoElement {
    let supported =
        cfg!(not(target_arch = "wasm32")) && falcon_platform::shared().app_updater().is_some();
    render_with_support(store, resolved, supported)
}

fn render_with_support(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    supported: bool,
) -> impl IntoElement {
    let phase = store.app_update.get();
    let enabled = supported && !phase.busy();
    let ui = &resolved.ui;
    let st = store.clone();
    view()
        .flex_col()
        .pt(zpx(32.0))
        .gap(zpx(10.0))
        .child(
            text(t!("native.update.title"))
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            text(if supported {
                phase.hint()
            } else {
                t!("native.update.unsupported")
            })
            .test_id("app-update-status")
            .text_xs()
            .text_color(rgb_to_hsla(if matches!(phase, Phase::Failed(_)) {
                ui.destructive
            } else {
                ui.muted_foreground
            })),
        )
        .when(
            matches!(
                phase,
                Phase::Downloading {
                    percent: Some(_),
                    ..
                }
            ),
            |v| {
                let Phase::Downloading {
                    percent: Some(percent),
                    ..
                } = &phase
                else {
                    return v;
                };
                v.child(
                    view()
                        .test_id("app-update-progress")
                        .h(zpx(4.0))
                        .w(zpx(200.0))
                        .rounded(zpx(2.0))
                        .bg(rgb_to_hsla(ui.secondary))
                        .child(
                            view()
                                .h(zpx(4.0))
                                .w(zpx(2.0 * f32::from(*percent)))
                                .rounded(zpx(2.0))
                                .bg(rgb_to_hsla(ui.tint)),
                        ),
                )
            },
        )
        .child(
            view()
                .test_id("app-update-action")
                .disabled(!enabled)
                .px(zpx(10.0))
                .py(zpx(4.0))
                .rounded(zpx(8.0))
                .bg(rgb_to_hsla(ui.app))
                .opacity(if enabled { 1.0 } else { 0.5 })
                .when(enabled, |v| {
                    v.cursor_pointer()
                        .on_press(move || st.on_app_update_action())
                })
                .child(
                    text(phase.label())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.foreground)),
                ),
        )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use nsh_core::element::Root;
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    fn available() -> AvailableUpdate {
        AvailableUpdate {
            version: "99.0.0".into(),
            asset_name: "Falcon.pkg".into(),
            download_url: "https://github.com/Lee94/falcon/releases/download/v99.0.0/Falcon.pkg"
                .into(),
            installs_local_service: true,
        }
    }

    struct StepUpdater {
        next: Mutex<mpsc::Receiver<()>>,
        checks: AtomicUsize,
        installs: AtomicUsize,
    }

    impl StepUpdater {
        fn wait(&self) {
            self.next
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
    }

    impl AppUpdater for StepUpdater {
        fn check(&self, _: &str) -> UpdateCheck {
            let count = self.checks.fetch_add(1, Ordering::SeqCst);
            self.wait();
            match count {
                0 => UpdateCheck::Available(available()),
                1 => UpdateCheck::UpToDate {
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                _ => UpdateCheck::Failed(UpdateFailure::NoRelease),
            }
        }

        fn install(&self, _: &AvailableUpdate) -> Result<InstalledUpdate, UpdateFailure> {
            Err(UpdateFailure::Install("test failure".into()))
        }

        fn install_with_progress(
            &self,
            _: &AvailableUpdate,
            progress: &mut dyn FnMut(UpdateProgress),
        ) -> Result<InstalledUpdate, UpdateFailure> {
            if self.installs.fetch_add(1, Ordering::SeqCst) != 0 {
                return self.install(&available());
            }
            self.wait();
            progress(UpdateProgress::Downloading { percent: Some(37) });
            self.wait();
            progress(UpdateProgress::Installing);
            self.wait();
            Ok(InstalledUpdate {
                version: "99.0.0".into(),
                backup_path: Some("/backup/Falcon.app".into()),
                relaunch: true,
                relaunch_bundle: Some("/test/Falcon.app".into()),
                staged_path: None,
            })
        }
    }

    #[component]
    fn UpdateHarness() -> impl IntoElement {
        let theme = crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        let store = WorkspaceStore::install(&crate::FalconLaunchOptions {
            offline_demo: true,
            ..Default::default()
        });
        let (next, rx) = mpsc::channel();
        let updater = Arc::new(StepUpdater {
            next: Mutex::new(rx),
            checks: AtomicUsize::new(0),
            installs: AtomicUsize::new(0),
        });
        let check_store = store.clone();
        let check_updater = updater.clone();
        let install_store = store.clone();
        let dirty_store = store.clone();
        let close_store = store.clone();
        let check_again = store.clone();
        let failed_store = store.clone();
        view()
            .flex_col()
            .w(zpx(900.0))
            .h(zpx(700.0))
            .child(
                view()
                    .test_id("check")
                    .on_press(move || check_store.check_with_updater(check_updater.clone()))
                    .child(text("check")),
            )
            .child(
                view()
                    .test_id("next")
                    .on_press(move || {
                        next.send(()).unwrap();
                    })
                    .child(text("next")),
            )
            .child(
                view()
                    .test_id("install")
                    .on_press(move || {
                        install_store.install_with_updater(updater.clone(), available())
                    })
                    .child(text("install")),
            )
            .child(
                view()
                    .test_id("dirty")
                    .on_press(move || {
                        let mut file = crate::store::OpenFileBuffer::from_disk_text(
                            "proj-falcon",
                            "dirty.txt",
                            "old",
                            "base",
                            None,
                        );
                        file.current_body = "changed".into();
                        dirty_store
                            .open_files
                            .set(std::collections::HashMap::from([(
                                "file:dirty".into(),
                                file,
                            )]));
                    })
                    .child(text("dirty")),
            )
            .child(
                view()
                    .test_id("close")
                    .on_press(move || close_store.active_dialog.set(ActiveDialog::None))
                    .child(text("close")),
            )
            .child(
                view()
                    .test_id("reset")
                    .on_press(move || check_again.app_update.set(Phase::Idle))
                    .child(text("reset")),
            )
            .child(
                view()
                    .test_id("fail")
                    .on_press(move || {
                        failed_store
                            .app_update
                            .set(Phase::Failed(UpdateFailure::Network("offline".into())))
                    })
                    .child(text("fail")),
            )
            .child(dynamic(move || {
                view()
                    .child(text(format!("dirty={}", store.dirty_file_keys().len())))
                    .child(text(store.toast_banner.get().unwrap_or_default()))
                    .child(text(menu_label(&store.app_update.get())))
                    .child(render_with_support(&store, &theme.resolved(), true))
            }))
            .child(crate::views::modal_layer())
    }

    fn harness() -> Headless {
        let mut config = EngineConfig::new(
            Root::of::<UpdateHarness>().translations(crate::i18n::translations()),
        );
        config.viewport = Size::new(Points(1024.0), Points(768.0));
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.ui_sets = vec![nsh_base::registry(), crate::registry()];
        let mut app = Headless::start(config).unwrap();
        app.first_render().unwrap();
        app
    }

    #[test]
    fn check_install_progress_and_restart_keep_unsaved_file_guard_headless() {
        let mut app = harness();
        app.press("id:check").unwrap();
        app.wait_for("Checking…").unwrap();
        assert!(!app.screen().enabled("id:app-update-action").unwrap());
        app.press("id:check").unwrap();
        app.press("id:next").unwrap();
        app.wait_for("v99.0.0 is available").unwrap();
        app.press("id:app-update-action").unwrap();
        app.wait_for("id:app-update-install").unwrap();
        app.press("id:app-update-cancel").unwrap();
        app.wait_for_gone("id:app-update-install", Duration::from_secs(2))
            .unwrap();
        app.press("label:settings-close").unwrap();
        app.press("id:install").unwrap();
        app.wait_for("Downloading v99.0.0…").unwrap();
        app.press("id:install").unwrap();
        app.press("id:next").unwrap();
        app.wait_for("Downloading v99.0.0: 37%").unwrap();
        app.wait_for("id:app-update-progress").unwrap();
        assert!(!app.screen().enabled("id:app-update-action").unwrap());
        app.press("id:next").unwrap();
        app.wait_for("Installing v99.0.0…").unwrap();
        app.press("id:next").unwrap();
        app.wait_for("Restart App").unwrap();
        app.press("id:dirty").unwrap();
        app.press("id:app-update-action").unwrap();
        app.wait_for("id:unsaved-cancel").unwrap();
        app.wait_for("dirty.txt").unwrap();
        app.press("id:unsaved-cancel").unwrap();
        app.wait_for("Restart App").unwrap();
        app.press("id:app-update-action").unwrap();
        app.press("id:unsaved-discard").unwrap();
        app.wait_for("The new version is installed but could not be reopened. Quit Falcon and open it from Applications.").unwrap();
        app.wait_for("dirty=1").unwrap();
        app.press("id:reset").unwrap();
        app.press("id:check").unwrap();
        app.press("id:next").unwrap();
        app.wait_for(&format!("Current version: v{}", env!("CARGO_PKG_VERSION")))
            .unwrap();
        app.press("id:check").unwrap();
        app.press("id:next").unwrap();
        app.wait_for("No published release was found").unwrap();
        assert!(app.screen().enabled("id:app-update-action").unwrap());
        app.press("id:install").unwrap();
        app.wait_for("Installation failed: test failure").unwrap();
        app.press("id:fail").unwrap();
        app.wait_for("offline").unwrap();
    }

    #[test]
    fn staged_windows_download_has_no_restart_and_update_translations_match() {
        let staged = Phase::Installed(InstalledUpdate {
            version: "99.0.0".into(),
            backup_path: None,
            relaunch: false,
            relaunch_bundle: None,
            staged_path: Some("C:/Downloads/Falcon.exe".into()),
        });
        assert!(!staged.busy());
        let zh: serde_json::Value =
            serde_json::from_str(include_str!("../locales/zh-CN.json")).unwrap();
        let en: serde_json::Value =
            serde_json::from_str(include_str!("../locales/en.json")).unwrap();
        assert_eq!(
            zh["native"]["update"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            en["native"]["update"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
        for key in [
            "native.update.restart",
            "native.update.downloadProgress",
            "native.update.unsupported",
        ] {
            assert_ne!(
                crate::i18n::translations().lookup("en", key),
                crate::i18n::translations().lookup("zh-CN", key)
            );
        }
    }
}
