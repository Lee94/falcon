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
pub mod shell;
pub mod store;
pub mod terminal;
pub mod theme;
pub mod views;
pub mod zoom;

use std::sync::Arc;

use falcon_core::pane_key::parse_pane_key;
use falcon_platform::{ServiceControl, ServicePresence};
use falcon_theme::{ThemeSettings, derive_theme, load_theme_settings, resolve_theme_mode};
use nsh_core::desktop::{MainWindowOptions, TrayTitleRun};
use nsh_core::element::desktop::{MenuItem, main_window, quit, set_tray, tray};
use nsh_core::element::{Registry, Root};
use nsh_core::platform::Idiom;
use nsh_core::prelude::*;

use crate::i18n::t;
use crate::shell::{desktop_shell, mobile_shell, tui_shell};
pub use crate::store::FalconLaunchOptions;
use crate::store::{ActiveDialog, SettingsTab, WorkspaceStore};
use crate::terminal::{FalconTerminal, bind_safe_key};
use crate::theme::{
    install_theme, nsh_appearance_of, rgb_to_hsla, rgba_to_hsla, tokens_from_resolved, use_theme,
};
use crate::views::{
    AgentPaneView, DiffPaneView, FilePaneView, FilesPanel, GitPanel, PluginPanel, modal_layer,
};
use crate::zoom::{install_zoom, zpx};

/// `FalconApp` 根组件属性（所有字段满足 `JsValue + Clone + PartialEq`）。
#[derive(Clone, PartialEq, Props)]
pub struct FalconAppProps {
    pub offline_demo: bool,
    pub server_url: Option<String>,
    pub initial_theme_json: Option<String>,
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
    };
    let theme_ctrl = install_theme(initial_theme);
    let zoom_ctrl = install_zoom();
    let store = WorkspaceStore::install(&launch_opts);

    // 同步深色/浅色与缩放全局快捷键（全部经 `bind_safe_key` 过滤宿主保留键）
    {
        let st_pal = store.clone();
        bind_safe_key("mod+k", false, move || {
            let next = if st_pal.active_dialog.get() == ActiveDialog::Palette {
                ActiveDialog::None
            } else {
                ActiveDialog::Palette
            };
            st_pal.active_dialog.set(next);
        });
    }
    {
        let st_set = store.clone();
        bind_safe_key("mod+,", false, move || {
            st_set
                .active_dialog
                .set(ActiveDialog::Settings(SettingsTab::Appearance));
        });
    }
    {
        let st_new = store.clone();
        bind_safe_key("mod+t", false, move || {
            st_new.create_terminal_in_active_project();
        });
    }
    {
        let st_detach = store.clone();
        bind_safe_key("mod+shift+w", false, move || {
            let fk = st_detach.focused_pane_key.get();
            if !fk.is_empty() {
                st_detach.detach_pane(&fk);
            }
        });
    }
    {
        let st_close = store.clone();
        bind_safe_key("mod+w", false, move || {
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
        bind_safe_key("mod+=", false, move || zoom_ctrl.step_in());
        bind_safe_key("mod+-", false, move || zoom_ctrl.step_out());
        bind_safe_key("mod+0", false, move || zoom_ctrl.reset());
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

    view()
        .size_full()
        .relative()
        .overflow_hidden()
        .bg(move || rgb_to_hsla(theme_ctrl.resolved().ui.app))
        .child(dynamic(move || match idiom() {
            Idiom::Terminal => tui_shell().into_element(),
            Idiom::Phone => mobile_shell().into_element(),
            _ => desktop_shell().into_element(),
        }))
        .child(toast_overlay())
        .child(modal_layer())
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
                                                svc.lan_access().map(|s| s.enabled).unwrap_or(false),
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
        items.push(
            MenuItem::new(t!("native.menu.quit"))
                .shortcut("mod+q")
                .on_select(quit),
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

    let offline_demo = options.offline_demo;
    let server_url = options.server_url;
    let initial_theme_json = options
        .initial_theme
        .as_ref()
        .and_then(|th| serde_json::to_string(th).ok());

    let root = Root::new::<FalconApp>(move || FalconAppProps {
        offline_demo,
        server_url: server_url.clone(),
        initial_theme_json: initial_theme_json.clone(),
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

    fn ensure_test_platform() {
        let _ = falcon_platform::shared();
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
        };

        let root = falcon_root_with_options(opts.clone());
        assert_eq!(root.appearance_ref(), Some(Appearance::Dark));

        let mut app = headless_with_options(opts, ColorScheme::Light);
        app.first_render().expect("first render");
        app.wait_for("当前底色判定：深色外观")
            .expect("surface derived dark appearance despite OS ColorScheme::Light and Light slot");

        // 对照验证：浅色槽位放浅底主题时判定为浅色外观
        let light_entry = find_theme(catalog, "Falcon Light")
            .or_else(|| catalog.iter().find(|e| !derive_theme(&e.colors).is_dark))
            .expect("at least one light theme in catalog");
        let mut light_settings = ThemeSettings::default();
        light_settings.mode = ThemePref::Light;
        light_settings.light = choice_of(light_entry);
        assert_eq!(
            light_settings.slot(ThemeMode::Light).name,
            light_entry.name
        );

        let mut light_app = headless_with_options(
            FalconLaunchOptions {
                offline_demo: true,
                server_url: Some("http://127.0.0.1:4923".into()),
                initial_theme: Some(light_settings),
            },
            ColorScheme::Dark,
        );
        light_app.first_render().expect("first render light");
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
            },
            ColorScheme::Dark,
        );
        app.first_render().expect("first render");

        // 首屏领域元素校验
        app.wait_for("falcon · nsh-client").unwrap();
        app.wait_for("附属项目").unwrap();
        app.wait_for("源项目：falcon").unwrap();
        app.wait_for("远端主机：build-box-01").unwrap();
        app.wait_for("cargo check · 1.2s").unwrap();
        app.wait_for("14.2%").unwrap();

        // 1. Detach（收起窗口）：窗口从列式画布摘除，但会话仍在侧栏标记为后台运行中且可接回
        app.press("label:detach-t:sess-main")
            .expect("press detach button on sess-main");
        app.wait_for("后台运行中").unwrap();
        app.wait_for("接回窗口").unwrap();

        // 2. Reattach（接回窗口）：从侧栏重新挂回列式画布
        app.press("label:reattach-sess-main")
            .expect("press reattach button on sess-main");
        app.wait_for_gone("后台运行中", Duration::from_secs(2))
            .unwrap();

        // 3. Terminate（终止会话）：sess-main 有前台命令 `cargo check -p falcon-nsh`，必须先弹出确认框
        app.press("label:terminate-sess-main")
            .expect("press terminate button on busy session");
        app.wait_for("前台程序仍在运行").unwrap();
        app.press("仍然终止").expect("confirm force terminate");
        app.wait_for_gone("cargo check -p falcon-nsh", Duration::from_secs(2))
            .unwrap();
    }

    /// 移动端（Android `Idiom::Phone`）与终端工作台（`Idiom::Terminal`）自适应骨架无头验证。
    #[test]
    fn mobile_and_tui_idioms_mount_cleanly_headless() {
        ensure_test_platform();
        let opts = FalconLaunchOptions {
            offline_demo: true,
            server_url: Some("http://127.0.0.1:4923".into()),
            initial_theme: None,
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
        let res_ok = verifier.verify_and_accept(
            &root_b64,
            Some("dev.falcon.plugin.gpu"),
            &pkg_v5,
        );
        assert!(
            matches!(
                res_ok,
                PackageVerificationState::Verified { build: 5, .. }
            ),
            "expected Verified(build=5), got {res_ok:?}"
        );

        // 2. 篡改任意一字节立即拒绝
        let mut tampered = make_pkg(6, &server_rel, &cert);
        let mid = tampered.len() / 2;
        tampered[mid] ^= 0x5a;
        let res_tamper = verifier.verify_and_accept(
            &root_b64,
            Some("dev.falcon.plugin.gpu"),
            &tampered,
        );
        assert!(matches!(res_tamper, PackageVerificationState::Untrusted { .. }));

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
        let res_rogue = verifier.verify_and_accept(
            &root_b64,
            Some("dev.falcon.plugin.gpu"),
            &rogue_pkg,
        );
        assert!(matches!(res_rogue, PackageVerificationState::Untrusted { .. }));

        // 4. 同一信任根签出的旧构建号（build 4 < 5）被持久水位拦截回滚
        let mut reopened = ClientPluginVerifier::open_persistent(tmp.path()).unwrap();
        let older_pkg = make_pkg(4, &server_rel, &cert);
        let res_rollback = reopened.verify_and_accept(
            &root_b64,
            Some("dev.falcon.plugin.gpu"),
            &older_pkg,
        );
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
            let vp = TerminalViewport::compute(
                960.0,
                720.0,
                8.0,
                18.0,
                13.0,
                true,
                true,
                &snap,
            );
            std::hint::black_box(paint_snapshot(&snap, &resolved, vp, Some("中文输入")));
        }

        let mut samples_ms = Vec::with_capacity(200);
        for _ in 0..200 {
            let t0 = Instant::now();
            let snap = sess.snapshot();
            let vp = TerminalViewport::compute(
                960.0,
                720.0,
                8.0,
                18.0,
                13.0,
                true,
                true,
                &snap,
            );
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

    /// ADR 0018 对账回归：`falcon-core::app_icon::APP_ICON_IDS` 中全部内置图标均已在 `assets/app-icons/` 下就位。
    #[test]
    fn app_icons_match_core_app_icon_ids() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/app-icons");
        for id in falcon_core::app_icon::APP_ICON_IDS {
            let path = dir.join(format!("{id}.png"));
            assert!(path.is_file(), "missing builtin app icon asset: {}", path.display());
        }
    }
}
