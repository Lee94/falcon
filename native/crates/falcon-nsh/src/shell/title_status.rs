//! OSC 7501 titlebar slots: chip → subtasks → session name → cwd (ADR 0020).
//! Metadata polling updates only these small components; animations bind directly to native nodes.

use std::time::Duration;

use falcon_core::program_status::{
    self, BadgeTone, ClockReading, ProgramBadge, StateClock, SubTask,
};
use falcon_proto::{ProgramBlockKind, ProgramStatusSummary, SessionState};
use nsh_core::prelude::*;

use super::motion::StatusMotion;
use super::popups::themed_tooltip;
use super::widgets::{Glyph, glyph_sized};
use crate::i18n::t;
use crate::store::use_workspace;
use crate::theme::{rgb_to_hsla, use_theme};
use crate::zoom::zpx;

#[derive(Clone, PartialEq, Props)]
pub struct TitleStatusProps {
    pub session_id: String,
    #[prop(default)]
    pub edge: bool,
}

#[component]
pub fn TitleStatus(props: TitleStatusProps) -> impl IntoElement {
    let store = use_workspace();
    let theme = use_theme();
    let session = store.ensure_terminal_session(&props.session_id);
    let runtime = signal(session.meta());
    if !props.edge {
        let focused = store.focused_pane_key;
        let key = falcon_core::pane_key::term_key(&props.session_id);
        let seen_session = session.clone();
        // A first/reconnect snapshot can arrive after focus_session already acknowledged REST.
        effect(move || {
            if focused.get() == key && program_status::has_finished(&runtime.get().program_status) {
                seen_session.mark_status_seen();
                runtime.set(seen_session.meta());
            }
        });
    }
    set_interval(Duration::from_millis(100), move || {
        let next = session.meta();
        if runtime.get_untracked() != next {
            runtime.set(next);
        }
    });
    let clock = stored(StateClock::new());
    let started = nsh_core::time::now();
    let tooltip_now = signal(0u64);
    let sid = props.session_id;
    let clock_id = sid.clone();
    let status = computed(move || {
        let rec = store.sessions.get().into_iter().find(|s| s.id == sid);
        let meta = runtime.get();
        let summary = rec
            .as_ref()
            .and_then(|rec| effective_summary(rec.state, rec.program_summary.as_ref(), &meta));
        let tasks = program_status::subtasks(&meta.program_status);
        let activity = summary.as_ref().and_then(|summary| {
            program_status::activity_text(
                meta.title.as_deref().unwrap_or_default(),
                summary.app.as_deref(),
            )
        });
        let now = nsh_core::time::now().duration_since(started).as_millis() as u64;
        clock.with(|c| c.observe(&sid, summary.as_ref(), now));
        (
            summary.and_then(|s| program_status::badge(&s)),
            tasks,
            activity,
        )
    });
    let motion = StatusMotion::new();
    let last_tag = stored(None);
    effect(move || {
        let (badge, _, _) = status.get();
        let tag = (badge.as_ref().map(|b| (b.tone, b.kind)), reduce_motion());
        if last_tag.get() != Some(tag) {
            last_tag.replace(Some(tag));
            motion.play(tag.0.map(|t| t.0), tag.1);
        }
    });
    view()
        .when(props.edge, |v| {
            v.absolute()
                .left(zpx(0.0))
                .right(zpx(0.0))
                .bottom(zpx(0.0))
                .h(zpx(2.0))
        })
        .when(!props.edge, |v| v.flex_none())
        .child(dynamic(move || {
            let (badge, tasks, activity) = status.get();
            let Some(badge) = badge else {
                return view().into_element();
            };
            let resolved = theme.resolved();
            let ui = &resolved.ui;
            let color = rgb_to_hsla(match badge.tone {
                BadgeTone::Working => ui.tint_strong,
                BadgeTone::Blocked => ui.warning,
                BadgeTone::Done => ui.success,
                BadgeTone::Error => ui.destructive,
            });
            if props.edge {
                return view()
                    .test_id("program-status-edge")
                    .size_full()
                    .bg(color)
                    .bind_opacity(&motion.edge)
                    .into_element();
            }
            let blocked = badge.tone == BadgeTone::Blocked;
            let fg = if blocked {
                rgb_to_hsla(ui.warning_foreground)
            } else {
                color
            };
            // NSH mounts tooltip children eagerly. Refresh the reading whenever it opens.
            let tip_badge = badge.clone();
            let tip_tasks = tasks.clone();
            let tip_id = clock_id.clone();
            let tip = dynamic(move || {
                let now = tooltip_now.get();
                let reading = clock.with(|c| c.reading(&tip_id, now));
                text(tooltip_text(
                    &tip_badge,
                    activity.clone(),
                    reading,
                    &tip_tasks,
                ))
            });
            let icon = match badge.tone {
                BadgeTone::Working => Glyph::Refresh,
                BadgeTone::Done => Glyph::Check,
                BadgeTone::Error => Glyph::Close,
                BadgeTone::Blocked => match badge.kind {
                    Some(ProgramBlockKind::Permission) => Glyph::Shield,
                    Some(ProgramBlockKind::Question) => Glyph::Question,
                    Some(ProgramBlockKind::Auth) => Glyph::Key,
                    _ => Glyph::Warning,
                },
            };
            let mut pill = view()
                .relative()
                .flex_none()
                .p(zpx(2.0))
                .rounded_full()
                .test_id("program-status-chip")
                .tooltip(themed_tooltip(tip, None, &resolved).on_open(move || {
                    tooltip_now
                        .set(nsh_core::time::now().duration_since(started).as_millis() as u64);
                }))
                .bind_opacity(&motion.fade)
                .child(
                    view()
                        .absolute()
                        .left(zpx(0.0))
                        .right(zpx(0.0))
                        .top(zpx(0.0))
                        .bottom(zpx(0.0))
                        .rounded_full()
                        .bg(color)
                        .bind_opacity(&motion.halo),
                )
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap(zpx(4.0))
                        .h(zpx(18.0))
                        .px(zpx(7.0))
                        .rounded_full()
                        .bg(if blocked { color } else { color.opacity(0.14) })
                        .text_color(fg)
                        .child(
                            view()
                                .w(zpx(12.0))
                                .h(zpx(12.0))
                                .items_center()
                                .justify_center()
                                .bind_rotate(&motion.rotation)
                                .bind_scale(&motion.scale)
                                .bind_translate_x(&motion.shake.mul(zpx(1.0).0))
                                .child(glyph_sized(icon, zpx(12.0))),
                        )
                        .child(text(t!(badge.short_key)).text_size(zpx(11.0)))
                        .when_some(badge.progress, |v, p| {
                            v.child(text(t!("programStatus.progress", n = p)).text_size(zpx(11.0)))
                        }),
                );
            if !tasks.is_empty() {
                let waiting = tasks.iter().any(|t| t.tone == BadgeTone::Blocked);
                pill = view()
                    .flex_row()
                    .flex_none()
                    .items_center()
                    .gap(zpx(6.0))
                    .child(pill)
                    .child(
                        view()
                            .test_id("program-status-subtasks")
                            .flex_row()
                            .items_center()
                            .gap(zpx(3.0))
                            .tooltip(themed_tooltip(
                                text(subtasks_tooltip_text(&tasks)),
                                None,
                                &resolved,
                            ))
                            .text_color(rgb_to_hsla(if waiting {
                                ui.warning
                            } else {
                                ui.muted_foreground
                            }))
                            .child(glyph_sized(Glyph::ListTree, zpx(11.0)))
                            .child(text(tasks.len().to_string()).text_size(zpx(11.0))),
                    );
            }
            pill.into_element()
        }))
}

fn effective_summary(
    state: SessionState,
    rest: Option<&ProgramStatusSummary>,
    meta: &crate::terminal::SessionRuntimeMeta,
) -> Option<ProgramStatusSummary> {
    if state != SessionState::Active || meta.state != SessionState::Active {
        return None;
    }
    if meta.program_status_received {
        program_status::summarize(&meta.program_status)
    } else {
        rest.cloned()
    }
}

fn tooltip_text(
    badge: &ProgramBadge,
    activity: Option<String>,
    clock: Option<ClockReading>,
    tasks: &[SubTask],
) -> String {
    let label = t!(badge.label_key);
    let mut lines = vec![match &badge.detail {
        Some(d) => format!("{label} · {d}"),
        None => label,
    }];
    lines.extend(activity.filter(|a| badge.detail.as_ref() != Some(a)));
    if let Some(reading) = clock {
        let (key, ms) = match reading {
            ClockReading::Running(ms) => ("programStatus.clockRunning", ms),
            ClockReading::Waiting(ms) => ("programStatus.clockWaiting", ms),
            ClockReading::Took(ms) => ("programStatus.clockTook", ms),
        };
        lines.push(t!(key, t = program_status::format_duration(ms)));
    }
    if !tasks.is_empty() {
        lines.push(subtasks_tooltip_text(tasks));
    }
    lines.join("\n")
}

fn subtasks_tooltip_text(tasks: &[SubTask]) -> String {
    let mut lines = vec![t!("programStatus.subtasks", n = tasks.len())];
    lines.extend(tasks.iter().map(|task| match &task.detail {
        Some(detail) => format!("  {} · {detail}", t!(task.label_key)),
        None => format!("  {}", t!(task.label_key)),
    }));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::SessionRuntimeMeta;
    use falcon_client::{SessionEvent, SessionSink};
    use falcon_proto::{ProgramState, ProgramStatusRecord};
    use nsh_core::element::Root;
    use nsh_engine::EngineConfig;
    use nsh_testing::Headless;

    fn record(id: &str, state: ProgramState) -> ProgramStatusRecord {
        ProgramStatusRecord {
            id: id.into(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: None,
            updated_at: 2,
        }
    }

    #[derive(Clone, PartialEq, Props)]
    struct AcknowledgementHarnessProps {
        live: bool,
    }

    #[component]
    fn AcknowledgementHarness(props: AcknowledgementHarnessProps) -> impl IntoElement {
        crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        let store = crate::store::WorkspaceStore::install(&crate::FalconLaunchOptions {
            offline_demo: true,
            ..Default::default()
        });
        store.focused_pane_key.set(String::new());
        store.sessions.update(|sessions| {
            let session = sessions.iter_mut().find(|s| s.id == "sess-main").unwrap();
            session.program_status = Some((ProgramState::Done, "finished".into()));
            session.program_summary = Some(ProgramStatusSummary {
                state: ProgramState::Done,
                ..rest_summary()
            });
        });
        let session = store.ensure_terminal_session("sess-main");
        if props.live {
            session.on_event(SessionEvent::ProgramStatus(vec![record(
                "",
                ProgramState::Done,
            )]));
        }
        let focus = store.clone();
        view()
            .child(title_status().session_id("sess-main"))
            .child(
                view()
                    .test_id("acknowledge")
                    .on_press(move || focus.focus_session("sess-main"))
                    .child(text("focus")),
            )
            .child(
                view()
                    .test_id("late-finish")
                    .on_press(move || {
                        session.on_event(SessionEvent::ProgramStatus(vec![record(
                            "",
                            ProgramState::Error,
                        )]));
                    })
                    .child(text("late finish")),
            )
            .child(text(move || {
                let s = store
                    .sessions
                    .get()
                    .into_iter()
                    .find(|s| s.id == "sess-main")
                    .unwrap();
                if s.program_status.is_none() && s.program_summary.is_none() {
                    "REST cleared"
                } else {
                    "REST pending"
                }
            }))
    }

    #[test]
    fn focusing_finished_status_clears_rest_live_and_late_snapshots() {
        for live in [false, true] {
            let root =
                Root::new::<AcknowledgementHarness>(move || AcknowledgementHarnessProps { live })
                    .translations(crate::i18n::translations());
            let mut config = EngineConfig::new(root);
            config.environment.reduce_motion = true;
            config.clock = nsh_engine::Clock::manual(std::time::UNIX_EPOCH);
            let mut app = Headless::start(config).unwrap();
            app.first_render().unwrap();
            app.wait_for("id:program-status-chip").unwrap();
            app.press("id:acknowledge").unwrap();
            app.wait_for("text:REST cleared").unwrap();
            app.wait_for_gone("id:program-status-chip", Duration::from_secs(2))
                .unwrap();
            app.press("id:late-finish").unwrap();
            app.run_for(Duration::from_millis(200)).unwrap();
            assert!(!app.screen().matches("id:program-status-chip").unwrap());
        }
    }

    #[test]
    fn acknowledgement_preserves_unfinished_subtasks() {
        let session = crate::terminal::TerminalSession::new_offline("test", 80, 24, true);
        session.on_event(SessionEvent::ProgramStatus(vec![
            record("", ProgramState::Working),
            record("done", ProgramState::Done),
            record("error", ProgramState::Error),
            record("waiting", ProgramState::Blocked),
        ]));
        session.mark_status_seen();
        let meta = session.meta();
        assert!(meta.program_status_received);
        assert_eq!(
            meta.program_status
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["", "waiting"]
        );
    }

    #[derive(Clone, PartialEq, Props)]
    struct TooltipHarnessProps {}

    #[component]
    fn TooltipHarness(_props: TooltipHarnessProps) -> impl IntoElement {
        crate::theme::install_theme(None);
        crate::zoom::install_zoom();
        crate::shell::action_owner::ActionOwner::install();
        let store = crate::store::WorkspaceStore::install(&crate::FalconLaunchOptions {
            offline_demo: true,
            ..Default::default()
        });
        // The clock only times transitions witnessed by this client, never an already-running job.
        store.sessions.update(|sessions| {
            let session = sessions.iter_mut().find(|s| s.id == "sess-main").unwrap();
            session.program_summary = None;
        });
        view()
            .flex_col()
            .p(zpx(30.0))
            .child(title_status().session_id("sess-main"))
            .child(
                view()
                    .test_id("start-program")
                    .on_press(move || {
                        store.sessions.update(|sessions| {
                            let session =
                                sessions.iter_mut().find(|s| s.id == "sess-main").unwrap();
                            session.program_summary = Some(rest_summary());
                        });
                    })
                    .child(text("start program")),
            )
            .child(
                view()
                    .test_id("away")
                    .w(zpx(200.0))
                    .h(zpx(100.0))
                    .child(text("away")),
            )
    }

    #[test]
    fn tooltip_uses_current_elapsed_time_on_every_open_without_status_changes() {
        let root = Root::new::<TooltipHarness>(|| TooltipHarnessProps {})
            .translations(crate::i18n::translations());
        let mut config = EngineConfig::new(root);
        config.environment.os = nsh_core::platform::Os::Linux;
        config.environment.idiom = nsh_core::platform::Idiom::Desktop;
        config.environment.reduce_motion = true;
        config.clock = nsh_engine::Clock::manual(std::time::UNIX_EPOCH);
        let mut app = Headless::start(config).unwrap();
        app.first_render().unwrap();
        app.press("id:start-program").unwrap();
        app.wait_for("id:program-status-chip").unwrap();
        app.run_for(Duration::from_secs(10)).unwrap();
        app.hover("id:program-status-chip").unwrap();
        app.run_for(Duration::from_millis(600)).unwrap();
        assert!(
            app.screen()
                .texts()
                .iter()
                .any(|t| t.contains("Running for 0:10"))
        );
        app.hover("id:away").unwrap();
        app.run_for(Duration::from_secs(20)).unwrap();
        app.hover("id:program-status-chip").unwrap();
        app.run_for(Duration::from_millis(600)).unwrap();
        assert!(
            app.screen()
                .texts()
                .iter()
                .any(|t| t.contains("Running for 0:31"))
        );
    }

    fn rest_summary() -> ProgramStatusSummary {
        ProgramStatusSummary {
            state: ProgramState::Working,
            kind: None,
            progress: Some(42),
            app: Some("agent".into()),
            title: Some("Inspect changes".into()),
            message: Some("Running checks".into()),
            updated_at: 1,
        }
    }

    #[test]
    fn rest_details_survive_until_the_first_ws_snapshot_and_empty_snapshot_clears() {
        let rest = rest_summary();
        let mut meta = SessionRuntimeMeta::default();
        assert_eq!(
            effective_summary(SessionState::Active, Some(&rest), &meta),
            Some(rest.clone())
        );
        meta.program_status_received = true;
        assert_eq!(
            effective_summary(SessionState::Active, Some(&rest), &meta),
            None
        );
    }

    #[test]
    fn live_records_override_rest_and_inactive_sessions_hide_status() {
        let rest = rest_summary();
        let meta = SessionRuntimeMeta {
            program_status_received: true,
            program_status: vec![ProgramStatusRecord {
                id: "child".into(),
                state: ProgramState::Blocked,
                kind: Some(ProgramBlockKind::Permission),
                progress: Some(73),
                app: Some("agent".into()),
                title: Some("\u{1b}[31mAllow access?\u{1b}[0m".into()),
                message: None,
                updated_at: 2,
            }],
            ..Default::default()
        };
        let summary = effective_summary(SessionState::Active, Some(&rest), &meta).unwrap();
        let badge = program_status::badge(&summary).unwrap();
        assert_eq!(
            (badge.tone, badge.kind, badge.progress),
            (
                BadgeTone::Blocked,
                Some(ProgramBlockKind::Permission),
                Some(73)
            )
        );
        assert!(!badge.detail.unwrap().contains('\u{1b}'));
        let tasks = program_status::subtasks(&meta.program_status);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].tone, BadgeTone::Blocked);
        assert_eq!(
            effective_summary(SessionState::Dead, Some(&rest), &meta),
            None
        );
        let dead_meta = SessionRuntimeMeta {
            state: SessionState::Dead,
            ..meta
        };
        assert_eq!(
            effective_summary(SessionState::Active, Some(&rest), &dead_meta),
            None
        );
    }
}
