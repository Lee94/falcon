//! 就地重命名会话（web 的 RenameDialog）。清空 = 回到自动标题。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::WindowExt;
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, Window, div};
use rust_i18n::t;

use crate::workspace::Workspace;
use crate::zoom::zpx;

pub fn open(ws: &Entity<Workspace>, session_id: &str, window: &mut Window, cx: &mut App) {
    let current = ws
        .read(cx)
        .session(session_id)
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(t!("rename.placeholder").to_string())
            .default_value(current)
    });
    let ws = ws.clone();
    let id = session_id.to_string();
    let focus_input = input.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let input2 = input.clone();
        let ws = ws.clone();
        let id = id.clone();
        dialog
            .title(t!("rename.title").to_string())
            .w(zpx(380.))
            .child(div().child(Input::new(&input)))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                    .child(DialogAction::new().child(Button::new("rename-ok").primary().label(t!("common.confirm").to_string()))),
            )
            .on_ok(move |_, _, cx| {
                let name = input2.read(cx).value().trim().to_string();
                ws.update(cx, |w, cx| w.rename_session(&id, name, cx));
                true
            })
    });
    // 打开对话框会把焦点交给对话框自己的容器，输入框得在那之后再要一次
    focus_input.update(cx, |i, cx| i.focus(window, cx));
}
