//! 会话总览：状态 / 项目筛选、可见行选择与批量操作。
use crate::{
    i18n::t,
    shell::{
        popups::themed_tooltip,
        widgets::{Glyph, glyph, icon_button},
    },
    store::{ActiveDialog, MobileTab, WorkspaceStore, use_workspace},
    theme::{rgb_to_hsla, rgba_to_hsla, use_theme},
    zoom::zpx,
};
use falcon_proto::SessionState;
use nsh_core::prelude::*;

impl WorkspaceStore {
    pub(crate) fn reset_overview_filter(&self) {
        self.overview_filter.set(None);
        self.overview_project.set(None);
        self.overview_selected.set(Vec::new());
    }

    pub(crate) fn filter_overview_project(&self, project_id: &str) {
        self.reset_overview_filter();
        self.overview_project.set(Some(project_id.to_string()));
        self.overview_open.set(true);
        self.mobile_tab.set(MobileTab::Workspace);
    }

    fn request_terminate_many(&self) {
        let selected = self.overview_selected.get();
        let session_ids: Vec<_> = self
            .sessions
            .get()
            .iter()
            .filter(|session| selected.contains(&session.id))
            .map(|session| session.id.clone())
            .collect();
        if !session_ids.is_empty() {
            self.active_dialog.set(ActiveDialog::ConfirmTerminateMany {
                session_ids,
                connection_generation: self.connection_generation(),
            });
        }
    }

    fn request_clear_all_dead(&self) {
        let session_ids: Vec<_> = self
            .sessions
            .get()
            .iter()
            .filter(|session| session.state == SessionState::Dead)
            .map(|session| session.id.clone())
            .collect();
        if session_ids.is_empty() {
            self.show_toast(t!("overview.noDead"));
        } else {
            self.active_dialog.set(ActiveDialog::ConfirmClearAllDead {
                session_ids,
                connection_generation: self.connection_generation(),
            });
        }
    }

    fn confirm_overview_batch(&self, dialog: &ActiveDialog) {
        if self.active_dialog.get() != *dialog {
            return;
        }
        let (session_ids, generation, clear) = match dialog {
            ActiveDialog::ConfirmTerminateMany {
                session_ids,
                connection_generation,
            } => (session_ids, *connection_generation, false),
            ActiveDialog::ConfirmClearAllDead {
                session_ids,
                connection_generation,
            } => (session_ids, *connection_generation, true),
            _ => return,
        };
        if self.connection_generation() != generation || !self.server_connected.get() {
            self.active_dialog.set(ActiveDialog::None);
            self.show_toast(t!("session.notConnected"));
            return;
        }
        self.active_dialog.set(ActiveDialog::None);
        self.overview_selected.set(Vec::new());
        for session in self
            .sessions
            .get()
            .iter()
            .filter(|session| session_ids.contains(&session.id))
        {
            if clear {
                // A record may have been reattached while its confirmation was open.
                if session.state == SessionState::Dead {
                    self.request_clear_session_record(&session.id);
                }
            } else {
                self.force_terminate_session(&session.id);
            }
        }
    }
}

fn selection_checkbox(
    id: impl Into<SharedString>,
    checked: bool,
    mixed: bool,
    title: String,
    resolved: &falcon_theme::ResolvedTheme,
    toggle: impl Fn() + 'static,
) -> impl IntoElement {
    let ui = &resolved.ui;
    let id = id.into();
    nsh_base::checkbox()
        .accessibility_label(title.clone())
        .checked(checked)
        .indeterminate(mixed)
        .w(zpx(24.0))
        .h(zpx(24.0))
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(zpx(4.0))
        .border_1()
        .border_color(rgba_to_hsla(ui.border))
        .bg(rgb_to_hsla(if checked || mixed {
            ui.tint
        } else {
            ui.background
        }))
        .text_color(rgb_to_hsla(ui.foreground))
        .cursor_pointer()
        .on_change(move |_: &bool| toggle())
        .child(
            view()
                .test_id(&id)
                .size_full()
                .items_center()
                .justify_center()
                .tooltip(themed_tooltip(text(title), None, resolved))
                .when(checked || mixed, |v| {
                    v.child(glyph(if mixed { Glyph::Minus } else { Glyph::Check }))
                }),
        )
}

pub(crate) fn render_batch_confirmation(
    store: &WorkspaceStore,
    resolved: &falcon_theme::ResolvedTheme,
    dialog: ActiveDialog,
) -> AnyElement {
    let (ids, clear) = match &dialog {
        ActiveDialog::ConfirmTerminateMany { session_ids, .. } => (session_ids, false),
        ActiveDialog::ConfirmClearAllDead { session_ids, .. } => (session_ids, true),
        _ => return view().into_element(),
    };
    let ui = &resolved.ui;
    let cancel = store.clone();
    let confirm = store.clone();
    let sessions = store.sessions.get();
    let projects = store.projects.get();
    let mut list = view().flex_col().gap(zpx(8.0));
    for session in sessions.iter().filter(|session| ids.contains(&session.id)) {
        let project = projects
            .iter()
            .find(|project| project.id == session.project_id);
        list = list.child(
            view()
                .label(format!("overview-confirm-session-{}", session.id))
                .flex_col()
                .gap(zpx(4.0))
                .p(zpx(10.0))
                .rounded(zpx(6.0))
                .bg(rgb_to_hsla(ui.app))
                .child(
                    text(session.display_title())
                        .text_sm()
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(
                    text(format!(
                        "{} · {}",
                        project
                            .map(|project| project.name.clone())
                            .unwrap_or_else(|| t!("sidebar.standalone")),
                        t!(match session.state {
                            SessionState::Active => "session.state_active",
                            SessionState::Unverified => "session.state_unverified",
                            SessionState::Dead => "session.state_dead",
                            SessionState::Unknown => "session.state_unknown",
                        })
                    ))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
                ),
        );
    }
    view()
        .label("overview-batch-confirmation")
        .p(zpx(24.0))
        .flex_col()
        .min_h_0()
        .gap(zpx(16.0))
        .child(
            text(if clear {
                t!("native.overview.clearAllDeadTitle", n = ids.len())
            } else {
                t!("session.terminateManyTitle", n = ids.len())
            })
            .text_lg()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb_to_hsla(ui.foreground)),
        )
        .child(
            text(if clear {
                t!("native.overview.clearAllDeadBody")
            } else {
                t!("session.terminateManyBody")
            })
            .text_sm()
            .text_color(rgb_to_hsla(ui.muted_foreground)),
        )
        .child(scroll().max_h(zpx(280.0)).min_h_0().child(list))
        .when(clear, |v| {
            v.child(
                text(t!("native.overview.clearAllDeadFootnote"))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            )
        })
        .child(
            view()
                .flex_row()
                .justify_end()
                .gap(zpx(8.0))
                .child(
                    view()
                        .label("overview-batch-cancel")
                        .focusable(true)
                        .px(zpx(12.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(ui.secondary))
                        .on_press(move || cancel.active_dialog.set(ActiveDialog::None))
                        .child(
                            text(t!("common.cancel"))
                                .text_sm()
                                .text_color(rgb_to_hsla(ui.secondary_foreground)),
                        ),
                )
                .child(
                    view()
                        .label("overview-batch-confirm")
                        .focusable(true)
                        .px(zpx(12.0))
                        .py(zpx(6.0))
                        .rounded(zpx(6.0))
                        .cursor_pointer()
                        .bg(rgb_to_hsla(if clear { ui.primary } else { ui.destructive }))
                        .on_press(move || confirm.confirm_overview_batch(&dialog))
                        .child(
                            text(if clear {
                                t!("native.overview.clearAllDeadConfirm")
                            } else {
                                t!("session.terminateManyConfirm")
                            })
                            .text_sm()
                            .text_color(rgb_to_hsla(if clear {
                                ui.primary_foreground
                            } else {
                                ui.destructive_foreground
                            })),
                        ),
                ),
        )
        .into_element()
}

#[component]
pub fn SessionOverview() -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let filter = store.overview_filter;
    let selected = store.overview_selected;
    let project_filter = store.overview_project;
    let cleanup = store.clone();
    effect(move || {
        let sessions = cleanup.sessions.get();
        let picked = selected.get_untracked();
        let retained: Vec<_> = picked
            .iter()
            .filter(|id| sessions.iter().any(|s| &s.id == *id))
            .cloned()
            .collect();
        if retained != picked {
            selected.set(retained);
        }
    });
    dynamic(move || {
        let resolved = theme.resolved();
        let ui = &resolved.ui;
        let sessions = store.sessions.get();
        let projects = store.projects.get();
        let rows: Vec<_> = sessions
            .iter()
            .filter(|session| {
                filter.get().is_none_or(|state| session.state == state)
                    && project_filter
                        .get()
                        .as_ref()
                        .is_none_or(|id| &session.project_id == id)
            })
            .collect();
        let picked = selected.get();
        let all_selected =
            !rows.is_empty() && rows.iter().all(|session| picked.contains(&session.id));
        let mixed = !all_selected && rows.iter().any(|session| picked.contains(&session.id));
        let visible_ids: Vec<_> = rows.iter().map(|session| session.id.clone()).collect();
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
                        .label(format!("overview-card-{}", state.as_str()))
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
                            selected.set(Vec::new());
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
                    .child(selection_checkbox(
                        "overview-select-all",
                        all_selected,
                        mixed,
                        t!("overview.selectAll"),
                        &resolved,
                        move || {
                            selected.set(if all_selected {
                                Vec::new()
                            } else {
                                visible_ids.clone()
                            })
                        },
                    ))
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
        for session in &rows {
            let st = store.clone();
            let id = session.id.clone();
            let toggle_id = id.clone();
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
                    .label(format!("overview-row-{id}"))
                    .h(zpx(44.0))
                    .flex_row()
                    .items_center()
                    .gap(zpx(16.0))
                    .px(zpx(12.0))
                    .rounded(zpx(8.0))
                    .hover(|v| v.bg(rgb_to_hsla(ui.tint)))
                    .child(selection_checkbox(
                        format!("overview-select-{id}"),
                        picked.contains(&id),
                        false,
                        t!("overview.selectOne"),
                        &resolved,
                        move || {
                            selected.update(|ids| {
                                if ids.contains(&toggle_id) {
                                    ids.retain(|id| id != &toggle_id);
                                } else {
                                    ids.push(toggle_id.clone());
                                }
                            })
                        },
                    ))
                    // The open target is a sibling, so a checkbox press cannot open the session.
                    .child(
                        view()
                            .label(format!("overview-session-{id}"))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex_row()
                            .items_center()
                            .gap(zpx(16.0))
                            .when(session.state != SessionState::Dead, |v| {
                                v.cursor_pointer().on_press(move || st.focus_session(&id))
                            })
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
                    ),
            );
        }
        let create = store.clone();
        let reset = store.clone();
        let terminate = store.clone();
        let clear = store.clone();
        let clear_project = store.clone();
        let project_name = project_filter.get().map(|id| {
            projects
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or(id)
        });
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
                                text(
                                    filter
                                        .get()
                                        .map(|state| {
                                            t!(match state {
                                                SessionState::Active => "session.state_active",
                                                SessionState::Unverified =>
                                                    "session.state_unverified",
                                                SessionState::Dead => "session.state_dead",
                                                SessionState::Unknown => "session.state_unknown",
                                            })
                                        })
                                        .unwrap_or_else(|| t!("overview.filterAll")),
                                )
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                            )
                            .when(project_name.is_some(), |v| {
                                v.child(
                                    view()
                                        .label("overview-project-filter")
                                        .flex_row()
                                        .items_center()
                                        .gap(zpx(4.0))
                                        .child(
                                            text(project_name.unwrap_or_default())
                                                .text_xs()
                                                .text_color(rgb_to_hsla(ui.foreground)),
                                        )
                                        .child(icon_button(
                                            "overview-clear-project",
                                            t!("native.overview.clearProjectFilter"),
                                            Glyph::Close,
                                            false,
                                            &resolved,
                                            move || {
                                                clear_project.overview_project.set(None);
                                                clear_project.overview_selected.set(Vec::new());
                                            },
                                        )),
                                )
                            })
                            .child(
                                view()
                                    .label("overview-reset-filter")
                                    .px(zpx(8.0))
                                    .py(zpx(4.0))
                                    .rounded(zpx(6.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.muted))
                                    .on_press(move || reset.reset_overview_filter())
                                    .child(
                                        text(t!("overview.all"))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.foreground)),
                                    ),
                            ),
                    )
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(8.0))
                            .when(!picked.is_empty(), |v| {
                                v.child(
                                    view()
                                        .label("overview-terminate-selected")
                                        .focusable(true)
                                        .px(zpx(10.0))
                                        .py(zpx(6.0))
                                        .rounded(zpx(6.0))
                                        .cursor_pointer()
                                        .bg(rgb_to_hsla(ui.destructive))
                                        .on_press(move || terminate.request_terminate_many())
                                        .child(
                                            text(t!(
                                                "overview.terminateSelected",
                                                n = picked.len()
                                            ))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.destructive_foreground)),
                                        ),
                                )
                            })
                            .child(
                                view()
                                    .label("overview-clear-dead")
                                    .focusable(true)
                                    .px(zpx(10.0))
                                    .py(zpx(6.0))
                                    .rounded(zpx(6.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.secondary))
                                    .on_press(move || clear.request_clear_all_dead())
                                    .child(
                                        text(t!("overview.clearAllDead"))
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                    ),
                            ),
                    )
                    .child(table)
                    .when(rows.is_empty() && !sessions.is_empty(), |v| {
                        v.child(
                            text(t!("overview.emptyFilteredTitle"))
                                .text_sm()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                        )
                    })
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
