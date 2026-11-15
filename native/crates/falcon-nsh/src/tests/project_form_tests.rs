use super::*;

fn listing(path: &str, parent: Option<&str>, entries: &[(&str, &str)]) -> String {
    serde_json::json!({
        "path": path, "parent": parent, "home": "/srv/repos", "roots": ["/"],
        "entries": entries.iter().map(|(name, path)| serde_json::json!({"name": name, "path": path})).collect::<Vec<_>>()
    }).to_string()
}

fn probe_responses() -> Vec<(String, u16, String)> {
    vec![
        (
            "GET /api/shells".into(),
            200,
            r#"{"kind":"posix","default":"/bin/zsh","shells":["/bin/zsh","/bin/fish"]}"#.into(),
        ),
        (
            "GET /api/fs/list?path=%2Fsrv%2Frepos%2Falpha".into(),
            200,
            listing(
                "/srv/repos/alpha",
                Some("/srv/repos"),
                &[("nested", "/srv/repos/alpha/nested")],
            ),
        ),
        (
            "GET /api/fs/list".into(),
            200,
            listing(
                "/srv/repos",
                Some("/srv"),
                &[("alpha", "/srv/repos/alpha"), ("beta", "/srv/repos/beta")],
            ),
        ),
    ]
}

fn probe_app(mock: &MockHttpServer) -> Headless {
    let mut app = headless_with_options(
        FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        },
        ColorScheme::Dark,
    );
    app.first_render().unwrap();
    app
}

fn inline_project() -> serde_json::Value {
    serde_json::json!([{
        "id": "inline", "name": "Legacy", "type": "ssh", "workingDir": "/remote/repo", "createdAt": 0,
        "ssh": { "host": "old.example", "port": 22, "username": "dev", "authMethod": "agent", "hasSecret": false }
    }])
}

fn inline_app(mock: &MockHttpServer) -> Headless {
    let mut app = headless_with_options(
        FalconLaunchOptions {
            server_url: Some(mock.url.clone()),
            ..Default::default()
        },
        ColorScheme::Dark,
    );
    app.first_render().unwrap();
    app.wait_for("Legacy").unwrap();
    app
}

#[test]
fn legacy_ssh_group_creation_never_probes_or_creates_on_backend() {
    let mock = MockHttpServer::with_workspace_bootstrap_projects(
        vec![(
            "POST /api/projects ".into(),
            500,
            r#"{"error":"ssh rejected"}"#.into(),
        )],
        inline_project(),
    );
    let mut app = inline_app(&mock);
    mock.clear_observed_requests();
    app.press("label:new-project-s:legacy:dev@old.example:22")
        .unwrap();
    app.wait_for("label:主机").unwrap();
    assert!(!app.shows("选择文件夹"));
    app.type_text("label:项目名称", "Remote draft").unwrap();
    app.type_text("label:主机", "new.example").unwrap();
    app.type_text("label:用户名", "dev").unwrap();
    mock.assert_no_request_starting_with("GET /api/fs/list");
    mock.assert_no_request_starting_with("GET /api/shells");
    app.press("label:project-submit").unwrap();
    let request =
        mock.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/projects "])[0].clone();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["type"], "ssh");
    assert_eq!(body["ssh"]["host"], "new.example");
    mock.release_response();
    app.wait_for("ssh rejected").unwrap();
}

#[test]
fn project_picker_navigation_preserves_form_and_uses_discovered_shell() {
    let mock = MockHttpServer::new_with_auto_responses(
        vec![(
            "POST /api/projects ".into(),
            500,
            r#"{"error":"save rejected"}"#.into(),
        )],
        probe_responses(),
    );
    let mut app = probe_app(&mock);
    app.press("label:new-project-s:local").unwrap();
    app.wait_for("alpha").unwrap();
    app.press_key("escape").unwrap();
    app.wait_for("label:项目名称").unwrap();
    app.press("label:project-submit").unwrap();
    mock.assert_no_request_starting_with("POST /api/projects ");
    app.type_text("label:项目名称", "Draft").unwrap();
    app.press("label:project-shell-menu").unwrap();
    app.wait_for("/bin/fish").unwrap();
    app.press("/bin/fish").unwrap();
    app.press("浏览…").unwrap();
    app.wait_for("alpha").unwrap();
    app.type_text("label:筛选当前目录", "alpha").unwrap();
    assert_eq!(
        app.screen().input_text("label:筛选当前目录").unwrap(),
        Some("alpha")
    );
    assert!(!app.shows("beta"));
    app.press("alpha").unwrap();
    app.wait_for("nested").unwrap();
    app.press("上一级").unwrap();
    app.wait_for("beta").unwrap();
    app.press("取消").unwrap();
    app.wait_for("label:项目名称").unwrap();
    assert_eq!(
        app.screen().input_text("label:项目名称").unwrap(),
        Some("Draft")
    );
    app.press("浏览…").unwrap();
    app.wait_for("alpha").unwrap();
    app.clear("label:folder-picker-path").unwrap();
    app.type_text("label:folder-picker-path", "/srv/repos/alpha")
        .unwrap();
    app.press_key("enter").unwrap();
    app.wait_for("nested").unwrap();
    app.press("选择此文件夹").unwrap();
    app.wait_for("正在创建…").unwrap();
    app.press("label:project-submit").unwrap();
    let request = mock.wait_for_request();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["name"], "Draft");
    assert_eq!(body["workingDir"], "/srv/repos/alpha");
    assert_eq!(body["shell"], "/bin/fish");
    mock.release_response();
    app.wait_for("save rejected").unwrap();
    assert_eq!(
        app.screen().input_text("label:项目名称").unwrap(),
        Some("Draft")
    );
    mock.assert_no_duplicate_request();
}

#[test]
fn inline_ssh_edits_disable_saved_connection_probes_and_ignore_late_shells() {
    let mock = MockHttpServer::with_workspace_bootstrap_projects(
        vec![
            (
                "GET /api/shells?projectId=inline".into(),
                200,
                r#"{"kind":"posix","default":"/old/shell","shells":["/old/shell"]}"#.into(),
            ),
            (
                "GET /api/shells?projectId=inline".into(),
                200,
                r#"{"kind":"posix","default":"/saved/shell","shells":["/saved/shell"]}"#.into(),
            ),
            (
                "GET /api/fs/list?path=%2Fremote%2Frepo&projectId=inline".into(),
                200,
                listing(
                    "/remote/repo",
                    Some("/remote"),
                    &[("saved-subdir", "/remote/repo/subdir")],
                ),
            ),
        ],
        inline_project(),
    );
    let mut app = inline_app(&mock);
    app.press("label:project-menu-inline").unwrap();
    app.press("编辑项目").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/shells?projectId=inline"]);
    app.clear("label:主机").unwrap();
    app.type_text("label:主机", "new.example").unwrap();
    mock.release_response();
    mock.wait_for_response();
    app.next_change(Duration::from_millis(100)).unwrap();
    app.wait_for("先选一台远端主机，才能浏览那边的目录")
        .unwrap();
    mock.clear_observed_requests();
    app.press("浏览…").unwrap();
    mock.assert_no_request_starting_with("GET /api/fs/list");
    app.press("label:project-shell-menu").unwrap();
    app.wait_for("自定义…").unwrap();
    assert!(!app.shows("/old/shell"));
    app.press("自定义…").unwrap();
    app.clear("label:主机").unwrap();
    app.type_text("label:主机", "old.example").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/shells?projectId=inline"]);
    mock.release_response();
    mock.wait_for_response();
    app.press("浏览…").unwrap();
    mock.wait_for_observed_prefixes_pumping(
        &mut app,
        &["GET /api/fs/list?path=%2Fremote%2Frepo&projectId=inline"],
    );
    mock.release_response();
    app.wait_for("saved-subdir").unwrap();
}

#[test]
fn project_multi_picker_uses_host_presets_and_keeps_cross_directory_selection() {
    let mock = MockHttpServer::new_with_auto_responses(
        vec![(
            "POST /api/projects ".into(),
            500,
            r#"{"error":"multi rejected"}"#.into(),
        )],
        probe_responses(),
    );
    let mut app = probe_app(&mock);
    app.press("label:host-menu-s:host:host-prod-01").unwrap();
    app.press("新建多仓库项目…").unwrap();
    app.wait_for("alpha").unwrap();
    mock.wait_for_observed_prefixes_pumping(
        &mut app,
        &[
            "GET /api/fs/list?hostId=host-prod-01",
            "GET /api/shells?hostId=host-prod-01",
        ],
    );
    app.press("label:folder-select-/srv/repos/alpha").unwrap();
    app.press("alpha").unwrap();
    app.wait_for("nested").unwrap();
    app.press("label:folder-select-/srv/repos/alpha/nested")
        .unwrap();
    app.press("添加选中的 2 个文件夹").unwrap();
    app.wait_for("label:项目名称").unwrap();
    app.type_text("label:项目名称", "Multi").unwrap();
    app.press("label:project-submit").unwrap();
    app.wait_for("正在创建…").unwrap();
    let request = mock.wait_for_request();
    let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
    assert_eq!(body["type"], "ssh");
    assert_eq!(body["hostId"], "host-prod-01");
    assert_eq!(
        body["repos"],
        serde_json::json!(["/srv/repos/alpha", "/srv/repos/alpha/nested"])
    );
    mock.release_response();
    app.wait_for("multi rejected").unwrap();
}

#[test]
fn project_picker_drops_late_results_after_escape_and_shell_failure_keeps_input() {
    let mock = MockHttpServer::new_with_auto_responses(
        vec![(
            "GET /api/fs/list ".into(),
            200,
            listing("/late", None, &[("late-folder", "/late/folder")]),
        )],
        vec![(
            "GET /api/shells".into(),
            500,
            r#"{"error":"offline"}"#.into(),
        )],
    );
    let mut app = probe_app(&mock);
    app.press("label:new-project-s:local").unwrap();
    app.wait_for("正在读取…").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/fs/list "]);
    app.press_key("escape").unwrap();
    app.wait_for("label:项目名称").unwrap();
    app.type_text("label:Shell", "/custom/shell --login")
        .unwrap();
    mock.release_response();
    mock.wait_for_response();
    app.type_text("label:项目名称", "Still here").unwrap();
    assert!(!app.shows("late-folder"));
    assert_eq!(
        app.screen().input_text("label:Shell").unwrap(),
        Some("/custom/shell --login")
    );
}

#[test]
fn project_shell_target_switch_ignores_late_response_and_keeps_custom_commands() {
    let mock = MockHttpServer::new(vec![
        (
            "GET /api/shells ".into(),
            200,
            r#"{"kind":"posix","default":"/old/shell","shells":["/old/shell"]}"#.into(),
        ),
        (
            "GET /api/shells?hostId=host-prod-01".into(),
            200,
            r#"{"kind":"posix","default":"/remote/shell","shells":["/remote/shell"]}"#.into(),
        ),
    ]);
    let mut app = probe_app(&mock);
    app.press_key("ctrl+shift+p").unwrap();
    app.wait_for("新建项目…").unwrap();
    app.press("新建项目…").unwrap();
    app.wait_for("label:项目名称").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/shells "]);
    app.press("远程 SSH").unwrap();
    app.press("label:project-ssh-hosts >> build-box-01")
        .unwrap();
    mock.release_response();
    mock.wait_for_response();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/shells?hostId=host-prod-01"]);
    app.next_change(Duration::from_millis(100)).unwrap();
    app.press("label:project-shell-menu").unwrap();
    app.wait_for("自定义…").unwrap();
    assert!(!app.shows("/old/shell"));
    app.press("自定义…").unwrap();
    app.type_text("label:Shell", "/custom/fish --login")
        .unwrap();
    mock.release_response();
    mock.wait_for_response();
    app.next_change(Duration::from_millis(100)).unwrap();
    app.press("label:project-shell-menu").unwrap();
    app.wait_for("/remote/shell").unwrap();
    assert!(!app.shows("/old/shell"));
    assert_eq!(
        app.screen().input_text("label:Shell").unwrap(),
        Some("/custom/fish --login")
    );
}
