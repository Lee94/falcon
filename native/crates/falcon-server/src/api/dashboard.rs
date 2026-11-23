//! 官方 agent-browser Dashboard 的静态导出，与节点网关使用同一个来源。
use axum::{
    Router,
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};

use super::AppState;
include!("../../dashboard/assets.rs");

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/browser-dashboard", get(index))
        .route("/browser-dashboard/", get(index))
        .route("/browser-dashboard/{*path}", get(|Path(path): Path<String>| async move { serve(&path) }))
}

async fn index() -> Response {
    serve("index.html")
}

fn serve(path: &str) -> Response {
    let Some((mime, bytes)) = asset(path) else { return StatusCode::NOT_FOUND.into_response() };
    ([
        (header::CONTENT_TYPE, mime),
        (header::CONTENT_ENCODING, "gzip"),
        (header::CACHE_CONTROL, if path.starts_with("_next/static/") { "public, max-age=31536000, immutable" } else { "no-cache" }),
        (header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' blob: data:; connect-src 'self'; object-src 'none'; base-uri 'none'"),
    ], bytes).into_response()
}
