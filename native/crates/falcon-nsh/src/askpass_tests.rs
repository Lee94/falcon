#[derive(Clone, PartialEq, Props)]
struct AskpassTestProps {
    url: String,
    switch_url: Option<String>,
    poll: bool,
}

#[component]
fn AskpassTest(props: AskpassTestProps) -> impl IntoElement {
    install_theme(None);
    install_zoom();
    shell::action_owner::ActionOwner::install();
    let store = WorkspaceStore::install(&FalconLaunchOptions {
        offline_demo: true,
        test_client_url: Some(props.url.clone()),
        ..Default::default()
    });
    store.server_connected.set(props.poll);
    let ws = store.clone();
    let blocked = store.clone();
    let connect = store.clone();
    let disconnect = store.clone();
    let pending = store.clone();
    view()
        .size_full()
        .child(
            view()
                .test_id("askpass-test-ws")
                .h(Points(24.0))
                .child(text("WS"))
                .on_press(move || {
                    use falcon_client::{SessionEvent, SessionSink};
                    let terminal = ws.ensure_terminal_session("sess-main");
                    for (id, prompt) in [
                        ("first", "First prompt"),
                        ("second", "Second prompt"),
                        ("first", "Duplicate"),
                    ] {
                        terminal.on_event(SessionEvent::Askpass {
                            id: id.into(),
                            prompt: prompt.into(),
                        });
                    }
                    ws.drain_askpass_events();
                }),
        )
        .child(
            view()
                .test_id("askpass-test-blocked")
                .h(Points(24.0))
                .child(text("Blocked"))
                .on_press(move || {
                    blocked.active_dialog.set(ActiveDialog::Profiles);
                    blocked.enqueue_askpass("first".into(), "First prompt".into());
                }),
        )
        .child(
            view()
                .test_id("askpass-test-connect")
                .h(Points(24.0))
                .child(text("Connect"))
                .on_press(move || connect.server_connected.set(true)),
        )
        .child(
            view()
                .test_id("askpass-test-disconnect")
                .h(Points(24.0))
                .child(text("Disconnect"))
                .on_press(move || disconnect.server_connected.set(false)),
        )
        .child(text(move || {
            format!("askpass-pending={}", pending.askpass.busy.get())
        }))
        .child(
            modal_layer()
                .test_modal_controls(true)
                .test_switch_url(props.switch_url),
        )
}

fn askpass_test_app(server: &MockHttpServer, poll: bool, switch_url: Option<String>) -> Headless {
    ensure_test_platform();
    let url = server.url.clone();
    let root = Root::new::<AskpassTest>(move || AskpassTestProps {
        url: url.clone(),
        poll,
        switch_url: switch_url.clone(),
    })
    .translations(i18n::translations());
    let mut cfg = EngineConfig::new(root);
    cfg.viewport = Size::new(Points(1440.0), Points(900.0));
    cfg.environment.os = nsh_core::platform::Os::Linux;
    cfg.environment.idiom = Idiom::Desktop;
    cfg.ui_sets = vec![nsh_base::registry(), registry()];
    let mut app = Headless::start(cfg).unwrap();
    app.first_render().unwrap();
    app
}

fn askpass_body(request: &str) -> serde_json::Value {
    serde_json::from_str(mock_request_body(request)).unwrap()
}

#[test]
fn askpass_ws_queue_submits_password_once_then_escape_cancels_next() {
    let server = MockHttpServer::new(vec![
        (
            "POST /api/askpass/first/answer".into(),
            200,
            r#"{"ok":true}"#.into(),
        ),
        (
            "POST /api/askpass/second/answer".into(),
            200,
            r#"{"ok":true}"#.into(),
        ),
    ]);
    let mut app = askpass_test_app(&server, false, None);
    app.press("id:askpass-test-ws").unwrap();
    app.wait_for("First prompt").unwrap();
    app.press_key("t").unwrap();
    assert_eq!(
        app.screen().input_text("id:askpass-input").unwrap(),
        Some("t")
    );
    app.type_text("id:askpass-input", "est-password").unwrap();
    app.press_key("enter").unwrap();
    app.wait_for("askpass-pending=true").unwrap();
    let request = server
        .wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"])
        .remove(0);
    assert_eq!(
        askpass_body(&request),
        serde_json::json!({"password":"test-password"})
    );
    app.press("id:askpass-submit").unwrap();
    app.press_key("escape").unwrap();
    server.release_response();
    app.wait_for("Second prompt").unwrap();
    assert_eq!(
        app.screen().input_text("id:askpass-input").unwrap(),
        Some("")
    );
    app.press_key("x").unwrap();
    assert_eq!(
        app.screen().input_text("id:askpass-input").unwrap(),
        Some("x")
    );
    app.press_key("escape").unwrap();
    let request = server
        .wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/second/answer"])
        .remove(0);
    assert_eq!(askpass_body(&request), serde_json::json!({"cancel":true}));
    server.release_response();
    app.wait_for_gone("id:askpass-input", Duration::from_secs(3))
        .unwrap();
    app.press("id:askpass-test-ws").unwrap();
    assert!(!app.shows("id:askpass-input"));
    server.assert_no_duplicate_request();
}

#[test]
fn askpass_waits_for_other_dialog_and_close_button_cancels() {
    let server = MockHttpServer::new(vec![(
        "POST /api/askpass/first/answer".into(),
        200,
        r#"{"ok":true}"#.into(),
    )]);
    let mut app = askpass_test_app(&server, false, None);
    app.press("id:askpass-test-blocked").unwrap();
    assert!(!app.shows("First prompt"));
    app.press_key("escape").unwrap();
    app.wait_for("First prompt").unwrap();
    app.press("label:modal-close").unwrap();
    let request = server
        .wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"])
        .remove(0);
    assert_eq!(askpass_body(&request), serde_json::json!({"cancel":true}));
    server.release_response();
    app.wait_for_gone("id:askpass-input", Duration::from_secs(3))
        .unwrap();
}

#[test]
fn askpass_retry_failure_and_cancel_button_handle_expired_prompt() {
    let server = MockHttpServer::new(vec![
        (
            "POST /api/askpass/first/answer".into(),
            500,
            r#"{"error":"failed"}"#.into(),
        ),
        (
            "POST /api/askpass/first/answer".into(),
            404,
            r#"{"error":"expired"}"#.into(),
        ),
        (
            "POST /api/askpass/second/answer".into(),
            200,
            r#"{"ok":true}"#.into(),
        ),
    ]);
    let mut app = askpass_test_app(&server, false, None);
    app.press("id:askpass-test-ws").unwrap();
    app.type_text("id:askpass-input", "test-password").unwrap();
    app.press("id:askpass-submit").unwrap();
    server.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"]);
    server.release_response();
    app.wait_for("Could not send the response. Retry or cancel.")
        .unwrap();
    assert_eq!(
        app.screen().input_text("id:askpass-input").unwrap(),
        Some("")
    );
    app.press("id:askpass-cancel").unwrap();
    let request = server
        .wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"])
        .remove(0);
    assert_eq!(askpass_body(&request), serde_json::json!({"cancel":true}));
    server.release_response();
    app.wait_for("Second prompt").unwrap();
    app.press("id:askpass-cancel").unwrap();
    server.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/second/answer"]);
    server.release_response();
    app.wait_for_gone("id:askpass-input", Duration::from_secs(3))
        .unwrap();
}

#[test]
fn askpass_pending_poll_is_connection_and_activity_scoped() {
    let server = MockHttpServer::new(vec![
        (
            "GET /api/askpass/pending".into(),
            200,
            r#"[{"id":"first","prompt":"Polled prompt"}]"#.into(),
        ),
        (
            "POST /api/askpass/first/answer".into(),
            200,
            r#"{"ok":true}"#.into(),
        ),
        ("GET /api/askpass/pending".into(), 200, "[]".into()),
    ]);
    let mut app = askpass_test_app(&server, false, None);
    app.wait(Duration::from_millis(1600)).unwrap();
    server.assert_no_request_starting_with("GET /api/askpass/pending");
    app.set_environment("inactive", |e| {
        e.lifecycle = nsh_core::platform::Lifecycle::Inactive
    })
    .unwrap();
    app.press("id:askpass-test-connect").unwrap();
    app.wait(Duration::from_millis(1600)).unwrap();
    server.assert_no_request_starting_with("GET /api/askpass/pending");
    app.set_environment("active", |e| {
        e.lifecycle = nsh_core::platform::Lifecycle::Active
    })
    .unwrap();
    server.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/askpass/pending"]);
    server.release_response();
    app.wait_for("Polled prompt").unwrap();
    app.press("id:askpass-cancel").unwrap();
    server.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"]);
    server.release_response();
    app.wait_for_gone("id:askpass-input", Duration::from_secs(3))
        .unwrap();
    server.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/askpass/pending"]);
    server.release_response();
    server.wait_for_response();
    app.press("id:askpass-test-disconnect").unwrap();
    app.wait(Duration::from_millis(1700)).unwrap();
    server.assert_no_duplicate_request();
}

#[test]
fn askpass_late_answer_does_not_close_new_server_dialog() {
    let old = MockHttpServer::new(vec![(
        "POST /api/askpass/first/answer".into(),
        200,
        r#"{"ok":true}"#.into(),
    )]);
    let new = MockHttpServer::with_workspace_bootstrap(Vec::new());
    let mut app = askpass_test_app(&old, false, Some(new.url.clone()));
    app.press("id:askpass-test-ws").unwrap();
    app.type_text("id:askpass-input", "old-password").unwrap();
    app.press("id:askpass-submit").unwrap();
    old.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"]);
    app.press("id:test-modal-switch-server").unwrap();
    app.wait_for_gone("id:askpass-input", Duration::from_secs(3))
        .unwrap();
    app.press("id:askpass-test-ws").unwrap();
    app.wait_for("First prompt").unwrap();
    old.release_response();
    old.wait_for_response();
    app.wait(Duration::from_millis(150)).unwrap();
    assert!(app.shows("First prompt"));
    assert_eq!(
        app.screen().input_text("id:askpass-input").unwrap(),
        Some("")
    );
}

#[test]
fn askpass_late_poll_does_not_enqueue_prompts_from_previous_server() {
    let old = MockHttpServer::new(vec![(
        "GET /api/askpass/pending".into(),
        200,
        r#"[{"id":"late","prompt":"Previous server prompt"}]"#.into(),
    )]);
    let new = MockHttpServer::with_workspace_bootstrap(vec![
        (
            "GET /api/askpass/pending".into(),
            200,
            r#"[{"id":"first","prompt":"Current server prompt"}]"#.into(),
        ),
        (
            "POST /api/askpass/first/answer".into(),
            200,
            r#"{"ok":true}"#.into(),
        ),
    ]);
    let mut app = askpass_test_app(&old, true, Some(new.url.clone()));
    old.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/askpass/pending"]);
    app.press("id:askpass-test-ws").unwrap();
    app.press("id:test-modal-switch-server").unwrap();
    new.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/askpass/pending"]);
    new.release_response();
    app.wait_for("Current server prompt").unwrap();
    old.release_response();
    old.wait_for_response();
    app.wait(Duration::from_millis(150)).unwrap();
    assert!(!app.shows("Previous server prompt"));
    app.press("id:askpass-cancel").unwrap();
    new.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/askpass/first/answer"]);
    new.release_response();
    app.wait_for_gone("id:askpass-input", Duration::from_secs(3))
        .unwrap();
}

#[test]
fn askpass_translations_have_chinese_and_english() {
    let translations = i18n::translations();
    for key in [
        "title",
        "placeholder",
        "submit",
        "cancel",
        "sending",
        "failed",
    ] {
        let key = format!("native.askpass.{key}");
        let zh = translations.lookup("zh-CN", &key).unwrap();
        let en = translations.lookup("en-US", &key).unwrap();
        assert_ne!(zh, en);
    }
    assert_eq!(
        translations.lookup("zh-CN", "native.terminal.retryNow").as_deref(),
        Some("立即重连"),
    );
    assert_eq!(
        translations.lookup("en-US", "native.terminal.retryNow").as_deref(),
        Some("Retry now"),
    );
}
