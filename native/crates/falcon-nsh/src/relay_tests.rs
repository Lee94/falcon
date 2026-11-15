// Headless regressions exercise the production pane and HTTP mutation paths.
fn relay_mock(responses: Vec<(String, u16, String)>, snapshot: String) -> MockHttpServer {
    MockHttpServer::new_with_auto_responses(
        responses,
        vec![
            ("GET /api/relays".into(), 200, snapshot),
            ("GET /api/system".into(), 200, "{}".into()),
            (
                "GET /api/app-icon".into(),
                200,
                r#"{"selected":"default"}"#.into(),
            ),
            ("GET /api/projects HTTP/1.1".into(), 200, "[]".into()),
            ("GET /api/sessions".into(), 200, "[]".into()),
            ("GET /api/hosts".into(), 200, "[]".into()),
            ("GET /api/agents".into(), 200, "[]".into()),
            (
                "GET /api/plugins HTTP/1.1".into(),
                200,
                r#"{"dir":"","plugins":[],"trustRoot":"unused"}"#.into(),
            ),
        ],
    )
}

fn relay_app(mock: &MockHttpServer) -> Headless {
    headless_with_locale(
        FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        },
        ColorScheme::Dark,
            "en-US",
    )
}

#[test]
fn relay_polling_stops_on_navigation_and_close() {
    let mock = relay_mock(vec![], relay_list_json("polled"));
    let mut app = headless_with_locale(
        FalconLaunchOptions {
            offline_demo: false,
            server_url: Some(mock.url.clone()),
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        },
        ColorScheme::Dark,
            "en-US",
    );
    open_relay_settings(&mut app);
    app.wait_for("id:relay-toggle-polled").unwrap();
    mock.clear_observed_requests();
    app.wait(Duration::from_millis(3150)).unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
    app.wait_for("id:relays-go-hosts").unwrap();
    app.wait_for_gone(
        "id:relay-bind-port-host-prod-01",
        Duration::from_millis(100),
    )
    .unwrap();
    app.press("id:relays-go-hosts").unwrap();
    app.wait_for_gone("id:relay-share-port-local", Duration::from_secs(1))
        .unwrap();
    mock.clear_observed_requests();
    app.wait(Duration::from_millis(3200)).unwrap();
    mock.assert_no_request_starting_with("GET /api/relays");
    app.press("label:settings-tab-Relays").unwrap();
    app.wait_for("id:relay-share-port-local").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
    // Closing also disposes the pane and its periodic task.
    app.press_key("Escape").unwrap();
    app.wait_for_gone("id:relay-share-port-local", Duration::from_secs(1))
        .unwrap();
    mock.clear_observed_requests();
    app.wait(Duration::from_millis(3200)).unwrap();
    mock.assert_no_request_starting_with("GET /api/relays");
    app.press("id:test-open-relays-settings").unwrap();
    app.wait_for("id:relay-share-port-local").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
}

#[test]
fn relay_same_port_status_and_url_survive_refresh() {
    let snapshot = serde_json::json!({
        "forwards": [
            {"id":"local-a","hostId":"host-prod-01","kind":"local","bindHost":"127.0.0.1","bindPort":9000,"destHost":"db.internal","destPort":5432,"enabled":true,"state":"error","error":"connection refused","createdAt":0},
            {"id":"local-b","hostId":"another-host","kind":"local","bindHost":"127.0.0.1","bindPort":9000,"destHost":"127.0.0.1","destPort":9000,"enabled":false,"state":"stopped","createdAt":0},
            {"id":"remote-a","hostId":"host-prod-01","kind":"remote","bindHost":"127.0.0.1","bindPort":9000,"destHost":"127.0.0.1","destPort":9000,"enabled":false,"state":"starting","createdAt":0}
        ],
        "shares": [
            {"id":"public-a","name":"preview","destHost":"127.0.0.1","destPort":8080,"enabled":true,"state":"active","publicUrl":"https://preview.trycloudflare.com","createdAt":0},
            {"id":"public-b","destHost":"127.0.0.1","destPort":8080,"enabled":false,"state":"stopped","createdAt":0},
            {"id":"public-remote","hostId":"host-prod-01","destHost":"127.0.0.1","destPort":8080,"enabled":true,"state":"active","createdAt":0}
        ]
    }).to_string();
    let mock = relay_mock(vec![], snapshot);
    let mut app = relay_app(&mock);
    open_relay_settings(&mut app);
    for id in [
        "relay-same-local-a",
        "relay-same-public-a",
        "relay-same-public-b",
        "relay-dot-remote-a",
    ] {
        app.wait_for(&format!("id:{id}")).unwrap();
    }
    app.wait_for("connection refused").unwrap();
    app.wait_for_gone("id:relay-same-remote-a", Duration::from_millis(100))
        .unwrap();
    app.wait_for_gone("id:relay-same-public-remote", Duration::from_millis(100))
        .unwrap();
    app.press("label:relay-copy-public-a").unwrap();
    assert_eq!(
        app.screen().clipboard(),
        Some("https://preview.trycloudflare.com")
    );
    app.hover("label:relay-copy-public-a").unwrap();
    app.wait_for("Copied").unwrap();
    app.wait(Duration::from_millis(1600)).unwrap();
    app.wait_for_gone("Copied", Duration::from_millis(500))
        .unwrap();
}

#[test]
fn relay_inline_rename_edit_delete_and_forward_defaults() {
    let original = relay_share_json("managed");
    let mut renamed: serde_json::Value = serde_json::from_str(&original).unwrap();
    renamed["name"] = "preview".into();
    let mock = relay_mock(
        vec![
            ("PATCH /api/shares/managed".into(), 200, renamed.to_string()),
            ("PATCH /api/shares/managed".into(), 200, original),
            (
                "PATCH /api/shares/managed".into(),
                200,
                relay_share_json("managed"),
            ),
            (
                "DELETE /api/shares/managed".into(),
                200,
                r#"{"ok":true}"#.into(),
            ),
            (
                "POST /api/forwards".into(),
                500,
                r#"{"error":"forward defaults verified"}"#.into(),
            ),
        ],
        relay_list_json("managed"),
    );
    let mut app = relay_app(&mock);
    open_relay_settings(&mut app);
    app.wait_for("id:relay-name-managed").unwrap();
    app.press("id:relay-name-managed").unwrap();
    app.type_text("id:relay-name-input-managed", "preview")
        .unwrap();
    app.press_key("Enter").unwrap();
    let request = mock.wait_for_observed_prefixes_pumping(&mut app, &["PATCH /api/shares/managed"])
        [0]
    .clone();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["name"], "preview");
    assert!(body.get("enabled").is_none());
    mock.release_response();
    mock.wait_for_response();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
    // Wait for both the write callback and the authoritative list. The
    // snapshot deliberately differs from the optimistic PATCH response.
    app.wait_for_gone("id:relay-name-input-managed", Duration::from_secs(3))
        .unwrap();
    app.wait_for("id:relay-row-managed >> text:Name (optional)")
        .unwrap();
    app.press("label:relay-edit-managed").unwrap();
    app.wait_for("id:relay-share-port-edit").unwrap();
    assert_eq!(
        app.screen().input_text("id:relay-share-port-edit").unwrap(),
        Some("8080".into())
    );
    app.clear("id:relay-dest-host-edit").unwrap();
    app.type_text("id:relay-dest-host-edit", "[2001:4860::8888]:8443")
        .unwrap();
    app.clear("id:relay-share-port-edit").unwrap();
    app.type_text("id:relay-share-port-edit", "5432").unwrap();
    app.press("id:relay-edit-save").unwrap();
    let request = mock.wait_for_observed_prefixes_pumping(&mut app, &["PATCH /api/shares/managed"])
        [0]
    .clone();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["destHost"], "2001:4860::8888");
    assert_eq!(body["destPort"], 5432);
    mock.release_response();
    mock.wait_for_response();
    app.wait_for("label:relay-delete-managed").unwrap();
    mock.clear_observed_requests();
    app.press("id:relay-toggle-managed >> role:switch").unwrap();
    let request = mock.wait_for_observed_prefixes_pumping(&mut app, &["PATCH /api/shares/managed"])
        [0]
    .clone();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["enabled"], false);
    mock.release_response();
    mock.wait_for_response();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
    app.wait_until("relay write finished", Duration::from_secs(3), |screen| {
        screen
            .enabled("label:relay-delete-managed")
            .unwrap_or(false)
    })
    .unwrap();
    mock.clear_observed_requests();
    app.press("label:relay-delete-managed").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["DELETE /api/shares/managed"]);
    mock.release_response();
    mock.wait_for_response();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
    app.type_text("id:relay-bind-port-host-prod-01", "9090")
        .unwrap();
    app.press("id:relay-create-forward-host-prod-01").unwrap();
    let request =
        mock.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/forwards"])[0].clone();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["hostId"], "host-prod-01");
    // Like GPUI, omit blank addresses so the server supplies loopback.
    assert!(body.get("bindHost").is_none());
    assert!(body.get("destHost").is_none());
    assert_eq!(body["bindPort"], 9090);
    assert_eq!(body["destPort"], 9090);
    mock.release_response();
    mock.wait_for_response();
    app.wait_for("forward defaults verified").unwrap();
}

#[test]
fn relay_destination_typing_preserves_complete_addresses() {
    let cases = [
        ("2001:4860::8888", "2001:4860::8888", 9090, false),
        ("[2001:4860::8888]:8443", "2001:4860::8888", 8443, false),
        ("db.internal:8080", "db.internal", 8080, false),
        ("db.internal:8080", "db.internal", 5432, true),
    ];
    let mock = relay_mock(
        cases
            .iter()
            .map(|_| {
                (
                    "POST /api/forwards".into(),
                    500,
                    r#"{"error":"keep draft"}"#.into(),
                )
            })
            .collect(),
        r#"{"forwards":[],"shares":[]}"#.into(),
    );
    let mut app = relay_app(&mock);
    open_relay_settings(&mut app);
    app.type_text("id:relay-bind-port-host-prod-01", "9090")
        .unwrap();
    for (raw, host, port, edit_port) in cases {
        app.clear("id:relay-dest-host-host-prod-01").unwrap();
        app.type_text("id:relay-dest-host-host-prod-01", raw)
            .unwrap();
        assert_eq!(
            app.screen()
                .input_text("id:relay-dest-host-host-prod-01")
                .unwrap(),
            Some(raw.into())
        );
        if edit_port {
            app.clear("id:relay-dest-port-host-prod-01").unwrap();
            app.type_text("id:relay-dest-port-host-prod-01", &port.to_string())
                .unwrap();
        }
        app.press("id:relay-create-forward-host-prod-01").unwrap();
        let request =
            mock.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/forwards"])[0].clone();
        let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(body["destHost"], host);
        assert_eq!(body["destPort"], port);
        mock.release_response();
        mock.wait_for_response();
        app.wait_until(
            "destination write finished",
            Duration::from_secs(3),
            |screen| {
                screen
                    .enabled("id:relay-create-forward-host-prod-01")
                    .unwrap_or(false)
            },
        )
        .unwrap();
    }
}
