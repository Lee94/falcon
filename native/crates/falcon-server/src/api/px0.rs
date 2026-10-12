//! `/px0/<项目 id>/*`：把请求反代到该项目的 px0（ADR 0017）。移植自 `packages/server/src/px0/routes.ts`。
//!
//! 鉴权是登录 cookie，由 require_login 统一挡（`/px0/` 与 `/api/` 同一口径）。请求体原样以流
//! 交给上游，不解析、不设上限；响应也按流转回来——SSE（`/api/stream`）要边收边发。
//!
//! 每个请求一条新连接（hyper 的 http1 握手直接跑在这条连接上）：远端的连接是一条 forwardOut
//! 通道，不适合进 keep-alive 池；本机回环建连的代价可以忽略。
//!
//! 多仓库项目（决定九）：px0 只认一个仓库，按成员各起一个，挂在 `/px0/<项目 id>/<成员序号>/`；
//! `/px0/<项目 id>/` 本身是选成员的页面。项目是不是多仓库看库里的行，所以普通项目的路径里
//! 第一段长得像数字也不会被误认成成员。

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use falcon_core::px0::{px0_base_path, px0_member_base_path};
use futures::StreamExt as _;
use hyper_util::rt::TokioIo;

use super::AppState;
use super::error::ApiError;
use crate::db::Db;
use crate::px0::manager::{Px0Key, Px0Lease, Px0Stream};
use crate::px0::proxy::{
    Px0MemberLink, downstream_response_headers, member_label, px0_member_page, px0_status_page, upstream_request_headers,
};

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
    redirect_to(StatusCode::MOVED_PERMANENTLY, &px0_base_path(&project_id))
}

fn redirect_to(status: StatusCode, location: &str) -> Response {
    (status, [(LOCATION, HeaderValue::from_str(location).unwrap_or(HeaderValue::from_static("/")))]).into_response()
}

/// 多仓库项目前缀之后的路径 `<成员序号>[/<px0 的路径>]` → (序号, 斜杠后的部分)。
/// 序号只认规范的十进制（不认 `01`、`+1`）：px0 只认它自己的 base path，换个写法的请求
/// 到它那里也是 404，不如在这里就挡下
fn split_member(rest: &str) -> Option<(usize, Option<&str>)> {
    let (seg, tail) = match rest.split_once('/') {
        Some((seg, tail)) => (seg, Some(tail)),
        None => (rest, None),
    };
    let n: usize = seg.parse().ok()?;
    (n.to_string() == seg).then_some((n, tail))
}

fn member_not_found() -> Response {
    ApiError::not_found("这个多仓库项目里没有这个成员").into_response()
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
    // 多仓库项目：前缀本身是选成员的页面，第一段是成员序号。成员 JSON 坏了的行照普通项目
    // 处理（与 to_project 降级成普通项目外观同一口径）
    let (key, rest, title) = match Db::parse_multi_repos(row.multi_repos.as_deref()) {
        None => (Px0Key::project(&project_id), rest.as_str(), row.name.clone()),
        Some(members) => {
            if rest.is_empty() {
                // 只有一个成员就不用选了
                if members.len() == 1 {
                    return redirect_to(StatusCode::FOUND, &px0_member_base_path(&project_id, 0));
                }
                let links: Vec<Px0MemberLink> = members
                    .iter()
                    .enumerate()
                    .map(|(i, m)| Px0MemberLink { href: px0_member_base_path(&project_id, i), dir: m.dir.clone() })
                    .collect();
                return html_page(px0_member_page(&row.name, &links));
            }
            match split_member(&rest) {
                Some((i, tail)) if i < members.len() => {
                    let Some(tail) = tail else {
                        return redirect_to(StatusCode::MOVED_PERMANENTLY, &px0_member_base_path(&project_id, i));
                    };
                    (Px0Key::member(&project_id, i), tail, format!("{} / {}", row.name, member_label(&members[i].dir)))
                }
                _ => return member_not_found(),
            }
        }
    };
    // 只有入口页会拉起 px0：静态资源、API、SSE 在它没跑时一律 503，免得页面上
    // 残留的轮询把一个刚被空闲回收的实例又叫起来
    let is_entry = rest.is_empty() && (req.method() == Method::GET || req.method() == Method::HEAD);
    let retry = req.uri().query().is_some_and(|q| q.split('&').any(|kv| kv == "retry=1"));

    let route = state
        .engine
        .call(move |engine| async move {
            let px0 = &engine.px0;
            let lease = match px0.acquire(&key) {
                Some(lease) => Some(lease),
                None if !is_entry => return Route::Unavailable,
                None => {
                    if let Some(page) = px0.open(&key, &row, retry) {
                        return Route::Page(px0_status_page(&page, &key.base_path(), &title));
                    }
                    // open 说已经在跑（并发的另一个请求刚把它拉起来）：再拿一次
                    px0.acquire(&key)
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
        Route::Page(html) => html_page(html),
        Route::Proxy(lease, stream) => forward(lease, stream, req).await,
    }
}

fn html_page(html: String) -> Response {
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        html,
    )
        .into_response()
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{TestApp, body_bytes, header};
    use super::split_member;
    use crate::db::ProjectRow;
    use axum::http::StatusCode;

    #[test]
    fn member_segment_is_a_canonical_index() {
        assert_eq!(split_member("0"), Some((0, None)));
        assert_eq!(split_member("1/"), Some((1, Some(""))));
        assert_eq!(split_member("12/api/meta"), Some((12, Some("api/meta"))));
        for bad in ["", "/", "01/", "+1/", "-1/", "a/", "api/stream", " 1/"] {
            assert_eq!(split_member(bad), None, "{bad:?}");
        }
    }

    fn multi(app: &TestApp, id: &str, dirs: &[&str]) {
        let members: Vec<serde_json::Value> = dirs.iter().map(|d| serde_json::json!({ "dir": d })).collect();
        app.state.db.insert_project(&ProjectRow {
            id: id.into(),
            name: "多仓库".into(),
            project_type: "local".into(),
            multi_repos: Some(serde_json::Value::Array(members).to_string()),
            created_at: 1,
            ..Default::default()
        });
    }

    #[tokio::test]
    async fn multi_repo_entry_lists_members_and_routes_by_index() {
        let app = TestApp::new();
        multi(&app, "m1", &["/srv/web", "/srv/api"]);

        // 前缀本身是选成员的页面，不拉起 px0
        let res = app.get("/px0/m1/").await;
        assert_eq!(res.status(), StatusCode::OK);
        let html = String::from_utf8(body_bytes(res).await).unwrap();
        assert!(html.contains(r#"<a href="/px0/m1/0/">web</a>"#), "{html}");
        assert!(html.contains(r#"<a href="/px0/m1/1/">api</a>"#), "{html}");

        // 成员前缀补斜杠；越界、不规范的序号、多仓库项目前缀下的 px0 路径都是 404
        let res = app.get("/px0/m1/1").await;
        assert_eq!(res.status(), StatusCode::MOVED_PERMANENTLY);
        assert_eq!(header(&res, "location"), "/px0/m1/1/");
        for uri in ["/px0/m1/2/", "/px0/m1/01/", "/px0/m1/api/stream"] {
            assert_eq!(app.get(uri).await.status(), StatusCode::NOT_FOUND, "{uri}");
        }
        // 成员的 px0 没在跑：入口页之外一律 503，不会被拉起来
        assert_eq!(app.get("/px0/m1/0/api/meta").await.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn single_member_skips_the_chooser_and_plain_projects_ignore_digits() {
        let app = TestApp::new();
        multi(&app, "s1", &["/srv/only"]);
        let res = app.get("/px0/s1/").await;
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(header(&res, "location"), "/px0/s1/0/");

        // 普通项目路径里的数字段原样当 px0 的路径（没在跑 → 503），不当成员
        app.local_project("p1");
        assert_eq!(app.get("/px0/p1/0/api").await.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
