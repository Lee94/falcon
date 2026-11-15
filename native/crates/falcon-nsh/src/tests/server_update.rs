use super::*;

const STATUS: &str = "GET /api/system/update/status HTTP/1.1";
const CHECK: &str = "GET /api/system/update HTTP/1.1";
const START: &str = "POST /api/system/update HTTP/1.1";

fn task(phase: &str) -> String {
    serde_json::json!({"phase": phase, "targetVersion": "9.0.0", "error": null}).to_string()
}

fn check(current: &str, available: bool) -> String {
    serde_json::json!({"currentVersion": current, "latestVersion": "9.0.0", "available": available, "platform": "linux", "error": null}).to_string()
}

fn mock(responses: Vec<(&str, u16, String)>) -> MockHttpServer {
    MockHttpServer::new_with_auto_responses(
        responses
            .into_iter()
            .map(|(path, status, body)| (path.into(), status, body))
            .collect(),
        vec![
            (
                "GET /api/system HTTP/1.1".into(),
                200,
                r#"{"version":"1.0.0","platform":"linux"}"#.into(),
            ),
            (
                "GET /api/app-icon".into(),
                200,
                r#"{"selected":"default"}"#.into(),
            ),
            ("GET /api/projects HTTP/1.1".into(), 200, "[]".into()),
            ("GET /api/sessions".into(), 200, "[]".into()),
            (
                "GET /api/hosts HTTP/1.1".into(),
                200,
                serde_json::json!([ssh_host_value_with_auth(
                    "host-1",
                    "Update node",
                    "node.test",
                    22,
                    "dev",
                    "agent",
                    None,
                    0
                )])
                .to_string(),
            ),
            (
                "GET /api/relays".into(),
                200,
                r#"{"forwards":[],"shares":[]}"#.into(),
            ),
            ("GET /api/agents".into(), 200, "[]".into()),
            (
                "GET /api/plugins HTTP/1.1".into(),
                200,
                r#"{"dir":"","plugins":[],"trustRoot":"unused"}"#.into(),
            ),
        ],
    )
}

fn app(server: &MockHttpServer, switch_url: Option<String>) -> Headless {
    let mut app = headless_with_locale(
        FalconLaunchOptions {
            server_url: Some(server.url.clone()),
            test_client_url: Some(server.url.clone()),
            test_switch_url: switch_url,
            ..Default::default()
        },
        ColorScheme::Dark,
            "en-US",
    );
    app.first_render().unwrap();
    app
}

fn open(app: &mut Headless) {
    app.press("label:sidebar-settings").unwrap();
    app.wait_for("label:settings-tab-About").unwrap();
    app.press("label:settings-tab-About").unwrap();
    app.wait_for("id:server-update-open").unwrap();
    app.press("id:server-update-open").unwrap();
    app.wait_for("id:server-update-dialog").unwrap();
}

fn respond(server: &MockHttpServer, app: &mut Headless, prefix: &str) -> String {
    let request = server
        .wait_for_observed_prefixes_pumping(app, &[prefix])
        .remove(0);
    server.release_response();
    server.wait_for_response();
    request
}

fn ready(server: &MockHttpServer, app: &mut Headless) {
    respond(server, app, STATUS);
    respond(server, app, CHECK);
    app.wait_for("id:server-update-install").unwrap();
}

fn install(app: &mut Headless) {
    app.press("id:server-update-install").unwrap();
    app.wait_for("id:server-update-confirm").unwrap();
    app.press("id:server-update-confirm").unwrap();
}

#[test]
fn server_update_accepts_normalized_client_address_headless() {
    let server = mock(vec![
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("1.0.0", true)),
    ]);
    let url = server.url.replacen("http://", "HTTP://", 1);
    let mut app = headless_with_locale(
        FalconLaunchOptions {
            server_url: Some(url.clone()),
            test_client_url: Some(url),
            ..Default::default()
        },
        ColorScheme::Dark,
            "en-US",
    );
    app.first_render().unwrap();
    open(&mut app);
    ready(&server, &mut app);
    server.assert_no_request_starting_with("POST ");
}

#[test]
fn server_update_warns_about_non_durable_sessions_for_gateway_and_node_headless() {
    for node in [false, true] {
        let (status, check_path) = if node {
            (
                "GET /api/hosts/host-1/node/system/update/status HTTP/1.1",
                "GET /api/hosts/host-1/node/system/update HTTP/1.1",
            )
        } else {
            (STATUS, CHECK)
        };
        let server = mock(vec![
            (status, 200, task("idle")),
            (check_path, 200, check("1.0.0", true)),
        ]);
        let mut app = app(&server, None);
        if node {
            app.press("label:sidebar-settings").unwrap();
            app.press("label:settings-tab-Hosts").unwrap();
            app.wait_for("id:host-update-host-1").unwrap();
            app.press("id:host-update-host-1").unwrap();
        } else {
            open(&mut app);
        }
        respond(&server, &mut app, status);
        respond(&server, &mut app, check_path);
        app.wait_for("id:server-update-install").unwrap();
        app.press("id:server-update-install").unwrap();
        app.wait_for("id:server-update-confirm").unwrap();
        assert!(app.screen().texts().iter().any(|text| {
            text.contains("Non-durable sessions on that node will be interrupted")
                && text.contains("unsaved work may be lost")
                && text.contains("durable sessions can reconnect after the restart")
                && text.contains(if node { "Update node" } else { &server.url })
        }));
        app.press("id:server-update-cancel").unwrap();
        app.wait_for("id:server-update-install").unwrap();
        server.assert_no_request_starting_with("POST ");
    }
}

#[test]
fn server_update_lifecycle_survives_close_reopen_and_restart_headless() {
    let server = mock(vec![
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("1.0.0", true)),
        (START, 200, task("downloading")),
        (STATUS, 200, task("verifying")),
        (STATUS, 200, task("installing")),
        (STATUS, 200, task("restarting")),
        (STATUS, 503, r#"{"error":"restarting node"}"#.into()),
        (STATUS, 200, task("succeeded")),
        (CHECK, 200, check("9.0.0", false)),
    ]);
    let mut app = app(&server, None);
    open(&mut app);
    ready(&server, &mut app);
    app.press("id:server-update-install").unwrap();
    app.press("id:server-update-cancel").unwrap();
    app.wait_for("id:server-update-install").unwrap();
    install(&mut app);
    let request = respond(&server, &mut app, START);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(mock_request_body(&request)).unwrap(),
        serde_json::json!({"version":"9.0.0"})
    );
    app.wait_for("Downloading v9.0.0…").unwrap();
    assert!(!app.screen().enabled("id:server-update-check").unwrap());
    app.press_key("escape").unwrap();
    app.wait_for_gone("id:server-update-dialog", Duration::from_secs(1))
        .unwrap();
    respond(&server, &mut app, STATUS);
    open(&mut app);
    app.wait_for("Verifying v9.0.0…").unwrap();
    respond(&server, &mut app, STATUS);
    app.wait_for("Installing v9.0.0…").unwrap();
    respond(&server, &mut app, STATUS);
    app.wait_for("Restarting to v9.0.0…").unwrap();
    respond(&server, &mut app, STATUS);
    app.wait_for("id:server-update-reconnecting").unwrap();
    app.press("label:modal-close").unwrap();
    app.wait_for_gone("id:server-update-dialog", Duration::from_secs(1))
        .unwrap();
    respond(&server, &mut app, STATUS);
    respond(&server, &mut app, CHECK);
    app.wait_for(&format!("text:{}\nUpdated to v9.0.0", server.url))
        .unwrap();
    app.press("label:sidebar-settings").unwrap();
    app.press("label:settings-tab-About").unwrap();
    app.wait_for("text:v9.0.0").unwrap();
    server.assert_no_duplicate_request();
}

#[test]
fn server_update_existing_unknown_job_resumes_without_install_headless() {
    let server = mock(vec![
        (STATUS, 200, task("futurephase")),
        (STATUS, 200, task("failed")),
        (CHECK, 200, check("1.0.0", true)),
    ]);
    let mut app = app(&server, None);
    open(&mut app);
    respond(&server, &mut app, STATUS);
    app.wait_for("Updating the node to v9.0.0; waiting for the result…")
        .unwrap();
    assert!(!app.screen().matches("id:server-update-install").unwrap());
    respond(&server, &mut app, STATUS);
    respond(&server, &mut app, CHECK);
    app.wait_for("Update to v9.0.0 failed").unwrap();
    app.wait_for("id:server-update-install").unwrap();
    server.assert_no_request_starting_with("POST ");
}

#[test]
fn server_update_status_failure_blocks_install_until_explicit_recheck_headless() {
    let server = mock(vec![
        (STATUS, 500, r#"{"error":"status unavailable"}"#.into()),
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("1.0.0", true)),
    ]);
    let mut app = app(&server, None);
    open(&mut app);
    respond(&server, &mut app, STATUS);
    app.wait_for("status unavailable").unwrap();
    assert!(!app.screen().matches("id:server-update-install").unwrap());
    app.press("id:server-update-check").unwrap();
    ready(&server, &mut app);
    server.assert_no_request_starting_with("POST ");
}

#[test]
fn server_update_ambiguous_post_or_conflict_only_polls_and_idle_requires_recheck_headless() {
    for code in [500, 409] {
        let server = mock(vec![
            (STATUS, 200, task("idle")),
            (CHECK, 200, check("1.0.0", true)),
            (
                START,
                code,
                r#"{"error":"response lost or another updater"}"#.into(),
            ),
            (STATUS, 200, task("idle")),
        ]);
        let mut app = app(&server, None);
        open(&mut app);
        ready(&server, &mut app);
        install(&mut app);
        respond(&server, &mut app, START);
        app.wait_for("id:server-update-reconnecting").unwrap();
        respond(&server, &mut app, STATUS);
        app.wait_for("The node returned no update task, so the request outcome is unknown. Check the node version and status again before retrying.").unwrap();
        app.wait_for_gone("id:server-update-reconnecting", Duration::from_secs(1))
            .unwrap();
        assert!(!app.screen().matches("id:server-update-install").unwrap());
        assert!(app.screen().enabled("id:server-update-check").unwrap());
        server.assert_no_duplicate_request();
    }
}

#[test]
fn server_update_rejected_install_stops_and_requires_fresh_check_headless() {
    let server = mock(vec![
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("1.0.0", true)),
        (START, 422, r#"{"error":"version rejected"}"#.into()),
    ]);
    let mut app = app(&server, None);
    open(&mut app);
    ready(&server, &mut app);
    install(&mut app);
    respond(&server, &mut app, START);
    app.wait_for("version rejected").unwrap();
    assert!(!app.screen().matches("id:server-update-install").unwrap());
    assert!(app.screen().enabled("id:server-update-check").unwrap());
    server.assert_no_duplicate_request();
}

#[test]
fn server_update_node_and_gateway_keep_independent_jobs_headless() {
    let node_status = "GET /api/hosts/host-1/node/system/update/status HTTP/1.1";
    let node_check = "GET /api/hosts/host-1/node/system/update HTTP/1.1";
    let node_start = "POST /api/hosts/host-1/node/system/update HTTP/1.1";
    let server = mock(vec![
        (node_status, 200, task("idle")),
        (node_check, 200, check("2.0.0", true)),
        (node_start, 200, task("installing")),
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("1.0.0", true)),
        (node_status, 200, task("succeeded")),
        (node_check, 200, check("9.0.0", false)),
    ]);
    let mut app = app(&server, None);
    open_hosts_settings(&mut app);
    app.wait_for("id:host-update-host-1").unwrap();
    app.press("id:host-update-host-1").unwrap();
    respond(&server, &mut app, node_status);
    respond(&server, &mut app, node_check);
    app.wait_for("Current v2.0.0 · Latest v9.0.0 · linux")
        .unwrap();
    install(&mut app);
    respond(&server, &mut app, node_start);
    app.wait_for("Installing v9.0.0…").unwrap();
    app.press_key("escape").unwrap();
    open(&mut app);
    ready(&server, &mut app);
    app.wait_for("Current v1.0.0 · Latest v9.0.0 · linux")
        .unwrap();
    respond(&server, &mut app, node_status);
    respond(&server, &mut app, node_check);
    app.wait_for("Update node · dev@node.test:22\nUpdated to v9.0.0")
        .unwrap();
    app.wait_for("Current v1.0.0 · Latest v9.0.0 · linux")
        .unwrap();
    app.press_key("escape").unwrap();
    app.press("label:sidebar-settings").unwrap();
    app.press("label:settings-tab-About").unwrap();
    app.wait_for("text:v1.0.0").unwrap();
    server.assert_no_duplicate_request();
}

#[test]
fn server_update_finishes_on_original_gateway_after_connection_switch_headless() {
    let old = mock(vec![
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("1.0.0", true)),
        (START, 200, task("restarting")),
        (STATUS, 200, task("succeeded")),
        (CHECK, 200, check("9.0.0", false)),
    ]);
    let new = mock(vec![
        (STATUS, 200, task("idle")),
        (CHECK, 200, check("3.0.0", false)),
    ]);
    let mut app = app(&old, Some(new.url.clone()));
    open(&mut app);
    ready(&old, &mut app);
    install(&mut app);
    respond(&old, &mut app, START);
    app.wait_for("Restarting to v9.0.0…").unwrap();
    app.press("id:test-modal-switch-server").unwrap();
    new.wait_for_observed_prefixes_pumping(
        &mut app,
        &["GET /api/system HTTP/1.1", "GET /api/hosts"],
    );
    app.press_key("escape").unwrap();
    open(&mut app);
    respond(&new, &mut app, STATUS);
    respond(&new, &mut app, CHECK);
    app.wait_for("Current v3.0.0 · Latest v9.0.0 · linux")
        .unwrap();
    respond(&old, &mut app, STATUS);
    respond(&old, &mut app, CHECK);
    // Pump the old completion while viewing the new gateway, then inspect its system metadata.
    app.next_change(Duration::from_millis(100)).unwrap();
    app.wait_for("Current v3.0.0 · Latest v9.0.0 · linux")
        .unwrap();
    assert!(
        !app.screen()
            .texts()
            .iter()
            .any(|text| text.contains("Updated to v9.0.0"))
    );
    app.press_key("escape").unwrap();
    app.press("label:sidebar-settings").unwrap();
    app.press("label:settings-tab-About").unwrap();
    app.wait_for("text:v1.0.0").unwrap();
    old.assert_no_duplicate_request();
    new.assert_no_duplicate_request();
}

#[test]
fn server_update_locales_have_matching_keys_and_interpolate_both_languages() {
    let zh: serde_json::Value =
        serde_json::from_str(include_str!("../../locales/zh-CN.json")).unwrap();
    let en: serde_json::Value =
        serde_json::from_str(include_str!("../../locales/en.json")).unwrap();
    let zh = zh["native"]["serverUpdate"].as_object().unwrap();
    let en = en["native"]["serverUpdate"].as_object().unwrap();
    assert_eq!(
        zh.keys().collect::<std::collections::BTreeSet<_>>(),
        en.keys().collect::<std::collections::BTreeSet<_>>()
    );
    for locale in ["zh-CN", "en-US"] {
        let expected = if locale == "zh-CN" { zh } else { en };
        assert_eq!(
            crate::i18n::translations().translate(locale, "native.serverUpdate.title", &[]),
            expected["title"].as_str().unwrap()
        );
        for key in zh.keys() {
            let label = crate::i18n::translations().translate(
                locale,
                &format!("native.serverUpdate.{key}"),
                &[
                    ("version", "9.0.0".into()),
                    ("target", "test node".into()),
                    ("current", "1.0.0".into()),
                    ("latest", "9.0.0".into()),
                    ("platform", "linux".into()),
                ],
            );
            assert!(!label.contains("%{"), "{locale}: {key}");
            assert!(!label.contains("native.serverUpdate"));
        }
    }
}
