//! 检查并安装 GitHub Release（ADR 0026）。
//!
//! 设置「关于」和菜单栏状态项都进这里。检查与安装在后台执行器上跑，结果写回这一份状态，
//! 设置页观察它重画。菜单栏不进 GPUI 的更新周期，只读 [`SNAP`] 上的忙闲。

use std::sync::OnceLock;

use falcon_platform::{AvailableUpdate, InstalledUpdate, UpdateCheck, UpdateFailure};
use gpui_kit::component::notification::Notification;
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, Global, SharedString, Window};
use parking_lot::Mutex;
use rust_i18n::t;

use crate::dialogs;
use crate::toasts::ToastExt;

#[derive(Clone, Debug)]
pub(crate) enum Phase {
    Idle,
    Checking,
    UpToDate { version: String },
    Available(AvailableUpdate),
    Installing { version: String },
    Installed(InstalledUpdate),
    Failed(UpdateFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Snap {
    Idle,
    Checking,
    Installing,
}

static SNAP: Mutex<Snap> = Mutex::new(Snap::Idle);

struct UpdateGlobal(Entity<UpdateModel>);

impl Global for UpdateGlobal {}

pub(crate) struct UpdateModel {
    phase: Phase,
}

fn model(cx: &mut App) -> Entity<UpdateModel> {
    if cx.has_global::<UpdateGlobal>() {
        return cx.global::<UpdateGlobal>().0.clone();
    }
    let model = cx.new(|_| UpdateModel { phase: Phase::Idle });
    cx.set_global(UpdateGlobal(model.clone()));
    model
}

pub(crate) fn phase(cx: &App) -> Phase {
    let Some(global) = cx.try_global::<UpdateGlobal>() else {
        return Phase::Idle;
    };
    // 先把句柄克隆出来。global() 还借着 cx 时再 read(cx) 过不了借用检查
    let model = global.0.clone();
    model.read(cx).phase.clone()
}

fn set_phase(phase: Phase, cx: &mut App) {
    *SNAP.lock() = match &phase {
        Phase::Checking => Snap::Checking,
        Phase::Installing { .. } => Snap::Installing,
        _ => Snap::Idle,
    };
    model(cx).update(cx, |model, cx| {
        model.phase = phase;
        cx.notify();
    });
}

fn busy() -> bool {
    !matches!(*SNAP.lock(), Snap::Idle)
}

/// 菜单栏上的「检查更新」。窗口都关着时先开一扇，确认框和通知才有地方画
pub fn check_for_app_update(cx: &mut App) {
    if !crate::window::present(cx) {
        crate::open_startup_window(cx);
    }
    start_check(cx);
}

/// 菜单项文案与能不能点。给 AppKit 菜单用，不经过 GPUI 的更新
pub fn app_update_menu_state() -> (String, bool) {
    match *SNAP.lock() {
        Snap::Checking => (t!("native.menuBar.checkingUpdate").to_string(), false),
        Snap::Installing => (t!("native.menuBar.installingUpdate").to_string(), false),
        Snap::Idle => (t!("native.menuBar.checkUpdate").to_string(), true),
    }
}

pub(crate) fn on_about_action(window: &mut Window, cx: &mut App) {
    match phase(cx) {
        Phase::Checking | Phase::Installing { .. } => {}
        Phase::Available(update) => open_confirm(update, window, cx),
        _ => start_check(cx),
    }
}

pub(crate) fn observe(cx: &mut App) -> Entity<UpdateModel> {
    model(cx)
}

fn start_check(cx: &mut App) {
    if busy() {
        return;
    }
    let Some(updater) = falcon_platform::get(cx).app_updater() else {
        set_phase(Phase::Failed(UpdateFailure::NoPackage { platform: std::env::consts::OS.into() }), cx);
        notify(cx, NoteKind::Danger, t!("native.update.failed").to_string(), Some(failure_body(&UpdateFailure::NoPackage {
            platform: std::env::consts::OS.into(),
        })), true);
        return;
    };
    set_phase(Phase::Checking, cx);
    notify(cx, NoteKind::Info, t!("native.update.checking").to_string(), None, false);
    let current = env!("CARGO_PKG_VERSION").to_string();
    cx.spawn(async move |cx| {
        let result = cx.background_executor().spawn(async move { updater.check(&current) }).await;
        cx.update(|cx| apply_check(result, cx));
    })
    .detach();
}

fn apply_check(result: UpdateCheck, cx: &mut App) {
    match result {
        UpdateCheck::UpToDate { version } => {
            set_phase(Phase::UpToDate { version: version.clone() }, cx);
            notify(
                cx,
                NoteKind::Success,
                t!("native.update.upToDate").to_string(),
                Some(t!("native.update.upToDateBody", version = version).to_string()),
                false,
            );
        }
        UpdateCheck::Available(update) => {
            set_phase(Phase::Available(update.clone()), cx);
            prompt_install(update, cx);
        }
        UpdateCheck::Failed(err) => {
            let body = failure_body(&err);
            set_phase(Phase::Failed(err), cx);
            notify(cx, NoteKind::Danger, t!("native.update.failed").to_string(), Some(body), true);
        }
    }
}

fn prompt_install(update: AvailableUpdate, cx: &mut App) {
    if cx.windows().is_empty() {
        crate::open_startup_window(cx);
    }
    let Some(handle) = cx.windows().into_iter().next() else {
        log::warn!("没有窗口，无法确认安装");
        return;
    };
    handle
        .update(cx, move |_, window, cx| open_confirm(update, window, cx))
        .ok();
}

fn open_confirm(update: AvailableUpdate, window: &mut Window, cx: &mut App) {
    let title = t!("native.update.confirmTitle", version = update.version.clone()).to_string();
    let body = confirm_body(&update);
    let confirm_label = t!("native.update.confirmOk").to_string();
    // confirm 的回调是 Fn，不能把 update 移走。确认只会走一次，busy() 挡掉重复安装
    dialogs::confirm(
        dialogs::ConfirmOpts { title, body, confirm_label, ..Default::default() },
        move |_, cx| start_install(update.clone(), cx),
        window,
        cx,
    );
}

fn confirm_body(update: &AvailableUpdate) -> String {
    if update.installs_local_service {
        t!("native.update.confirmBody", asset = update.asset_name.clone()).to_string()
    } else {
        t!("native.update.confirmBodyDownload", asset = update.asset_name.clone()).to_string()
    }
}

fn start_install(update: AvailableUpdate, cx: &mut App) {
    if busy() {
        return;
    }
    let Some(updater) = falcon_platform::get(cx).app_updater() else {
        return;
    };
    let version = update.version.clone();
    set_phase(Phase::Installing { version: version.clone() }, cx);
    notify(cx, NoteKind::Info, t!("native.update.installing", version = version).to_string(), None, true);
    cx.spawn(async move |cx| {
        let result = cx.background_executor().spawn(async move { updater.install(&update) }).await;
        cx.update(|cx| apply_install(result, cx));
    })
    .detach();
}

fn apply_install(result: Result<InstalledUpdate, UpdateFailure>, cx: &mut App) {
    match result {
        Ok(update) => {
            let bundle = update.relaunch_bundle.clone();
            let body = installed_body(&update);
            set_phase(Phase::Installed(update), cx);
            notify(cx, NoteKind::Success, t!("native.update.installedTitle").to_string(), Some(body), true);
            if let Some(bundle) = bundle {
                let footnote = t!("native.update.relaunchFootnote").to_string();
                crate::window::quit_and_then(
                    Some(footnote),
                    move |cx| match falcon_platform::get(cx).schedule_relaunch(std::path::Path::new(&bundle)) {
                        Ok(()) => cx.quit(),
                        Err(err) => {
                            log::warn!("退出后重新打开失败：{err}");
                            notify(
                                cx,
                                NoteKind::Danger,
                                t!("native.update.failed").to_string(),
                                Some(t!("native.update.relaunchFailed").to_string()),
                                true,
                            );
                        }
                    },
                    cx,
                );
            }
        }
        Err(err) => {
            let body = failure_body(&err);
            set_phase(Phase::Failed(err), cx);
            notify(cx, NoteKind::Danger, t!("native.update.failed").to_string(), Some(body), true);
        }
    }
}

pub(crate) fn status_hint(phase: &Phase) -> Option<String> {
    Some(match phase {
        Phase::Idle => return None,
        Phase::Checking => t!("native.update.checking").to_string(),
        Phase::UpToDate { version } => t!("native.update.upToDateBody", version = version.clone()).to_string(),
        Phase::Available(update) => t!("native.update.available", version = update.version.clone()).to_string(),
        Phase::Installing { version } => t!("native.update.installing", version = version.clone()).to_string(),
        Phase::Installed(update) => installed_body(update),
        Phase::Failed(err) => failure_body(err),
    })
}

pub(crate) fn button_label(phase: &Phase) -> String {
    match phase {
        Phase::Checking => t!("native.update.checking").to_string(),
        Phase::Installing { .. } => t!("native.update.installingShort").to_string(),
        Phase::Available(update) => t!("native.update.install", version = update.version.clone()).to_string(),
        _ => t!("native.update.check").to_string(),
    }
}

pub(crate) fn button_enabled(phase: &Phase) -> bool {
    !matches!(phase, Phase::Checking | Phase::Installing { .. })
}

fn installed_body(update: &InstalledUpdate) -> String {
    if let Some(path) = &update.staged_path {
        return t!("native.update.staged", path = path.clone()).to_string();
    }
    if update.relaunch {
        let backup = update.backup_path.clone().unwrap_or_default();
        if backup.is_empty() {
            return t!("native.update.installedFresh", version = update.version.clone()).to_string();
        }
        return t!("native.update.installedRelaunch", version = update.version.clone(), backup = backup).to_string();
    }
    t!("native.update.installedOther", version = update.version.clone()).to_string()
}

fn failure_body(err: &UpdateFailure) -> String {
    match err {
        UpdateFailure::NoRelease => t!("native.update.noRelease").to_string(),
        UpdateFailure::Network(message) => t!("native.update.network", error = message.clone()).to_string(),
        UpdateFailure::NoPackage { platform } => t!("native.update.noPackage", platform = platform.clone()).to_string(),
        UpdateFailure::BadDownloadUrl => t!("native.update.badUrl").to_string(),
        UpdateFailure::Install(message) | UpdateFailure::Service(message) => message.clone(),
    }
}

enum NoteKind {
    Info,
    Success,
    Danger,
}

struct AppUpdateNote;

fn notify(cx: &mut App, kind: NoteKind, title: String, body: Option<String>, sticky: bool) {
    static KEY: OnceLock<SharedString> = OnceLock::new();
    let key = KEY.get_or_init(|| SharedString::from("app-update")).clone();
    let Some(handle) = cx.windows().into_iter().next() else {
        log::info!("更新：{title} {}", body.as_deref().unwrap_or(""));
        return;
    };
    handle
        .update(cx, move |_, window, cx| {
            let title_text: SharedString = title.into();
            let mut note = match kind {
                NoteKind::Info => Notification::info(title_text.clone()),
                NoteKind::Success => Notification::success(title_text.clone()),
                NoteKind::Danger => Notification::error(title_text.clone()),
            };
            note = note.id1::<AppUpdateNote>(key.clone());
            if let Some(body) = body {
                note = note.title(title_text).message(body);
            }
            if sticky {
                note = note.autohide(false);
            }
            window.push_keyed_toast(key, note, cx);
        })
        .ok();
}
