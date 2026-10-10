//! HTTP 面：axum 路由、鉴权、错误形状、静态资源。对应 `packages/server/src/routes.ts` 与
//! `index.ts` 的 Fastify 部分。路由路径、请求 / 响应体、状态码都照 Node 版（决定一：协议不变）。
//!
//! 鉴权与 Node 版同一口径（routes.ts 的 onRequest 钩子）：
//! - `/api/auth/*` 不鉴权；
//! - 原始字节路由（rawToken）、应用图标的取图路由（publicAsset）、askpass helper 各自验身份，
//!   不走登录 cookie——它们挂在 [`public_router`] 上；
//! - `/px0/` 与 `/api/` 同一口径，没登录的浏览器导航送去 `/?next=` 登录；
//! - 其余 `/api/*` 一律要登录 cookie，挂在 [`protected_router`] 上。

pub mod app_icon;
pub mod askpass;
pub mod auth_routes;
pub mod body;
pub mod error;
pub mod files;
pub mod git_routes;
pub mod hosts;
pub mod input;
pub mod meegle;
pub mod projects;
pub mod px0;
pub mod relays;
pub mod sessions;
pub mod static_files;
pub mod system;
#[cfg(test)]
pub(crate) mod test_support;
pub mod worktrees;
pub mod ws;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{ACCEPT, COOKIE};
use axum::http::{HeaderMap, Method};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Redirect, Response};

use crate::askpass::hub::AskpassHub;
use crate::auth::{Auth, COOKIE_NAME};
use crate::config::ServerConfig;
use crate::crypto::SecretBox;
use crate::db::Db;
use crate::engine::EngineHandle;
use crate::meegle::client::MeegleClient;
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
    /// 飞书项目 CLI（只在后端本机起进程，Send + Sync，不进引擎）
    pub meegle: MeegleClient,
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
        meegle: MeegleClient,
    ) -> Self {
        let loopback = crate::config::is_loopback(&config.host);
        let auth = Auth::new(db.clone(), loopback);
        AppState { inner: Arc::new(Inner { config, db, auth, secrets, askpass, engine, meegle }) }
    }

    /// 请求带来的登录 cookie 有没有效（不需要认证的部署恒为 true）
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

/// 整个应用。`web_dist` 存在时托管前端产物（浏览器版的宿主页、wasm、字体、图标）
pub fn app(state: AppState) -> Router {
    let web_dist = static_files::web_dist();
    let mut router = Router::new()
        .merge(auth_routes::router())
        .merge(ws::router())
        .merge(public_router())
        .merge(protected_router().layer(middleware::from_fn_with_state(state.clone(), require_login)));
    router = match web_dist {
        Some(dir) => router.fallback_service(static_files::service(dir)),
        None if static_files::embedded::available() => router.fallback(static_files::embedded_service),
        None => {
            log::warn!("web 静态资源目录不存在，未托管 UI（FALCON_WEB_DIST 指了不存在的目录？）");
            router.fallback(static_files::not_found)
        }
    };
    router.with_state(state)
}

/// 不走登录 cookie、各自验身份的路由：原始字节（作用域令牌）、应用图标的取图路由
/// （publicAsset，含不在 /api 下的 PWA 清单）、askpass helper（Bearer）
fn public_router() -> Router<AppState> {
    Router::new().merge(askpass::helper_router()).merge(files::raw_router()).merge(app_icon::public_router())
}

/// 要登录的 `/api/*`（S4 起逐组填进来）
fn protected_router() -> Router<AppState> {
    Router::new()
        .merge(sessions::router())
        .merge(askpass::router())
        .merge(system::router())
        .merge(files::router())
        .merge(app_icon::router())
        .merge(hosts::router())
        .merge(projects::router())
        .merge(relays::router())
        .merge(git_routes::router())
        .merge(worktrees::router())
        .merge(meegle::router())
        .merge(px0::router())
        // 没有对应路由的 /api/*：同 Node 版的 onRequest 钩子，没登录先 401，登录了才 404
        .route("/api/{*rest}", axum::routing::any(static_files::not_found))
}

/// `/api/*` 与 `/px0/*` 的登录检查（Node 版 onRequest 钩子的那一段）
async fn require_login(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.authenticated(req.headers()) {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if path.starts_with("/px0/") {
        // 浏览器导航过来的（原生客户端把入口交给系统浏览器时，那边多半没登录过）：
        // 送去首页登录，带上原地址，登录成功后回来（web 的 lib/loginNext.ts）
        let html = req.headers().get(ACCEPT).and_then(|v| v.to_str().ok()).is_some_and(|a| a.contains("text/html"));
        if req.method() == Method::GET && html {
            let full = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or(path);
            return Redirect::to(&format!("/?next={}", encode_uri_component(full))).into_response();
        }
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
