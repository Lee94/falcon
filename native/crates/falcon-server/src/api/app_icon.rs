//! 应用图标的路由（ADR 0018，TS 的 `registerAppIconRoutes`）：选择存在 settings 表，
//! 自定义图片落在 `<dataDir>/app-icon/custom.png`。
//!
//! 公开（不要登录）的只有「取图」这几条：登录页的标签页图标也得是对的，浏览器取它时不带
//! 登录态（Node 版路由上的 `config.publicAsset`）——它们挂在 [`public_router`] 上。
//! PWA 的清单与 iOS 主屏幕图标跳转 2026-10-11 随 PWA 删掉了。
//! 改选择、上传、删除照常要登录（[`router`]）。

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use falcon_proto::{AppIconState, PASTE_IMAGE_MAX_BYTES};

use super::AppState;
use super::auth_routes::LenientJson;
use super::body::{Media, body_too_large, drain_body, media_of, query_param, unsupported_media_type};
use super::error::{ApiError, ApiResult};
use crate::app_icon::{
    AppIcons, check_custom_icon, custom_icon_cache_control, custom_icon_reject_status, favicon_url,
};
use crate::db::Db;

/// 要登录的：取 / 改选择、上传 / 删除自定义图
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/app-icon", get(get_state).put(select))
        .route("/api/app-icon/custom", axum::routing::put(put_custom).delete(delete_custom))
}

/// 不要登录的取图路由
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/api/app-icon/custom.png", get(custom_png))
        .route("/api/app-icon/favicon", get(favicon))
}

fn icons(state: &AppState) -> AppIcons<std::sync::Arc<Db>> {
    AppIcons::new(state.db.clone(), &state.config.data_dir)
}

/// 文件读写挪到阻塞线程池（settings 的 SQLite 调用与别的路由一样直接做）
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> std::io::Result<T> + Send + 'static) -> ApiResult<T> {
    match tokio::task::spawn_blocking(f).await {
        Ok(res) => res.map_err(ApiError::internal),
        Err(e) => Err(ApiError::internal(e)),
    }
}

async fn get_state(State(state): State<AppState>) -> Json<AppIconState> {
    Json(icons(&state).state())
}

async fn select(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<AppIconState>> {
    icons(&state).select(body.0.get("selected")).map(Json).ok_or_else(|| ApiError::bad_request("没有这个图标"))
}

/// 自定义图：客户端已经规整成 512 的 PNG（服务端不解码图片，只看签名与 IHDR）。
/// 上传即选中
async fn put_custom(State(state): State<AppState>, headers: HeaderMap, body: Body) -> ApiResult<Json<AppIconState>> {
    // Fastify 先按 Content-Type 收请求体：image/* 攒成 Buffer（上限同粘贴图片），
    // 别的类型不是 Buffer（JSON 对象 / 字符串 / 流），认不出的类型 415
    let buf = match media_of(&headers) {
        Media::Unsupported => {
            drain_body(body);
            return Err(unsupported_media_type());
        }
        Media::Image => {
            axum::body::to_bytes(body, PASTE_IMAGE_MAX_BYTES as usize).await.map_err(|_| body_too_large())?
        }
        Media::None | Media::Json | Media::Text | Media::OctetStream => {
            drain_body(body);
            return Err(ApiError::bad_request("请求体必须是 image/png"));
        }
    };
    if let Some(problem) = check_custom_icon(&buf) {
        let status = StatusCode::from_u16(custom_icon_reject_status(buf.len())).unwrap_or(StatusCode::BAD_REQUEST);
        return Err(ApiError::new(status, problem));
    }
    let icons = icons(&state);
    blocking(move || icons.save_custom(&buf)).await.map(Json)
}

async fn delete_custom(State(state): State<AppState>) -> ApiResult<Json<AppIconState>> {
    let icons = icons(&state);
    blocking(move || icons.remove_custom()).await.map(Json)
}

/// 地址里的 v 是内容哈希：对上了就能永久缓存；对不上（旧页面拿着旧地址）给当前这张、不缓存
async fn custom_png(State(state): State<AppState>, uri: Uri) -> ApiResult<Response> {
    let icons = icons(&state);
    let Some(file) = icons.custom_file() else {
        return Err(ApiError::not_found("没有自定义图标"));
    };
    let v = query_param(&uri, "v");
    let cache = custom_icon_cache_control(v.as_deref(), icons.state().custom.as_deref());
    let bytes = tokio::fs::read(&file).await.map_err(ApiError::internal)?;
    Ok(([(CACHE_CONTROL, cache), (CONTENT_TYPE, "image/png")], bytes).into_response())
}

/// index.html 的 `<link rel="icon">` 指向这条：服务端按当前选择跳过去，页面不用等 JS 就是
/// 对的图标。跳转本身不缓存，换了图标下次加载就换。
/// 302 同 Fastify 的 `reply.redirect` 默认（axum 的 `Redirect::to` 是 303）
async fn favicon(State(state): State<AppState>) -> Response {
    let target = favicon_url(&icons(&state).state());
    (StatusCode::FOUND, [(CACHE_CONTROL, "no-cache".to_string()), (LOCATION, target)]).into_response()
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{TestApp, body_bytes, header, json_of};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;

    /// 只有签名 + IHDR 头的"PNG"：服务端只看这两样
    fn fake_png(edge: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 33];
        buf[..8].copy_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
        buf[8..12].copy_from_slice(&13u32.to_be_bytes());
        buf[12..16].copy_from_slice(b"IHDR");
        buf[16..20].copy_from_slice(&edge.to_be_bytes());
        buf[20..24].copy_from_slice(&edge.to_be_bytes());
        buf
    }

    fn put_custom(content_type: &str, body: Vec<u8>) -> Request<Body> {
        Request::put("/api/app-icon/custom")
            .header("content-type", content_type)
            .header("content-length", body.len().to_string())
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn select_upload_serve_and_remove() {
        let app = TestApp::new();
        let (status, body) = json_of(app.get("/api/app-icon").await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "selected": "emberwing", "custom": null })));

        let req = Request::put("/api/app-icon")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"selected":"flash"}"#))
            .unwrap();
        let (status, body) = json_of(app.send(req).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "selected": "flash", "custom": null })));
        let req = Request::put("/api/app-icon").body(Body::from(r#"{"selected":"custom"}"#)).unwrap();
        let (status, body) = json_of(app.send(req).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "没有这个图标" })));

        // 拒收：不是 PNG、非正方形、不是 image/*
        let (status, body) = json_of(app.send(put_custom("image/png", b"GIF89a".to_vec())).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "只收 PNG" })));
        let (status, body) = json_of(app.send(put_custom("application/octet-stream", fake_png(512))).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "请求体必须是 image/png" })));
        let (status, body) = json_of(app.send(put_custom("video/mp4", fake_png(512))).await).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(body["code"], "FST_ERR_CTP_INVALID_MEDIA_TYPE");
        let mut huge = fake_png(512);
        huge.resize(crate::app_icon::CUSTOM_ICON_MAX_BYTES + 1, 0);
        let (status, body) = json_of(app.send(put_custom("image/png", huge)).await).await;
        assert_eq!((status, body), (StatusCode::PAYLOAD_TOO_LARGE, json!({ "error": "图片太大（上限 2MB）" })));

        let png = fake_png(512);
        let (status, body) = json_of(app.send(put_custom("image/png", png.clone())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["selected"], "custom");
        let version = body["custom"].as_str().unwrap().to_string();
        assert_eq!(version.len(), 16);
        assert_eq!(std::fs::read(app.data_dir.join("app-icon").join("custom.png")).unwrap(), png);
        // 临时文件没留下
        assert_eq!(std::fs::read_dir(app.data_dir.join("app-icon")).unwrap().count(), 1);

        // 取图：版本对得上就永久缓存，对不上不缓存
        let res = app.get(&format!("/api/app-icon/custom.png?v={version}")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header(&res, "content-type"), "image/png");
        assert_eq!(header(&res, "cache-control"), "public, max-age=31536000, immutable");
        assert_eq!(body_bytes(res).await, png);
        let res = app.get("/api/app-icon/custom.png?v=old").await;
        assert_eq!(header(&res, "cache-control"), "no-cache");

        // 标签页图标的跳转跟着选择走
        let res = app.get("/api/app-icon/favicon").await;
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(header(&res, "location"), format!("/api/app-icon/custom.png?v={version}"));
        assert_eq!(header(&res, "cache-control"), "no-cache");

        // 删掉回默认
        let req = Request::delete("/api/app-icon/custom").body(Body::empty()).unwrap();
        let (status, body) = json_of(app.send(req).await).await;
        assert_eq!((status, body), (StatusCode::OK, json!({ "selected": "emberwing", "custom": null })));
        let (status, body) = json_of(app.get("/api/app-icon/custom.png").await).await;
        assert_eq!((status, body), (StatusCode::NOT_FOUND, json!({ "error": "没有自定义图标" })));
        let res = app.get("/api/app-icon/favicon").await;
        assert_eq!(header(&res, "location"), "/icons/emberwing/icon-192.png");
    }

    #[tokio::test]
    async fn asset_routes_are_public_the_rest_need_login() {
        let app = TestApp::new();
        assert!(app.state.auth.set_password("secret1", None));
        assert_eq!(app.get("/api/app-icon").await.status(), StatusCode::UNAUTHORIZED);
        let req = Request::delete("/api/app-icon/custom").body(Body::empty()).unwrap();
        assert_eq!(app.send(req).await.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(app.get("/api/app-icon/favicon").await.status(), StatusCode::FOUND);
        assert_eq!(app.get("/api/app-icon/custom.png").await.status(), StatusCode::NOT_FOUND);
    }
}
