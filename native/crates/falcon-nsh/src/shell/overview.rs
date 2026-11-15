//! 会话总览：与 GPUI 相同的三种会话状态卡、筛选与按项目定位的会话表。
use crate::{
    i18n::t,
    store::{ActiveDialog, use_workspace},
    theme::{rgb_to_hsla, rgba_to_hsla, use_theme},
    zoom::zpx,
};
use falcon_proto::SessionState;
use nsh_core::prelude::*;

#[component]
pub fn SessionOverview() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let filter = signal(None::<SessionState>);
    dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let sessions = store.sessions.get();
        let projects = store.projects.get();
        let mut cards = view().flex_row().gap(zpx(12.0));
        for (state, key) in [
            (SessionState::Active, "session.state_active"),
            (SessionState::Unverified, "session.state_unverified"),
            (SessionState::Dead, "session.state_dead"),
        ] {
            let count = sessions.iter().filter(|s| s.state == state).count();
            cards =
                cards.child(
                    view()
                        .w(zpx(176.0))
                        .min_h(zpx(84.0))
                        .px(zpx(14.0))
                        .py(zpx(12.0))
                        .rounded(zpx(10.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(if filter.get() == Some(state) {
                            ui.tint
                        } else {
                            ui.app
                        }))
                        .on_press(move || {
                            filter
                                .update(|f| *f = if *f == Some(state) { None } else { Some(state) })
                        })
                        .child(
                            view()
                                .flex_row()
                                .items_center()
                                .gap(zpx(6.0))
                                .child(view().w(zpx(6.0)).h(zpx(6.0)).rounded_full().bg(
                                    rgb_to_hsla(match state {
                                        SessionState::Active => ui.success,
                                        SessionState::Unverified => ui.warning,
                                        _ => ui.destructive,
                                    }),
                                ))
                                .child(
                                    text(t!(key))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                                ),
                        )
                        .child(
                            text(count.to_string())
                                .text_size(zpx(28.0))
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                );
        }
        let mut table = view()
            .flex_col()
            .rounded(zpx(10.0))
            .overflow_hidden()
            .bg(rgb_to_hsla(ui.app))
            .gap(zpx(2.0))
            .child(
                view()
                    .h(zpx(36.0))
                    .flex_row()
                    .items_center()
                    .gap(zpx(16.0))
                    .px(zpx(12.0))
                    .border_b_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .child(
                        text(t!("overview.session"))
                            .flex_1()
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(
                        text(t!("overview.where"))
                            .flex_1()
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(
                        text(t!("overview.state"))
                            .w(zpx(92.0))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(
                        text(t!("overview.durability"))
                            .w(zpx(120.0))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    ),
            );
        for session in sessions
            .iter()
            .filter(|s| filter.get().is_none_or(|state| s.state == state))
        {
            let st = store.clone();
            let id = session.id.clone();
            let location = projects
                .iter()
                .find(|p| p.id == session.project_id)
                .map(|p| {
                    format!(
                        "{} · {}",
                        p.name,
                        p.host_name.clone().unwrap_or_else(|| t!("forward.local"))
                    )
                })
                .unwrap_or_else(|| t!("sidebar.standalone"));
            table = table.child(
                view()
                    .label(format!("overview-session-{id}"))
                    .h(zpx(44.0))
                    .flex_row()
                    .items_center()
                    .gap(zpx(16.0))
                    .px(zpx(12.0))
                    .rounded(zpx(8.0))
                    .cursor_pointer()
                    .hover(|v| v.bg(rgb_to_hsla(ui.tint)))
                    .on_press(move || st.focus_session(&id))
                    .child(
                        text(session.display_title())
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .number_of_lines(1)
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .child(
                        text(location)
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .number_of_lines(1)
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(
                        text(t!(match session.state {
                            SessionState::Active => "session.state_active",
                            SessionState::Unverified => "session.state_unverified",
                            SessionState::Dead => "session.state_dead",
                            SessionState::Unknown => "session.state_unknown",
                        }))
                        .w(zpx(92.0))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                    )
                    .child(
                        text(if session.durable {
                            t!("session.durable")
                        } else {
                            t!("session.nonDurable")
                        })
                        .w(zpx(120.0))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                    ),
            );
        }
        let create = store.clone();
        scroll()
            .label("session-overview")
            .size_full()
            .rounded(zpx(12.0))
            .bg(rgb_to_hsla(ui.background))
            .child(
                view()
                    .p(zpx(24.0))
                    .flex_col()
                    .gap(zpx(20.0))
                    .child(
                        text(t!("overview.title"))
                            .text_size(zpx(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb_to_hsla(ui.foreground)),
                    )
                    .child(cards)
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.0))
                            .child(
                                text(t!("overview.filter"))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                            .child(
                                view()
                                    .px(zpx(8.0))
                                    .py(zpx(4.0))
                                    .rounded(zpx(6.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.muted))
                                    .on_press(move || filter.set(None))
                                    .child(
                                        text(t!("overview.all"))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.foreground)),
                                    ),
                            ),
                    )
                    .child(table)
                    .when(sessions.is_empty(), |v| {
                        v.child(
                            view()
                                .py(zpx(24.0))
                                .cursor_pointer()
                                .on_press(move || {
                                    create.active_dialog.set(ActiveDialog::NewProject)
                                })
                                .child(
                                    text(t!("overview.emptyNoSessionTitle"))
                                        .text_sm()
                                        .text_color(rgb_to_hsla(ui.foreground)),
                                )
                                .child(
                                    text(t!("overview.emptyNoSessionBody"))
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                                ),
                        )
                    }),
            )
            .into_element()
    })
}
