//! `falcon-nsh`：基于 Native Shell (`nsh 0.5`) 的 Falcon 六端统一客户端界面库（ADR 0028）。
//!
//! 架构要点：
//! - **纯 Rust 主应用，主路径零 QuickJS 开销**：工作区排布（`falcon-core`）、终端仿真（`falcon-term`）、
//!   465 套 Ghostty 主题派生（`falcon-theme`）与 HTTP/WS 协议（`falcon-client` / `falcon-proto`）全部原生直连；
//! - **六端自适应同源渲染**：桌面（macOS / Windows / Linux）、Android（`Idiom::Phone`）、Web 静态站（`wasm32`）
//!   与 TUI 终端（`Idiom::Terminal`）共用同一棵响应式组件树；
//! - **右侧栏插件双轨制**：完整兼容存量声明式 JSON `PluginView`（ADR 0025），同时通过 [`ClientPluginVerifier`]
//!   支持自托管服务端 Ed25519 信任根签名的 `.nshpkg` 沙盒面板包与构建号防回滚。

pub mod fonts;
pub mod i18n;
pub mod project_operations;
pub(crate) mod session_operations;
pub mod appearance;
pub mod shell;
pub mod store;
pub mod terminal;
pub mod theme;
pub mod views;
pub mod zoom;

use std::sync::Arc;

use falcon_core::pane_key::parse_pane_key;
use falcon_core::shortcuts::{
    BindingRole, Command as ShortcutCommand, bindings as shortcut_bindings,
};
use falcon_platform::{ServiceControl, ServicePresence};
use falcon_theme::{ThemeSettings, derive_theme, load_theme_settings, resolve_theme_mode};
use nsh_core::desktop::{MainWindowOptions, TitleBarOptions, TitleBarStyle, TrayTitleRun};
use nsh_core::element::desktop::{MenuItem, main_window, set_tray, tray};
use nsh_core::element::{Registry, Root};
use nsh_core::platform::Idiom;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::shell::{desktop_shell, mobile_shell, tui_shell};
pub use crate::store::FalconLaunchOptions;
use crate::store::{ActiveDialog, SettingsTab, WorkspaceStore};
#[cfg(test)]
use crate::store::SessionRecord;
use crate::terminal::{FalconTerminal, bind_safe_key, bind_safe_key_when};
use crate::theme::{
    install_theme, nsh_appearance_of, rgb_to_hsla, rgba_to_hsla, tokens_from_resolved, use_theme,
};
use crate::views::{
    AgentPaneView, DiffPaneView, FilePaneView, FilesPanel, GitPanel, PluginPanel,
    file_operation_layer, modal_layer,
};
use crate::zoom::{install_zoom, zpx};

/// `FalconApp` 根组件属性（所有字段满足 `JsValue + Clone + PartialEq`）。
#[derive(Clone, PartialEq, Props)]
pub struct FalconAppProps {
    pub offline_demo: bool,
    pub server_url: Option<String>,
    pub initial_theme_json: Option<String>,
    #[cfg(test)]
    pub test_client_url: Option<String>,
    #[cfg(test)]
    pub test_switch_url: Option<String>,
    #[cfg(test)]
    pub test_live_file_operations: bool,
}

fn bind_primary_command(command: ShortcutCommand, handler: impl Fn() + 'static) {
    let Some(binding) = shortcut_bindings(command, apple_keys())
        .into_iter()
        .find(|binding| binding.role == BindingRole::Primary)
    else {
        return;
    };
    let spec = binding.keystroke.gpui().replace('-', "+");
    bind_safe_key_when(&spec, false, nsh_core::keymap::When::Always, handler);
}

/// 跨六端统一根组件。
#[component]
pub fn FalconApp(props: FalconAppProps) -> impl IntoElement {
    let initial_theme: Option<ThemeSettings> = props
        .initial_theme_json
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok());
    let launch_opts = FalconLaunchOptions {
        offline_demo: props.offline_demo,
        server_url: props.server_url.clone(),
        initial_theme: initial_theme.clone(),
        #[cfg(test)]
        test_client_url: props.test_client_url.clone(),
        #[cfg(test)]
        test_switch_url: props.test_switch_url.clone(),
        #[cfg(test)]
        test_live_file_operations: props.test_live_file_operations,
    };
    let theme_ctrl = install_theme(initial_theme);
    let zoom_ctrl = install_zoom();
    shell::action_owner::ActionOwner::install();
    let store = WorkspaceStore::install(&launch_opts);
    crate::appearance::install_effects(store.clone());
    {
        let st_close = store.clone();
        main_window().on_close_press(move |_| st_close.request_close_window());
    }
    #[cfg(all(feature = "snapshot", not(target_arch = "wasm32")))]
    install_review_scene(store.clone());

    // 同步深色/浅色与缩放全局快捷键
    {
        let st_pal = store.clone();
        let st_pal_secondary = store.clone();
        bind_primary_command(ShortcutCommand::Palette, move || {
            let next = if st_pal.active_dialog.get() == ActiveDialog::Palette {
                ActiveDialog::None
            } else {
                ActiveDialog::Palette
            };
            st_pal.active_dialog.set(next);
        });
        if apple_keys() {
            bind_safe_key_when("cmd+k", false, nsh_core::keymap::When::Always, move || {
                let next = if st_pal_secondary.active_dialog.get() == ActiveDialog::Palette {
                    ActiveDialog::None
                } else {
                    ActiveDialog::Palette
                };
                st_pal_secondary.active_dialog.set(next);
            });
        }
    }
    {
        let st_quick_open = store.clone();
        bind_primary_command(ShortcutCommand::QuickOpen, move || {
            st_quick_open.open_quick_open();
        });
    }
    {
        let st_set = store.clone();
        bind_safe_key_when("mod+,", false, nsh_core::keymap::When::Always, move || {
            st_set
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Appearance));
        });
    }
    {
        let st_new = store.clone();
        bind_primary_command(ShortcutCommand::NewTerminal, move || {
            st_new.create_terminal_in_active_project();
        });
    }
    {
        let st_detach = store.clone();
        let detach_key = if apple_keys() {
            "cmd+shift+w"
        } else {
            "ctrl+shift+alt+w"
        };
        bind_safe_key_when(
            detach_key,
            false,
            nsh_core::keymap::When::Always,
            move || {
                let fk = st_detach.focused_pane_key.get();
                if !fk.is_empty() {
                    st_detach.detach_pane(&fk);
                }
            },
        );
    }
    {
        let st_close = store.clone();
        bind_primary_command(ShortcutCommand::CloseTab, move || {
            let fk = st_close.focused_pane_key.get();
            if let Some(falcon_core::pane_key::PaneItem::Terminal { id, .. }) = parse_pane_key(&fk)
            {
                st_close.request_terminate_session(&id);
            } else if !fk.is_empty() {
                st_close.detach_pane(&fk);
            }
        });
    }
    {
        let sidebar = store.sidebar_open;
        bind_primary_command(ShortcutCommand::ToggleSidebar, move || {
            sidebar.update(|v| *v = !*v)
        });
        let files = store.clone();
        bind_primary_command(ShortcutCommand::ToggleFilesPanel, move || {
            files.toggle_right_panel(crate::store::RightPanelKind::Files)
        });
        let changes = store.clone();
        bind_primary_command(ShortcutCommand::ToggleChangesPanel, move || {
            changes.toggle_right_panel(crate::store::RightPanelKind::Git)
        });
    }
    {
        bind_safe_key("mod+=", false, move || zoom_ctrl.step_in());
        bind_safe_key("mod+-", false, move || zoom_ctrl.step_out());
        let overview = store.overview_open;
        bind_primary_command(ShortcutCommand::Overview, move || overview.set(true));
    }

    // 处理 `falcon://` URL Scheme 深链（打开指定服务端或会话）
    {
        let st_link = store.clone();
        spawn(async move {
            loop {
                let raw_url = next_url_scheme_open().await;
                if let Some(rest) = raw_url.strip_prefix("falcon://connect?url=") {
                    st_link.connect_live_server(rest);
                }
            }
        });
    }

    // macOS 常驻菜单栏状态项（托管 LaunchAgent 启停、局域网开关、多实例标记与检查更新）
    if os() == nsh_core::platform::Os::MacOs && !props.offline_demo {
        install_macos_tray(store.clone());
    }

    let window_background = store.appearance.window_background;
    view()
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(move || {
            let app = theme_ctrl.resolved().ui.app;
            let background = crate::appearance::effective_background(
                window_background.get(),
                idiom() == Idiom::Desktop,
            );
            rgba_to_hsla(falcon_theme::Rgba {
                r: app.r,
                g: app.g,
                b: app.b,
                a: (background.fill_alpha() * 255.0).round() as u8,
            })
        })
        .child(dynamic(move || match idiom() {
            Idiom::Terminal => tui_shell().into_element(),
            Idiom::Phone => mobile_shell().into_element(),
            _ => desktop_shell().into_element(),
        }))
        .child(toast_overlay())
        .child({
            #[cfg(test)]
            {
                modal_layer()
                    .test_modal_controls(
                        props.test_client_url.is_some() || props.test_switch_url.is_some(),
                    )
                    .test_switch_url(props.test_switch_url.clone())
                    .into_element()
            }
            #[cfg(not(test))]
            {
                modal_layer().into_element()
            }
        })
        .child(file_operation_layer())
        .child({
            #[cfg(test)]
            {
                if props.test_client_url.is_some() || props.test_switch_url.is_some() {
                    test_workspace_harness(store.clone(), props.test_switch_url.clone())
                        .into_element()
                } else {
                    view().into_element()
                }
            }
            #[cfg(not(test))]
            {
                view().into_element()
            }
        })
}

#[cfg(test)]
#[component]
fn SheetMenuDispatchTest() -> impl IntoElement {
    let theme = install_theme(None);
    install_zoom();
    shell::action_owner::ActionOwner::install();
    let store = WorkspaceStore::install(&FalconLaunchOptions {
        offline_demo: true,
        ..Default::default()
    });
    let count = signal(0usize);
    view()
        .size_full()
        .child(crate::shell::popups::overflow_menu(
            view()
                .test_id("sheet-menu-trigger")
                .focusable(true)
                .w(Points(100.0))
                .h(Points(30.0))
                .child(text("Menu")),
            vec![crate::shell::popups::PopupAction::new(
                crate::shell::popups::ServerUrlGuard::capture(&store),
                "Increment",
                false,
                false,
                move || count.update(|value| *value += 1),
            )],
            &theme.resolved(),
        ))
        .child(text(move || format!("action-count={}", count.get())))
}

#[cfg(test)]
fn test_workspace_harness(store: WorkspaceStore, switch_url: Option<String>) -> impl IntoElement {
    let save_path = "native/mock-save-integration.txt";
    let save_key = falcon_core::pane_key::file_key("proj-falcon", save_path);
    let preview_path = "native/mock-preview.md";
    let stale_path = "native/crates/falcon-nsh/src/views/files.rs";
    let stale_key = falcon_core::pane_key::file_key("proj-falcon", stale_path);

    let open_save = store.clone();
    let mut controls = view().w_full().flex_row().flex_wrap().gap(Points(4.0)).child(
        view()
            .test_id("test-open-save-file")
            .on_press(move || open_save.open_file_pane("proj-falcon", save_path))
            .child(text("open-save")),
    );
    let install_dirty_file = store.clone();
    let install_dirty_key = save_key.clone();
    controls = controls.child(
        view()
            .test_id("test-install-dirty-file-pane")
            .on_press(move || {
                let mut buffer = crate::store::OpenFileBuffer::from_disk_text(
                    "proj-falcon",
                    save_path,
                    "disk",
                    "disk-v1",
                    None,
                );
                buffer.current_body = "dirty-reconnect".into();
                buffer.edit_revision = 1;
                install_dirty_file.open_files.update(|files| {
                    files.insert(install_dirty_key.clone(), buffer);
                });
                install_dirty_file.columns.set(vec![falcon_core::layout::column(
                    [install_dirty_key.clone()],
                    None,
                )]);
                install_dirty_file
                    .focused_pane_key
                    .set(install_dirty_key.clone());
            })
            .child(text("install-dirty-file-pane")),
    );
    let reconnect_file_store = store.clone();
    let reconnect_file_key = save_key.clone();
    controls = controls.child(
        view()
            .test_id("test-reconnect-file-state")
            .child(dynamic(move || {
                let has_pane = falcon_core::layout::pane_keys(
                    &reconnect_file_store.columns.get(),
                )
                .contains(&reconnect_file_key);
                let files = reconnect_file_store.open_files.get();
                let body = files
                    .get(&reconnect_file_key)
                    .map(|buffer| buffer.current_body.as_str())
                    .unwrap_or("");
                text(format!("reconnect-file={has_pane}:{body}")).into_element()
            })),
    );
    let pending_save_store = store.clone();
    let pending_save_key = save_key.clone();
    let pending_save_state = signal("idle".to_string());
    let state_for_button = pending_save_state.clone();
    controls = controls.child(
        view()
            .test_id("test-save-after-pane-removal")
            .on_press(move || {
                let mut buffer = crate::store::OpenFileBuffer::from_disk_text(
                    "proj-falcon",
                    save_path,
                    "before",
                    "disk-v1",
                    None,
                );
                buffer.current_body = "after".into();
                buffer.edit_revision = 1;
                pending_save_store.open_files.update(|files| {
                    files.insert(pending_save_key.clone(), buffer);
                });
                pending_save_store.open_file_pane("proj-falcon", save_path);
                let dispatched = pending_save_store.save_file_buffer(&pending_save_key);
                let columns = pending_save_store.columns.get();
                pending_save_store.columns.set(falcon_core::layout::remove_pane(
                    &columns,
                    &pending_save_key,
                ));
                pending_save_store.open_files.update(|files| {
                    files.remove(&pending_save_key);
                });
                let blocked = pending_save_store.has_open_file_mapping("proj-falcon");
                state_for_button.set(format!("dispatched={dispatched};blocked={blocked}"));
            })
            .child(text("save-after-pane-removal")),
    );
    let state_for_label = pending_save_state;
    controls = controls.child(
        view()
            .test_id("test-save-mapping-state")
            .child(dynamic(move || text(state_for_label.get()).into_element())),
    );
    let open_stale = store.clone();
    controls = controls.child(
        view()
            .test_id("test-open-stale-file")
            .on_press(move || open_stale.open_file_pane("proj-falcon", stale_path))
            .child(text("open-stale")),
    );
    let advance_mapping = store.clone();
    controls = controls.child(
        view()
            .test_id("test-advance-project-mapping")
            .on_press(move || {
                advance_mapping
                    .project_operations
                    .test_advance_mapping_generation("proj-falcon");
                advance_mapping.on_project_mapping_change_started("proj-falcon");
            })
            .child(text("advance-project-mapping")),
    );
    let advance_mapping_only = store.clone();
    controls = controls.child(
        view()
            .test_id("test-advance-mapping-generation-only")
            .on_press(move || {
                advance_mapping_only
                    .project_operations
                    .test_advance_mapping_generation("proj-falcon");
            })
            .child(text("advance-mapping-generation-only")),
    );
    let pending_mapping = store.clone();
    controls = controls.child(
        view()
            .test_id("test-begin-pending-mapping-change")
            .on_press(move || {
                pending_mapping
                    .project_operations
                    .test_begin_pending_mapping_change("proj-falcon");
            })
            .child(text("begin-pending-mapping-change")),
    );
    let add_project_b = store.clone();
    controls = controls.child(
        view()
            .test_id("test-add-project-b")
            .on_press(move || {
                add_project_b.projects.update(|projects| {
                    if !projects.iter().any(|project| project.id == "proj-b") {
                        projects.push(crate::store::ProjectRecord {
                            id: "proj-b".into(),
                            name: "B".into(),
                            path: "/workspace/b".into(),
                            host_id: None,
                            host_name: None,
                            derived_from: None,
                            branch: None,
                            source: None,
                        });
                    }
                });
            })
            .child(text("add-project-b")),
    );
    let open_preview = store.clone();
    controls = controls.child(
        view()
            .test_id("test-open-preview-file")
            .on_press(move || open_preview.open_file_pane("proj-falcon", preview_path))
            .child(text("open-preview")),
    );
    let restored_pane_store = store.clone();
    let restored_pane_key = falcon_core::pane_key::file_key("proj-falcon", save_path);
    let restored_pane_state = signal(String::new());
    let restored_pane_state_for_button = restored_pane_state.clone();
    controls = controls.child(
        view()
            .test_id("test-install-restored-file-pane")
            .on_press(move || {
                restored_pane_store
                    .open_files
                    .update(|files| drop(files.remove(&restored_pane_key)));
                restored_pane_store.columns.set(vec![falcon_core::layout::column(
                    [restored_pane_key.clone()],
                    None,
                )]);
                restored_pane_state_for_button.set(format!(
                    "restored-file-pane-mapping={}",
                    restored_pane_store.has_open_file_mapping("proj-falcon")
                ));
            })
            .child(text("install-restored-file-pane")),
    );
    let restored_pane_state_for_label = restored_pane_state;
    controls = controls.child(
        view()
            .test_id("test-restored-file-pane-state")
            .child(dynamic(move || {
                text(restored_pane_state_for_label.get()).into_element()
            })),
    );
    let reload_save = store.clone();
    let reload_key = save_key.clone();
    controls = controls.child(
        view()
            .test_id("test-reload-save-file")
            .on_press(move || reload_save.request_reload_file(&reload_key))
            .child(text("reload-save")),
    );
    let quick_open = store.clone();
    controls = controls.child(
        view()
            .test_id("test-open-quick-open")
            .on_press(move || quick_open.open_quick_open())
            .child(text("quick-open")),
    );
    let mut secondary_controls = view().w_full().flex_row().flex_wrap().gap(Points(4.0));
    let split_down = store.clone();
    secondary_controls = secondary_controls.child(
        view()
            .test_id("test-split-down")
            .on_press(move || {
                split_down.create_terminal_with_request(
                    falcon_proto::CreateSessionRequest::default(),
                    crate::store::SessionPlacement::Down,
                );
            })
            .child(text("split-down")),
    );
    let split_right = store.clone();
    secondary_controls = secondary_controls.child(
        view()
            .test_id("test-split-right")
            .on_press(move || {
                split_right.create_terminal_with_request(
                    falcon_proto::CreateSessionRequest::default(),
                    crate::store::SessionPlacement::Right,
                );
            })
            .child(text("split-right")),
    );
    let close_stale = store.clone();
    let close_key = stale_key.clone();
    secondary_controls = secondary_controls.child(
        view()
            .test_id("test-close-stale-file")
            .on_press(move || close_stale.request_close_file(&close_key))
            .child(text("close-stale")),
    );
    let focus_agent = store.clone();
    secondary_controls = secondary_controls.child(
        view()
            .test_id("test-focus-agent-session")
            .on_press(move || focus_agent.focus_session("sess-agent"))
            .child(text("focus-agent-session")),
    );
    let open_relays = store.clone();
    secondary_controls = secondary_controls.child(
        view().test_id("test-open-relays-settings").on_press(move || {
            open_relays.active_dialog.set(ActiveDialog::Settings(SettingsTab::Relays));
        }).child(text("open-relays")),
    );
    let refresh_relays = store.clone();
    secondary_controls = secondary_controls.child(
        view().test_id("test-refresh-relays").on_press(move || refresh_relays.refresh_relays())
            .child(text("refresh-relays")),
    );
    let duplicate_relay = store.clone();
    secondary_controls = secondary_controls.child(
        view().test_id("test-force-duplicate-share").on_press(move || {
            duplicate_relay.create_public_share("", 8080, "draft");
        }).child(text("duplicate-share")),
    );
    let rebuild_relay_hosts = store.clone();
    secondary_controls = secondary_controls.child(
        view().test_id("test-rebuild-relay-hosts").on_press(move || {
            if let Some(mut host) = rebuild_relay_hosts.hosts.get().first().cloned() {
                host.id = "relay-test-host".into();
                host.name = "relay-test-host".into();
                rebuild_relay_hosts.hosts.update(|hosts| hosts.push(host));
            }
        }).child(text("rebuild-relay-hosts")),
    );
    if let Some(url) = switch_url {
        let switch = store.clone();
        let switch_url = url.clone();
        secondary_controls = secondary_controls.child(
            view()
                .test_id("test-switch-server")
                .on_press(move || switch.connect_live_server(&switch_url))
                .child(text("switch-server")),
        );
        let open_profiles = store.clone();
        let profile_url = url;
        secondary_controls = secondary_controls.child(
            view()
                .test_id("test-open-connect-server")
                .on_press(move || {
                    let profile = falcon_core::profiles::ServerProfile {
                        id: "test-switch-profile".into(),
                        name: "Test switch".into(),
                        url: profile_url.clone(),
                        plaintext_ok: true,
                    };
                    open_profiles.profiles.update(|profiles| profiles.upsert(profile));
                    open_profiles
                        .active_dialog
                        .set(crate::store::ActiveDialog::Profiles);
                })
                .child(text("connect-profiles")),
        );
    }
    let mark_session_list = store.clone();
    secondary_controls = secondary_controls.child(
        view()
            .test_id("test-mark-session-list-sentinel")
            .on_press(move || {
                mark_session_list.sessions.set(vec![SessionRecord {
                    id: "session-list-sentinel".into(),
                    project_id: "proj-falcon".into(),
                    name: "session-list-sentinel".into(),
                    auto_title: String::new(),
                    cwd: "/workspace/falcon".into(),
                    durable: false,
                    state: falcon_proto::SessionState::Active,
                    agent: None,
                    program_status: None,
                    foreground_cmd: None,
                }]);
            })
            .child(text("mark-session-list")),
    );

    let controls = view()
        .w_full()
        .flex_col()
        .gap(Points(4.0))
        .child(controls)
        .child(secondary_controls);
    let probe_store = store;
    let probe = view()
        .pointer_events(nsh_core::element::PointerEvents::None)
        .child(dynamic(move || {
        let files = probe_store.open_files.get();
        let mut state = view().flex_row().gap(Points(2.0));
        if let Some(buffer) = files.get(&save_key) {
            state = state
                .child(text(format!("save-state={:?}", buffer.state)))
                .child(text(format!("save-dirty={}", buffer.is_dirty())))
                .child(text(format!("save-saving={}", buffer.saving)))
                .child(text(format!("save-hash={}", buffer.disk_hash)));
        } else {
            state = state.child(text("save-state=closed"));
        }
        if let Some(buffer) = files.get(&stale_key) {
            state = state
                .child(text(format!("stale-state={:?}", buffer.state)))
                .child(text(format!("stale-hash={}", buffer.disk_hash)))
                .child(text(format!("stale-body={}", buffer.current_body)));
        } else {
            state = state.child(text("stale-state=closed"));
        }
        state = state
            .child(text(format!(
                "active-project={}",
                probe_store.active_project_id.get()
            )))
            .child(text(format!(
                "quick-open-project={}",
                probe_store.quick_open_project.get()
            )))
            .child(text(format!(
                "quick-open-loading={}",
                probe_store.quick_open_loading.get()
            )))
            .child(text(format!(
                "quick-open-error-present={}",
                probe_store.quick_open_error.get().is_some()
            )))
            .child(text(format!(
                "quick-open-paths={}",
                probe_store.quick_open_paths.get().join(",")
            )))
            .child(text(format!(
                "server-connected={}",
                probe_store.server_connected.get()
            )))
            .child(text(format!("server-url={}", probe_store.server_url.get())));
        state.into_element()
    }));

    view()
        .absolute()
        .top(Points(0.0))
        .left(Points(0.0))
        .right(Points(0.0))
        .bottom(Points(0.0))
        .pointer_events(nsh_core::element::PointerEvents::BoxNone)
        .child(
            view()
                .absolute()
                .bottom(Points(0.0))
                .left(Points(0.0))
                .w_full()
                .child(probe),
        )
        .child(
            view()
                .absolute()
                .bottom(Points(0.0))
                .right(Points(0.0))
                .w(Points(420.0))
                .flex_col()
                .child(controls),
        )
}

#[cfg(test)]
pub(crate) fn test_modal_workspace_controls(
    store: WorkspaceStore,
    switch_url: Option<String>,
) -> impl IntoElement {
    let refresh_relays = store.clone();
    let duplicate_relay = store.clone();
    let rebuild_relay_hosts = store.clone();
    let mut controls = view()
        .test_id("test-modal-refresh-relays")
        .on_press(move || refresh_relays.refresh_relays())
        .w_full()
        .h(Points(20.0))
        .child(text("r"));
    controls = controls.child(
        view()
            .test_id("test-modal-force-duplicate-share")
            .on_press(move || duplicate_relay.create_public_share("", 8080, "draft"))
            .w_full()
            .h(Points(20.0))
            .child(text("d")),
    );
    controls = controls.child(
        view()
            .test_id("test-modal-rebuild-relay-hosts")
            .on_press(move || {
                if let Some(mut host) = rebuild_relay_hosts.hosts.get().first().cloned() {
                    host.id = "relay-test-host".into();
                    host.name = "relay-test-host".into();
                    rebuild_relay_hosts.hosts.update(|hosts| hosts.push(host));
                }
            })
            .w_full()
            .h(Points(20.0))
            .child(text("h")),
    );
    if let Some(url) = switch_url {
        let switch = store;
        controls = controls.child(
            view()
                .test_id("test-modal-switch-server")
                .on_press(move || switch.connect_live_server(&url))
                .w_full()
                .h(Points(20.0))
                .child(text("s")),
        );
    }
    view()
        .absolute()
        .left(Points(0.0))
        .top(Points(0.0))
        .w(Points(16.0))
        .h(Points(80.0))
        .opacity(0.0)
        .flex_col()
        .child(controls)
}

fn install_macos_tray(store: WorkspaceStore) {
    let presence_sig: Signal<Option<ServicePresence>> = signal(None);
    let lan_sig = signal(false);
    let busy_sig: Signal<Option<ServiceControl>> = signal(None);
    let mark_sig = signal(String::new());
    let err_sig: Signal<Option<String>> = signal(None);

    #[cfg(not(target_arch = "wasm32"))]
    {
        let refresh = move || {
            spawn(async move {
                if let Ok((pres, lan, mark)) = nsh_core::prelude::blocking(|| {
                    let plat = falcon_platform::shared();
                    let (pres, lan) = match plat.local_service() {
                        Some(svc) => (
                            svc.presence(),
                            svc.lan_access().map(|s| s.enabled).unwrap_or(false),
                        ),
                        None => (None, false),
                    };
                    (pres, lan, plat.instance_mark())
                })
                .await
                {
                    presence_sig.set(pres);
                    lan_sig.set(lan);
                    mark_sig.set(mark);
                }
            });
        };
        refresh();
        set_interval(std::time::Duration::from_secs(5), refresh);
    }

    effect(move || {
        let mut items = Vec::new();
        items.push(MenuItem::new(t!("native.menuBar.open")).on_select(|| {
            main_window().show();
            main_window().focus();
        }));

        if falcon_platform::shared().local_service().is_some() {
            items.push(MenuItem::separator());
            let busy = busy_sig.get();
            let pres = presence_sig.get();
            let status_label = if let Some(ctrl) = busy {
                match ctrl {
                    ServiceControl::Start => t!("native.menuBar.busyStart"),
                    ServiceControl::Stop => t!("native.menuBar.busyStop"),
                    ServiceControl::Restart => t!("native.menuBar.busyRestart"),
                }
            } else if let Some(err) = err_sig.get() {
                t!("native.menuBar.failed", error = err)
            } else {
                match pres {
                    None => t!("native.menuBar.querying"),
                    Some(ServicePresence::NotInstalled) => t!("native.menuBar.missing"),
                    Some(ServicePresence::Stopped) => t!("native.menuBar.stopped"),
                    Some(ServicePresence::Running { pid: Some(pid) }) => {
                        t!("native.menuBar.runningPid", pid = pid.to_string())
                    }
                    Some(ServicePresence::Running { pid: None }) => t!("native.menuBar.running"),
                }
            };
            items.push(MenuItem::new(status_label).disabled(true));

            let is_running = matches!(pres, Some(ServicePresence::Running { .. }));
            let is_installed = !matches!(pres, Some(ServicePresence::NotInstalled));

            let run_ctrl = move |ctrl: ServiceControl| {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    busy_sig.set(Some(ctrl));
                    err_sig.set(None);
                    spawn(async move {
                        let outcome = nsh_core::prelude::blocking(move || {
                            let plat = falcon_platform::shared();
                            match plat.local_service() {
                                Some(svc) => match svc.control(ctrl) {
                                    Ok(next_pres) => (Ok(()), Some(next_pres)),
                                    Err(err) => (Err(err), svc.presence()),
                                },
                                None => (Ok(()), None),
                            }
                        })
                        .await;
                        busy_sig.set(None);
                        if let Ok((res, next_pres)) = outcome {
                            presence_sig.set(next_pres);
                            if let Err(e) = res {
                                err_sig.set(Some(e));
                            }
                        }
                    });
                }
                #[cfg(target_arch = "wasm32")]
                let _ = ctrl;
            };

            items.push(
                MenuItem::new(t!("native.menuBar.start"))
                    .disabled(busy.is_some() || is_running)
                    .on_select(move || run_ctrl(ServiceControl::Start)),
            );
            items.push(
                MenuItem::new(t!("native.menuBar.stop"))
                    .disabled(busy.is_some() || !is_running)
                    .on_select(move || run_ctrl(ServiceControl::Stop)),
            );
            items.push(
                MenuItem::new(t!("native.menuBar.restart"))
                    .disabled(busy.is_some() || !is_installed)
                    .on_select(move || run_ctrl(ServiceControl::Restart)),
            );

            let lan_now = lan_sig.get();
            items.push(
                MenuItem::new(t!("native.menuBar.lanAccess"))
                    .checked(lan_now)
                    .disabled(busy.is_some())
                    .on_select(move || {
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            let target = !lan_now;
                            err_sig.set(None);
                            spawn(async move {
                                if let Ok((res, cur)) = nsh_core::prelude::blocking(move || {
                                    let plat = falcon_platform::shared();
                                    match plat.local_service() {
                                        Some(svc) => match svc.set_lan_access(target) {
                                            Ok(state) => (Ok(()), state.enabled),
                                            Err(err) => (
                                                Err(err),
                                                svc.lan_access()
                                                    .map(|s| s.enabled)
                                                    .unwrap_or(false),
                                            ),
                                        },
                                        None => (Ok(()), false),
                                    }
                                })
                                .await
                                {
                                    lan_sig.set(cur);
                                    if let Err(e) = res {
                                        err_sig.set(Some(e));
                                    }
                                }
                            });
                        }
                    }),
            );
        }

        if falcon_platform::shared().app_updater().is_some() {
            items.push(MenuItem::separator());
            let st_upd = store.clone();
            items.push(
                MenuItem::new(t!("native.menuBar.checkUpdate")).on_select(move || {
                    main_window().show();
                    main_window().focus();
                    st_upd
                        .active_dialog
                        .set(ActiveDialog::Settings(SettingsTab::About));
                }),
            );
        }

        items.push(MenuItem::separator());
        let st_quit = store.clone();
        items.push(
            MenuItem::new(t!("native.menu.quit"))
                .shortcut(if apple_keys() {
                    "cmd+q"
                } else {
                    "ctrl+shift+q"
                })
                .on_select(move || st_quit.request_quit()),
        );

        let mark = mark_sig.get();
        let mut t_item = tray().tooltip("Falcon").menu(items);
        if !mark.is_empty() {
            t_item = t_item.title([TrayTitleRun::new(mark)]);
        }
        set_tray(t_item);
    });
}

#[component]
fn ToastOverlay() -> impl IntoElement {
    let store = crate::store::use_workspace();
    let theme_ctrl = use_theme();

    view().child(dynamic(move || {
        let Some(msg) = store.toast_banner.get() else {
            return view().into_element();
        };
        let resolved = theme_ctrl.resolved();
        let ui = &resolved.ui;
        let _ = t!("native.nsh.appTitle");
        view()
            .absolute()
            .right(zpx(16.0))
            .bottom(zpx(16.0))
            .px(zpx(12.0))
            .py(zpx(8.0))
            .rounded(zpx(8.0))
            .border_1()
            .border_color(rgba_to_hsla(ui.border))
            .bg(rgb_to_hsla(ui.popover))
            .child(
                text(msg)
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.popover_foreground)),
            )
            .into_element()
    }))
}

/// 构建默认生产环境的 NSH `Root`（自动挂载内嵌 WOFF2 字体、中文文案、首帧 Ghostty 主题 Token 与 `Appearance`）。
pub fn falcon_root() -> Root {
    falcon_root_with_options(FalconLaunchOptions::default())
}

/// 按给定启动配置构建 NSH `Root`（供无头回归测试、TUI、Web 与桌面入口共用）。
pub fn falcon_root_with_options(options: FalconLaunchOptions) -> Root {
    let initial_settings = options
        .initial_theme
        .clone()
        .unwrap_or_else(|| load_theme_settings(None));
    let slot = resolve_theme_mode(initial_settings.mode, true);
    let initial_resolved = Arc::new(derive_theme(&initial_settings.slot(slot).colors));

    let mut win = MainWindowOptions::default();
    win.attributes.title = Some("Falcon".to_string());
    win.attributes.close_on_press = Some(false);
    // 桌面端隐藏系统标题栏，由 identity_strip 兼任（已带 window_drag）：
    // 否则系统栏（标题文字 + 窗口按钮）与应用自己的身份条上下并排两条。
    // Hidden 下 NSH 保留系统窗口控件——macOS 红绿灯居中在这条 34pt 栏内、
    // Windows 的 caption 按钮以 Win11 风格画在右侧（含 snap layouts），
    // 两端让位宽度由 identity_strip 按 title_bar() 环境预留。
    let mut title_bar = TitleBarOptions::default();
    title_bar.style = TitleBarStyle::Hidden;
    title_bar.height = Some(Points(34.0));
    win.attributes.title_bar = Some(title_bar);

    let offline_demo = options.offline_demo;
    let server_url = options.server_url;
    #[cfg(test)]
    let test_client_url = options.test_client_url;
    #[cfg(test)]
    let test_switch_url = options.test_switch_url;
    #[cfg(test)]
    let test_live_file_operations = options.test_live_file_operations;
    let initial_theme_json = options
        .initial_theme
        .as_ref()
        .and_then(|th| serde_json::to_string(th).ok());

    let root = Root::new::<FalconApp>(move || FalconAppProps {
        offline_demo,
        server_url: server_url.clone(),
        initial_theme_json: initial_theme_json.clone(),
        #[cfg(test)]
        test_client_url: test_client_url.clone(),
        #[cfg(test)]
        test_switch_url: test_switch_url.clone(),
        #[cfg(test)]
        test_live_file_operations,
    })
    .app_id("com.falcon.app")
    .url_schemes(["falcon"])
    .translations(i18n::translations())
    .tokens(tokens_from_resolved(&initial_resolved))
    .appearance(nsh_appearance_of(&initial_resolved))
    .main_window(win);

    fonts::register_embedded_fonts(root)
}

/// 导出 Falcon 自定义 UI 组件集注册表（供宿主与 `.nshpkg` 沙盒面板包桥接使用）。
pub fn registry() -> Registry {
    let mut reg = Registry::ui_set("falcon/ui");
    reg.register::<FalconTerminal>();
    reg.register::<PluginPanel>();
    reg.register::<AgentPaneView>();
    reg.register::<FilesPanel>();
    reg.register::<FilePaneView>();
    reg.register::<GitPanel>();
    reg.register::<DiffPaneView>();
    reg
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use falcon_theme::{
        ThemeMode, ThemePref, ThemeSettings, choice_of, derive_theme, find_theme, load_catalog,
    };
    use nsh_core::platform::{Appearance, ColorScheme};
    use nsh_engine::EngineConfig;
    use nsh_package::{App as PkgApp, Contents as PkgContents, SecretKey, certify};
    use nsh_testing::Headless;

    use super::*;
    use crate::store::PackageVerificationState;
    use crate::terminal::{TerminalSession, TerminalViewport, paint_snapshot};
    use crate::views::ClientPluginVerifier;

    mod preview_popup_tests {
        include!("preview_popup_tests.rs");
    }

    fn ensure_test_platform() {
        let _ = falcon_platform::shared();
    }

    fn accept_mock_connection(
        listener: &TcpListener,
        stop: &Receiver<()>,
        timeout: Duration,
    ) -> Option<TcpStream> {
        let deadline = Instant::now() + timeout;
        loop {
            if stop.try_recv().is_ok() {
                return None;
            }
            match listener.accept() {
                Ok((stream, _)) => return Some(stream),
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return None;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return None,
            }
        }
    }

    fn read_mock_request(stream: &TcpStream) -> Option<String> {
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).ok()? == 0 {
            return None;
        }
        if request_line.is_empty() {
            return None;
        }
        let mut content_length = 0usize;
        let mut header = String::new();
        loop {
            header.clear();
            if reader.read_line(&mut header).is_err() || header.is_empty() || header == "\r\n" {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        let mut remaining = content_length;
        let mut body = Vec::with_capacity(content_length);
        let mut chunk = [0; 1024];
        while remaining > 0 {
            let chunk_len = remaining.min(chunk.len());
            match reader.read(&mut chunk[..chunk_len]) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    remaining -= read;
                    body.extend_from_slice(&chunk[..read]);
                }
            }
        }
        let mut request = request_line.trim_end().to_string();
        if !body.is_empty() {
            request.push_str("\r\n\r\n");
            request.push_str(&String::from_utf8_lossy(&body));
        }
        Some(request)
    }

    fn mock_request_body(request: &str) -> &str {
        request
            .split_once("\r\n\r\n")
            .map_or("", |(_, body)| body)
    }

    fn write_mock_response(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = match status {
            200 => "OK",
            409 => "Conflict",
            500 => "Internal Server Error",
            _ => "Mock Response",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
    }

    struct MockHttpServer {
        url: String,
        requests: Receiver<String>,
        observed_requests: Receiver<String>,
        unexpected_requests: Receiver<String>,
        responses_sent: Receiver<()>,
        release: Sender<()>,
        stop: Sender<()>,
        worker: Option<JoinHandle<()>>,
    }

    impl MockHttpServer {
        fn new(responses: Vec<(String, u16, String)>) -> Self {
            Self::new_with_auto_responses(responses, Vec::new())
        }

        fn with_workspace_bootstrap(responses: Vec<(String, u16, String)>) -> Self {
            Self::with_workspace_bootstrap_and_hosts(responses, Vec::new())
        }

        fn with_workspace_bootstrap_and_sessions(
            responses: Vec<(String, u16, String)>,
            sessions: serde_json::Value,
        ) -> Self {
            Self::with_workspace_bootstrap_data_and_sessions(
                responses,
                Vec::new(),
                serde_json::json!([{
                    "id": "proj-falcon",
                    "name": "falcon",
                    "type": "local",
                    "workingDir": "/workspace/falcon",
                    "createdAt": 0
                }]),
                sessions,
            )
        }

        fn with_workspace_bootstrap_and_hosts(
            responses: Vec<(String, u16, String)>,
            hosts: Vec<serde_json::Value>,
        ) -> Self {
            Self::with_workspace_bootstrap_data(
                responses,
                hosts,
                serde_json::json!([{
                    "id": "proj-falcon",
                    "name": "falcon",
                    "type": "local",
                    "workingDir": "/workspace/falcon",
                    "createdAt": 0
                }]),
            )
        }

        fn with_workspace_bootstrap_projects(
            responses: Vec<(String, u16, String)>,
            projects: serde_json::Value,
        ) -> Self {
            Self::with_workspace_bootstrap_data(responses, Vec::new(), projects)
        }

        fn with_workspace_bootstrap_data(
            responses: Vec<(String, u16, String)>,
            hosts: Vec<serde_json::Value>,
            projects: serde_json::Value,
        ) -> Self {
            Self::with_workspace_bootstrap_data_and_sessions(
                responses,
                hosts,
                projects,
                serde_json::json!([]),
            )
        }

        fn with_workspace_bootstrap_data_and_sessions(
            responses: Vec<(String, u16, String)>,
            hosts: Vec<serde_json::Value>,
            projects: serde_json::Value,
            sessions: serde_json::Value,
        ) -> Self {
            let hosts = serde_json::Value::Array(hosts).to_string();
            Self::new_with_auto_responses(
                responses,
                vec![
                    ("GET /api/system".into(), 200, "{}".into()),
                    (
                        "GET /api/app-icon".into(),
                        200,
                        r#"{"selected":"default"}"#.into(),
                    ),
                    (
                        "GET /api/projects HTTP/1.1".into(),
                        200,
                        projects.to_string(),
                    ),
                    ("GET /api/sessions".into(), 200, sessions.to_string()),
                    ("GET /api/hosts".into(), 200, hosts),
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

        fn new_with_auto_responses(
            responses: Vec<(String, u16, String)>,
            auto_responses: Vec<(String, u16, String)>,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let expected_prefixes: Vec<_> = responses
                .iter()
                .map(|(prefix, _, _)| prefix.clone())
                .collect();
            let (request_tx, requests) = mpsc::channel();
            let (observed_tx, observed_requests) = mpsc::channel();
            let (unexpected_tx, unexpected_requests) = mpsc::channel();
            let (response_sent_tx, responses_sent) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let (stop, stop_rx) = mpsc::channel();
            let worker = thread::spawn(move || {
                let mut responses = responses.into_iter().peekable();
                loop {
                    let timeout = if responses.peek().is_some() || !auto_responses.is_empty() {
                        Duration::from_secs(5)
                    } else {
                        Duration::from_millis(400)
                    };
                    let Some(mut stream) = accept_mock_connection(&listener, &stop_rx, timeout)
                    else {
                        return;
                    };
                    let Some(request) = read_mock_request(&stream) else {
                        continue;
                    };
                    let _ = observed_tx.send(request.clone());
                    if let Some((_, status, body)) = auto_responses
                        .iter()
                        .find(|(prefix, _, _)| request.starts_with(prefix))
                    {
                        write_mock_response(&mut stream, *status, body);
                        continue;
                    }

                    if let Some((expected_prefix, _, _)) = responses.peek() {
                        if request.starts_with(expected_prefix) {
                            let (_, status, body) = responses.next().unwrap();
                            let _ = request_tx.send(request);
                            if release_rx.recv_timeout(Duration::from_secs(5)).is_err() {
                                return;
                            }
                            write_mock_response(&mut stream, status, &body);
                            let _ = response_sent_tx.send(());
                        } else {
                            write_mock_response(
                                &mut stream,
                                500,
                                r#"{"error":"unplanned mock request"}"#,
                            );
                        }
                    } else {
                        if expected_prefixes
                            .iter()
                            .any(|prefix| request.starts_with(prefix))
                        {
                            let _ = unexpected_tx.send(request.clone());
                        }
                        write_mock_response(
                            &mut stream,
                            500,
                            r#"{"error":"unexpected mock request"}"#,
                        );
                    }
                }
            });

            Self {
                url: format!("http://{address}"),
                requests,
                observed_requests,
                unexpected_requests,
                responses_sent,
                release,
                stop,
                worker: Some(worker),
            }
        }

        fn wait_for_request(&self) -> String {
            match self.requests.recv_timeout(Duration::from_secs(3)) {
                Ok(request) => request,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let observed: Vec<_> = self.observed_requests.try_iter().collect();
                    panic!("mock HTTP request should arrive; observed {observed:?}");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("mock HTTP request channel disconnected");
                }
            }
        }

        fn wait_for_observed_prefixes(&self, prefixes: &[&str]) -> Vec<String> {
            let mut found = vec![None; prefixes.len()];
            let mut observed = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            while found.iter().any(Option::is_none) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let request = self
                    .observed_requests
                    .recv_timeout(remaining)
                    .unwrap_or_else(|error| {
                        panic!(
                            "requests starting with {prefixes:?} not observed: {found:?}; received {observed:?}; {error}"
                        )
                    });
                observed.push(request.clone());
                for (index, prefix) in prefixes.iter().enumerate() {
                    if found[index].is_none() && request.starts_with(prefix) {
                        found[index] = Some(request.clone());
                    }
                }
            }
            found.into_iter().map(Option::unwrap).collect()
        }

        fn wait_for_observed_prefixes_pumping(
            &self,
            app: &mut Headless,
            prefixes: &[&str],
        ) -> Vec<String> {
            let mut found = vec![None; prefixes.len()];
            let mut observed = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            while found.iter().any(Option::is_none) {
                loop {
                    match self.observed_requests.try_recv() {
                        Ok(request) => {
                            observed.push(request.clone());
                            for (index, prefix) in prefixes.iter().enumerate() {
                                if found[index].is_none() && request.starts_with(prefix) {
                                    found[index] = Some(request.clone());
                                }
                            }
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            panic!("mock HTTP request channel disconnected")
                        }
                    }
                }
                if found.iter().all(Option::is_some) {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(
                    !remaining.is_zero(),
                    "requests starting with {prefixes:?} not observed: {found:?}; received {observed:?}"
                );
                app.next_change(remaining.min(Duration::from_millis(50)))
                    .unwrap();
            }
            found.into_iter().map(Option::unwrap).collect()
        }

        fn release_response(&self) {
            self.release.send(()).unwrap();
        }

        fn wait_for_response(&self) {
            self.responses_sent
                .recv_timeout(Duration::from_secs(3))
                .expect("mock HTTP response should be sent");
        }

        fn assert_no_duplicate_request(&self) {
            match self
                .unexpected_requests
                .recv_timeout(Duration::from_millis(500))
            {
                Ok(request) => panic!("unexpected duplicate HTTP request: {request}"),
                Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
            }
        }

        fn assert_no_request_starting_with(&self, prefix: &str) {
            let deadline = Instant::now() + Duration::from_millis(600);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.observed_requests.recv_timeout(remaining) {
                    Ok(request) if request.starts_with(prefix) => {
                        panic!("unexpected request {request}")
                    }
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {
                        return;
                    }
                }
            }
        }

        fn clear_observed_requests(&self) {
            let _ = self.observed_requests.try_iter().count();
        }
    }

    impl Drop for MockHttpServer {
        fn drop(&mut self) {
            for _ in 0..8 {
                let _ = self.release.send(());
            }
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    struct HeldSessionRequest {
        request: String,
        response: Sender<(u16, String)>,
        snapshot_body: Option<String>,
    }

    struct SessionRaceMock {
        url: String,
        session_list_requests: Receiver<String>,
        snapshots: Receiver<HeldSessionRequest>,
        writes: Receiver<HeldSessionRequest>,
        sessions_body: Arc<Mutex<String>>,
        outstanding: Arc<Mutex<Vec<Sender<(u16, String)>>>>,
        stop: Sender<()>,
        worker: Option<JoinHandle<()>>,
    }

    impl SessionRaceMock {
        fn new(sessions_body: String) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let sessions_body = Arc::new(Mutex::new(sessions_body));
            let outstanding = Arc::new(Mutex::new(Vec::new()));
            let session_list_count = Arc::new(AtomicUsize::new(0));
            let (session_list_tx, session_list_requests) = mpsc::channel();
            let (snapshot_tx, snapshots) = mpsc::channel();
            let (write_tx, writes) = mpsc::channel();
            let (stop, stop_rx) = mpsc::channel();

            let body_for_worker = sessions_body.clone();
            let outstanding_for_worker = outstanding.clone();
            let count_for_worker = session_list_count.clone();
            let worker = thread::spawn(move || {
                while stop_rx.try_recv().is_err() {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let sessions_body = body_for_worker.clone();
                            let outstanding = outstanding_for_worker.clone();
                            let session_list_count = count_for_worker.clone();
                            let session_list_tx = session_list_tx.clone();
                            let snapshot_tx = snapshot_tx.clone();
                            let write_tx = write_tx.clone();
                            thread::spawn(move || {
                                let Some(request) = read_mock_request(&stream) else {
                                    return;
                                };
                                if request.starts_with("GET /api/sessions") {
                                    let list_index = session_list_count
                                        .fetch_add(1, Ordering::SeqCst);
                                    let _ = session_list_tx.send(request.clone());
                                    if list_index == 1 {
                                        let snapshot_body = sessions_body.lock().unwrap().clone();
                                        let (response, response_rx) = mpsc::channel();
                                        outstanding.lock().unwrap().push(response.clone());
                                        let held = HeldSessionRequest {
                                            request,
                                            response,
                                            snapshot_body: Some(snapshot_body),
                                        };
                                        if snapshot_tx.send(held).is_ok()
                                            && let Ok((status, body)) = response_rx
                                                .recv_timeout(Duration::from_secs(5))
                                        {
                                            write_mock_response(&mut stream, status, &body);
                                            return;
                                        }
                                        write_mock_response(
                                            &mut stream,
                                            500,
                                            r#"{"error":"mock snapshot timed out"}"#,
                                        );
                                        return;
                                    }
                                    let body = sessions_body.lock().unwrap().clone();
                                    write_mock_response(&mut stream, 200, &body);
                                    return;
                                }

                                if request.starts_with("PATCH /api/sessions/")
                                    || request.starts_with("DELETE /api/sessions/")
                                {
                                    let (response, response_rx) = mpsc::channel();
                                    outstanding.lock().unwrap().push(response.clone());
                                    if write_tx
                                        .send(HeldSessionRequest {
                                            request,
                                            response,
                                            snapshot_body: None,
                                        })
                                        .is_ok()
                                        && let Ok((status, body)) =
                                            response_rx.recv_timeout(Duration::from_secs(5))
                                    {
                                        write_mock_response(&mut stream, status, &body);
                                        return;
                                    }
                                    write_mock_response(
                                        &mut stream,
                                        500,
                                        r#"{"error":"mock write timed out"}"#,
                                    );
                                    return;
                                }

                                let (status, body) = if request.starts_with("GET /api/system") {
                                    (200, "{}".to_string())
                                } else if request.starts_with("GET /api/app-icon") {
                                    (200, r#"{"selected":"default"}"#.to_string())
                                } else if request.starts_with("GET /api/projects") {
                                    (
                                        200,
                                        serde_json::json!([{
                                            "id": "proj-falcon",
                                            "name": "falcon",
                                            "type": "local",
                                            "workingDir": "/workspace/falcon",
                                            "createdAt": 0
                                        }])
                                        .to_string(),
                                    )
                                } else if request.starts_with("GET /api/hosts") {
                                    (200, "[]".to_string())
                                } else if request.starts_with("GET /api/relays") {
                                    (200, r#"{"forwards":[],"shares":[]}"#.to_string())
                                } else if request.starts_with("GET /api/agents") {
                                    (200, "[]".to_string())
                                } else if request.starts_with("GET /api/plugins") {
                                    (
                                        200,
                                        r#"{"dir":"","plugins":[],"trustRoot":"unused"}"#
                                            .to_string(),
                                    )
                                } else {
                                    (500, r#"{"error":"unexpected mock request"}"#.to_string())
                                };
                                write_mock_response(&mut stream, status, &body);
                            });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => return,
                    }
                }
            });

            Self {
                url: format!("http://{address}"),
                session_list_requests,
                snapshots,
                writes,
                sessions_body,
                outstanding,
                stop,
                worker: Some(worker),
            }
        }

        fn wait_for_session_list_request(&self) -> String {
            self.session_list_requests
                .recv_timeout(Duration::from_secs(3))
                .expect("session list request should arrive")
        }

        fn wait_for_snapshot(&self) -> HeldSessionRequest {
            self.snapshots
                .recv_timeout(Duration::from_secs(3))
                .expect("delayed session snapshot should arrive")
        }

        fn wait_for_write(&self) -> HeldSessionRequest {
            self.writes
                .recv_timeout(Duration::from_secs(3))
                .expect("session write request should arrive")
        }

        fn set_sessions_body(&self, body: String) {
            *self.sessions_body.lock().unwrap() = body;
        }

        fn respond(request: HeldSessionRequest, status: u16, body: String) {
            request
                .response
                .send((status, body))
                .expect("session mock request should still be pending");
        }
    }

    impl Drop for SessionRaceMock {
        fn drop(&mut self) {
            for response in self.outstanding.lock().unwrap().iter() {
                let _ = response.send((200, r#"{"ok":true}"#.into()));
            }
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn workspace_text_file(text: &str, version: &str) -> String {
        serde_json::json!({
            "preview": {
                "kind": "text",
                "text": text,
                "size": text.len(),
                "truncated": false,
                "version": version,
            },
            "rawBase": "/api/projects/proj-falcon/raw/token/",
        })
        .to_string()
    }

    fn session_list_json(id: &str, name: &str, state: &str) -> String {
        serde_json::json!([{
            "id": id,
            "projectId": "proj-falcon",
            "projectName": "falcon",
            "projectType": "local",
            "name": name,
            "title": "mock command",
            "state": state,
            "durable": false,
            "createdAt": 0,
            "lastActiveAt": 0
        }])
        .to_string()
    }

    fn headless_with_options(opts: FalconLaunchOptions, os_scheme: ColorScheme) -> Headless {
        ensure_test_platform();
        let root = falcon_root_with_options(opts);
        let mut cfg = EngineConfig::new(root);
        cfg.viewport = Size::new(Points(1440.0), Points(900.0));
        cfg.environment.os = nsh_core::platform::Os::Linux;
        cfg.environment.idiom = Idiom::Desktop;
        cfg.environment.color_scheme = os_scheme;
        cfg.ui_sets = vec![nsh_base::registry(), registry()];
        Headless::start(cfg).expect("start headless FalconApp")
    }

    fn git_working_response(
        files: Vec<serde_json::Value>,
        conflict: Option<serde_json::Value>,
    ) -> String {
        let mut response = serde_json::json!({
            "available": true,
            "repoName": "mock",
            "fileCount": files.len(),
            "files": files,
        });
        if let Some(conflict) = conflict {
            response["conflict"] = conflict;
        }
        response.to_string()
    }

    fn git_diff_response(diff: &str) -> String {
        serde_json::json!({"available": true, "diff": diff, "truncated": false}).to_string()
    }

    fn open_mock_git_panel(server: &MockHttpServer, first_file: &str) -> Headless {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("label:activity-changes").unwrap();
        app.press("id:git-refresh").unwrap();
        app.wait_for("id:git-loading").unwrap();
        let request = server.wait_for_request();
        assert!(request.starts_with("GET /api/projects/proj-falcon/git/working"), "{request}");
        server.release_response();
        server.wait_for_response();
        let first_file_selector = format!("text:{first_file}");
        app.wait_for(&first_file_selector).unwrap();
        app
    }

    #[test]
    fn git_discard_selected_cancel_payload_failure_and_success_headless() {
        let working = git_working_response(
            vec![
                serde_json::json!({"path":"tracked.txt","index":" ","work":"M","added":1,"deleted":1}),
                serde_json::json!({"path":"new.txt","index":"?","work":"?","added":1,"deleted":0}),
                serde_json::json!({"path":"renamed.txt","origPath":"old-name.txt","index":"R","work":" ","added":1,"deleted":1}),
            ],
            None,
        );
        let empty = git_working_response(Vec::new(), None);
        let operation_path = "POST /api/projects/proj-falcon/git/op".to_string();
        let working_path = "GET /api/projects/proj-falcon/git/working".to_string();
        let server = MockHttpServer::new(vec![
            (working_path.clone(), 200, working),
            (operation_path.clone(), 200, r#"{"ok":false,"detail":"restore denied"}"#.into()),
            (operation_path, 200, r#"{"ok":true,"detail":"restored"}"#.into()),
            (working_path, 200, empty),
        ]);
        let mut app = open_mock_git_panel(&server, "tracked.txt");
        app.wait_for("id:git-file-1").unwrap();
        server.clear_observed_requests();

        app.press("id:git-discard-selected").unwrap();
        app.wait_for("id:git-confirm-action").unwrap();
        app.wait_for("text:未跟踪文件将被永久删除。").unwrap();
        app.press("id:git-cancel-confirm").unwrap();
        app.wait_for_gone("id:git-confirm-action", Duration::from_secs(2)).unwrap();
        server.assert_no_request_starting_with("POST /api/projects/proj-falcon/git/op");

        app.press("id:git-discard-selected").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let request = server.wait_for_request();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload["op"], "restore");
        assert_eq!(payload["paths"], serde_json::json!(["tracked.txt", "old-name.txt", "renamed.txt"]));
        assert_eq!(payload["untracked"], serde_json::json!(["new.txt"]));
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:restore denied").unwrap();
        app.wait_for("id:git-file-0").unwrap();
        server.assert_no_request_starting_with("GET /api/projects/proj-falcon/git/working");

        app.press("id:git-discard-all").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let request = server.wait_for_request();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload["op"], "restore");
        assert_eq!(payload["paths"], serde_json::json!(["tracked.txt", "old-name.txt", "renamed.txt"]));
        assert_eq!(payload["untracked"], serde_json::json!(["new.txt"]));
        server.release_response();
        server.wait_for_response();
        let refresh = server.wait_for_request();
        assert!(refresh.starts_with("GET /api/projects/proj-falcon/git/working"));
        server.release_response();
        server.wait_for_response();
        app.wait_for_gone("id:git-file-0", Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn git_confirmation_rejects_mapping_changes_and_pending_updates_headless() {
        let working_path = "GET /api/projects/proj-falcon/git/working".to_string();
        let operation_path = "POST /api/projects/proj-falcon/git/op";
        let working = git_working_response(
            vec![serde_json::json!({"path":"tracked.txt","index":" ","work":"M"})],
            None,
        );
        let server = MockHttpServer::new(vec![
            (working_path.clone(), 200, working.clone()),
            (working_path.clone(), 200, working),
        ]);
        let mut app = open_mock_git_panel(&server, "tracked.txt");
        server.clear_observed_requests();

        app.press("id:git-discard-selected").unwrap();
        app.wait_for("id:git-confirm-action").unwrap();
        app.press("id:test-advance-project-mapping").unwrap();
        app.wait_for_gone("id:git-confirm-action", Duration::from_secs(2)).unwrap();
        server.assert_no_request_starting_with(operation_path);

        app.press("id:git-refresh").unwrap();
        app.wait_for("id:git-loading").unwrap();
        server.wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()]);
        server.release_response();
        server.wait_for_response();
        app.wait_for_gone("id:git-loading", Duration::from_secs(2)).unwrap();
        app.wait_for("id:git-file-0").unwrap();
        server.clear_observed_requests();

        app.press("id:git-discard-selected").unwrap();
        app.wait_for("id:git-confirm-action").unwrap();
        app.press("id:test-advance-mapping-generation-only").unwrap();
        app.press("id:git-confirm-action").unwrap();
        app.wait_for_gone("id:git-confirm-action", Duration::from_secs(2)).unwrap();
        server.assert_no_request_starting_with(operation_path);

        app.press("id:git-discard-selected").unwrap();
        app.wait_for("id:git-confirm-action").unwrap();
        app.press("id:test-begin-pending-mapping-change").unwrap();
        app.press("id:git-confirm-action").unwrap();
        app.wait_for_gone("id:git-confirm-action", Duration::from_secs(2)).unwrap();
        server.assert_no_request_starting_with(operation_path);
    }

    #[test]
    fn sidebar_terminal_create_clears_git_paths_before_failed_project_refresh_headless() {
        let project_a_git = "GET /api/projects/proj-falcon/git/working".to_string();
        let project_b_git = "GET /api/projects/proj-b/git/working".to_string();
        let project_b_files = "GET /api/projects/proj-b/files";
        let project_b_agents = "GET /api/agents";
        let project_b_session = "POST /api/projects/proj-b/sessions";
        let project_b_git_operation = "POST /api/projects/proj-b/git/op";
        let server = MockHttpServer::new_with_auto_responses(
            vec![(
                project_a_git.clone(),
                200,
                git_working_response(
                    vec![serde_json::json!({"path":"a-only.txt","index":" ","work":"M"})],
                    None,
                ),
            )],
            vec![
                (
                    project_b_git.clone(),
                    500,
                    r#"{"error":"project B Git listing failed"}"#.into(),
                ),
                (
                    project_b_files.into(),
                    200,
                    r#"{"path":"/workspace/b","entries":[]}"#.into(),
                ),
                (project_b_agents.into(), 200, "[]".into()),
                (
                    project_b_session.into(),
                    500,
                    r#"{"error":"mock terminal creation failure"}"#.into(),
                ),
            ],
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-add-project-b").unwrap();
        app.wait_for("label:new-terminal-proj-b").unwrap();
        app.press("label:activity-changes").unwrap();
        app.press("id:git-refresh").unwrap();
        app.wait_for("id:git-loading").unwrap();
        let initial_git = server
            .wait_for_observed_prefixes_pumping(&mut app, &[project_a_git.as_str()]);
        assert!(initial_git[0].starts_with(&project_a_git), "{}", initial_git[0]);
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:a-only.txt").unwrap();
        app.wait_for("id:git-discard-all").unwrap();
        server.clear_observed_requests();

        app.press("label:new-terminal-proj-b").unwrap();
        app.wait_for("id:git-discard-all").unwrap();
        let requests = server.wait_for_observed_prefixes_pumping(
            &mut app,
            &[
                project_b_git.as_str(),
                project_b_files,
                project_b_agents,
                project_b_session,
            ],
        );
        let project_b_git_request = &requests[0];
        assert!(project_b_git_request.starts_with(&project_b_git));
        assert!(requests[1].starts_with(project_b_files));
        assert!(requests[2].starts_with(project_b_agents));
        assert!(requests[3].starts_with(project_b_session));
        app.wait_for_gone("id:git-file-0", Duration::from_secs(2))
            .unwrap();
        assert!(!app.screen().enabled("id:git-discard-all").unwrap());
        app.wait_for_gone("id:git-confirm-action", Duration::from_secs(2))
            .unwrap();
        server.assert_no_request_starting_with(project_b_git_operation);
        app.wait_for_gone("id:git-loading", Duration::from_secs(2))
            .unwrap();
        server.assert_no_request_starting_with(project_b_git_operation);
    }

    #[test]
    fn git_discard_confirmation_keeps_actions_visible_with_many_paths_headless() {
        let files = (0..32)
            .map(|index| serde_json::json!({
                "path": format!("many-{index:02}.txt"), "index":" ", "work":"M"
            }))
            .collect();
        let working_path = "GET /api/projects/proj-falcon/git/working".to_string();
        let server = MockHttpServer::new(vec![(
            working_path,
            200,
            git_working_response(files, None),
        )]);
        let mut app = open_mock_git_panel(&server, "many-00.txt");
        server.clear_observed_requests();

        app.press("id:git-discard-all").unwrap();
        app.wait_for("id:git-confirm-action").unwrap();
        app.press("id:git-cancel-confirm").unwrap();
        server.assert_no_request_starting_with("POST /api/projects/proj-falcon/git/op");
    }

    #[test]
    fn git_conflict_continue_abort_take_and_failures_headless() {
        let conflict = Some(serde_json::json!({"kind":"cherry-pick"}));
        let working = git_working_response(
            vec![serde_json::json!({"path":"conflict.txt","index":"U","work":"U","added":1,"deleted":1})],
            conflict,
        );
        let empty = git_working_response(Vec::new(), None);
        let working_path = "GET /api/projects/proj-falcon/git/working".to_string();
        let operation_path = "POST /api/projects/proj-falcon/git/op".to_string();
        let server = MockHttpServer::new(vec![
            (working_path.clone(), 200, working.clone()),
            (operation_path.clone(), 200, r#"{"ok":false,"detail":"continue rejected"}"#.into()),
            (operation_path.clone(), 200, r#"{"ok":true,"detail":"continued"}"#.into()),
            (working_path.clone(), 200, working.clone()),
            (operation_path.clone(), 200, r#"{"ok":true,"detail":"aborted"}"#.into()),
            (working_path.clone(), 200, working.clone()),
            (operation_path.clone(), 200, r#"{"ok":false,"detail":"take rejected"}"#.into()),
            (operation_path.clone(), 409, r#"{"error":"server locked"}"#.into()),
            (operation_path.clone(), 200, r#"{"ok":true,"detail":"taken"}"#.into()),
            (working_path.clone(), 200, empty),
        ]);
        let mut app = open_mock_git_panel(&server, "conflict.txt");
        app.wait_for("id:git-take-ours").unwrap();
        server.clear_observed_requests();

        app.press("id:git-continue").unwrap();
        let request = server.wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0].clone();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload, serde_json::json!({"op":"continue"}));
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:continue rejected").unwrap();
        server.assert_no_request_starting_with("GET /api/projects/proj-falcon/git/working");

        app.press("id:git-continue").unwrap();
        let request = server.wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0].clone();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload, serde_json::json!({"op":"continue"}));
        server.release_response();
        server.wait_for_response();
        let refresh = server.wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0].clone();
        assert!(refresh.starts_with("GET /api/projects/proj-falcon/git/working"));
        app.wait_for("id:git-loading").unwrap();
        server.release_response();
        server.wait_for_response();
        app.wait_for_gone("id:git-loading", Duration::from_secs(2)).unwrap();
        app.wait_for("id:git-take-ours").unwrap();
        server.clear_observed_requests();
        app.wait_for("id:git-file-0").unwrap();

        app.press("id:git-abort").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let request = server.wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0].clone();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload, serde_json::json!({"op":"abort"}));
        server.release_response();
        server.wait_for_response();
        let refresh = server.wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0].clone();
        assert!(refresh.starts_with("GET /api/projects/proj-falcon/git/working"));
        app.wait_for("id:git-loading").unwrap();
        server.release_response();
        server.wait_for_response();
        app.wait_for_gone("id:git-loading", Duration::from_secs(2)).unwrap();
        app.wait_for("id:git-take-ours").unwrap();
        server.clear_observed_requests();

        app.press("id:git-take-ours").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let request = server.wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0].clone();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload["op"], "take");
        assert_eq!(payload["side"], "ours");
        assert_eq!(payload["paths"], serde_json::json!(["conflict.txt"]));
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:take rejected").unwrap();
        server.assert_no_request_starting_with("GET /api/projects/proj-falcon/git/working");

        app.press("id:git-take-theirs").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let request = server.wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0].clone();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload["op"], "take");
        assert_eq!(payload["side"], "theirs");
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:server locked").unwrap();
        server.assert_no_request_starting_with("GET /api/projects/proj-falcon/git/working");

        app.press("id:git-take-ours").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let request = server.wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0].clone();
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(payload["side"], "ours");
        server.release_response();
        server.wait_for_response();
        let refresh = server.wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0].clone();
        assert!(refresh.starts_with("GET /api/projects/proj-falcon/git/working"));
        app.wait_for("id:git-loading").unwrap();
        server.release_response();
        server.wait_for_response();
        app.wait_for_gone("id:git-loading", Duration::from_secs(2)).unwrap();
        app.wait_for_gone("id:git-take-ours", Duration::from_secs(2)).unwrap();
    }

    #[test]
    fn git_multi_repo_selection_routes_working_diff_commit_and_operation_headless() {
        let repo_a = "/workspace/a";
        let repo_b = "/workspace/b";
        let working_path = "GET /api/projects/proj-falcon/git/working".to_string();
        let diff_path = "GET /api/projects/proj-falcon/git/diff".to_string();
        let commit_path = "POST /api/projects/proj-falcon/git/commit".to_string();
        let operation_path = "POST /api/projects/proj-falcon/git/op".to_string();
        let server = MockHttpServer::with_workspace_bootstrap_projects(
            vec![
                (working_path.clone(), 200, git_working_response(
                    vec![serde_json::json!({"path":"a.txt","index":" ","work":"M"})], None)),
                (working_path.clone(), 200, git_working_response(
                    vec![serde_json::json!({"path":"b.txt","index":" ","work":"M"})], None)),
                (diff_path.clone(), 200, git_diff_response(
                    "diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old-b\n+repo-b-change\n")),
                (commit_path.clone(), 200, r#"{"ok":true,"detail":"committed"}"#.into()),
                (working_path.clone(), 200, git_working_response(
                    vec![serde_json::json!({"path":"b.txt","index":" ","work":"M"})], None)),
                (operation_path.clone(), 200, r#"{"ok":true,"detail":"restored"}"#.into()),
                (working_path.clone(), 200, git_working_response(Vec::new(), None)),
            ],
            serde_json::json!([{
                "id":"proj-falcon","name":"multi","type":"local","workingDir":"/workspace","createdAt":0,
                "multi":{"repos":[{"dir":repo_a},{"dir":repo_b}]}
            }]),
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        let initial = server
            .wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0]
            .clone();
        assert!(initial.starts_with(&working_path) && initial.contains("repo=%2Fworkspace%2Fa"), "{initial}");
        server.release_response();
        server.wait_for_response();
        app.press("label:activity-changes").unwrap();
        app.wait_for("id:git-repo-select").unwrap();
        app.press("id:git-discard-selected").unwrap();
        app.wait_for("id:git-confirm-action").unwrap();
        app.press("id:git-repo-select").unwrap();
        app.press_key("down").unwrap();
        app.press_key("enter").unwrap();
        let second = server
            .wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0]
            .clone();
        assert!(second.starts_with(&working_path) && second.contains("repo=%2Fworkspace%2Fb"), "{second}");
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:git-file-0").unwrap();
        server.clear_observed_requests();
        app.press("id:git-confirm-action").unwrap();
        app.wait_for_gone("id:git-confirm-action", Duration::from_secs(2)).unwrap();
        app.wait_for("Git 操作上下文已更改，操作未发送。").unwrap();
        server.assert_no_request_starting_with(&operation_path);

        app.press("id:git-file-0").unwrap();
        let diff = server
            .wait_for_observed_prefixes_pumping(&mut app, &[diff_path.as_str()])[0]
            .clone();
        assert!(diff.starts_with(&diff_path) && diff.contains("path=b.txt")
            && diff.contains("repo=%2Fworkspace%2Fb"), "{diff}");
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:-old-b").unwrap();
        app.wait_for("text:+repo-b-change").unwrap();

        app.type_text("id:git-commit-message", "first").unwrap();
        app.press("id:git-commit").unwrap();
        let commit = server
            .wait_for_observed_prefixes_pumping(&mut app, &[commit_path.as_str()])[0]
            .clone();
        assert!(commit.starts_with(&commit_path) && commit.contains("repo=%2Fworkspace%2Fb"), "{commit}");
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&commit)).unwrap();
        assert_eq!(payload["message"], "first");
        assert_eq!(payload["all"], false);
        assert_eq!(payload["paths"], serde_json::json!(["b.txt"]));
        app.type_text("id:git-commit-message", " newer").unwrap();
        server.release_response();
        server.wait_for_response();
        let refreshed = server
            .wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0]
            .clone();
        assert!(refreshed.starts_with(&working_path) && refreshed.contains("repo=%2Fworkspace%2Fb"), "{refreshed}");
        server.release_response();
        server.wait_for_response();
        assert_eq!(app.screen().input_text("id:git-commit-message").unwrap(), Some("first newer"));

        app.press("id:git-discard-selected").unwrap();
        app.press("id:git-confirm-action").unwrap();
        let operation = server
            .wait_for_observed_prefixes_pumping(&mut app, &[operation_path.as_str()])[0]
            .clone();
        assert!(operation.starts_with(&operation_path) && operation.contains("repo=%2Fworkspace%2Fb"), "{operation}");
        let payload: serde_json::Value = serde_json::from_str(mock_request_body(&operation)).unwrap();
        assert_eq!(payload["op"], "restore");
        assert_eq!(payload["paths"], serde_json::json!(["b.txt"]));
        server.release_response();
        server.wait_for_response();
        let refreshed = server
            .wait_for_observed_prefixes_pumping(&mut app, &[working_path.as_str()])[0]
            .clone();
        assert!(refreshed.starts_with(&working_path) && refreshed.contains("repo=%2Fworkspace%2Fb"), "{refreshed}");
        server.release_response();
        server.wait_for_response();
    }

    #[test]
    fn git_diff_a_to_b_to_a_ignores_both_stale_responses_headless() {
        let working = git_working_response(
            vec![
                serde_json::json!({"path":"a.txt","origPath":"old-a.txt","index":"R","work":" "}),
                serde_json::json!({"path":"b.txt","index":"?","work":"?"}),
            ],
            None,
        );
        let working_path = "GET /api/projects/proj-falcon/git/working".to_string();
        let diff_path = "GET /api/projects/proj-falcon/git/diff".to_string();
        let server = MockHttpServer::new(vec![
            (working_path, 200, working),
            (diff_path.clone(), 200, git_diff_response(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1 @@\n+old-a-response\n")),
            (diff_path.clone(), 200, git_diff_response(
                "diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -0,0 +1 @@\n+stale-b-response\n")),
            (diff_path.clone(), 200, git_diff_response(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1 @@\n+latest-a-response\n")),
        ]);
        let mut app = open_mock_git_panel(&server, "a.txt");
        app.wait_for("id:git-file-1").unwrap();
        server.clear_observed_requests();

        app.press("id:git-file-0").unwrap();
        let first = server.wait_for_request();
        assert!(first.starts_with(&diff_path)
            && first.contains("path=a.txt")
            && first.contains("origPath=old-a.txt"), "{first}");
        app.press("id:git-file-1").unwrap();
        app.press("id:git-file-0").unwrap();
        server.release_response();
        server.wait_for_response();

        let second = server.wait_for_request();
        assert!(second.starts_with(&diff_path)
            && second.contains("path=b.txt")
            && second.contains("untracked"), "{second}");
        server.release_response();
        server.wait_for_response();

        let third = server.wait_for_request();
        assert!(third.starts_with(&diff_path)
            && third.contains("path=a.txt")
            && third.contains("origPath=old-a.txt"), "{third}");
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:+latest-a-response").unwrap();
        assert!(!app.shows("text:+old-a-response"));
        assert!(!app.shows("text:+stale-b-response"));
    }

    #[test]
    fn git_conflict_status_recognizes_all_unmerged_pairs() {
        for (index, work) in [
            ("U", "U"), ("A", "A"), ("D", "D"), ("A", "U"),
            ("U", "A"), ("D", "U"), ("U", "D"),
        ] {
            let change = crate::store::GitChangeItem {
                path: "conflict.txt".into(),
                orig_path: None,
                index: index.into(),
                work: work.into(),
                untracked: false,
                status: format!("{index}{work}"),
                staged: true,
                additions: 0,
                deletions: 0,
                old_text: String::new(),
                new_text: String::new(),
            };
            assert!(crate::store::git_change_is_conflicted(&change), "{index}{work}");
        }
    }

    fn ssh_host_value(
        id: &str,
        name: &str,
        host: &str,
        port: u16,
        username: &str,
        project_count: u32,
    ) -> serde_json::Value {
        ssh_host_value_with_auth(
            id,
            name,
            host,
            port,
            username,
            "password",
            Some("~/.ssh/id_ed25519"),
            project_count,
        )
    }

    fn ssh_host_value_with_auth(
        id: &str,
        name: &str,
        host: &str,
        port: u16,
        username: &str,
        auth_method: &str,
        key_path: Option<&str>,
        project_count: u32,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": name,
            "host": host,
            "port": port,
            "username": username,
            "authMethod": auth_method,
            "keyPath": key_path,
            "hasSecret": true,
            "projectCount": project_count,
            "createdAt": 0
        })
    }

    fn open_hosts_settings(app: &mut Headless) {
        app.first_render().expect("first render");
        app.press("label:sidebar-settings").expect("open settings");
        app.wait_for("label:settings-tab-Hosts")
            .expect("hosts settings tab");
        app.press("label:settings-tab-Hosts")
            .expect("open hosts settings");
        app.wait_for("id:host-add-manual")
            .expect("host form actions");
    }

    /// ADR 0006 铁律回归：当系统处于浅色模式且用户在浅色槽位放了深底 Ghostty 主题（Dracula）时，
    /// 根 `Appearance` 与界面派生必须严格按主题底色亮度判定为深色，绝不被系统 `ColorScheme::Light` 带偏。
    #[test]
    fn adr_0006_dark_theme_in_light_slot_sets_dark_appearance_headless() {
        ensure_test_platform();
        let catalog = load_catalog();
        let dracula = find_theme(catalog, "Dracula").expect("Dracula theme in Ghostty catalog");
        let mut settings = ThemeSettings::default();
        settings.mode = ThemePref::Light;
        settings.light = choice_of(dracula);

        let opts = FalconLaunchOptions {
            offline_demo: true,
            server_url: Some("http://127.0.0.1:4923".into()),
            initial_theme: Some(settings.clone()),
            #[cfg(test)]
            test_client_url: None,
            #[cfg(test)]
            test_switch_url: None,
            #[cfg(test)]
            test_live_file_operations: false,
        };

        let root = falcon_root_with_options(opts.clone());
        assert_eq!(root.appearance_ref(), Some(Appearance::Dark));

        let mut app = headless_with_options(opts, ColorScheme::Light);
        app.first_render().expect("first render");
        app.press("label:sidebar-settings").unwrap();
        app.wait_for("当前底色判定：深色外观")
            .expect("surface derived dark appearance despite OS ColorScheme::Light and Light slot");

        // 对照验证：浅色槽位放浅底主题时判定为浅色外观
        let light_entry = find_theme(catalog, "Falcon Light")
            .or_else(|| catalog.iter().find(|e| !derive_theme(&e.colors).is_dark))
            .expect("at least one light theme in catalog");
        let mut light_settings = ThemeSettings::default();
        light_settings.mode = ThemePref::Light;
        light_settings.light = choice_of(light_entry);
        assert_eq!(light_settings.slot(ThemeMode::Light).name, light_entry.name);

        let mut light_app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                server_url: Some("http://127.0.0.1:4923".into()),
                initial_theme: Some(light_settings),
                #[cfg(test)]
                test_client_url: None,
                #[cfg(test)]
                test_switch_url: None,
                #[cfg(test)]
                test_live_file_operations: false,
            },
            ColorScheme::Dark,
        );
        light_app.first_render().expect("first render light");
        light_app.press("label:sidebar-settings").unwrap();
        light_app
            .wait_for("当前底色判定：浅色外观")
            .expect("surface derived light appearance despite OS ColorScheme::Dark");
    }

    /// 浮动岛工作区与会话生命周期无头端到端测试：
    /// 1. 首屏同时展示源项目、worktree 附属项目、SSH 远端主机、窗口标题栏 `ProgramStatus` 胶囊与右侧栏系统监控插件；
    /// 2. 点击「收起窗口」(Detach) 仅移除窗口，侧栏出现「后台运行中」与「接回窗口」，持久会话不丢；
    /// 3. 点击「接回窗口」(Reattach) 恢复窗口；
    /// 4. 对正在运行前台命令的会话点「终止会话」先触发忙进程确认护栏，确认「仍然终止」后才销毁会话。
    #[test]
    fn headless_floating_islands_detach_keeps_session_alive_vs_terminate_guard() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                server_url: Some("http://127.0.0.1:4923".into()),
                initial_theme: None,
                #[cfg(test)]
                test_client_url: None,
                #[cfg(test)]
                test_switch_url: None,
                #[cfg(test)]
                test_live_file_operations: false,
            },
            ColorScheme::Dark,
        );
        app.first_render().expect("first render");

        // 首屏领域元素校验
        app.wait_for("feat/nsh-unified-client").unwrap();
        app.wait_for("build-box-01").unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();
        app.wait_for("cargo check · 1.2s").unwrap();
        app.wait_for("14.2%").unwrap();

        // 1. Detach（收起窗口）：窗口从列式画布摘除，但会话仍在侧栏标记为后台运行中且可接回
        app.press("label:detach-t:sess-main")
            .expect("press detach button on sess-main");
        app.wait_for("label:reattach-sess-main").unwrap();

        // 2. Reattach（接回窗口）：从侧栏重新挂回列式画布
        app.press("label:reattach-sess-main")
            .expect("press reattach button on sess-main");
        app.wait_for_gone("label:reattach-sess-main", Duration::from_secs(2))
            .unwrap();

        // 3. Terminate（终止会话）：sess-main 有前台命令 `cargo check -p falcon-nsh`，必须先弹出确认框
        app.press("label:terminate-sess-main")
            .expect("press terminate button on busy session");
        app.wait_for("前台程序仍在运行").unwrap();
        app.press("仍然终止").expect("confirm force terminate");
        app.wait_for_gone("cargo check -p falcon-nsh", Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn failed_http_termination_keeps_session_and_pane_headless() {
        let mock = MockHttpServer::new(vec![(
            "POST /api/sessions/sess-main/terminate".into(),
            500,
            r#"{"error":"mock termination failed"}"#.into(),
        )]);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.wait_for("id:active-pane-t:sess-main").unwrap();
        app.press("label:terminate-sess-main").unwrap();
        app.wait_for("前台程序仍在运行").unwrap();
        app.press("仍然终止").unwrap();
        let request = mock.wait_for_request();
        assert_eq!(request, "POST /api/sessions/sess-main/terminate HTTP/1.1");
        app.press("仍然终止").unwrap();
        mock.release_response();

        app.wait_for("mock termination failed").unwrap();
        app.wait_for("cargo check -p falcon-nsh").unwrap();
        app.wait_for("id:active-pane-t:sess-main").unwrap();
        app.wait_for("label:terminate-sess-main").unwrap();
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn session_rename_failure_keeps_the_typed_draft_headless() {
        let mock = MockHttpServer::with_workspace_bootstrap_and_sessions(
            vec![(
                "PATCH /api/sessions/sess-main".into(),
                500,
                r#"{"error":"mock rename failed"}"#.into(),
            )],
            serde_json::from_str(&session_list_json("sess-main", "DraftBase", "active"))
                .unwrap(),
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                test_switch_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-switch-server").unwrap();
        app.wait_for("server-connected=true").unwrap();
        app.wait_for("DraftBase").unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();
        app.right_click("label:session-sess-main").unwrap();
        app.press("重命名会话…").unwrap();

        let input = "id:rename-session-input";
        app.wait_for(input).unwrap();
        assert_eq!(app.screen().input_text(input).unwrap(), Some("DraftBase".into()));
        app.type_text(input, " updated").unwrap();
        let draft = "DraftBase updated";
        assert_eq!(app.screen().input_text(input).unwrap(), Some(draft.into()));
        app.press("id:rename-session-submit").unwrap();
        let request = mock.wait_for_request();
        assert!(request.starts_with("PATCH /api/sessions/sess-main"), "{request}");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(mock_request_body(&request)).unwrap(),
            serde_json::json!({"name": draft})
        );
        assert_eq!(app.screen().input_text(input).unwrap(), Some(draft.into()));

        mock.release_response();
        app.wait_for("会话重命名失败").unwrap();
        assert_eq!(app.screen().input_text(input).unwrap(), Some(draft.into()));
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn session_rename_empty_name_restores_the_automatic_title_headless() {
        let mock = MockHttpServer::with_workspace_bootstrap_and_sessions(
            vec![(
                "PATCH /api/sessions/sess-main".into(),
                200,
                r#"{"ok":true}"#.into(),
            )],
            serde_json::from_str(&session_list_json("sess-main", "Manual", "active")).unwrap(),
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                test_switch_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-switch-server").unwrap();
        app.wait_for("Manual").unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();
        app.right_click("label:session-sess-main").unwrap();
        app.press("重命名会话…").unwrap();
        let input = "id:rename-session-input";
        app.wait_for(input).unwrap();
        app.type_text(input, " ").unwrap();
        for _ in 0..7 {
            app.press_key("backspace").unwrap();
        }
        assert_eq!(app.screen().input_text(input).unwrap(), Some(""));
        app.press("id:rename-session-submit").unwrap();
        let request = mock.wait_for_request();
        assert!(request.starts_with("PATCH /api/sessions/sess-main"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(mock_request_body(&request)).unwrap(),
            serde_json::json!({"name": ""})
        );
        mock.release_response();
        app.wait_for_gone(input, Duration::from_secs(2)).unwrap();
        app.wait_for("mock command").unwrap();
    }

    #[test]
    fn session_clear_failure_keeps_the_dead_record_headless() {
        let mock = MockHttpServer::with_workspace_bootstrap_and_sessions(
            vec![(
                "DELETE /api/sessions/sess-dead".into(),
                500,
                r#"{"error":"mock clear failed"}"#.into(),
            )],
            serde_json::from_str(&session_list_json("sess-dead", "Lost record", "dead"))
                .unwrap(),
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                test_switch_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-switch-server").unwrap();
        app.wait_for("Lost record").unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();
        app.right_click("label:session-sess-dead").unwrap();
        app.press("清除记录").unwrap();
        let request = mock.wait_for_request();
        assert!(
            request.starts_with("DELETE /api/sessions/sess-dead"),
            "clear record must use clear_session: {request}"
        );
        mock.release_response();
        app.wait_for("清除会话记录失败").unwrap();
        app.wait_for("label:session-sess-dead").unwrap();
    }

    #[test]
    fn session_rename_retries_a_stale_bootstrap_snapshot_headless() {
        let old_sessions = session_list_json("sess-main", "Before", "active");
        let mock = SessionRaceMock::new(old_sessions);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                test_switch_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-switch-server").unwrap();
        assert!(mock
            .wait_for_session_list_request()
            .starts_with("GET /api/sessions"));
        app.wait_for("Before").unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();

        app.press("id:test-switch-server").unwrap();
        assert!(mock
            .wait_for_session_list_request()
            .starts_with("GET /api/sessions"));
        app.wait_for("server-connected=true").unwrap();
        let snapshot = mock.wait_for_snapshot();

        app.right_click("label:session-sess-main").unwrap();
        app.press("重命名会话…").unwrap();
        let input = "id:rename-session-input";
        app.wait_for(input).unwrap();
        app.type_text(input, " After").unwrap();
        app.press("id:rename-session-submit").unwrap();
        let write = mock.wait_for_write();
        assert!(write.request.starts_with("PATCH /api/sessions/sess-main"));
        let stale_body = snapshot
            .snapshot_body
            .as_ref()
            .expect("delayed list captured its old body")
            .clone();
        mock.set_sessions_body(session_list_json("sess-main", "Before After", "active"));

        SessionRaceMock::respond(write, 200, r#"{"ok":true}"#.into());
        app.wait_for_gone(input, Duration::from_secs(2)).unwrap();
        SessionRaceMock::respond(snapshot, 200, stale_body);
        assert!(mock
            .wait_for_session_list_request()
            .starts_with("GET /api/sessions"));
        app.wait_for("Before After").unwrap();
    }

    #[test]
    fn clearing_dead_session_does_not_resurrect_from_a_stale_snapshot_headless() {
        let stale_sessions = session_list_json("sess-dead", "Lost record", "dead");
        let mock = SessionRaceMock::new(stale_sessions);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                test_switch_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-switch-server").unwrap();
        assert!(mock
            .wait_for_session_list_request()
            .starts_with("GET /api/sessions"));
        app.wait_for("Lost record").unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();

        app.press("id:test-switch-server").unwrap();
        assert!(mock
            .wait_for_session_list_request()
            .starts_with("GET /api/sessions"));
        app.wait_for("server-connected=true").unwrap();
        let snapshot = mock.wait_for_snapshot();
        app.right_click("label:session-sess-dead").unwrap();
        app.press("清除记录").unwrap();
        let write = mock.wait_for_write();
        assert!(
            write.request.starts_with("DELETE /api/sessions/sess-dead"),
            "clear record must use DELETE, not terminate: {}",
            write.request
        );
        let stale_body = snapshot
            .snapshot_body
            .as_ref()
            .expect("delayed list captured the dead record")
            .clone();
        mock.set_sessions_body("[]".into());

        SessionRaceMock::respond(write, 200, r#"{"ok":true}"#.into());
        app.wait_for_gone("label:session-sess-dead", Duration::from_secs(2))
            .unwrap();
        SessionRaceMock::respond(snapshot, 200, stale_body);
        assert!(mock
            .wait_for_session_list_request()
            .starts_with("GET /api/sessions"));
        app.wait_for_gone("label:session-sess-dead", Duration::from_secs(2))
            .unwrap();
        assert!(!app.shows("Lost record"), "stale snapshot did not restore the record");
    }

    #[test]
    fn session_same_server_reconnect_preserves_file_pane_and_dirty_buffer_headless() {
        let mock = MockHttpServer::with_workspace_bootstrap_and_sessions(
            Vec::new(),
            serde_json::from_str(&session_list_json(
                "sess-main",
                "Reconnect target",
                "active",
            ))
            .unwrap(),
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                test_switch_url: Some(mock.url.clone()),
                test_live_file_operations: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-switch-server").unwrap();
        app.wait_for("Reconnect target").unwrap();

        app.press("id:test-install-dirty-file-pane").unwrap();
        app.wait_for("reconnect-file=true:dirty-reconnect").unwrap();

        app.press("id:test-mark-session-list-sentinel").unwrap();
        app.wait_for("session-list-sentinel").unwrap();
        mock.clear_observed_requests();
        app.press("id:test-switch-server").unwrap();
        app.wait_for("Reconnect target").unwrap();
        assert!(mock
            .wait_for_observed_prefixes(&[
                "GET /api/projects HTTP/1.1",
                "GET /api/sessions",
            ])
            .first()
            .is_some_and(|request| request.starts_with("GET /api/projects HTTP/1.1")));
        assert!(app.shows("reconnect-file=true:dirty-reconnect"));
    }

    #[test]
    fn session_restored_file_pane_blocks_project_mapping_change_headless() {
        let mock = MockHttpServer::new(Vec::new());
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(mock.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-install-restored-file-pane").unwrap();
        app.wait_for("restored-file-pane-mapping=true").unwrap();
    }

    #[test]
    fn failed_http_save_preserves_dirty_hash_then_success_clears_it_headless() {
        let path = "native/mock-save-integration.txt";
        let file_key = falcon_core::pane_key::file_key("proj-falcon", path);
        let mock = MockHttpServer::new(vec![
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("", "disk-v1"),
            ),
            (
                "PUT /api/projects/proj-falcon/file".into(),
                500,
                r#"{"error":"mock save failed"}"#.into(),
            ),
            (
                "PUT /api/projects/proj-falcon/file".into(),
                200,
                serde_json::json!({
                    "path": path,
                    "size": 20,
                    "version": "disk-v2",
                })
                .to_string(),
            ),
        ]);
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
        app.press("id:test-open-save-file").unwrap();
        let read_request = mock
            .wait_for_observed_prefixes_pumping(
                &mut app,
                &["GET /api/projects/proj-falcon/file"],
            )[0]
            .clone();
        app.wait_for("save-state=Loading").unwrap();
        assert!(
            read_request.starts_with("GET /api/projects/proj-falcon/file"),
            "unexpected read request: {read_request}"
        );
        mock.release_response();
        mock.wait_for_response();

        let editor = format!("id:file-editor-{file_key}");
        app.wait_for(&editor).unwrap();
        app.wait_for("save-hash=disk-v1").unwrap();
        app.type_text(&editor, "first line\nsecond line\n").unwrap();
        app.wait_for("save-dirty=true").unwrap();
        assert_eq!(
            app.screen().input_text(&editor).unwrap(),
            Some("first line\nsecond line\n")
        );

        let save_button = format!("id:file-save-{file_key}");
        app.wait_for(&save_button).unwrap();
        app.type_text(&editor, "").unwrap();
        app.press(&save_button).unwrap();
        let failed_save = mock
            .wait_for_observed_prefixes_pumping(
                &mut app,
                &["PUT /api/projects/proj-falcon/file"],
            )[0]
            .clone();
        app.wait_for("save-saving=true").unwrap();
        assert!(failed_save.starts_with("PUT /api/projects/proj-falcon/file"));
        assert_eq!(mock_request_body(&failed_save), "first line\nsecond line\n");
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("mock save failed").unwrap();
        app.wait_for("save-dirty=true").unwrap();
        app.wait_for("save-saving=false").unwrap();
        app.wait_for("save-hash=disk-v1").unwrap();

        app.wait_for(&save_button).unwrap();
        app.type_text(&editor, "").unwrap();
        app.press(&save_button).unwrap();
        let retry = mock
            .wait_for_observed_prefixes_pumping(
                &mut app,
                &["PUT /api/projects/proj-falcon/file"],
            )[0]
            .clone();
        app.wait_for("save-saving=true").unwrap();
        assert!(retry.starts_with("PUT /api/projects/proj-falcon/file"));
        assert!(
            retry.contains("base=disk-v1"),
            "retry used unexpected base: {retry}"
        );
        assert_eq!(mock_request_body(&retry), "first line\nsecond line\n");
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("save-dirty=false").unwrap();
        app.wait_for("save-saving=false").unwrap();
        app.wait_for("save-hash=disk-v2").unwrap();
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn pending_file_save_after_pane_removal_blocks_mapping_change_headless() {
        let mock = MockHttpServer::with_workspace_bootstrap(vec![(
            "PUT /api/projects/proj-falcon/file".into(),
            200,
            serde_json::json!({
                "path": "native/mock-save-integration.txt",
                "size": 5,
                "version": "disk-v2",
            })
            .to_string(),
        )]);
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
        app.press("id:test-save-after-pane-removal").unwrap();
        app.wait_for("text:dispatched=true;blocked=true").unwrap();
        let request = mock.wait_for_request();
        assert!(request.starts_with("PUT /api/projects/proj-falcon/file"));
        mock.release_response();
        mock.wait_for_response();
    }

    #[test]
    fn file_editor_multiline_input_persists_full_body_headless() {
        let path = "native/mock-save-integration.txt";
        let file_key = falcon_core::pane_key::file_key("proj-falcon", path);
        let mock = MockHttpServer::new(vec![
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("", "editor-v1"),
            ),
            (
                "PUT /api/projects/proj-falcon/file".into(),
                200,
                serde_json::json!({
                    "path": path,
                    "size": 23,
                    "version": "editor-v2",
                })
                .to_string(),
            ),
        ]);
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
        app.press("id:test-open-save-file").unwrap();
        app.wait_for("save-state=Loading").unwrap();
        assert!(
            mock.wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/file")
        );
        mock.release_response();
        mock.wait_for_response();

        let editor = format!("id:file-editor-{file_key}");
        app.wait_for(&editor).unwrap();
        app.wait_for("save-hash=editor-v1").unwrap();
        app.type_text(&editor, "first line\nsecond line\n").unwrap();
        app.wait_for("save-dirty=true").unwrap();
        assert_eq!(
            app.screen().input_text(&editor).unwrap(),
            Some("first line\nsecond line\n")
        );
        let save_button = format!("id:file-save-{file_key}");
        app.wait_for(&save_button).unwrap();
        app.type_text(&editor, "").unwrap();
        app.press(&save_button).unwrap();
        app.wait_for("save-saving=true").unwrap();
        let save_request = mock.wait_for_request();
        assert_eq!(mock_request_body(&save_request), "first line\nsecond line\n");
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("save-dirty=false").unwrap();
        app.wait_for("save-hash=editor-v2").unwrap();
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn file_editor_reload_replaces_body_after_generation_change_headless() {
        let path = "native/mock-save-integration.txt";
        let file_key = falcon_core::pane_key::file_key("proj-falcon", path);
        let mock = MockHttpServer::new(vec![
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("old body", "old-v1"),
            ),
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("replacement body", "new-v2"),
            ),
        ]);
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
        app.press("id:test-open-save-file").unwrap();
        let initial = mock.wait_for_request();
        assert!(initial.starts_with("GET /api/projects/proj-falcon/file"));
        mock.release_response();
        mock.wait_for_response();

        let editor = format!("id:file-editor-{file_key}");
        app.wait_for(&editor).unwrap();
        assert_eq!(
            app.screen().input_text(&editor).unwrap(),
            Some("old body")
        );
        app.press("id:test-reload-save-file").unwrap();
        app.wait_for("save-state=Loading").unwrap();
        let reload = mock.wait_for_request();
        assert!(reload.starts_with("GET /api/projects/proj-falcon/file"));
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("save-hash=new-v2").unwrap();
        assert_eq!(
            app.screen().input_text(&editor).unwrap(),
            Some("replacement body")
        );
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn file_editor_preview_toggle_preserves_current_body_headless() {
        let path = "native/mock-preview.md";
        let file_key = falcon_core::pane_key::file_key("proj-falcon", path);
        let mock = MockHttpServer::new(vec![(
            "GET /api/projects/proj-falcon/file".into(),
            200,
            workspace_text_file("preview body", "preview-v1"),
        )]);
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
        let request = mock.wait_for_request();
        assert!(request.starts_with("GET /api/projects/proj-falcon/file"));
        mock.release_response();
        mock.wait_for_response();

        let preview_toggle = format!("id:file-markdown-preview-{file_key}");
        let editor = format!("id:file-editor-{file_key}");
        app.wait_for("preview body").unwrap();
        app.press(&preview_toggle).unwrap();
        app.wait_for(&editor).unwrap();
        app.type_text(&editor, " edited").unwrap();
        assert_eq!(
            app.screen().input_text(&editor).unwrap(),
            Some("preview body edited")
        );
        app.press(&preview_toggle).unwrap();
        app.wait_for_gone(&editor, Duration::from_secs(2)).unwrap();
        app.wait_for("preview body edited").unwrap();
        app.press(&preview_toggle).unwrap();
        app.wait_for(&editor).unwrap();
        assert_eq!(
            app.screen().input_text(&editor).unwrap(),
            Some("preview body edited")
        );
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn focusing_session_in_another_project_refreshes_project_data_headless() {
        let mock = MockHttpServer::new_with_auto_responses(
            vec![("GET /__mock_hold__".into(), 200, "{}".into())],
            vec![
                ("GET /api/".into(), 500, r#"{"error":"mock"}"#.into()),
                ("POST /api/".into(), 500, r#"{"error":"mock"}"#.into()),
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
        app.press("id:test-focus-agent-session").unwrap();
        app.wait_for("active-project=proj-falcon-nsh").unwrap();
        app.wait_for("id:active-pane-t:sess-agent").unwrap();
        let observed = mock.wait_for_observed_prefixes_pumping(&mut app, &[
            "GET /api/projects/proj-falcon-nsh/files",
            "GET /api/agents",
        ]);
        let file_request = &observed[0];
        assert!(file_request.contains("proj-falcon-nsh"));
        let agent_request = &observed[1];
        assert!(agent_request.contains("proj-falcon-nsh"));
    }

    #[test]
    fn stale_http_read_after_reopen_does_not_mutate_new_buffer_headless() {
        let mock = MockHttpServer::new(vec![
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("old response", "old-v1"),
            ),
            (
                "GET /api/projects/proj-falcon/file".into(),
                200,
                workspace_text_file("new response", "new-v1"),
            ),
        ]);
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
        app.press("id:test-open-stale-file").unwrap();
        app.wait_for("stale-state=Loading").unwrap();
        let first_read = mock.wait_for_request();
        assert!(first_read.starts_with("GET /api/projects/proj-falcon/file"));
        app.press("id:test-close-stale-file").unwrap();
        app.wait_for("stale-state=closed").unwrap();

        app.press("id:test-open-stale-file").unwrap();
        app.wait_for("stale-state=Loading").unwrap();
        mock.release_response();
        mock.wait_for_response();
        let second_read = mock.wait_for_request();
        assert!(second_read.starts_with("GET /api/projects/proj-falcon/file"));
        for _ in 0..5 {
            app.next_change(Duration::from_millis(50)).unwrap();
            assert!(app.shows("stale-state=Loading"));
        }

        mock.release_response();
        mock.wait_for_response();
        app.wait_for("stale-state=Editable").unwrap();
        app.wait_for("stale-hash=new-v1").unwrap();
        app.wait_for("stale-body=new response").unwrap();
        assert!(!app.shows("stale-body=old response"));
    }

    #[test]
    fn stale_http_read_after_server_switch_does_not_mutate_new_buffer_headless() {
        let old_server = MockHttpServer::with_workspace_bootstrap(vec![(
            "GET /api/projects/proj-falcon/file?".into(),
            200,
            workspace_text_file("old server response", "old-server-v1"),
        )]);
        let new_server = MockHttpServer::with_workspace_bootstrap(vec![(
            "GET /api/projects/proj-falcon/file?".into(),
            200,
            workspace_text_file("new server response", "new-server-v1"),
        )]);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(old_server.url.clone()),
                test_switch_url: Some(new_server.url.clone()),
                test_live_file_operations: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-open-stale-file").unwrap();
        app.wait_for("stale-state=Loading").unwrap();
        assert!(
            old_server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/file?")
        );

        app.press("id:test-switch-server").unwrap();
        app.wait_for(&format!("server-url={}", new_server.url))
            .unwrap();
        app.wait_for("server-connected=true").unwrap();
        app.press("id:test-open-stale-file").unwrap();
        app.wait_for("stale-state=Loading").unwrap();
        assert!(
            new_server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/file?")
        );

        old_server.release_response();
        old_server.wait_for_response();
        for _ in 0..5 {
            app.next_change(Duration::from_millis(50)).unwrap();
            assert!(app.shows("stale-state=Loading"));
            assert!(!app.shows("stale-body=old server response"));
        }

        new_server.release_response();
        new_server.wait_for_response();
        app.wait_for("stale-hash=new-server-v1").unwrap();
        app.wait_for("stale-body=new server response").unwrap();
        old_server.assert_no_duplicate_request();
        new_server.assert_no_duplicate_request();
    }

    #[test]
    fn stale_file_read_after_project_mapping_change_is_ignored_headless() {
        let server = MockHttpServer::with_workspace_bootstrap(vec![(
            "GET /api/projects/proj-falcon/file?".into(),
            200,
            workspace_text_file("stale mapping response", "mapping-v1"),
        )]);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(server.url.clone()),
                test_live_file_operations: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-open-stale-file").unwrap();
        app.wait_for("stale-state=Loading").unwrap();
        assert!(server
            .wait_for_request()
            .starts_with("GET /api/projects/proj-falcon/file?"));

        app.press("id:test-advance-project-mapping").unwrap();
        server.release_response();
        server.wait_for_response();
        for _ in 0..5 {
            app.next_change(Duration::from_millis(50)).unwrap();
            assert!(app.shows("stale-state=Loading"));
            assert!(!app.shows("stale-body=stale mapping response"));
        }
        server.assert_no_duplicate_request();
    }

    #[test]
    fn file_operations_mkdir_rename_delete_partial_failure_and_keyboard_headless() {
        let listing = |entries: &[(&str, &str)]| {
            serde_json::json!({
                "path": "/workspace/falcon",
                "entries": entries.iter().map(|(name, kind)| serde_json::json!({
                    "name": name,
                    "path": format!("/workspace/falcon/{name}"),
                    "kind": kind,
                    "size": 1
                })).collect::<Vec<_>>(),
                "truncated": false
            })
            .to_string()
        };
        let list_prefix = "GET /api/projects/proj-falcon/files?path=";
        let server = MockHttpServer::new_with_auto_responses(
            vec![
                (list_prefix.into(), 200, listing(&[("old.txt", "file"), ("keep.txt", "file")])),
                (
                    "POST /api/projects/proj-falcon/mkdir".into(),
                    200,
                    r#"{"path":"/workspace/falcon/docs"}"#.into(),
                ),
                (
                    list_prefix.into(),
                    200,
                    listing(&[("docs", "dir"), ("old.txt", "file"), ("keep.txt", "file")]),
                ),
                (
                    "POST /api/projects/proj-falcon/rename".into(),
                    200,
                    r#"{"path":"/workspace/falcon/old.txt-renamed"}"#.into(),
                ),
                (
                    list_prefix.into(),
                    200,
                    listing(&[("docs", "dir"), ("old.txt-renamed", "file"), ("keep.txt", "file")]),
                ),
                (
                    "POST /api/projects/proj-falcon/remove".into(),
                    200,
                    r#"{"removed":["/workspace/falcon/old.txt-renamed"],"errors":[{"path":"/workspace/falcon/keep.txt","error":"permission denied"}]}"#.into(),
                ),
                (
                    list_prefix.into(),
                    200,
                    listing(&[("docs", "dir"), ("keep.txt", "file")]),
                ),
            ],
            Vec::new(),
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(server.url.clone()),
                test_live_file_operations: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("label:activity-files").unwrap();
        app.wait_for("label:files-refresh").unwrap();
        if !app.shows("id:files-loading") {
            app.press("label:files-refresh").unwrap();
        }
        app.wait_for("id:files-loading").unwrap();
        assert!(server.wait_for_observed_prefixes_pumping(&mut app, &[list_prefix])[0]
            .starts_with(list_prefix));
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:file-select-old.txt").unwrap();

        app.press("id:file-operations-new-folder").unwrap();
        app.wait_for("id:file-operation-name").unwrap();
        app.type_text("id:file-operation-name", "docs").unwrap();
        app.press_key("enter").unwrap();
        let request = server
            .wait_for_observed_prefixes_pumping(
                &mut app,
                &["POST /api/projects/proj-falcon/mkdir"],
            )[0]
            .clone();
        assert!(request.starts_with("POST /api/projects/proj-falcon/mkdir"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(mock_request_body(&request)).unwrap(),
            serde_json::json!({"path":"/workspace/falcon/docs","recursive":false})
        );
        server.release_response();
        server.wait_for_response();
        assert!(server.wait_for_observed_prefixes_pumping(&mut app, &[list_prefix])[0]
            .starts_with(list_prefix));
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:file-select-docs").unwrap();

        app.press("id:file-select-old.txt").unwrap();
        app.press("id:file-operations-rename").unwrap();
        app.wait_for("id:file-operation-name").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:file-operation-name", Duration::from_secs(2))
            .unwrap();
        server.assert_no_request_starting_with("POST /api/projects/proj-falcon/rename");

        app.press("id:file-operations-rename").unwrap();
        app.wait_for("id:file-operation-name").unwrap();
        app.press("id:file-select-old.txt").unwrap();
        app.type_text("id:file-operation-name", "-renamed").unwrap();
        app.press_key("enter").unwrap();
        let request = server
            .wait_for_observed_prefixes_pumping(
                &mut app,
                &["POST /api/projects/proj-falcon/rename"],
            )[0]
            .clone();
        assert!(request.starts_with("POST /api/projects/proj-falcon/rename"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(mock_request_body(&request)).unwrap(),
            serde_json::json!({"path":"/workspace/falcon/old.txt","name":"old.txt-renamed"})
        );
        server.release_response();
        server.wait_for_response();
        assert!(server.wait_for_observed_prefixes_pumping(&mut app, &[list_prefix])[0]
            .starts_with(list_prefix));
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:file-select-old.txt-renamed").unwrap();

        app.press("id:file-select-old.txt-renamed").unwrap();
        app.press("id:file-select-keep.txt").unwrap();
        app.press("id:file-operations-delete").unwrap();
        app.wait_for("id:file-operation-confirmation").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:file-operation-confirmation", Duration::from_secs(2))
            .unwrap();
        server.assert_no_request_starting_with("POST /api/projects/proj-falcon/remove");

        app.press("id:file-operations-delete").unwrap();
        app.wait_for("id:file-operation-confirmation").unwrap();
        app.press("id:file-operation-confirm-confirmation").unwrap();
        let request = server
            .wait_for_observed_prefixes_pumping(
                &mut app,
                &["POST /api/projects/proj-falcon/remove"],
            )[0]
            .clone();
        assert!(request.starts_with("POST /api/projects/proj-falcon/remove"));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(mock_request_body(&request)).unwrap(),
            serde_json::json!({"paths":["/workspace/falcon/keep.txt","/workspace/falcon/old.txt-renamed"]})
        );
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:/permission denied/").unwrap();
        assert!(server.wait_for_observed_prefixes_pumping(&mut app, &[list_prefix])[0]
            .starts_with(list_prefix));
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:file-select-keep.txt").unwrap();
        server.assert_no_duplicate_request();
    }

    #[test]
    fn selecting_server_profile_preserves_dirty_file_confirmation_headless() {
        let switch_url = "http://127.0.0.1:49317".to_string();
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_switch_url: Some(switch_url),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("label:activity-files").unwrap();
        app.press("text:Cargo.toml").unwrap();
        let file_key = falcon_core::pane_key::file_key("proj-falcon", "native/Cargo.toml");
        let editor = format!("id:file-editor-{file_key}");
        let titlebar = format!("label:titlebar-{file_key}");
        app.type_text(&editor, "dirty-switch-check").unwrap();
        app.press("id:test-open-connect-server").unwrap();
        app.wait_for("id:connect-server-profile-test-switch-profile")
            .unwrap();
        app.press("id:connect-server-profile-test-switch-profile")
            .unwrap();
        app.wait_for("文件有未保存的修改").unwrap();
        app.press("id:unsaved-cancel").unwrap();
        app.wait_for(&titlebar).unwrap();
    }

    #[test]
    fn stale_quick_open_index_after_server_switch_is_ignored_headless() {
        let old_server = MockHttpServer::with_workspace_bootstrap(vec![(
            "GET /api/projects/proj-falcon/files/index".into(),
            200,
            serde_json::json!({"paths":["old-server.rs"],"truncated":false}).to_string(),
        )]);
        let new_server = MockHttpServer::with_workspace_bootstrap(vec![(
            "GET /api/projects/proj-falcon/files/index".into(),
            200,
            serde_json::json!({"paths":["new-server.rs"],"truncated":false}).to_string(),
        )]);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(old_server.url.clone()),
                test_switch_url: Some(new_server.url.clone()),
                test_live_file_operations: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-open-quick-open").unwrap();
        assert!(
            old_server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/files/index")
        );
        app.press("id:test-modal-switch-server").unwrap();
        app.wait_for(&format!("server-url={}", new_server.url))
            .unwrap();
        app.wait_for("server-connected=true").unwrap();
        assert!(
            new_server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/files/index")
        );
        old_server.release_response();
        old_server.wait_for_response();
        for _ in 0..4 {
            app.next_change(Duration::from_millis(50)).unwrap();
            assert!(!app.shows("old-server.rs"));
        }
        new_server.release_response();
        new_server.wait_for_response();
        app.wait_for("quick-open-paths=new-server.rs").unwrap();
        assert!(!app.shows("old-server.rs"));
        old_server.assert_no_duplicate_request();
        new_server.assert_no_duplicate_request();
    }

    #[test]
    fn quick_open_index_success_empty_and_error_headless() {
        let index_path = "GET /api/projects/proj-falcon/files/index".to_string();
        let server = MockHttpServer::new(vec![
            (
                index_path.clone(),
                200,
                serde_json::json!({"paths":["native/Cargo.toml"],"truncated":false}).to_string(),
            ),
            (
                index_path.clone(),
                200,
                serde_json::json!({"paths":[],"truncated":false}).to_string(),
            ),
            (
                index_path,
                500,
                serde_json::json!({"error":"index unavailable"}).to_string(),
            ),
        ]);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();

        app.press_key("alt+p").unwrap();
        assert!(
            server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/files/index")
        );
        server.release_response();
        server.wait_for_response();
        app.wait_for("quick-open-paths=native/Cargo.toml").unwrap();
        app.press_key("enter").unwrap();
        let file_key = falcon_core::pane_key::file_key("proj-falcon", "native/Cargo.toml");
        app.wait_for(&format!("id:file-editor-{file_key}")).unwrap();

        app.press("id:test-open-quick-open").unwrap();
        assert!(
            server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/files/index")
        );
        server.release_response();
        server.wait_for_response();
        app.wait_for("text:没有匹配项").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("id:quick-open-input").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:quick-open-input", Duration::from_secs(2))
            .unwrap();

        app.press("id:test-open-quick-open").unwrap();
        assert!(
            server
                .wait_for_request()
                .starts_with("GET /api/projects/proj-falcon/files/index")
        );
        server.release_response();
        server.wait_for_response();
        app.wait_for("quick-open-error-present=true").unwrap();
        app.wait_for("id:quick-open-input").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:quick-open-input", Duration::from_secs(2))
            .unwrap();
        server.assert_no_duplicate_request();
    }

    #[test]
    fn quick_open_keeps_focus_and_reveals_last_row_headless() {
        let paths = (0..24)
            .map(|index| format!("src/file-{index:02}.rs"))
            .collect::<Vec<_>>();
        let response = serde_json::json!({"paths": paths, "truncated": true}).to_string();
        let server = MockHttpServer::new(vec![(
            "GET /api/projects/proj-falcon/files/index".into(),
            200,
            response,
        )]);
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                test_client_url: Some(server.url.clone()),
                test_live_file_operations: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:test-open-quick-open").unwrap();
        server.wait_for_request();
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:quick-open-file-23").unwrap();
        app.wait_for("quick-open-loading=false").unwrap();
        app.type_text("id:quick-open-input", "src/").unwrap();
        assert_eq!(
            app.screen().input_text("id:quick-open-input").unwrap(),
            Some("src/")
        );
        app.press_key("down").unwrap();
        app.type_text("id:quick-open-input", "file-").unwrap();
        assert_eq!(
            app.screen().input_text("id:quick-open-input").unwrap(),
            Some("src/file-")
        );
        app.wait_for("id:quick-open-file-23").unwrap();
        app.press_key("up").unwrap();
        app.wait_for("id:quick-open-file-23-selected").unwrap();
        app.press_key("escape").unwrap();
        server.assert_no_duplicate_request();
    }

    #[test]
    fn project_menu_supports_escape_outside_dismissal_and_keyboard_selection() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                server_url: Some("http://127.0.0.1:4923".into()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();

        app.press("label:project-menu-proj-falcon").unwrap();
        assert!(app.shows("用 px0 审阅"), "project menu opened");
        app.press_key("down").unwrap();
        app.press_key("escape").unwrap();
        assert!(!app.shows("用 px0 审阅"), "Escape closes the menu");
        app.press_key("enter").unwrap();
        assert!(
            app.shows("用 px0 审阅"),
            "Enter reopens the menu after Escape restores trigger focus"
        );
        app.press_key("escape").unwrap();

        app.press("label:project-menu-proj-falcon").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("label:session-sess-3")
            .expect("the second menu action creates exactly one terminal");
        assert!(!app.shows("用 px0 审阅"), "Enter closes the menu");
        assert!(!app.shows("label:session-sess-4"), "the action is not invoked twice");

        app.press("label:project-menu-proj-falcon").unwrap();
        app.press("label:sidebar-settings").unwrap();
        assert!(!app.shows("用 px0 审阅"), "outside press dismisses the menu");
    }

    #[test]
    fn host_menu_modal_focus_handoff_and_close_restore_headless() {
        ensure_test_platform();
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();

        app.press("label:host-menu-s:host:host-prod-01").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.wait_for("远端主机").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("id:modal-layer-panel").unwrap();
        app.wait_for("id:host-add-manual").unwrap();
        app.hover("id:modal-layer-panel").unwrap();
        app.wait_for_gone("更多操作", Duration::from_secs(2))
            .expect("background trigger tooltip leaves focus with the modal");

        app.press("id:host-add-manual").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        app.type_text("id:host-form-name", "Draft").unwrap();
        app.press_key("x").unwrap();
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("Draftx"),
            "modal rerenders do not steal input focus"
        );

        app.press_key("escape").unwrap();
        app.wait_for_gone("id:modal-layer-panel", Duration::from_secs(2))
            .expect("Escape closes the modal");
        app.press_key("enter").unwrap();
        app.wait_for("检测连接")
            .expect("closing the modal restores focus to its menu trigger");
        app.press_key("escape").unwrap();
        app.wait_for_gone("检测连接", Duration::from_secs(2))
            .expect("one Escape closes the restored host menu");
    }

    #[test]
    fn overflow_menu_sheet_keyboard_dispatches_once_on_phone_and_pad_headless() {
        ensure_test_platform();
        for idiom in [Idiom::Phone, Idiom::Pad] {
            let mut cfg = EngineConfig::new(Root::of::<SheetMenuDispatchTest>());
            cfg.viewport = match idiom {
                Idiom::Phone => Size::new(Points(390.0), Points(844.0)),
                _ => Size::new(Points(834.0), Points(1112.0)),
            };
            cfg.environment.os = nsh_core::platform::Os::Web;
            cfg.environment.idiom = idiom;
            cfg.environment.color_scheme = ColorScheme::Dark;
            cfg.ui_sets = vec![nsh_base::registry(), registry()];
            let mut app = Headless::start(cfg).unwrap();
            app.first_render().unwrap();

            // A standalone real menu avoids the terminal consuming Tab.
            app.press_key("tab").unwrap();
            app.press_key("enter").unwrap();
            app.wait_for("role:menuitem[Increment]").unwrap();
            assert_eq!(
                app.screen().accessibility_state("id:sheet-menu-trigger").unwrap().expanded,
                Some(true),
            );
            app.press_key("enter").unwrap();
            app.wait_for("action-count=1")
                .expect("sheet selection must dispatch without trigger focus return");
            app.wait_for_gone("role:menuitem[Increment]", Duration::from_secs(2)).unwrap();

            // Refocusing must not run an old pending selection a second time.
            app.press_key("tab").unwrap();
            app.press_key("enter").unwrap();
            app.wait_for("role:menuitem[Increment]").unwrap();
            assert!(app.shows("action-count=1"));
            app.press_key("enter").unwrap();
            app.wait_for("action-count=2").unwrap();
            app.wait_for_gone("role:menuitem[Increment]", Duration::from_secs(2)).unwrap();
            assert!(app.shows("action-count=2"));
        }
    }

    #[test]
    fn host_menu_mouse_selection_opens_modal_headless() {
        ensure_test_platform();
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();

        app.press("label:host-menu-s:host:host-prod-01").unwrap();
        app.press("role:menuitem[远端主机]").unwrap();
        app.wait_for("id:modal-layer-panel")
            .expect("mouse selection dispatches after the popup focus lifecycle");
        app.hover("id:modal-layer-panel").unwrap();
        app.wait_for_gone("更多操作", Duration::from_secs(2))
            .expect("the modal does not leave the background trigger tooltip visible");
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:modal-layer-panel", Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn host_context_menu_mouse_selection_opens_modal_headless() {
        ensure_test_platform();
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();

        app.right_click("label:host-menu-s:host:host-prod-01")
            .unwrap();
        app.wait_for("远端主机").unwrap();
        app.press("远端主机").unwrap();
        app.wait_for("id:modal-layer-panel")
            .expect("the pointer context-menu action opens its dialog");
        app.hover("id:modal-layer-panel").unwrap();
        app.wait_for_gone("更多操作", Duration::from_secs(2))
            .expect("the context-menu dialog has no background trigger tooltip");
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:modal-layer-panel", Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn session_context_menu_detach_and_terminate_keep_existing_guards() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("label:sessions-toggle-proj-falcon").unwrap();

        app.right_click("label:session-sess-main").unwrap();
        assert!(app.shows("收起窗口"));
        assert!(app.shows("终止会话…"));
        assert!(app.shows("重命名会话…"));
        app.press_key("escape").unwrap();
        assert!(!app.shows("收起窗口"), "one Escape closes the context menu");

        app.right_click("label:session-sess-main").unwrap();
        app.press("收起窗口").unwrap();
        app.wait_for("label:reattach-sess-main")
            .expect("detach removes the pane but keeps the session record");

        app.right_click("label:session-sess-main").unwrap();
        assert!(app.shows("接回会话"));
        assert!(!app.shows("收起窗口"));
        app.press("终止会话…").unwrap();
        app.wait_for("前台程序仍在运行")
            .expect("terminate keeps the busy-process confirmation guard");
        app.press("取消").unwrap();
        app.wait_for("label:session-sess-main")
            .expect("cancelling does not terminate the session");
    }

    #[test]
    fn desktop_tree_activity_bar_and_settings_navigation() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.wait_for("label:right-activity-bar").unwrap();
        app.press("label:activity-files").unwrap();
        app.wait_for("搜索文件…").unwrap();
        app.press("label:activity-files").unwrap();
        app.wait_for_gone("label:right-panel-island", Duration::from_secs(2))
            .unwrap();
        app.press("label:activity-changes").unwrap();
        app.wait_for("label:right-panel-island").unwrap();
        app.press("label:sidebar-collapse").unwrap();
        app.wait_for("label:sidebar-expand").unwrap();
        app.press("label:sidebar-expand").unwrap();
        app.wait_for("label:sidebar-island").unwrap();
        app.press("label:folder-toggle-proj-falcon").unwrap();
        app.wait_for_gone("feat/nsh-unified-client", Duration::from_secs(2))
            .unwrap();
        app.press("label:folder-toggle-proj-falcon").unwrap();
        app.wait_for("feat/nsh-unified-client").unwrap();
        app.press("label:sidebar-settings").unwrap();
        app.press("label:settings-tab-Hosts").unwrap();
        app.wait_for("dev@10.0.4.12:22").unwrap();
        // Tailscale 入口在离线模式下如实显示请求失败，不应退回到发真实网络请求。
        app.press("id:host-add-tailscale").unwrap();
        app.wait_for("label:host-tailscale-panel").unwrap();
        app.wait_for("读取 Tailscale 失败。").unwrap();
        app.press("id:host-add-manual").unwrap();
        app.wait_for("保存主机").unwrap();
        app.press("label:settings-tab-About").unwrap();
        app.wait_for("Falcon").unwrap();
        app.press("label:settings-close").unwrap();
        app.press("label:open-overview").unwrap();
        app.wait_for("label:session-overview").unwrap();
        app.press("label:overview-session-sess-main").unwrap();
        app.wait_for_gone("label:session-overview", Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn saved_host_edit_prefills_preserves_blank_secret_and_retries_http_failure_headless() {
        let original = ssh_host_value_with_auth(
            "host-1",
            "Saved Host",
            "ssh.remote.test",
            2222,
            "deploy",
            "key",
            Some("~/.ssh/work_key"),
            1,
        );
        let updated = ssh_host_value_with_auth(
            "host-1",
            "Saved Host Edited",
            "ssh.remote.test",
            2222,
            "deploy",
            "key",
            Some("~/.ssh/work_key"),
            1,
        );
        let server = MockHttpServer::with_workspace_bootstrap_and_hosts(
            vec![
                (
                    "PUT /api/hosts/host-1".into(),
                    500,
                    r#"{"error":"credentials rejected"}"#.into(),
                ),
                (
                    "PUT /api/hosts/host-1".into(),
                    200,
                    updated.to_string(),
                ),
            ],
            vec![original],
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        open_hosts_settings(&mut app);
        app.wait_for("label:host-edit-host-1").unwrap();
        app.press("label:host-settings-menu-host-1").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("id:host-form-key-path").unwrap();
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("Saved Host")
        );
        assert_eq!(
            app.screen().input_text("id:host-form-address").unwrap(),
            Some("ssh.remote.test")
        );
        assert_eq!(
            app.screen().input_text("id:host-form-port").unwrap(),
            Some("2222")
        );
        assert_eq!(
            app.screen().input_text("id:host-form-username").unwrap(),
            Some("deploy")
        );
        assert_eq!(
            app.screen().input_text("id:host-form-key-path").unwrap(),
            Some("~/.ssh/work_key")
        );
        assert_eq!(
            app.screen().input_text("id:host-form-secret").unwrap(),
            Some("")
        );

        app.type_text("id:host-form-name", " Edited").unwrap();
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("Saved Host Edited")
        );
        app.press("id:host-form-submit").unwrap();
        app.wait_for("正在保存…").unwrap();
        let failed_request = server.wait_for_request();
        let expected_body = serde_json::json!({
            "name": "Saved Host Edited",
            "host": "ssh.remote.test",
            "port": 2222,
            "username": "deploy",
            "authMethod": "key",
            "keyPath": "~/.ssh/work_key"
        });
        let failed_body: serde_json::Value =
            serde_json::from_str(mock_request_body(&failed_request)).unwrap();
        assert_eq!(failed_body, expected_body);
        assert!(failed_body.get("secret").is_none());
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:host-form-error").unwrap();
        assert!(app.shows("credentials rejected"));
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("Saved Host Edited")
        );
        assert_eq!(
            app.screen().input_text("id:host-form-secret").unwrap(),
            Some("")
        );
        assert!(app.shows("Saved Host"));

        app.press("id:host-form-submit").unwrap();
        app.wait_for("正在保存…").unwrap();
        let retry_request = server.wait_for_request();
        let retry_body: serde_json::Value =
            serde_json::from_str(mock_request_body(&retry_request)).unwrap();
        assert_eq!(retry_body, expected_body);
        assert!(retry_body.get("secret").is_none());
        server.release_response();
        server.wait_for_response();
        app.wait_for("Saved Host Edited").unwrap();
        app.wait_for_gone("id:host-form-key-path", Duration::from_secs(2))
            .unwrap();
        assert!(app.shows("Saved Host Edited"));
        app.press("label:host-edit-host-1").unwrap();
        app.wait_for("id:host-form-key-path").unwrap();
        app.type_text("id:host-form-name", "-discarded").unwrap();
        server.clear_observed_requests();
        app.press("id:host-form-cancel").unwrap();
        app.wait_for_gone("id:host-form-key-path", Duration::from_secs(2))
            .unwrap();
        assert!(app.shows("Saved Host Edited"));
        server.assert_no_request_starting_with("PUT /api/hosts/host-1");
        server.assert_no_duplicate_request();
    }

    #[test]
    fn saved_host_delayed_update_preserves_reopened_editor_and_draft_headless() {
        let original = ssh_host_value_with_auth(
            "host-1",
            "Saved Host",
            "ssh.remote.test",
            2222,
            "deploy",
            "key",
            Some("~/.ssh/work_key"),
            1,
        );
        let updated = ssh_host_value_with_auth(
            "host-1",
            "Server Updated",
            "ssh.remote.test",
            2222,
            "deploy",
            "key",
            Some("~/.ssh/work_key"),
            1,
        );
        let server = MockHttpServer::with_workspace_bootstrap_and_hosts(
            vec![("PUT /api/hosts/host-1".into(), 200, updated.to_string())],
            vec![original],
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        open_hosts_settings(&mut app);
        app.wait_for("label:host-edit-host-1").unwrap();
        app.press("label:host-edit-host-1").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        app.type_text("id:host-form-name", " Pending").unwrap();
        app.press("id:host-form-submit").unwrap();
        let pending_update = server.wait_for_request();
        assert!(pending_update.starts_with("PUT /api/hosts/host-1"));
        app.wait_for("正在保存…").unwrap();

        app.press("label:settings-close").unwrap();
        app.wait_for_gone("id:host-form-name", Duration::from_secs(2))
            .unwrap();
        app.press("label:sidebar-settings").unwrap();
        app.wait_for("label:settings-tab-Hosts").unwrap();
        app.press("label:settings-tab-Hosts").unwrap();
        app.wait_for("label:host-edit-host-1").unwrap();
        app.press("label:host-edit-host-1").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        assert!(
            !app.shows("正在保存…"),
            "reopened editor inherited the previous request's busy state"
        );
        app.type_text("id:host-form-name", " New Draft").unwrap();
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("Saved Host New Draft")
        );

        server.release_response();
        server.wait_for_response();
        app.wait_for("Server Updated").unwrap();
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("Saved Host New Draft")
        );
        server.assert_no_duplicate_request();
    }

    #[test]
    fn saved_host_delayed_failure_after_close_and_stale_connection_are_ignored_headless() {
        let original = ssh_host_value_with_auth(
            "host-1",
            "Saved Host",
            "ssh.remote.test",
            2222,
            "deploy",
            "key",
            Some("~/.ssh/work_key"),
            1,
        );
        let new_server_host = ssh_host_value(
            "host-1",
            "New Server Host",
            "ssh.new.test",
            22,
            "runner",
            0,
        );
        let old_server = MockHttpServer::with_workspace_bootstrap_and_hosts(
            vec![
                (
                    "PUT /api/hosts/host-1".into(),
                    500,
                    r#"{"error":"closed save failed"}"#.into(),
                ),
                (
                    "POST /api/hosts".into(),
                    500,
                    r#"{"error":"stale save failed"}"#.into(),
                ),
            ],
            vec![original],
        );
        let new_server = MockHttpServer::with_workspace_bootstrap_and_hosts(
            vec![(
                "GET /api/never-used-host-route".into(),
                200,
                "{}".into(),
            )],
            vec![new_server_host],
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(old_server.url.clone()),
                test_switch_url: Some(new_server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        open_hosts_settings(&mut app);
        app.wait_for("label:host-edit-host-1").unwrap();
        app.press("label:host-edit-host-1").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        app.type_text("id:host-form-name", " Closed").unwrap();
        app.press("id:host-form-submit").unwrap();
        app.wait_for("正在保存…").unwrap();
        let pending_update = old_server.wait_for_observed_prefixes_pumping(&mut app, &["PUT /api/hosts/host-1"])[0].clone();
        assert!(pending_update.starts_with("PUT /api/hosts/host-1"));

        app.press("label:settings-close").unwrap();
        app.wait_for_gone("id:host-form-name", Duration::from_secs(2))
            .unwrap();
        old_server.release_response();
        old_server.wait_for_response();
        app.press("label:sidebar-settings").unwrap();
        app.wait_for("label:settings-tab-Hosts").unwrap();
        app.press("label:settings-tab-Hosts").unwrap();
        app.wait_for("label:host-edit-host-1").unwrap();
        app.press("label:host-edit-host-1").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        assert!(!app.shows("closed save failed"));
        app.press("id:host-form-cancel").unwrap();
        app.wait_for_gone("id:host-form-key-path", Duration::from_secs(2))
            .unwrap();
        app.press("id:host-add-manual").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        app.type_text("id:host-form-name", "New Host").unwrap();
        app.type_text("id:host-form-address", "ssh.new.test").unwrap();
        app.type_text("id:host-form-username", "runner").unwrap();
        app.press("id:host-form-submit").unwrap();
        app.wait_for("正在保存…").unwrap();
        let pending_create = old_server.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/hosts"])[0].clone();
        assert!(pending_create.starts_with("POST /api/hosts"));

        app.press("id:test-modal-switch-server").unwrap();
        app.wait_for(&format!("server-url={}", new_server.url))
            .unwrap();
        new_server.wait_for_observed_prefixes_pumping(&mut app, &[
            "GET /api/system",
            "GET /api/projects HTTP/1.1",
        ]);
        app.wait_for("server-connected=true").unwrap();
        app.wait_for("New Server Host").unwrap();
        app.press("id:host-add-manual").unwrap();
        app.wait_for("id:host-form-name").unwrap();
        app.type_text("id:host-form-name", "New UI").unwrap();
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("New UI")
        );
        old_server.release_response();
        old_server.wait_for_response();
        app.wait_for_gone("正在保存…", Duration::from_secs(2))
            .unwrap();
        assert!(!app.shows("stale save failed"));
        assert_eq!(
            app.screen().input_text("id:host-form-name").unwrap(),
            Some("New UI")
        );
        old_server.assert_no_duplicate_request();
        new_server.assert_no_duplicate_request();
    }

    #[test]
    fn saved_host_delete_cancel_conflict_and_success_are_server_confirmed_headless() {
        let server = MockHttpServer::with_workspace_bootstrap_and_hosts(
            vec![
                (
                    "DELETE /api/hosts/host-1".into(),
                    409,
                    r#"{"error":"host is still referenced by projects"}"#.into(),
                ),
                (
                    "DELETE /api/hosts/host-1".into(),
                    200,
                    r#"{"ok":true}"#.into(),
                ),
            ],
            vec![
                ssh_host_value("host-1", "Saved Host", "ssh.remote.test", 22, "deploy", 2),
                ssh_host_value("host-2", "Other Node", "ssh.other.test", 22, "runner", 0),
            ],
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        open_hosts_settings(&mut app);
        app.wait_for("label:host-edit-host-1").unwrap();
        app.wait_for("label:host-edit-host-2").unwrap();

        app.press("label:host-settings-menu-host-1").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("删除主机「Saved Host」？").unwrap();
        app.wait_for("有 2 个项目正在使用该主机，请先改绑或删除这些项目。")
            .unwrap();
        app.press("id:host-delete-cancel").unwrap();
        app.wait_for_gone("id:host-delete-confirmation", Duration::from_secs(2))
            .unwrap();
        server.assert_no_request_starting_with("DELETE /api/hosts/host-1");

        app.press("label:host-settings-menu-host-1").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.press_key("down").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("id:host-delete-confirm").unwrap();
        app.press("id:host-delete-confirm").unwrap();
        app.wait_for("正在删除…").unwrap();
        let failed_request = server.wait_for_request();
        assert!(failed_request.starts_with("DELETE /api/hosts/host-1"));
        server.release_response();
        server.wait_for_response();
        app.wait_for("id:host-delete-error").unwrap();
        assert!(app.shows("host is still referenced by projects"));
        assert!(app.shows("Saved Host"));
        assert!(app.shows("Other Node"));

        app.press("id:host-delete-confirm").unwrap();
        app.wait_for("正在删除…").unwrap();
        let success_request = server.wait_for_request();
        assert!(success_request.starts_with("DELETE /api/hosts/host-1"));
        server.release_response();
        server.wait_for_response();
        app.wait_for_gone("label:host-edit-host-1", Duration::from_secs(2))
            .unwrap();
        app.wait_for("label:host-edit-host-2").unwrap();
        assert!(app.shows("Other Node"));
        server.assert_no_duplicate_request();
    }

    #[test]
    fn saved_host_create_validation_and_success_use_the_stable_form_headless() {
        let mut created = ssh_host_value_with_auth(
            "host-new",
            "New Node",
            "ssh.new.test",
            22,
            "runner",
            "agent",
            None,
            0,
        );
        created["hasSecret"] = serde_json::Value::Bool(false);
        let server = MockHttpServer::with_workspace_bootstrap_and_hosts(
            vec![("POST /api/hosts".into(), 200, created.to_string())],
            vec![ssh_host_value(
                "host-existing",
                "Existing Node",
                "ssh.existing.test",
                22,
                "runner",
                0,
            )],
        );
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        open_hosts_settings(&mut app);
        app.wait_for("label:host-edit-host-existing").unwrap();
        app.press("id:host-add-manual").unwrap();
        app.wait_for("id:host-form-fields").unwrap();
        app.press("id:host-form-submit").unwrap();
        app.wait_for("名称、主机地址和用户名不能为空。").unwrap();
        server.assert_no_request_starting_with("POST /api/hosts");

        app.type_text("id:host-form-name", "New Node").unwrap();
        app.type_text("id:host-form-address", "ssh.new.test").unwrap();
        app.type_text("id:host-form-username", "runner").unwrap();
        app.type_text("id:host-form-port", "70000").unwrap();
        app.press("id:host-form-submit").unwrap();
        app.wait_for("端口必须是 1 到 65535 之间的整数。").unwrap();
        let invalid_port = app
            .screen()
            .input_text("id:host-form-port")
            .unwrap()
            .unwrap();
        assert!(invalid_port.parse::<u16>().ok().filter(|port| *port > 0).is_none());
        server.assert_no_request_starting_with("POST /api/hosts");
        drop(app);

        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: false,
                server_url: Some(server.url.clone()),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        open_hosts_settings(&mut app);
        app.wait_for("label:host-edit-host-existing").unwrap();
        app.press("id:host-add-manual").unwrap();
        app.wait_for("id:host-form-fields").unwrap();
        app.type_text("id:host-form-name", "New Node").unwrap();
        app.type_text("id:host-form-address", "ssh.new.test").unwrap();
        app.type_text("id:host-form-username", "runner").unwrap();
        app.press("id:host-form-submit").unwrap();
        app.wait_for("正在保存…").unwrap();
        let request = server.wait_for_request();
        let body: serde_json::Value =
            serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "name": "New Node",
                "host": "ssh.new.test",
                "port": 22,
                "username": "runner",
                "authMethod": "agent"
            })
        );
        assert!(body.get("secret").is_none());
        server.release_response();
        server.wait_for_response();
        app.wait_for("label:host-edit-host-new").unwrap();
        assert!(app.shows("New Node"));
        server.assert_no_duplicate_request();
    }

    #[test]
    fn desktop_resize_clamps_sidebar_width_and_keeps_layout_visible() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        let width = |app: &Headless| {
            app.screen()
                .layout_nodes()
                .iter()
                .find(|n| n.test_id.as_deref() == Some("sidebar-island"))
                .unwrap()
                .bounds
                .size
                .width
                .0
        };
        assert_eq!(width(&app), 260.0);
        app.drag(
            "label:resize-sidebar",
            Point::new(Points(60.0), Points(0.0)),
        )
        .unwrap();
        assert_eq!(width(&app), 320.0);
        app.drag(
            "label:resize-sidebar",
            Point::new(Points(800.0), Points(0.0)),
        )
        .unwrap();
        assert_eq!(width(&app), 420.0);
        assert!(app.shows("cargo check · 1.2s"));
    }

    /// 移动端（Android `Idiom::Phone`）与终端工作台（`Idiom::Terminal`）自适应骨架无头验证。
    #[test]
    fn dirty_file_close_can_cancel_save_and_discard_headless() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("label:activity-files").unwrap();

        let path = "native/Cargo.toml";
        let pane_key = falcon_core::pane_key::file_key("proj-falcon", path);
        let file_editor = format!("id:file-editor-{pane_key}");
        let close_button = format!("label:close-{pane_key}");
        let titlebar = format!("label:titlebar-{pane_key}");

        app.press("text:Cargo.toml").unwrap();
        app.type_text(&file_editor, "cancel-check").unwrap();
        app.press(&close_button).unwrap();
        app.wait_for("文件有未保存的修改").unwrap();
        app.press("id:unsaved-cancel").unwrap();
        app.wait_for(&titlebar).unwrap();

        app.press(&close_button).unwrap();
        app.wait_for("文件有未保存的修改").unwrap();
        app.press("id:unsaved-save").unwrap();
        app.wait_for_gone(&titlebar, Duration::from_secs(2))
            .unwrap();

        app.press("text:Cargo.toml").unwrap();
        app.type_text(&file_editor, "discard-check").unwrap();
        app.press(&close_button).unwrap();
        app.wait_for("文件有未保存的修改").unwrap();
        app.press("id:unsaved-discard").unwrap();
        app.wait_for_gone(&titlebar, Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn terminal_controls_follow_focus_and_stop_at_file_editor_headless() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press("id:terminal-view-sess-main").unwrap();
        app.press_key("right").unwrap();
        app.wait_for("text:terminal-input-count-1").unwrap();
        app.press_key("ctrl+c").unwrap();
        app.wait_for("text:terminal-input-count-2").unwrap();

        app.press("label:activity-files").unwrap();
        let file_key = falcon_core::pane_key::file_key("proj-falcon", "native/Cargo.toml");
        let file_editor = format!("id:file-editor-{file_key}");
        app.press("text:Cargo.toml").unwrap();
        app.wait_for(&file_editor).unwrap();
        app.type_text(&file_editor, "x").unwrap();
        app.press_key("right").unwrap();
        app.wait_for_gone("text:terminal-input-count-3", Duration::from_millis(200))
            .unwrap();
    }

    #[test]
    fn palette_theme_picker_uses_active_slot_and_closes_headless() {
        let mut theme_settings = ThemeSettings::default();
        theme_settings.mode = ThemePref::Dark;
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                initial_theme: Some(theme_settings),
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        app.type_text("id:palette-input", "主题").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("主题")
        );
        app.wait_for("id:palette-entry-command:theme-picker-selected")
            .unwrap();
        app.wait_for("text:选择主题…").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:palette-input", Duration::from_secs(2))
            .unwrap();

        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        app.type_text("id:palette-input", "choose theme").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("choose theme")
        );
        app.wait_for("id:palette-entry-command:theme-picker-selected")
            .unwrap();
        app.wait_for("text:选择主题…").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:palette-input", Duration::from_secs(2))
            .unwrap();

        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        app.type_text("id:palette-input", ">主题").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some(">主题")
        );
        app.wait_for("id:palette-entry-command:theme-picker-selected")
            .unwrap();
        app.wait_for("text:选择主题…").unwrap();
        app.press("id:palette-entry-command:theme-picker-selected")
            .unwrap();
        app.wait_for("id:theme-picker-filter").unwrap();
        app.type_text("id:theme-picker-filter", "Dracula").unwrap();
        assert_eq!(
            app.screen().input_text("id:theme-picker-filter").unwrap(),
            Some("Dracula")
        );
        app.wait_for("text:Dracula").unwrap();
        app.press("text:Dracula").unwrap();
        app.wait_for_gone("id:theme-picker-filter", Duration::from_secs(2))
            .unwrap();
        app.wait_for("label:theme-slot-Dark >> text:Dracula")
            .unwrap();
        app.wait_for("label:theme-slot-Light >> text:Falcon Light")
            .unwrap();
        app.wait_for("label:settings-close").unwrap();

        app.press("label:theme-slot-Dark").unwrap();
        app.wait_for("id:theme-picker-filter").unwrap();
        app.press_key("escape").unwrap();
        app.wait_for_gone("label:settings-close", Duration::from_secs(2))
            .unwrap();
        app.press("label:sidebar-settings").unwrap();
        app.wait_for("label:settings-close").unwrap();
        app.wait_for_gone("id:theme-picker-filter", Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn palette_project_selection_focuses_an_attached_session_headless() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-project-proj-falcon-nsh").unwrap();
        app.press("id:palette-project-proj-falcon-nsh").unwrap();
        app.wait_for("id:active-pane-t:sess-agent").unwrap();
        app.wait_for_gone("id:active-pane-t:sess-main", Duration::from_secs(2))
            .unwrap();
    }

    #[test]
    fn palette_keyboard_wrap_dispatch_empty_enter_and_escape_headless() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        assert!(!app.shows("text:terminal-input-count-1"));
        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        app.type_text("id:palette-input", "no-such-command")
            .unwrap();
        app.wait_for("id:palette-empty").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("no-such-command")
        );
        app.press_key("enter").unwrap();
        app.wait_for("id:palette-input").unwrap();
        app.wait_for("id:palette-empty").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("no-such-command")
        );
        assert!(!app.shows("text:terminal-input-count-1"));
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:palette-input", Duration::from_secs(2))
            .unwrap();

        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("")
        );
        app.type_text("id:palette-input", "@").unwrap();
        app.press_key("up").unwrap();
        app.press_key("enter").unwrap();
        app.wait_for("id:active-pane-t:sess-agent").unwrap();
        assert!(!app.shows("text:terminal-input-count-1"));
    }

    #[test]
    fn palette_keeps_focus_and_reveals_last_row_headless() {
        let mut app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                ..Default::default()
            },
            ColorScheme::Dark,
        );
        app.first_render().unwrap();
        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        app.type_text("id:palette-input", "n").unwrap();
        app.press_key("down").unwrap();
        app.type_text("id:palette-input", "o-such-command").unwrap();
        app.wait_for("id:palette-empty").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("no-such-command")
        );
        app.press_key("escape").unwrap();
        app.wait_for_gone("id:palette-input", Duration::from_secs(2))
            .unwrap();

        app.press_key("ctrl+shift+p").unwrap();
        app.wait_for("id:palette-input").unwrap();
        assert_eq!(
            app.screen().input_text("id:palette-input").unwrap(),
            Some("")
        );
        app.press_key("up").unwrap();
        app.wait_for("id:palette-entry-command:maximize-selected")
            .unwrap();
        app.press_key("escape").unwrap();
    }

    #[test]
    fn split_creation_waits_for_http_success_before_layout_placement_headless() {
        let session = |id: &str| {
            serde_json::json!({
                "id": id,
                "projectId": "proj-falcon",
                "name": "",
                "state": "active",
                "durable": true,
                "createdAt": 0,
                "lastActiveAt": 0
            })
            .to_string()
        };
        let mock = MockHttpServer::new(vec![
            (
                "POST /api/projects/proj-falcon/sessions".into(),
                200,
                session("split-down"),
            ),
            (
                "POST /api/projects/proj-falcon/sessions".into(),
                200,
                session("split-right"),
            ),
        ]);
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
        app.press("id:terminal-view-sess-main").unwrap();
        app.press("id:test-split-down").unwrap();
        assert!(
            mock.wait_for_request()
                .starts_with("POST /api/projects/proj-falcon/sessions")
        );
        assert!(
            !app.screen()
                .matches("id:active-pane-t:split-down")
                .unwrap()
        );
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("id:active-pane-t:split-down")
            .unwrap();
        app.press("id:test-split-right").unwrap();
        assert!(
            mock.wait_for_request()
                .starts_with("POST /api/projects/proj-falcon/sessions")
        );
        assert!(
            !app.screen()
                .matches("id:active-pane-t:split-right")
                .unwrap()
        );
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("id:active-pane-t:split-right")
            .unwrap();
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn mobile_and_tui_idioms_mount_cleanly_headless() {
        ensure_test_platform();
        let opts = FalconLaunchOptions {
            offline_demo: true,
            server_url: Some("http://127.0.0.1:4923".into()),
            initial_theme: None,
            #[cfg(test)]
            test_client_url: None,
            #[cfg(test)]
            test_switch_url: None,
            #[cfg(test)]
            test_live_file_operations: false,
        };

        let mut mobile_cfg = EngineConfig::new(falcon_root_with_options(opts.clone()));
        mobile_cfg.viewport = Size::new(Points(390.0), Points(844.0));
        mobile_cfg.environment.os = nsh_core::platform::Os::Android;
        mobile_cfg.environment.idiom = Idiom::Phone;
        mobile_cfg.ui_sets = vec![nsh_base::registry(), registry()];
        let mut mobile_app = Headless::start(mobile_cfg).expect("start Android headless");
        mobile_app.first_render().expect("mobile first render");
        mobile_app.wait_for("Ctrl").unwrap();
        mobile_app.wait_for("项目与会话").unwrap();

        let mut tui_cfg = EngineConfig::new(falcon_root_with_options(opts));
        tui_cfg.viewport = Size::new(Points(960.0), Points(576.0));
        tui_cfg.environment.os = nsh_core::platform::Os::Linux;
        tui_cfg.environment.idiom = Idiom::Terminal;
        tui_cfg.ui_sets = vec![nsh_base::registry(), registry()];
        let mut tui_app = Headless::start(tui_cfg).expect("start TUI headless");
        tui_app.first_render().expect("tui first render");
        tui_app
            .wait_for("浏览模式：方向键切换焦点 · Enter 进入终端直通 · Ctrl+K 命令面板")
            .unwrap();
        tui_app.wait_for("直通输入").unwrap();
    }

    /// `.nshpkg` 双轨插件安全回归：验证自托管 Ed25519 信任根通过、单字节篡改拒绝、非授信根拒绝与低构建号防回滚拦截。
    #[test]
    fn nshpkg_dual_track_verifier_enforces_root_signature_tamper_and_rollback() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let server_root = SecretKey::generate().unwrap();
        let server_rel = SecretKey::generate().unwrap();
        let cert = certify(
            &server_root,
            &server_rel.public(),
            &["dev.falcon.plugin.*".into()],
            now.saturating_sub(60),
            now + 3600,
        );
        let root_b64 = server_root.public().to_base64();

        let make_pkg = |build_num: u64, rel: &SecretKey, rel_cert: &nsh_package::Certificate| {
            nsh_package::build(
                &PkgContents {
                    app: PkgApp {
                        id: "dev.falcon.plugin.gpu".into(),
                        name: "GPU Inspector".into(),
                        version: "0.5.1".into(),
                        build: build_num,
                    },
                    capabilities: None,
                    source: "export default function Panel() { return null; }\n".into(),
                    bytecode: None,
                    source_map: None,
                    assets: Vec::new(),
                    min_runtime: None,
                },
                rel,
                rel_cert,
            )
            .unwrap()
        };

        let tmp = tempfile::tempdir().unwrap();
        let mut verifier = ClientPluginVerifier::open_persistent(tmp.path()).unwrap();

        // 1. 合法包（build 5）验签通过并记录防回滚水位
        let pkg_v5 = make_pkg(5, &server_rel, &cert);
        let res_ok = verifier.verify_and_accept(&root_b64, Some("dev.falcon.plugin.gpu"), &pkg_v5);
        assert!(
            matches!(res_ok, PackageVerificationState::Verified { build: 5, .. }),
            "expected Verified(build=5), got {res_ok:?}"
        );

        // 2. 篡改任意一字节立即拒绝
        let mut tampered = make_pkg(6, &server_rel, &cert);
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0x5a;
        let res_tamper =
            verifier.verify_and_accept(&root_b64, Some("dev.falcon.plugin.gpu"), &tampered);
        assert!(matches!(
            res_tamper,
            PackageVerificationState::Untrusted { .. }
        ));

        // 3. 陌生根密钥签出的包拒绝
        let rogue_root = SecretKey::generate().unwrap();
        let rogue_rel = SecretKey::generate().unwrap();
        let rogue_cert = certify(
            &rogue_root,
            &rogue_rel.public(),
            &["dev.falcon.plugin.*".into()],
            now.saturating_sub(60),
            now + 3600,
        );
        let rogue_pkg = make_pkg(7, &rogue_rel, &rogue_cert);
        let res_rogue =
            verifier.verify_and_accept(&root_b64, Some("dev.falcon.plugin.gpu"), &rogue_pkg);
        assert!(matches!(
            res_rogue,
            PackageVerificationState::Untrusted { .. }
        ));

        // 4. 同一信任根签出的旧构建号（build 4 < 5）被持久水位拦截回滚
        let mut reopened = ClientPluginVerifier::open_persistent(tmp.path()).unwrap();
        let older_pkg = make_pkg(4, &server_rel, &cert);
        let res_rollback =
            reopened.verify_and_accept(&root_b64, Some("dev.falcon.plugin.gpu"), &older_pkg);
        assert_eq!(
            res_rollback,
            PackageVerificationState::RollbackBlocked {
                current: 5,
                incoming: 4,
            }
        );
    }

    /// Phase 0 性能门禁基准：向 `TermCore` 灌入 300,000 行混合中文宽字符 + Unicode 盒线 + 24-bit ANSI 真彩流，
    /// 随后连续测量 200 帧完整 `120×40` 网格快照抽取 + `paint_snapshot` 批处理耗时，断言 Release 下 `p95 < 4.0 ms`。
    #[test]
    fn phase0_gate_benchmark_300k_mixed_cjk_box_ansi_under_4ms_p95() {
        ensure_test_platform();
        let sess = TerminalSession::new_offline("bench-300k", 120, 40, true);
        let resolved = derive_theme(&ThemeSettings::default().dark.colors);

        let mut chunk = Vec::with_capacity(64 * 1024);
        let sample_line = "\x1b[38;2;97;175;239m┌─┬─┐\x1b[0m \x1b[48;2;32;38;48;38;2;152;195;121m持久会话渲染基准测试 120fps\x1b[0m │ \x1b[1;33mcargo test --release -p falcon-nsh\x1b[0m ├─┼─┤\r\n";
        let lines_per_batch = 500;
        for _ in 0..lines_per_batch {
            chunk.extend_from_slice(sample_line.as_bytes());
        }

        let ingest_start = Instant::now();
        for _ in 0..(300_000 / lines_per_batch) {
            sess.feed_output(&chunk);
        }
        let ingest_ms = ingest_start.elapsed().as_secs_f64() * 1000.0;

        // 预热 10 帧
        for _ in 0..10 {
            let snap = sess.snapshot();
            let vp = TerminalViewport::compute(960.0, 720.0, 8.0, 18.0, 13.0, true, true, &snap);
            std::hint::black_box(paint_snapshot(&snap, &resolved, vp, Some("中文输入")));
        }

        let mut samples_ms = Vec::with_capacity(200);
        for _ in 0..200 {
            let t0 = Instant::now();
            let snap = sess.snapshot();
            let vp = TerminalViewport::compute(960.0, 720.0, 8.0, 18.0, 13.0, true, true, &snap);
            let painter = paint_snapshot(&snap, &resolved, vp, Some("中文输入"));
            std::hint::black_box(painter);
            samples_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        }

        samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p50 = samples_ms[samples_ms.len() / 2];
        let p95 = samples_ms[(samples_ms.len() * 95) / 100];
        let max = *samples_ms.last().unwrap();

        println!(
            "[FalconTerminal 300k Benchmark] ingest_300k={ingest_ms:.1}ms | frame_120x40 p50={p50:.3}ms p95={p95:.3}ms max={max:.3}ms"
        );

        #[cfg(not(debug_assertions))]
        assert!(
            p95 < 4.0,
            "Release p95 frame time {p95:.3}ms exceeded 4.0ms gate!"
        );
        #[cfg(debug_assertions)]
        assert!(
            p95 < 50.0,
            "Debug p95 frame time {p95:.3}ms unexpectedly slow"
        );
    }

    fn relay_list_json(id: &str) -> String {
        serde_json::json!({
            "forwards": [],
            "shares": [{
                "id": id, "destHost": "127.0.0.1", "destPort": 8080,
                "enabled": true, "state": "active", "createdAt": 0
            }],
        }).to_string()
    }

    fn relay_share_json(id: &str) -> String {
        serde_json::json!({
            "id": id,
            "destHost": "127.0.0.1",
            "destPort": 8080,
            "enabled": true,
            "state": "active",
            "createdAt": 0,
        }).to_string()
    }

    fn open_relay_settings(app: &mut Headless) {
        app.first_render().unwrap();
        app.press("id:test-open-relays-settings").unwrap();
        app.wait_for("id:relay-share-port-local").unwrap();
        app.wait_for_gone("id:relay-actions-unavailable-local", Duration::from_secs(3)).unwrap();
    }

    #[test]
    fn relay_create_duplicate_suppressed_and_failure_keeps_draft_headless() {
        let mock = MockHttpServer::new(vec![(
            "POST /api/shares".into(), 500, r#"{"error":"mock relay write failed"}"#.into(),
        )]);
        let mut app = headless_with_options(FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        }, ColorScheme::Dark);
        open_relay_settings(&mut app);
        app.type_text("id:relay-share-port-local", " 8080 ").unwrap();
        app.type_text("id:relay-share-name-local", " draft ").unwrap();
        app.press("id:relay-create-share-local").unwrap();
        let request = mock.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/shares"])[0].clone();
        let body: serde_json::Value = serde_json::from_str(mock_request_body(&request)).unwrap();
        assert_eq!(body["destPort"], 8080);
        assert_eq!(body["name"], "draft");
        app.press("id:test-modal-force-duplicate-share").unwrap();
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("mock relay write failed").unwrap();
        assert_eq!(app.screen().input_text("id:relay-share-port-local").unwrap(), Some(" 8080 ".into()));
        assert_eq!(app.screen().input_text("id:relay-share-name-local").unwrap(), Some(" draft ".into()));
        mock.assert_no_duplicate_request();
    }

    #[test]
    fn relay_success_applies_confirmed_record_before_refresh_headless() {
        let mock = MockHttpServer::new_with_auto_responses(
            vec![
                ("POST /api/shares".into(), 200, relay_share_json("confirmed")),
                ("GET /api/relays".into(), 200, relay_list_json("confirmed")),
            ],
            vec![
                ("GET /api/system".into(), 200, "{}".into()),
                ("GET /api/app-icon".into(), 200, r#"{"selected":"default"}"#.into()),
                ("GET /api/projects HTTP/1.1".into(), 200, r#"[{"id":"proj-falcon","name":"falcon","type":"local","workingDir":"/workspace/falcon","createdAt":0}]"#.into()),
                ("GET /api/sessions".into(), 200, "[]".into()),
                ("GET /api/hosts".into(), 200, "[]".into()),
                ("GET /api/agents".into(), 200, "[]".into()),
                ("GET /api/plugins HTTP/1.1".into(), 200, r#"{"dir":"","plugins":[],"trustRoot":"unused"}"#.into()),
            ],
        );
        let mut app = headless_with_options(FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        }, ColorScheme::Dark);
        open_relay_settings(&mut app);
        app.type_text("id:relay-share-port-local", "8080").unwrap();
        app.press("id:relay-create-share-local").unwrap();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["POST /api/shares"]);
        mock.release_response();
        mock.wait_for_response();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
        app.wait_for("id:relay-toggle-confirmed").unwrap();
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("id:relay-toggle-confirmed").unwrap();
    }

    #[test]
    fn relay_empty_snapshot_keeps_creation_form_headless() {
        let mock = MockHttpServer::with_workspace_bootstrap(vec![]);
        let mut app = headless_with_options(FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        }, ColorScheme::Dark);
        open_relay_settings(&mut app);
        app.press("id:test-modal-refresh-relays").unwrap();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
        app.wait_for("id:relay-empty-shares").unwrap();
        app.wait_for("id:relay-empty-forwards").unwrap();
        assert!(app.screen().input_text("id:relay-share-port-local").unwrap().is_some());
        app.type_text("id:relay-share-port-local", "70000").unwrap();
        app.press("id:relay-create-share-local").unwrap();
        app.wait_for("id:relay-share-error-local").unwrap();
        assert_eq!(app.screen().input_text("id:relay-share-port-local").unwrap(), Some("70000".into()));
        mock.assert_no_request_starting_with("POST /api/shares");
    }

    #[test]
    fn relay_refresh_is_serialized_trailing_only_and_preserves_form_typing_headless() {
        let mock = MockHttpServer::new(vec![
            ("GET /api/relays".into(), 200, relay_list_json("stale")),
            ("GET /api/relays".into(), 200, relay_list_json("fresh")),
        ]);
        let mut app = headless_with_options(FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            ..Default::default()
        }, ColorScheme::Dark);
        open_relay_settings(&mut app);
        app.type_text("id:relay-share-port-local", "8181").unwrap();
        app.press("id:test-modal-rebuild-relay-hosts").unwrap();
        assert_eq!(app.screen().input_text("id:relay-share-port-local").unwrap(), Some("8181".into()));
        app.press("id:test-modal-refresh-relays").unwrap();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
        app.press("id:test-modal-refresh-relays").unwrap();
        mock.release_response();
        mock.wait_for_response();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
        mock.release_response();
        mock.wait_for_response();
        app.wait_for("id:relay-toggle-fresh").unwrap();
        app.wait_for_gone("id:relay-toggle-stale", Duration::from_millis(500)).unwrap();
        assert_eq!(app.screen().input_text("id:relay-share-port-local").unwrap(), Some("8181".into()));
        app.type_text("id:relay-share-port-local", "2").unwrap();
        assert_eq!(app.screen().input_text("id:relay-share-port-local").unwrap(), Some("81812".into()));
    }

    #[test]
    fn relay_old_snapshot_is_ignored_after_same_url_reconnect_headless() {
        let mock = MockHttpServer::new_with_auto_responses(
            vec![("GET /api/relays".into(), 200, relay_list_json("stale-connection"))],
            vec![
                ("GET /api/system".into(), 200, "{}".into()),
                ("GET /api/app-icon".into(), 200, r#"{"selected":"default"}"#.into()),
                ("GET /api/projects".into(), 200, r#"[{"id":"proj-falcon","name":"falcon","type":"local","workingDir":"/workspace/falcon","createdAt":0}]"#.into()),
                ("GET /api/sessions".into(), 200, "[]".into()),
                ("GET /api/hosts".into(), 200, "[]".into()),
                ("GET /api/agents".into(), 200, "[]".into()),
                ("GET /api/plugins".into(), 200, r#"{"dir":"","plugins":[],"trustRoot":"unused"}"#.into()),
            ],
        );
        let mut app = headless_with_options(FalconLaunchOptions {
            offline_demo: true,
            test_client_url: Some(mock.url.clone()),
            test_switch_url: Some(mock.url.clone()),
            ..Default::default()
        }, ColorScheme::Dark);
        open_relay_settings(&mut app);
        app.press("id:test-modal-refresh-relays").unwrap();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
        app.press("id:test-modal-switch-server").unwrap();
        mock.release_response();
        mock.wait_for_response();
        mock.wait_for_observed_prefixes_pumping(&mut app, &["GET /api/relays"]);
        app.wait_for_gone("id:relay-toggle-stale-connection", Duration::from_millis(500)).unwrap();
    }

    /// ADR 0018 对账回归：`falcon-core::app_icon::APP_ICON_IDS` 中全部内置图标均已在 `assets/app-icons/` 下就位。
    #[test]
    fn app_icons_match_core_app_icon_ids() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/app-icons");
        for id in falcon_core::app_icon::APP_ICON_IDS {
            let path = dir.join(format!("{id}.png"));
            assert!(
                path.is_file(),
                "missing builtin app icon asset: {}",
                path.display()
            );
        }
    }
}

/// 与 GPUI 自动化截图保持相同的可复现入口，只编入审阅构建。
#[cfg(all(feature = "snapshot", not(target_arch = "wasm32")))]
fn install_review_scene(store: WorkspaceStore) {
    let Ok(scene) = std::env::var("FALCON_SNAPSHOT_SCENE") else {
        return;
    };
    spawn(async move {
        timer(std::time::Duration::from_millis(2000)).await;
        if let Ok(name) = std::env::var("FALCON_SNAPSHOT_PROJECT") {
            if let Some(p) = store.projects.get().iter().find(|p| p.name == name) {
                store.select_project(&p.id);
            }
        }
        if let Some(session) = store
            .sessions
            .get()
            .iter()
            .find(|s| s.project_id == store.active_project_id.get())
        {
            store.focus_session(&session.id);
            store
                .ensure_terminal_session(&session.id)
                .send_input("\x15\x0c");
        }
        match scene.as_str() {
            "overview" => store.overview_open.set(true),
            "files" => {
                store
                    .right_panel_kind
                    .set(crate::store::RightPanelKind::Files);
                store.right_panel_open.set(true);
            }
            "changes" => {
                store
                    .right_panel_kind
                    .set(crate::store::RightPanelKind::Git);
                store.right_panel_open.set(true);
            }
            "terminal" => store.right_panel_open.set(false),
            "palette" => store.active_dialog.set(ActiveDialog::Palette),
            "profiles" => store.active_dialog.set(ActiveDialog::Profiles),
            "project" => store.active_dialog.set(ActiveDialog::NewProject),
            "file" => {
                let id = store.active_project_id.get();
                store.open_file_pane(&id, "README.md");
            }
            "diff" => {
                store
                    .right_panel_kind
                    .set(crate::store::RightPanelKind::Git);
                store.right_panel_open.set(true);
                if let Some(file) = store.git_changes.get().first() {
                    store.preview_git_path(&file.path);
                }
            }
            "appearance" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Appearance)),
            "notifications" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Notifications)),
            "terminal-settings" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Terminal)),
            "hosts" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Hosts)),
            "relays" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Relays)),
            "plugins" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Plugins)),
            "about" => store
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::About)),
            id => {
                store
                    .right_panel_kind
                    .set(crate::store::RightPanelKind::Plugin(id.to_string()));
                store.right_panel_open.set(true);
                store.dispatch_plugin_action(id, None, None);
            }
        }
    });
}
