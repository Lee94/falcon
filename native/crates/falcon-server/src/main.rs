//! falcon 服务端（Rust）的可执行入口。对应 `packages/server/src/index.ts`。
//!
//! 已接上：配置、数据库、加密、鉴权、静态资源（S2），会话引擎与会话 / WS / askpass 路由（S4）。
//! git、文件、中转等路由随 S5–S6 逐步接上（docs/design/rust-unification.md）。

use std::sync::Arc;

use anyhow::Context;
use falcon_server::api::{self, AppState};
use falcon_server::askpass::hub::AskpassHub;
use falcon_server::config::{is_loopback, parse_args};
use falcon_server::crypto::SecretBox;
use falcon_server::db::Db;
use falcon_server::engine::{self, EngineDeps};
use falcon_server::zellij::version::ZELLIJ_VERSION;

fn main() -> anyhow::Result<()> {
    init_logging();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let config = parse_args(&argv, |k| std::env::var(k).ok())?;
    let loopback = is_loopback(&config.host);

    let db = Arc::new(Db::open(&config.data_dir)?);
    let secrets = Arc::new(SecretBox::open(&config.data_dir)?);

    if !loopback && !falcon_server::auth::Auth::new(db.clone(), loopback).password_set() {
        eprintln!(
            "拒绝启动：绑定 {}（非 localhost）但尚未设置访问密码。\n请先在 localhost 上启动并通过界面设置密码，再对外绑定。",
            config.host
        );
        std::process::exit(1);
    }

    let askpass = Arc::new(AskpassHub::new());
    askpass.set_origin(&format!("http://127.0.0.1:{}", config.port));
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
        },
    )?;
    {
        let engine = engine.clone();
        askpass.set_on_prompt(move |p| engine.send(move |e| e.sessions.broadcast_askpass(&p)));
    }
    let state = AppState::new(config.clone(), db, secrets, askpass, engine);

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let addr = format!("{}:{}", config.host, config.port);
        let listener = tokio::net::TcpListener::bind(&addr).await.with_context(|| format!("监听 {addr} 失败"))?;
        log::info!(
            "falcon 已启动: http://{}:{}（数据目录 {}，Zellij {ZELLIJ_VERSION}）",
            if loopback { "localhost" } else { &config.host },
            config.port,
            config.data_dir.display()
        );
        // 不走 graceful shutdown：会话的 WS 连接不会自己断，等它们就永远退不出去。
        // 会话在 DB 中保持 active，下次启动由 recover_sessions_on_startup 归类
        tokio::select! {
            res = axum::serve(listener, api::app(state)) => res?,
            () = shutdown_signal() => {}
        }
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

/// 日志写 stderr。`FALCON_LOG=debug` 打开调试日志（与原生客户端同一个开关）
fn init_logging() {
    struct Logger(log::LevelFilter);
    impl log::Log for Logger {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= self.0
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{} {}] {}", r.level(), r.target(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let level = if std::env::var("FALCON_LOG").is_ok_and(|v| v == "debug") {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    };
    let _ = log::set_boxed_logger(Box::new(Logger(level))).map(|()| log::set_max_level(level));
}
