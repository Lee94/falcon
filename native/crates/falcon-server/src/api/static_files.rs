//! 前端产物托管：浏览器版客户端（native/web 的宿主页 + wasm + 字体 + 图标，
//! `native/scripts/build-web.sh` 的产物）。托管层只认目录里有 index.html。
//!
//! 来源的优先级：`FALCON_WEB_DIST`（要真有 index.html）> 编进二进制的那份（`embed-web`
//! feature，构建时 `FALCON_EMBED_WEB_DIR` 指定）> 开发构建的默认产物目录。
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
use tower_http::set_header::SetResponseHeader;

/// `FALCON_WEB_DIST` 优先，否则开发构建的默认产物目录。
/// 目录里没有 index.html 就当不存在——环境变量多半是从 falcon 终端里继承来的陈旧值。
/// 旧名 `MOJITO_WEB_DIST` 不再认：它只可能指向已删的 React 产物，认了反而会盖过编进去的
/// 浏览器版
pub fn web_dist() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FALCON_WEB_DIST").map(PathBuf::from) {
        if dir.join("index.html").is_file() {
            return Some(dir);
        }
        log::warn!("FALCON_WEB_DIST 指向的目录里没有 index.html，忽略：{}", dir.display());
    }
    if embedded::available() {
        return None;
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target-wasm/dist");
    dev.join("index.html").is_file().then_some(dev)
}

/// 编进二进制的前端产物
pub mod embedded {
    #[cfg(feature = "embed-web")]
    #[derive(rust_embed::Embed)]
    #[folder = "$FALCON_EMBED_WEB_DIR"]
    struct Assets;

    pub fn available() -> bool {
        #[cfg(feature = "embed-web")]
        return Assets::get("index.html").is_some();
        #[cfg(not(feature = "embed-web"))]
        false
    }

    /// 按路径取一个文件：(字节, Content-Type, ETag)
    #[cfg(feature = "embed-web")]
    pub fn get(path: &str) -> Option<(std::borrow::Cow<'static, [u8]>, String, String)> {
        let file = Assets::get(path)?;
        let etag = format!("\"{}\"", hex::encode(file.metadata.sha256_hash()));
        Some((file.data, file.metadata.mimetype().to_string(), etag))
    }

    #[cfg(not(feature = "embed-web"))]
    pub fn get(_path: &str) -> Option<(std::borrow::Cow<'static, [u8]>, String, String)> {
        None
    }
}

/// 从二进制里取文件的服务（[`embedded::available`] 为真时用）
pub async fn embedded_service(req: Request) -> Response {
    let method = req.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return not_found().await;
    }
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() || path.ends_with('/') { format!("{path}index.html") } else { path.to_string() };
    let hit = embedded::get(&path);
    let is_index = hit.is_none() || path == "index.html" || path.ends_with("/index.html");
    let found = match hit {
        Some(f) => Some(f),
        None => {
            let p = req.uri().path();
            if p.starts_with("/api/") || p.starts_with("/ws/") {
                return not_found().await;
            }
            embedded::get("index.html")
        }
    };
    let Some((bytes, mime, etag)) = found else { return not_found().await };
    let fresh = req
        .headers()
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let cache = if is_index { "no-cache" } else { "public, max-age=0" };
    let headers = [
        (CONTENT_TYPE, HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("application/octet-stream"))),
        (CACHE_CONTROL, HeaderValue::from_static(cache)),
        (axum::http::header::ETAG, HeaderValue::from_str(&etag).unwrap_or(HeaderValue::from_static("\"\""))),
    ];
    if fresh {
        return (StatusCode::NOT_MODIFIED, headers).into_response();
    }
    if method == Method::HEAD {
        return (headers, ()).into_response();
    }
    (headers, bytes.into_owned()).into_response()
}

/// 目录托管。ServeDir 只发 Last-Modified 不发 Cache-Control，浏览器会按启发式缓存把宿主页
/// 存上一阵——换了前端（比如 React 换成浏览器版）之后还拿着旧页面。一律 `no-cache`：
/// 每次都带着 Last-Modified 回来问一声，没变就是 304，不多传字节
pub fn service(dir: PathBuf) -> SetResponseHeader<ServeDir<axum::routing::MethodRouter>, HeaderValue> {
    let index = dir.join("index.html");
    let serve = ServeDir::new(dir).append_index_html_on_directories(true).fallback(axum::routing::any(
        move |req: Request| {
            let index = index.clone();
            async move { spa_fallback(&index, req).await }
        },
    ));
    SetResponseHeader::if_not_present(serve, CACHE_CONTROL, HeaderValue::from_static("no-cache"))
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
