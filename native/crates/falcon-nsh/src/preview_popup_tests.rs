use super::*;

fn demo() -> Headless {
    let mut app = headless_with_options(
        FalconLaunchOptions {
            offline_demo: true,
            ..Default::default()
        },
        ColorScheme::Dark,
    );
    app.first_render().unwrap();
    app
}

fn expand_cargo(app: &mut Headless) -> String {
    app.press("label:activity-files").unwrap();
    app.press("text:Cargo.toml").unwrap();
    let key = falcon_core::pane_key::file_key("proj-falcon", "native/Cargo.toml");
    app.wait_for(&format!("label:file-expand-{key}")).unwrap();
    app.press(&format!("label:file-expand-{key}")).unwrap();
    app.wait_for("id:preview-popup").unwrap();
    key
}

#[test]
fn preview_popup_preserves_edits_when_switching_and_closing() {
    let mut app = demo();
    let key = expand_cargo(&mut app);
    let editor = format!("id:file-editor-{key}");
    app.wait_for(&editor).unwrap();
    let before = app
        .screen()
        .input_text(&editor)
        .unwrap()
        .unwrap()
        .to_string();
    app.type_text(&editor, "# popup edit").unwrap();
    let edited = app
        .screen()
        .input_text(&editor)
        .unwrap()
        .unwrap()
        .to_string();
    assert_ne!(before, edited);
    // 文件栏搜索使用项目相对路径；切走再回来不重读/覆盖脏缓冲。
    app.type_text("id:preview-search", "lib.rs").unwrap();
    app.press("id:preview-file-0").unwrap();
    let other =
        falcon_core::pane_key::file_key("proj-falcon", "native/crates/falcon-nsh/src/lib.rs");
    app.wait_for(&format!("id:file-editor-{other}")).unwrap();
    app.type_text("id:preview-search", "").unwrap();
    for _ in "lib.rs".chars() {
        app.press_key("backspace").unwrap();
    }
    app.type_text("id:preview-search", "Cargo.toml").unwrap();
    app.press("id:preview-file-0").unwrap();
    app.wait_for(&editor).unwrap();
    assert_eq!(
        app.screen().input_text(&editor).unwrap(),
        Some(edited.as_str())
    );
    // 关闭浮层后工作区编辑器必须重新绑定到同一份最新文本。
    app.press_key("escape").unwrap();
    app.wait_for_gone("id:preview-popup", Duration::from_secs(2))
        .unwrap();
    app.wait_for(&editor).unwrap();
    assert_eq!(
        app.screen().input_text(&editor).unwrap(),
        Some(edited.as_str())
    );
}

#[test]
fn preview_popup_diff_keyboard_tree_and_sidebar() {
    let mut app = demo();
    app.press("label:activity-changes").unwrap();
    app.press("id:git-file-0").unwrap();
    app.press("label:git-diff-expand").unwrap();
    app.wait_for("id:preview-popup").unwrap();
    app.wait_for("id:preview-search").unwrap();
    app.press("label:preview-tree").unwrap();
    app.type_text("id:preview-search", "").unwrap();
    app.press_key("down").unwrap();
    app.wait_for("+pub struct PluginKeyring {").unwrap();
    app.press_key("up").unwrap();
    app.wait_for("+pub fn falcon_root() -> nsh_core::element::Root {")
        .unwrap();
    app.press("label:preview-sidebar").unwrap();
    app.wait_for_gone("id:preview-search", Duration::from_secs(2))
        .unwrap();
    app.press("label:preview-sidebar").unwrap();
    app.wait_for("id:preview-search").unwrap();
    app.type_text("id:preview-search", "nshpkg").unwrap();
    app.press_key("enter").unwrap();
    app.wait_for("+pub struct PluginKeyring {").unwrap();
    app.press("label:preview-close").unwrap();
    app.wait_for_gone("id:preview-popup", Duration::from_secs(2))
        .unwrap();
}

#[test]
fn preview_popup_releases_clean_sidebar_files_for_rename() {
    let path = "native/crates/falcon-nsh/src/lib.rs";
    let mock = MockHttpServer::new_with_auto_responses(
        Vec::new(),
        vec![
            (
                "GET /api/projects/proj-falcon/files/index".into(),
                200,
                format!(r#"{{"paths":["{path}"],"truncated":false}}"#),
            ),
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("clean sidebar file", "clean-v1"),
            ),
        ],
    );
    let mut app = headless_with_options(
        FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            test_live_file_operations: true,
            ..Default::default()
        },
        ColorScheme::Dark,
    );
    app.first_render().unwrap();
    expand_cargo(&mut app);
    app.type_text("id:preview-search", "lib.rs").unwrap();
    app.wait_for("id:preview-file-0").unwrap();
    app.press("id:preview-file-0").unwrap();
    let key = falcon_core::pane_key::file_key("proj-falcon", path);
    app.wait_for(&format!("id:file-editor-{key}")).unwrap();
    app.press_key("escape").unwrap();
    app.wait_for_gone("id:preview-popup", Duration::from_secs(2))
        .unwrap();
    app.press("id:file-select-src/lib.rs").unwrap();
    app.press("id:file-operations-rename").unwrap();
    app.wait_for("id:file-operation-name").unwrap();
}

#[test]
fn preview_popup_saves_shared_buffer_with_version_and_shortcut() {
    let path = "native/mock-preview.md";
    let key = falcon_core::pane_key::file_key("proj-falcon", path);
    let mock = MockHttpServer::new_with_auto_responses(
        vec![
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("preview body", "preview-v1"),
            ),
            (
                "PUT /api/projects/proj-falcon/file".into(),
                200,
                r#"{"path":"native/mock-preview.md","size":19,"version":"preview-v2"}"#.into(),
            ),
        ],
        vec![(
            "GET /api/projects/proj-falcon/files/index".into(),
            200,
            r#"{"paths":["native/mock-preview.md"],"truncated":false}"#.into(),
        )],
    );
    let mut app = headless_with_options(
        FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            test_live_file_operations: true,
            ..Default::default()
        },
        ColorScheme::Dark,
    );
    app.first_render().unwrap();
    app.press("id:test-open-preview-file").unwrap();
    mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/projects/proj-falcon/file"]);
    mock.release_response();
    mock.wait_for_response();
    app.wait_for("preview body").unwrap();
    app.press(&format!("label:file-expand-{key}")).unwrap();
    app.wait_for("id:preview-popup").unwrap();
    app.press(&format!(
        "id:preview-popup >> id:file-markdown-preview-{key}"
    ))
    .unwrap();
    let editor = format!("id:file-editor-{key}");
    app.wait_for(&editor).unwrap();
    app.type_text(&editor, " edited").unwrap();
    // 编辑时方向键不能切换文件；Ctrl+S 只保存当前浮层文件。
    app.press_key("up").unwrap();
    app.wait_for(&editor).unwrap();
    app.press_key("ctrl+s").unwrap();
    let writes =
        mock.wait_for_observed_prefixes_pumping(&mut app, &["PUT /api/projects/proj-falcon/file"]);
    assert!(writes[0].contains("base=preview-v1"), "{}", writes[0]);
    assert_eq!(mock_request_body(&writes[0]), "preview body edited");
    mock.release_response();
    mock.wait_for_response();
    app.wait_for("text:已同步").unwrap();
    app.press_key("escape").unwrap();
    app.wait_for_gone("id:preview-popup", Duration::from_secs(2))
        .unwrap();
}
