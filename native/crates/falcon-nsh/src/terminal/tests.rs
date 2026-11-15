use super::*;
use falcon_proto::{DeadReason, SessionState};
use nsh_core::element::Root;
use nsh_core::{EventKind, EventPayload};
use nsh_engine::EngineConfig;
use nsh_testing::{Headless, LayoutNode};

#[component]
fn TerminalHarness() -> impl IntoElement {
    crate::theme::install_theme(None);
    let store = crate::store::WorkspaceStore::install(&crate::FalconLaunchOptions {
        offline_demo: true,
        ..Default::default()
    });
    let session = store.ensure_terminal_session("sess-main");
    let replay = session.clone();
    let history = session.clone();
    let dead = session.clone();
    let active = session.clone();
    let tick = signal(0u64);
    set_interval(Duration::from_millis(16), move || tick.update(|t| *t += 1));
    view()
        .size_full()
        .flex_col()
        .child(view().h(Points(24.0)).test_id("reset").on_press(move || {
            replay.feed_replay(b"hello world");
            replay.scroll_to(0);
        }))
        .child(view().h(Points(24.0)).test_id("history").on_press(move || {
            for n in 0..200 {
                history.feed_output(format!("\r\nline {n}").as_bytes());
            }
        }))
        .child(view().h(Points(24.0)).test_id("dead").on_press(move || {
            dead.set_state(SessionState::Dead, Some(DeadReason::Exited));
        }))
        .child(view().h(Points(24.0)).test_id("active").on_press(move || {
            active.set_state(SessionState::Active, None);
        }))
        .child(text(move || {
            let _ = tick.get();
            format!("inputs:{}", session.test_input_count())
        }))
        .child(
            view().flex_1().min_h(Points(0.0)).child(
                falcon_terminal()
                    .session_id("sess-main")
                    .focused(true)
                    .tui_passthrough(false)
                    .use_native_surface(false),
            ),
        )
}

fn harness() -> Headless {
    let mut cfg =
        EngineConfig::new(Root::of::<TerminalHarness>().translations(crate::i18n::translations()));
    cfg.viewport = Size::new(Points(800.0), Points(480.0));
    cfg.environment.os = nsh_core::platform::Os::Linux;
    cfg.environment.idiom = Idiom::Desktop;
    cfg.ui_sets = vec![nsh_base::registry(), crate::registry()];
    let mut app = Headless::start(cfg).unwrap();
    app.first_render().unwrap();
    app.wait_for("id:terminal-view-sess-main").unwrap();
    app
}

fn terminal_node(app: &Headless) -> LayoutNode {
    app.screen()
        .layout_nodes()
        .into_iter()
        .find(|n| n.test_id.as_deref() == Some("terminal-view-sess-main"))
        .unwrap()
}

fn point(x: f32, y: f32) -> Point<Points> {
    Point::new(Points(x), Points(y))
}

#[test]
fn local_selection_copies_on_release_and_enables_context_menu() {
    let mut app = harness();
    app.press("id:reset").unwrap();
    app.right_click("id:terminal-view-sess-main").unwrap();
    let menu = app.menu().unwrap();
    assert!(menu[0].disabled, "copy requires selected text");
    app.close_menu().unwrap();

    let node = terminal_node(&app);
    let origin = node.bounds.origin;
    let at = |x, y| point(origin.x.0 + x, origin.y.0 + y);
    let start = point(0.0, 4.0);
    // Events are dispatched to the terminal surface, as the host's press recognizer does.
    app.engine().control().dispatch_event(
        node.id,
        EventKind::PressDown,
        EventPayload::Press(PressEvent {
            position: start,
            page_position: at(0.0, 4.0),
            modifiers: Default::default(),
            clicks: 1,
        }),
    );
    app.engine().control().dispatch_event(
        node.id,
        EventKind::MouseMove,
        EventPayload::Mouse(MouseEvent::new(
            point(41.0, 4.0),
            at(41.0, 4.0),
            Default::default(),
            Some(MouseButton::Left),
        )),
    );
    app.engine().control().dispatch_event(
        node.id,
        EventKind::MouseUp,
        EventPayload::Mouse(MouseEvent::released(
            point(41.0, 4.0),
            at(41.0, 4.0),
            Default::default(),
            MouseButton::Left,
        )),
    );
    app.wait_until("selected word copied", Duration::from_secs(2), |s| {
        s.clipboard().is_some()
    })
    .unwrap();
    assert_eq!(app.screen().clipboard(), Some("hello"));
    app.right_click("id:terminal-view-sess-main").unwrap();
    assert!(!app.menu().unwrap()[0].disabled);
    app.choose_menu_item("Copy").unwrap();
    assert_eq!(app.screen().clipboard(), Some("hello"));
    app.right_click("id:terminal-view-sess-main").unwrap();
    app.choose_menu_item("Clear").unwrap();
    app.right_click("id:terminal-view-sess-main").unwrap();
    assert!(app.menu().unwrap()[0].disabled, "clear removes selection");
}

#[test]
fn scrollbar_track_seeks_and_dead_banner_blocks_paste() {
    let mut app = harness();
    app.press("id:history").unwrap();
    app.wait_for("id:terminal-scrollbar-thumb").unwrap();
    let node = terminal_node(&app);
    app.press_at(
        "id:terminal-view-sess-main",
        point(node.bounds.size.width.0 - 4.0, 1.0),
    )
    .unwrap();
    app.wait_until(
        "thumb moved to history start",
        Duration::from_secs(2),
        |s| {
            s.layout_nodes().iter().any(|n| {
                n.test_id.as_deref() == Some("terminal-scrollbar-thumb")
                    && (n.bounds.origin.y.0 - node.bounds.origin.y.0).abs() < 2.0
            })
        },
    )
    .unwrap();
    app.press("id:dead").unwrap();
    app.wait_for("id:terminal-banner-clear").unwrap();
    assert!(app.shows("Session ended"));
    let mut paste = PasteEvent::default();
    paste.text = "blocked".into();
    app.paste("id:terminal-input-sess-main", &paste).unwrap();
    assert!(app.shows("inputs:0"));
    app.right_click("id:terminal-view-sess-main").unwrap();
    assert!(app.menu().unwrap()[1].disabled);
    app.close_menu().unwrap();
    app.press("id:active").unwrap();
    app.wait_for_gone("id:terminal-banner-clear", Duration::from_secs(2))
        .unwrap();
    app.paste("id:terminal-input-sess-main", &paste).unwrap();
    app.wait_for("text:inputs:1").unwrap();
}

#[test]
fn connection_events_gate_input_and_recover_on_active_state() {
    use falcon_client::{SessionEvent, SessionSink};
    let session = TerminalSession::new_offline("test", 20, 3, false);
    session.feed_output(b"\x1b]7;file://host/home/user/work\x1b\\hello");
    assert_eq!(session.output_cwd().as_deref(), Some("/home/user/work"));
    session.on_event(SessionEvent::Reconnecting { attempt: 2 });
    assert_eq!(
        session.meta().connection,
        session::ConnectionState::Reconnecting
    );
    session.send_paste("blocked");
    assert_eq!(session.test_input_count(), 0);
    session.on_event(SessionEvent::State {
        state: SessionState::Dead,
        dead_reason: Some(DeadReason::Exited),
    });
    session.on_event(SessionEvent::Disconnected {
        retry_attempt: 1,
        retry_in: Duration::from_secs(1),
    });
    assert_eq!(
        session.meta().connection,
        session::ConnectionState::Dead,
        "closed socket preserves dead actions"
    );
    assert!(!session.can_input());
    session.on_event(SessionEvent::Error("attach failed".into()));
    assert_eq!(session.meta().connection, session::ConnectionState::Error);
    session.on_event(SessionEvent::State {
        state: SessionState::Active,
        dead_reason: None,
    });
    assert!(session.can_input());
    session.send_paste("allowed");
    assert_eq!(session.test_input_count(), 1);
    assert!(session.meta().last_error.is_none());
}
