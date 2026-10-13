//! 浮层：确认框、重命名、askpass、持久会话的授权与部署、各种表单与设置。
//!
//! 一律由 gpui-component 的 `window.open_dialog` 打开，Esc 只关最上面那一层（旧 React 版的全局 Esc
//! 分发在这里由 Root 的对话框栈负责）。

pub mod askpass;
pub mod connect;
pub mod folder_picker;
pub mod host_form;
pub mod install;
pub mod overview;
pub mod project_form;
pub mod quick_open;
pub mod rename;
pub mod settings;
pub mod theme_picker;
pub mod worktree_form;

use falcon_proto::SessionState;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, SharedString, Window, div};
use rust_i18n::t;

use crate::theme::Ui;
use crate::toasts::ToastExt;
use crate::ui::{Mark, status_mark};
use crate::workspace::{ProgramAlertSpec, Toast, ToastKind, Workspace};
use crate::zoom::zpx;

/// 确认框里的一行（被波及的会话 / 目录 / 脏文件）
#[derive(Clone, Debug)]
pub struct ConfirmItem {
    pub name: String,
    pub state: Option<SessionState>,
    pub meta: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ConfirmOpts {
    pub title: String,
    pub body: String,
    pub list: Vec<ConfirmItem>,
    pub footnote: Option<String>,
    pub confirm_label: String,
    pub danger: bool,
}

/// 通用确认框（React 版的 ConfirmDialog）：标题、正文、被波及的清单、脚注、确认 / 取消。
pub fn confirm(
    opts: ConfirmOpts,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let on_confirm = std::rc::Rc::new(on_confirm);
    window.open_dialog(cx, move |dialog, _, cx| {
        let ui = Ui::global(cx).clone();
        let on_confirm = on_confirm.clone();
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .text_sm()
            .child(div().text_color(ui.muted_foreground).child(opts.body.clone()));
        if !opts.list.is_empty() {
            let mut list = div()
                .flex()
                .flex_col()
                .gap_1()
                .max_h(zpx(220.))
                .overflow_hidden()
                .rounded(zpx(8.))
                .bg(ui.app)
                .p_2();
            for item in &opts.list {
                let mut row = div().flex().items_center().gap_2().text_xs();
                if let Some(state) = item.state {
                    row = row.child(status_mark(Mark::from(state), zpx(12.), cx));
                }
                row = row.child(div().flex_1().truncate().child(item.name.clone()));
                if let Some(meta) = &item.meta {
                    row = row.child(div().text_color(ui.muted_foreground).child(meta.clone()));
                }
                list = list.child(row);
            }
            body = body.child(list);
        }
        if let Some(foot) = &opts.footnote {
            body = body.child(div().text_xs().text_color(ui.muted_foreground).child(foot.clone()));
        }
        let ok = Button::new("confirm-ok").label(SharedString::from(opts.confirm_label.clone()));
        let ok = if opts.danger { ok.danger() } else { ok.primary() };
        dialog
            .title(SharedString::from(opts.title.clone()))
            .w(zpx(460.))
            .child(body)
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                    .child(DialogAction::new().child(ok)),
            )
            .on_ok(move |_, window, cx| {
                on_confirm(window, cx);
                true
            })
    });
}

/// 程序状态提醒的通知 id（`id1` 用）：同一会话的提醒原地替换
struct ProgramAlertNote;

/// 某个会话卡在 blocked（ADR 0020 决定六）：弹一条不自动消失的提醒，带「切过去」；它不卡了
/// （`alert` 为 None）就撤掉。按会话 id 记 key，同一会话的新提醒原地替换，不叠一摞
pub fn show_program_alert(
    ws: &gpui_kit::Entity<Workspace>,
    session_id: &str,
    alert: Option<&ProgramAlertSpec>,
    window: &mut Window,
    cx: &mut App,
) {
    let key: SharedString = format!("program-alert-{session_id}").into();
    let Some(alert) = alert else {
        window.dismiss_toast(&key, cx);
        return;
    };
    let title: SharedString = alert.title.clone().into();
    let mut n = Notification::warning(title.clone()).id1::<ProgramAlertNote>(key.clone()).autohide(false);
    if let Some(body) = &alert.body {
        n = n.title(title).message(body.clone());
    }
    let (ws, sid) = (ws.clone(), session_id.to_string());
    n = n.action(move |_, _, cx| {
        let (ws, sid) = (ws.clone(), sid.clone());
        let note = cx.entity();
        Button::new("program-alert-go")
            .small()
            .primary()
            .label(t!("programStatus.toastGo").to_string())
            .on_click(move |_, window, cx| {
                ws.update(cx, |w, cx| w.open_session(&sid, cx));
                note.update(cx, |n, cx| n.dismiss(window, cx));
            })
    });
    window.push_keyed_toast(key, n, cx);
}

/// 工作区事件里的 toast → gpui-component 的通知
pub fn show_toast(toast: &Toast, window: &mut Window, cx: &mut App) {
    let title: SharedString = toast.title.clone().into();
    let mut n = match toast.kind {
        ToastKind::Info => Notification::info(title.clone()),
        ToastKind::Success => Notification::success(title.clone()),
        ToastKind::Warning => Notification::warning(title.clone()),
        ToastKind::Danger => Notification::error(title.clone()),
    };
    if let Some(body) = &toast.body {
        n = n.title(title).message(body.clone());
    }
    // 警告类（残留目录、失败）与标了 sticky 的（git 失败的原话）要人看见，不自动消失
    if toast.sticky || matches!(toast.kind, ToastKind::Warning) {
        n = n.autohide(false);
    }
    window.push_toast(n, cx);
}
