//! falcon 服务端（Rust）的可执行入口。对应 `packages/server/src/index.ts`。
//!
//! 现在只有 S2 的骨架：配置、数据库、加密、鉴权与 `/api/auth/*`、静态资源。会话、git、文件、
//! 中转等路由随 S3–S6 逐步接上（docs/design/rust-unification.md）。

use std::sync::Arc;

use anyhow::Context;
use falcon_server::api::{self, AppState};
use falcon_server::config::{is_loopback, parse_args};
use falcon_server::crypto::SecretBox;
use falcon_server::db::Db;
use falcon_server::zellij::version::ZELLIJ_VERSION;

fn main() -> anyhow::Result<()> {
    init_logging();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let config = parse_args(&argv, |k| std::env::var(k).ok())?;
    let loopback = is_loopback(&config.host);

    let db = Arc::new(Db::open(&config.data_dir)?);
    let secrets = SecretBox::open(&config.data_dir)?;
    db.recover_sessions_on_startup();
    let state = AppState::new(config.clone(), db, secrets);

    if !loopback && !state.auth.password_set() {
        eprintln!(
            "拒绝启动：绑定 {}（非 localhost）但尚未设置访问密码。\n请先在 localhost 上启动并通过界面设置密码，再对外绑定。",
            config.host
        );
        std::process::exit(1);
    }

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
        axum::serve(listener, api::app(state)).with_graceful_shutdown(shutdown_signal()).await?;
        // 会话在 DB 中保持 active，下次启动由 recover_sessions_on_startup 归类
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
