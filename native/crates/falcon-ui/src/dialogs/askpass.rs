//! sudo askpass（web 的 AskpassDialog）：宿主机上的 helper 向服务端要密码，服务端经会话 WS
//! 推 `askpass`（目标会话没人看时广播给所有 Viewer），另有 1.5s 的 `/api/askpass/pending`
//! 轮询兜底。对话框只展示队头；答完或取消就出队。

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::WindowExt;
use gpui_kit::prelude::*;
use gpui_kit::{App, Entity, Window, div};
use rust_i18n::t;

use crate::theme::Ui;
use crate::workspace::Workspace;
use crate::zoom::zpx;

pub fn open_head(ws: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let Some(head) = ws.read(cx).askpass.front().cloned() else {
        return;
    };
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(t!("askpass.placeholder").to_string())
    });
    let ws = ws.clone();
    let focus_input = input.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let ui = Ui::global(cx).clone();
        let input2 = input.clone();
        let (ws_ok, ws_cancel) = (ws.clone(), ws.clone());
        let (id_ok, id_cancel) = (head.id.clone(), head.id.clone());
        dialog
            .title(t!("askpass.title").to_string())
            .w(zpx(420.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .font_family(crate::fonts::BERKELEY)
                            .text_color(ui.muted_foreground)
                            .child(head.prompt.clone()),
                    )
                    .child(Input::new(&input)),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
                    .child(DialogAction::new().child(Button::new("askpass-ok").primary().label(t!("askpass.submit").to_string()))),
            )
            .on_ok(move |_, _, cx| {
                let password = input2.read(cx).value().to_string();
                answer(&ws_ok, &id_ok, Some(password), cx);
                true
            })
            .on_cancel(move |_, _, cx| {
                answer(&ws_cancel, &id_cancel, None, cx);
                true
            })
    });
    // 打开对话框会把焦点交给对话框自己的容器，输入框得在那之后再要一次——否则密码会被
    // 敲进原先聚焦的终端，在 shell 里明文回显
    focus_input.update(cx, |i, cx| i.focus(window, cx));
}

fn answer(ws: &Entity<Workspace>, id: &str, password: Option<String>, cx: &mut App) {
    let client = ws.read(cx).client.clone();
    let id = id.to_string();
    let ws = ws.clone();
    cx.spawn(async move |cx| {
        let _ = match password {
            Some(pw) => client.answer_askpass(&id, &pw).await,
            None => client.cancel_askpass(&id).await,
        };
        cx.update(|cx| ws.update(cx, |w, cx| w.shift_askpass(cx)));
    })
    .detach();
}
