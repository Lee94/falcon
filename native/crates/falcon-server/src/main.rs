//! falcon 服务端（Rust）的可执行入口。对应 `packages/server/src/index.ts`。
//!
//! 已接上：配置、数据库、加密、鉴权、静态资源（S2），会话引擎与会话 / WS / askpass 路由（S4）。
//! git、文件、中转等路由随 S5–S6 逐步接上（docs/design/rust-unification.md）。

// Windows release：GUI 子系统，计划任务（InteractiveToken）起服务端时系统压根不给它分配控制台。
// 控制台窗口是系统在跑到 main 之前就建好的，事后 ShowWindow(SW_HIDE) 只在传统 conhost 下有效：
// Windows Terminal 当默认终端时 GetConsoleWindow 拿到的不是那个可见窗口，隐藏是空操作。
// 代价与对策：① 每个控制台子进程都要 CREATE_NO_WINDOW（no_window.rs），否则它们各自弹一个窗口；
// ② 没有控制台时 stderr 句柄无效，输出被 std 静默丢弃，服务日志改写 <dataDir>/logs/falcon.log
// （logging.rs）；③ 手动跑 release 二进制的 CLI 时 shell 不再等它退出、输出也看不见，要重定向
// 或管道（`… service status > out.txt`）；管道调用不受影响。
// debug 构建仍是控制台程序：`cargo run -p falcon-server` 照旧在终端里看日志。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::sync::Arc;

use anyhow::Context;
use falcon_server::api::{self, AppState};
use falcon_server::askpass::hub::AskpassHub;
use falcon_server::config::{is_loopback, local_connect_host, parse_args};
use falcon_server::crypto::SecretBox;
use falcon_server::db::Db;
use falcon_server::engine::{self, EngineDeps};

fn main() -> anyhow::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    // 计划任务启动时带的内部标记：取走它（不属于 ServerConfig），并记下「这是服务实例」——
    // 它的日志要落文件。debug 构建仍是控制台程序，顺手把那个控制台隐藏掉
    #[cfg(windows)]
    let (argv, service_run) = {
        let mut argv = argv;
        let service_run = argv.first().is_some_and(|arg| arg == falcon_core::schtasks::SERVICE_RUN_ARG);
        if service_run {
            hide_service_console();
            argv.remove(0);
        }
        (argv, service_run)
    };
    // 先于一切子命令：service / node / update-helper 也可能要往宿主机释放或推送守护进程
    #[cfg(feature = "embed-sessiond")]
    falcon_server::sessiond::bin::register_embedded(embedded_sessiond);
    falcon_server::logging::init();
    // `falcon service <install|…>`：注册/管理系统服务（launchd / systemd 守护），不启动服务器
    if argv.first().map(String::as_str) == Some("service") {
        std::process::exit(falcon_server::service_cli::run_service_cli(&argv[1..]));
    }
    // `falcon respawn <pid> -- <argv…>`：重启帮手（POST /api/system/restart 派的），等 pid
    // 死后以 argv 重开一个。藏在同一个二进制里，不占 shell、不经 PATH
    if argv.first().map(String::as_str) == Some("respawn") {
        std::process::exit(falcon_server::restart::run_respawn_cli(&argv[1..]));
    }
    if argv.first().map(String::as_str) == Some("node") {
        std::process::exit(falcon_server::nodes::bootstrap::run_node_cli(&argv[1..]));
    }
    if argv.first().map(String::as_str) == Some("update-helper") {
        std::process::exit(falcon_server::server_update::run_helper_cli(&argv[1..]));
    }
    // 版本探测不能落入普通启动路径，更不能隐式打开默认用户数据目录。
    if matches!(argv.first().map(String::as_str), Some("version" | "--version")) {
        println!("{}", serde_json::json!({"version":env!("CARGO_PKG_VERSION"),
            "protocolVersion":falcon_server::nodes::PROTOCOL_VERSION,
            "platform":std::env::consts::OS,"arch":std::env::consts::ARCH}));
        return Ok(());
    }
    let mut config = parse_args(&argv, |k| std::env::var(k).ok())?;
    // 服务实例、或任何拿不到 stderr 的处境（双击 release 二进制）：日志落文件，位置与 macOS
    // launchd 的 StandardOutPath 一致。macOS / Linux 不用管：launchd / journald 自己收 stderr
    #[cfg(windows)]
    if service_run || !falcon_server::logging::stderr_usable() {
        falcon_server::logging::redirect_to_file(&config.data_dir);
    }
    let _node_process_lock = falcon_server::nodes::bootstrap::lock_node_process(&config.data_dir)?;
    falcon_server::server_update::startup_gate(&mut config)?;
    let loopback = is_loopback(&config.host);

    let db = Arc::new(Db::open(&config.data_dir)?);
    falcon_server::server_update::prepare_startup_db(&db, &config.data_dir)?;
    let secrets = Arc::new(SecretBox::open(&config.data_dir)?);

    let askpass = Arc::new(AskpassHub::new());
    askpass.set_origin(&format!("http://{}:{}", local_connect_host(&config.host), config.port));
    // 会话引擎：SessionManager 的构造里做启动恢复（上次 active 的会话归类为 unverified / dead）
    let (engine, _engine_thread) = engine::spawn(
        EngineDeps { db: db.clone(), secrets: secrets.clone(), data_dir: config.data_dir.clone(), askpass: askpass.clone() },
        |engine| {
            let sessions = engine.sessions.clone();
            // 本地 PTY 基底环境（login shell 解析 + locale 兜底）预热：
            // 结果按进程缓存，先跑起来，首个本地会话就不用等 login shell 启动
            tokio::task::spawn_local(async move {
                sessions.local.base_env().await;
            });
            // 启用中的中转（转发 / 公网发布）是服务，后端重启后应自己把隧道拉起来，不等用户再去设置里点
            engine.sessions.restore_relays();
            // 持久会话在 DB 里被标成 unverified：自动接回，不要等用户挨个点
            engine.sessions.resume_unverified();
            // 存档到期的附属项目由后台清扫自动删除，不等用户下次打开界面
            falcon_server::archive::start_archive_sweeper(engine);
        },
    )?;
    {
        let engine = engine.clone();
        askpass.set_on_prompt(move |p| engine.send(move |e| e.sessions.broadcast_askpass(&p)));
    }
    let plugins = falcon_server::plugins::PluginHost::new(
        &config.data_dir,
        falcon_server::sessions::local::local_base_env_fn(),
    );
    let state = AppState::new(config.clone(), db, secrets, askpass, engine, plugins);

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let addr = format!("{}:{}", config.host, config.port);
        let listener = tokio::net::TcpListener::bind(&addr).await.with_context(|| format!("监听 {addr} 失败"))?;
        let actual_port = listener.local_addr()?.port();
        falcon_server::server_update::record_listener(&state.config, actual_port);
        falcon_server::nodes::bootstrap::publish(&config, actual_port, &state.db)?;
        state.askpass.set_origin(&format!("http://{}:{}", local_connect_host(&config.host), actual_port));
        log::info!(
            "falcon 已启动: http://{}:{}（数据目录 {}）",
            if loopback { "localhost" } else { &config.host },
            config.port,
            config.data_dir.display()
        );
        // 不走 graceful shutdown：会话的 WS 连接不会自己断，等它们就永远退不出去。
        // 会话在 DB 中保持 active，下次启动由 recover_sessions_on_startup 归类
        let engine = state.engine.clone();
        let shutdown = state.shutdown.clone();
        tokio::select! {
            res = axum::serve(listener, api::app(state)) => res?,
            () = shutdown_signal() => {}
            () = shutdown.notified() => {}
        }
        // cloudflared / 本机 px0 是子进程，后端退出不会自动带走（见 Engine::shutdown）。
        let stop = engine.call(|e| async move { e.shutdown().await });
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), stop).await;
        anyhow::Ok(())
    })
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("注册 SIGTERM");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}

/// 发布构建内嵌的 4 个目标守护进程（gzip，`FALCON_EMBED_SESSIOND_DIR` 由 `cargo xtask server` /
/// CI 指好）。写在 bin target 里而不是库里，换守护进程只重编这个文件，原因见
/// `falcon_server::sessiond::bin::register_embedded`
#[cfg(feature = "embed-sessiond")]
fn embedded_sessiond(target: falcon_server::sessiond::command::SessiondTarget) -> &'static [u8] {
    use falcon_server::sessiond::command::SessiondTarget;
    match target {
        SessiondTarget::X86_64Linux => include_bytes!(concat!(env!("FALCON_EMBED_SESSIOND_DIR"), "/x86_64-unknown-linux-musl.gz")),
        SessiondTarget::Aarch64Linux => include_bytes!(concat!(env!("FALCON_EMBED_SESSIOND_DIR"), "/aarch64-unknown-linux-musl.gz")),
        SessiondTarget::Aarch64Mac => include_bytes!(concat!(env!("FALCON_EMBED_SESSIOND_DIR"), "/aarch64-apple-darwin.gz")),
        SessiondTarget::X86_64Windows => include_bytes!(concat!(env!("FALCON_EMBED_SESSIOND_DIR"), "/x86_64-pc-windows-gnu.gz")),
    }
}

/// debug 构建的兜底：它仍是控制台程序，计划任务起它时会有一个控制台窗口，隐藏掉。
/// 只隐藏、不 FreeConsole：后代控制台进程继续继承这个不可见的控制台，不会各自再开窗口。
/// release 是 GUI 子系统，根本没有控制台，GetConsoleWindow 返回 NULL，这里是空操作；
/// 默认终端是 Windows Terminal 时隐藏也不生效——那条路靠子系统解决，不靠这里
#[cfg(windows)]
fn hide_service_console() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    unsafe {
        let window = GetConsoleWindow();
        if !window.is_null() {
            ShowWindow(window, SW_HIDE);
        }
    }
}
