//! 前端产物托管：浏览器版（native/web 的宿主页 + wasm + 字体 + 图标，`native/scripts/build-web.sh`
//! 的产物）。S7 改成编进二进制（rust-embed），在那之前从目录读。
//!
//! 规则同 Node 版 index.ts：文件在就给文件；不在时，GET 且不是 `/api/`、`/ws/` 的一律回
//! index.html（前端自己处理路径），其余 404 `{ error: "Not Found" }`。

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::Request;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tower_http::services::ServeDir;

/// `FALCON_WEB_DIST`（旧名 `MOJITO_WEB_DIST`）优先，否则开发构建的默认产物目录。
/// 目录里没有 index.html 就当不存在——环境变量多半是从 falcon 终端里继承来的陈旧值
pub fn web_dist() -> Option<PathBuf> {
    let from_env = std::env::var_os("FALCON_WEB_DIST").or_else(|| std::env::var_os("MOJITO_WEB_DIST"));
    let dir = from_env
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target-wasm/dist"));
    dir.join("index.html").is_file().then_some(dir)
}

pub fn service(dir: PathBuf) -> ServeDir<axum::routing::MethodRouter> {
    let index = dir.join("index.html");
    ServeDir::new(dir).append_index_html_on_directories(true).fallback(axum::routing::any(
        move |req: Request| {
            let index = index.clone();
            async move { spa_fallback(&index, req).await }
        },
    ))
}

async fn spa_fallback(index: &Path, req: Request) -> Response {
    let path = req.uri().path();
    if req.method() != Method::GET || path.starts_with("/api/") || path.starts_with("/ws/") {
        return not_found().await;
    }
    match tokio::fs::read(index).await {
        Ok(bytes) => (
            [
                (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
                // 宿主页决定拉哪一份 wasm，必须每次重新验证，否则发版后旧页拉新 wasm 对不上
                (CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => {
            log::warn!("读 {} 失败：{e}", index.display());
            not_found().await
        }
    }
}

pub async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "Not Found" }))).into_response()
}
