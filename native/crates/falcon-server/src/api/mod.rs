//! HTTP 面：axum 路由、鉴权、错误形状。对应 `packages/server/src/routes.ts` 与
//! `index.ts` 的 Fastify 部分。路由路径、请求 / 响应体、状态码都照 Node 版（决定一：协议不变）。
//!
//! 鉴权与 Node 版同一口径（routes.ts 的 onRequest 钩子）：
//! - `/api/auth/*` 不鉴权；
//! - 原始字节路由（rawToken）、应用图标的取图路由（publicAsset）、askpass helper 各自验身份，
//!   不走登录 cookie——它们挂在 [`public_router`] 上；
//! - 其余 `/api/*` 与 `/px0/*` 一律要登录 cookie，挂在 [`protected_router`] 上。

pub mod app_icon;
pub mod askpass;
pub mod auth_routes;
pub mod body;
pub mod error;
pub mod files;
pub mod git_routes;
pub mod hosts;
pub mod input;
pub mod plugins;
pub mod projects;
pub mod px0;
pub mod relays;
pub mod sessions;
pub mod system;
pub mod server_update;
pub mod tailscale;
#[cfg(test)]
pub(crate) mod test_support;
pub mod worktrees;
pub mod ws;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use axum::Json;
use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::askpass::hub::AskpassHub;
use crate::auth::{Auth, COOKIE_NAME};
use crate::config::ServerConfig;
use crate::crypto::SecretBox;
use crate::db::Db;
use crate::engine::EngineHandle;
use crate::plugins::PluginHost;
use error::ApiError;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 各路由共用的状态。克隆便宜（全是 Arc）
#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub config: ServerConfig,
    pub db: Arc<Db>,
    pub auth: Auth<Arc<Db>>,
    pub secrets: Arc<SecretBox>,
    pub askpass: Arc<AskpassHub>,
    /// 会话引擎（LocalSet 上的 SessionManager 等），见 engine.rs
    pub engine: EngineHandle,
    /// 只缓存节点连接与列表；项目与会话的权威数据仍在所属节点。
    pub nodes: crate::nodes::Gateway,
    /// 右侧栏插件（命令插件只在后端本机起进程；系统监控看远端时才借引擎拿链路）
    pub plugins: PluginHost,
    /// `POST /api/system/restart` 要的关停信号：主循环拿它走与 SIGTERM 同一条退出路径
    /// （先 Engine::shutdown 收子进程再退出），launchd / systemd 或 respawn 帮手负责拉起
    pub shutdown: Arc<tokio::sync::Notify>,
    /// 重启已在途的标记：第二个 /api/system/restart 直接吞掉，别让两个 respawn 帮手抢端口
    pub restarting: AtomicBool,
}

impl std::ops::Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl AppState {
    pub fn new(
        config: ServerConfig,
        db: Arc<Db>,
        secrets: Arc<SecretBox>,
        askpass: Arc<AskpassHub>,
        engine: EngineHandle,
        plugins: PluginHost,
    ) -> Self {
        let auth = Auth::new(db.clone());
        AppState {
            inner: Arc::new(Inner {
                config,
                db,
                auth,
                secrets,
                askpass,
                engine,
                nodes: crate::nodes::Gateway::default(),
                plugins,
                shutdown: Arc::new(tokio::sync::Notify::new()),
                restarting: AtomicBool::new(false),
            }),
        }
    }

    /// 能连上就允许使用，不再看登录 cookie。
    pub fn authenticated(&self, headers: &HeaderMap) -> bool {
        self.auth.is_authenticated(cookie(headers, COOKIE_NAME).as_deref())
    }
}

/// 从 Cookie 头里取一个值。不引 cookie 库：只认 `name=value`，值原样返回（Node 的
/// @fastify/cookie 会做 decodeURIComponent，falcon_token 是 hex，两者没区别）
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(COOKIE).iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
        v.split(';').find_map(|pair| {
            let (k, val) = pair.trim().split_once('=')?;
            (k.trim() == name).then(|| val.trim().trim_matches('"').to_string())
        })
    })
}

/// 整个应用
pub fn app(state: AppState) -> Router {
    Router::new()
        .merge(auth_routes::router())
        .merge(ws::router())
        .merge(public_router())
        .merge(protected_router().layer(middleware::from_fn_with_state(state.clone(), require_login)))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), crate::nodes::middleware))
        .layer(middleware::from_fn_with_state(state.clone(), server_update::maintenance))
        .with_state(state)
}

pub async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not Found" }))).into_response()
}

/// 不走登录 cookie、各自验身份的路由：原始字节（作用域令牌）、应用图标的取图路由
/// （publicAsset）、askpass helper（Bearer）
fn public_router() -> Router<AppState> {
    Router::new()
        .merge(askpass::helper_router())
        .merge(files::raw_router())
        .merge(app_icon::public_router())
}

/// `/api/*` 与 `/px0/*`。`require_login` 还在，账户门禁去掉之后直接放行。
fn protected_router() -> Router<AppState> {
    Router::new()
        .merge(sessions::router())
        .merge(askpass::router())
        .merge(system::router())
        .merge(server_update::router())
        .merge(crate::nodes::router())
        .merge(files::router())
        .merge(app_icon::router())
        .merge(hosts::router())
        .merge(tailscale::router())
        .merge(projects::router())
        .merge(relays::router())
        .merge(git_routes::router())
        .merge(worktrees::router())
        .merge(plugins::router())
        .merge(px0::router())
        // 没有对应路由的 /api/*：404。账户门禁去掉之后不再先回 401。
        .route("/api/{*rest}", axum::routing::any(not_found))
}

/// 原先的登录检查。账户门禁去掉之后 `authenticated` 恒为 true，请求直接放行。
async fn require_login(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.authenticated(req.headers()) {
        return next.run(req).await;
    }
    ApiError::unauthorized().into_response()
}

/// JS `encodeURIComponent`：`A-Z a-z 0-9 - _ . ! ~ * ' ( )` 之外一律百分号编码（按 UTF-8 字节）
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.append(COOKIE, HeaderValue::from_static("a=1; falcon_token=abc ; b=2"));
        assert_eq!(cookie(&h, "falcon_token").as_deref(), Some("abc"));
        assert_eq!(cookie(&h, "nope"), None);
        let mut h = HeaderMap::new();
        h.append(COOKIE, HeaderValue::from_static("x=1"));
        h.append(COOKIE, HeaderValue::from_static("falcon_token=\"q\""));
        assert_eq!(cookie(&h, "falcon_token").as_deref(), Some("q"));
    }

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(encode_uri_component("/px0/p1/?a=b c&d=中"), "%2Fpx0%2Fp1%2F%3Fa%3Db%20c%26d%3D%E4%B8%AD");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
    }
}
