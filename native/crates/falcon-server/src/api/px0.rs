//! `/px0/<项目 id>/*`：把请求反代到该项目的 px0（ADR 0017）。移植自 `packages/server/src/px0/routes.ts`。
//!
//! 鉴权是登录 cookie，由 require_login 统一挡（`/px0/` 与 `/api/` 同一口径）。请求体原样以流
//! 交给上游，不解析、不设上限；响应也按流转回来——SSE（`/api/stream`）要边收边发。
//!
//! 每个请求一条新连接（hyper 的 http1 握手直接跑在这条连接上）：远端的连接是一条 forwardOut
//! 通道，不适合进 keep-alive 池；本机回环建连的代价可以忽略。

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use falcon_core::px0::px0_base_path;
use futures::StreamExt as _;
use hyper_util::rt::TokioIo;

use super::AppState;
use super::error::ApiError;
use crate::px0::manager::{Px0Lease, Px0Stream};
use crate::px0::proxy::{downstream_response_headers, px0_status_page, upstream_request_headers};

pub fn router() -> Router<AppState> {
    Router::new().route("/px0/{project_id}", any(redirect)).route("/px0/{project_id}/{*rest}", any(proxy)).route(
        "/px0/{project_id}/",
        any(|state: State<AppState>, Path(project_id): Path<String>, req: Request| async move {
            handle(state.0, project_id, String::new(), req).await
        }),
    )
}

/// px0 自己也会把不带尾斜杠的前缀重定向过去，但没在跑时轮不到它
async fn redirect(Path(project_id): Path<String>) -> Response {
    let location = px0_base_path(&project_id);
    (StatusCode::MOVED_PERMANENTLY, [(LOCATION, HeaderValue::from_str(&location).unwrap_or(HeaderValue::from_static("/")))])
        .into_response()
}

async fn proxy(State(state): State<AppState>, Path((project_id, rest)): Path<(String, String)>, req: Request) -> Response {
    handle(state, project_id, rest, req).await
}

/// 引擎里定下来的去向
enum Route {
    /// 已经在跑：拿着租约去连
    Proxy(Px0Lease, Option<Px0Stream>),
    /// 状态页（启动中 / 下载中 / 失败）
    Page(String),
    /// 远端实例的 forwardOut 通道开不出来
    ConnectFailed(String),
    Unavailable,
}

async fn handle(state: AppState, project_id: String, rest: String, req: Request) -> Response {
    let Some(row) = state.db.get_project(&project_id) else {
        return ApiError::not_found("项目不存在").into_response();
    };
    // 存档的附属项目到期连目录一起删，不能再往里开 px0（存档时已经停掉了）
    if row.worktree_archived_at.is_some() {
        return ApiError::conflict("项目已存档，请先恢复再用 px0 打开").into_response();
    }
    // 只有入口页会拉起 px0：静态资源、API、SSE 在它没跑时一律 503，免得页面上
    // 残留的轮询把一个刚被空闲回收的实例又叫起来
    let is_entry = rest.is_empty() && (req.method() == Method::GET || req.method() == Method::HEAD);
    let retry = req.uri().query().is_some_and(|q| q.split('&').any(|kv| kv == "retry=1"));

    let route = state
        .engine
        .call(move |engine| async move {
            let px0 = &engine.px0;
            let lease = match px0.acquire(&project_id) {
                Some(lease) => Some(lease),
                None if !is_entry => return Route::Unavailable,
                None => {
                    if let Some(page) = px0.open(&row, retry) {
                        return Route::Page(px0_status_page(&page, &px0_base_path(&project_id), &row.name));
                    }
                    // open 说已经在跑（并发的另一个请求刚把它拉起来）：再拿一次
                    px0.acquire(&project_id)
                }
            };
            let Some(lease) = lease else { return Route::Unavailable };
            // 本机实例在处理器里直接连（不占引擎）；远端走 forwardOut，得在引擎里开通道
            if lease.local_addr().is_some() {
                return Route::Proxy(lease, None);
            }
            match px0.connect(&lease).await {
                Ok(stream) => Route::Proxy(lease, Some(stream)),
                Err(e) => Route::ConnectFailed(e.to_string()),
            }
        })
        .await;
    let route = match route {
        Ok(r) => r,
        Err(e) => return ApiError::internal(e).into_response(),
    };
    match route {
        Route::Unavailable => ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "px0 未在运行").into_response(),
        Route::ConnectFailed(why) => upstream_failed(&why),
        Route::Page(html) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
                (CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            html,
        )
            .into_response(),
        Route::Proxy(lease, stream) => forward(lease, stream, req).await,
    }
}

fn upstream_failed(why: &str) -> Response {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("px0 连不上：{why}")).into_response()
}

/// 转发一个请求。租约跟着响应体走：响应体读完或客户端断开（关标签页、EventSource 重连）
/// 时随之释放，px0 的空闲回收据此计数
async fn forward(lease: Px0Lease, stream: Option<Px0Stream>, req: Request) -> Response {
    let stream: Px0Stream = match stream {
        Some(s) => s,
        None => {
            let Some(addr) = lease.local_addr() else { return upstream_failed("没有可连的地址") };
            match tokio::net::TcpStream::connect(addr).await {
                Ok(s) => Box::new(s),
                Err(e) => return upstream_failed(&e.to_string()),
            }
        }
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(pair) => pair,
        Err(e) => return upstream_failed(&e.to_string()),
    };
    // 连接驱动：响应体流完（或被丢下）后它自己结束
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let (parts, body) = req.into_parts();
    let path = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    let mut upstream = hyper::Request::builder().method(parts.method).uri(path);
    if let Some(h) = upstream.headers_mut() {
        *h = upstream_request_headers(&parts.headers, "127.0.0.1");
    }
    let upstream = match upstream.body(body) {
        Ok(r) => r,
        Err(e) => return upstream_failed(&e.to_string()),
    };
    let res = match sender.send_request(upstream).await {
        Ok(res) => res,
        Err(e) => return upstream_failed(&e.to_string()),
    };
    let (parts, incoming) = res.into_parts();
    // 租约捕进流里：流被丢下（响应结束 / 客户端断开）时一起释放
    let body = Body::new(incoming).into_data_stream().map(move |chunk| {
        let _keep = &lease;
        chunk
    });
    let mut out = Response::new(Body::from_stream(body));
    *out.status_mut() = parts.status;
    *out.headers_mut() = downstream_response_headers(&parts.headers);
    out
}
