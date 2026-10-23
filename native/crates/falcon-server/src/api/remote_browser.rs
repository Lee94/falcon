//! 远端浏览器入口与反代。
//!
//! 浏览器画面走 WebRTC，不经这条 HTTP 通道；这里只给 Neko 前端的静态资源、登录 cookie
//! 与 WebRTC 信令 WebSocket 做一次同源的、受 Falcon 登录态保护的转发。远端固定连它自己的
//! `127.0.0.1:<port>`：配置文件只允许选择宿主机与端口，不能把 Falcon 变成任意 HTTP 代理。

use std::collections::{BTreeMap, HashSet};
use std::net::Ipv4Addr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use hyper::StatusCode as HyperStatus;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use super::error::ApiError;
use super::{AppState, encode_uri_component};
use crate::db::{Db, SshHostRow};

const CONFIG_FILE: &str = "remote-browser.json";
const CONFIG_LIMIT: u64 = 64 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Neko v3 的默认会话 cookie 名；部署文档固定配成这个值。它只在 /remote-browser/<id>/
/// 这个路径下回放，Falcon 自己的 falcon_token 永不发给上游。
const UPSTREAM_COOKIE: &str = "NEKO_SESSION";
const PREFIX: &str = "/remote-browser/";
const PAGE: &str = include_str!("remote_browser_page.html");

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/remote-browser", get(|| async { Redirect::to("/remote-browser/open") }))
        .route("/remote-browser/open", get(open))
        .route("/remote-browser/{target_id}", get(target_no_slash))
        .route("/remote-browser/{target_id}/{*rest}", any(proxy))
}

async fn target_no_slash(Path(target_id): Path<String>) -> Response {
    Redirect::permanent(&format!("{PREFIX}{}/", encode_uri_component(&target_id))).into_response()
}

/// 打开哪个项目就固定哪台机器；窗口开住以后工作区焦点再变也不会把会话换走。
async fn open(State(state): State<AppState>, Query(query): Query<BTreeMap<String, String>>, req: Request) -> Response {
    if let Some(res) = login_required(&state, req.headers(), false) {
        return res;
    }
    let project_id = query.get("project").map(String::as_str).unwrap_or("");
    let config = match load_config(&state.config.data_dir).await {
        Ok(config) => config,
        Err(issue) => return issue_page("读取远端浏览器配置失败", &issue),
    };
    match resolve_for_open(&state.db, &config, project_id) {
        Ok(target) => {
            // `?usr=&pwd=` 是 Neko 前端的邀请式登录参数：配合部署文档里的 noauth provider，
            // 打开窗口后不再出现第二层登录；若管理员坚持 multiuser，会落到它的密码表单。
            Redirect::to(&format!("{PREFIX}{}/?usr=Falcon&pwd=falcon", encode_uri_component(&target.id))).into_response()
        }
        Err(issue) => issue_page("远端浏览器不可用", &issue),
    }
}

async fn proxy(State(state): State<AppState>, Path((target_id, _rest)): Path<(String, String)>, mut req: Request) -> Response {
    let upgrade = is_websocket_upgrade(req.headers());
    if let Some(res) = login_required(&state, req.headers(), upgrade) {
        return res;
    }
    if let Err(issue) = check_origin(req.method(), req.headers(), upgrade) {
        return ApiError::new(StatusCode::FORBIDDEN, issue).into_response();
    }
    let downstream_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));

    let config = match load_config(&state.config.data_dir).await {
        Ok(config) => config,
        Err(issue) => return proxy_issue(req.headers(), upgrade, "读取远端浏览器配置失败", &issue),
    };
    let Some(target) = config.targets.iter().find(|t| t.id == target_id).cloned() else {
        return proxy_issue(req.headers(), upgrade, "远端浏览器不可用", "没有这台机器的远端浏览器目标");
    };
    let host = match target.host_row(&state.db) {
        Ok(host) => host,
        Err(issue) => return proxy_issue(req.headers(), upgrade, "远端浏览器不可用", &issue),
    };
    // 用原始 path_and_query：Path 提取器已经按路由拆过，百分号编码不能再拼一遍
    let path = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());

    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, connect(&state, &target, host)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(issue)) => return proxy_issue(req.headers(), upgrade, "远端浏览器连不上", &issue),
        Err(_) => return proxy_issue(req.headers(), upgrade, "远端浏览器连不上", "连接超时"),
    };
    forward(req, stream, &target, path, downstream_upgrade).await
}

fn login_required(state: &AppState, headers: &HeaderMap, websocket: bool) -> Option<Response> {
    if state.authenticated(headers) {
        return None;
    }
    if !websocket && accepts_html(headers) {
        return Some(page(
            "登录后打开远端浏览器",
            "远端浏览器在对应主机上运行。这里先验证 Falcon 访问密码，登录成功后会回到刚才的地址。",
            true,
        ));
    }
    Some(ApiError::unauthorized().into_response())
}

fn proxy_issue(headers: &HeaderMap, websocket: bool, title: &str, message: &str) -> Response {
    if !websocket && accepts_html(headers) {
        return issue_page(title, message);
    }
    ApiError::new(StatusCode::BAD_GATEWAY, format!("{title}：{message}")).into_response()
}

fn issue_page(title: &str, message: &str) -> Response {
    page(title, message, false)
}

fn page(title: &str, message: &str, login: bool) -> Response {
    let html = PAGE
        .replace("{{TITLE}}", &html_escape(title))
        .replace("{{MESSAGE}}", &html_escape(message))
        .replace("{{LOGIN}}", if login { "true" } else { "false" });
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        html,
    )
        .into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn accepts_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/html"))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    targets: Vec<BrowserTarget>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct BrowserTarget {
    id: String,
    #[serde(rename = "hostId")]
    host_id: Option<String>,
    port: u16,
}

impl BrowserTarget {
    fn host_row(&self, db: &Db) -> Result<Option<SshHostRow>, String> {
        match self.host_id.as_deref() {
            None => Ok(None),
            Some(id) => db.get_host(id).map(Some).ok_or_else(|| "配置指向的 SSH 主机已删除".to_string()),
        }
    }
}

async fn load_config(data_dir: &std::path::Path) -> Result<ConfigFile, String> {
    let path = data_dir.join(CONFIG_FILE);
    let meta = tokio::fs::metadata(&path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("还没有配置。请在数据目录创建 {CONFIG_FILE}，并至少配置一个远端浏览器目标")
        } else {
            format!("读取 {CONFIG_FILE} 失败：{e}")
        }
    })?;
    if meta.len() > CONFIG_LIMIT {
        return Err(format!("{CONFIG_FILE} 超过 64 KB，不读取"));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|e| format!("读取 {CONFIG_FILE} 失败：{e}"))?;
    let mut config: ConfigFile = serde_json::from_slice(&bytes).map_err(|e| format!("{CONFIG_FILE} 不是合法配置：{e}"))?;
    validate_config(&mut config)?;
    Ok(config)
}

fn valid_target_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.len() <= 48
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
        && id != "open"
}

fn validate_config(config: &mut ConfigFile) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut local = false;
    let mut hosts = HashSet::new();
    for target in &mut config.targets {
        if !valid_target_id(&target.id) {
            return Err(format!("目标 id {:?} 不合法：小写字母 / 数字开头，可含 - 和 _，最长 48", target.id));
        }
        if !ids.insert(target.id.clone()) {
            return Err(format!("目标 id {:?} 重复", target.id));
        }
        if target.port == 0 {
            return Err(format!("目标 {:?} 的端口不能是 0", target.id));
        }
        match target.host_id.as_deref() {
            Some(host) if !hosts.insert(host.to_string()) => {
                return Err(format!("SSH 主机 {host} 配了多个远端浏览器目标；每台机器只留一个"))
            }
            None if std::mem::replace(&mut local, true) => return Err("本机只留一个远端浏览器目标".to_string()),
            _ => {}
        }
    }
    Ok(())
}

fn resolve_for_open(db: &Db, config: &ConfigFile, project_id: &str) -> Result<BrowserTarget, String> {
    let local = || {
        config
            .targets
            .iter()
            .find(|t| t.host_id.is_none())
            .cloned()
            .ok_or_else(|| "配置里没有本机目标。给 remote-browser.json 加一条不带 hostId 的 target".to_string())
    };
    if project_id.is_empty() {
        return local();
    }
    let Some(project) = db.get_project(project_id) else {
        return Err("焦点项目已不存在".to_string());
    };
    if project.project_type != "ssh" {
        return local();
    }
    let Some(host_id) = project.host_id.as_deref() else {
        return Err("这个 SSH 项目没有绑定已保存主机；先把它绑定到设置里的主机，再给该主机配置远端浏览器".to_string());
    };
    config
        .targets
        .iter()
        .find(|t| t.host_id.as_deref() == Some(host_id))
        .cloned()
        .ok_or_else(|| "这台 SSH 主机还没有远端浏览器目标；给它对应的 hostId 加一条 target".to_string())
}

pub trait RemoteIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + ?Sized> RemoteIo for T {}
pub type RemoteStream = Box<dyn RemoteIo>;

async fn connect(state: &AppState, target: &BrowserTarget, host: Option<SshHostRow>) -> Result<RemoteStream, String> {
    match host {
        None => TcpStream::connect((Ipv4Addr::LOCALHOST, target.port))
            .await
            .map(|s| Box::new(s) as RemoteStream)
            .map_err(|e| format!("连本机 127.0.0.1:{} 失败：{e}", target.port)),
        Some(host) => {
            let port = target.port;
            let name = host.name.clone();
            state
                .engine
                .call(move |engine| async move {
                    let link = engine.sessions.get_host_link(&host);
                    let channel = link.forward_out("127.0.0.1", port).await.map_err(|e| format!("经 {name} 连接远端浏览器失败：{e:#}"))?;
                    Ok(Box::new(channel.into_stream()) as RemoteStream)
                })
                .await
                .map_err(|e| e.to_string())?
        }
    }
}

const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn latin1(v: &HeaderValue) -> String {
    v.as_bytes().iter().map(|&b| char::from(b)).collect()
}

fn joined(headers: &HeaderMap, name: HeaderName, sep: &str) -> Option<String> {
    let values: Vec<String> = headers.get_all(name).iter().map(latin1).collect();
    (!values.is_empty()).then(|| values.join(sep))
}

fn connection_tokens(headers: &HeaderMap) -> HashSet<String> {
    joined(headers, header::CONNECTION, ",")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
        && connection_tokens(headers).contains("upgrade")
}

fn host_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
}

fn origin_host(origin: &str) -> Option<String> {
    let url = url::Url::parse(origin).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn check_origin(method: &Method, headers: &HeaderMap, websocket: bool) -> Result<(), String> {
    let needs_origin = websocket || !matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS);
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return needs_origin.then(|| "跨源请求缺少 Origin".to_string()).map_or(Ok(()), Err);
    };
    let Some(host) = host_header(headers) else {
        return Err("请求缺少 Host 头".to_string());
    };
    match origin_host(origin) {
        Some(origin_host) if origin_host == host => Ok(()),
        _ => Err("跨源请求被拒绝".to_string()),
    }
}

fn upstream_cookie(headers: &HeaderMap) -> Option<String> {
    for value in headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()) {
        for pair in value.split(';') {
            let Some((name, val)) = pair.split_once('=') else { continue };
            if name.trim() == UPSTREAM_COOKIE {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(format!("{UPSTREAM_COOKIE}={val}"));
                }
            }
        }
    }
    None
}

fn upstream_headers(headers: &HeaderMap, upstream_host: &str, upgrade: bool) -> HeaderMap {
    let named = connection_tokens(headers);
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let k = name.as_str();
        if matches!(k, "host" | "cookie" | "authorization" | "referer" | "forwarded" | "x-real-ip" | "x-client-ip")
            || k.starts_with("x-forwarded-")
            || HOP_BY_HOP.contains(&k)
            || named.contains(k)
            || (upgrade && k.starts_with("sec-websocket-"))
        {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    if let Ok(v) = HeaderValue::from_str(upstream_host) {
        out.insert(header::HOST, v);
        // 上游看到的 Origin 与它自己的 Host 一致；Falcon 侧的跨源检查在这之前已经做过。
        if upgrade || headers.contains_key(header::ORIGIN) {
            out.insert(header::ORIGIN, HeaderValue::from_str(&format!("http://{upstream_host}")).unwrap());
        }
    }
    if let Some(cookie) = upstream_cookie(headers).and_then(|c| HeaderValue::from_str(&c).ok()) {
        out.insert(header::COOKIE, cookie);
    }
    if upgrade {
        for name in [
            header::UPGRADE,
            header::SEC_WEBSOCKET_KEY,
            header::SEC_WEBSOCKET_VERSION,
            header::SEC_WEBSOCKET_PROTOCOL,
            header::SEC_WEBSOCKET_EXTENSIONS,
        ] {
            for value in headers.get_all(&name) {
                out.append(name.clone(), value.clone());
            }
        }
        out.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    }
    out
}

fn scoped_set_cookie(value: &HeaderValue, target_id: &str) -> Option<HeaderValue> {
    let text = value.to_str().ok()?;
    let mut parts = text.split(';');
    let pair = parts.next()?.trim();
    let (name, cookie_value) = pair.split_once('=')?;
    if name.trim() != UPSTREAM_COOKIE {
        return None;
    }
    let mut out = format!("{UPSTREAM_COOKIE}={}; Path={PREFIX}{target_id}/; HttpOnly; SameSite=Lax", cookie_value.trim());
    for attr in parts {
        let attr = attr.trim();
        if attr.is_empty() {
            continue;
        }
        let key = attr.split('=').next().unwrap_or("").trim().to_ascii_lowercase();
        if matches!(key.as_str(), "expires" | "max-age") {
            out.push_str("; ");
            out.push_str(attr);
        }
    }
    HeaderValue::from_str(&out).ok()
}

fn downstream_headers(headers: &HeaderMap, target_id: &str, upgrade: bool) -> HeaderMap {
    let named = connection_tokens(headers);
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        let k = name.as_str();
        if k == "set-cookie" || k == "alt-svc" {
            continue;
        }
        if !upgrade && (HOP_BY_HOP.contains(&k) || named.contains(k)) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    for value in headers.get_all(header::SET_COOKIE).iter().filter_map(|v| scoped_set_cookie(v, target_id)) {
        out.append(header::SET_COOKIE, value);
    }
    out
}

async fn forward(
    req: Request,
    stream: RemoteStream,
    target: &BrowserTarget,
    path: String,
    downstream_upgrade: Option<hyper::upgrade::OnUpgrade>,
) -> Response {
    let (mut sender, conn) = match tokio::time::timeout(CONNECT_TIMEOUT, hyper::client::conn::http1::handshake(TokioIo::new(stream))).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => return upstream_failed(&e.to_string()),
        Err(_) => return upstream_failed("HTTP 握手超时"),
    };
    if downstream_upgrade.is_some() {
        // `Connection` 的普通 Future 遇到 101 会把升级标成 manual，
        // `hyper::upgrade::on(response)` 就再也拿不到流；交给 HTTP 层回填 Upgraded。
        tokio::spawn(conn.with_upgrades());
    } else {
        tokio::spawn(async move {
            let _ = conn.await;
        });
    }

    let (parts, body) = req.into_parts();
    let upstream_host = format!("127.0.0.1:{}", target.port);
    let mut upstream = hyper::Request::builder().method(parts.method).uri(path);
    if let Some(h) = upstream.headers_mut() {
        *h = upstream_headers(&parts.headers, &upstream_host, downstream_upgrade.is_some());
    }
    let upstream = match upstream.body(body) {
        Ok(r) => r,
        Err(e) => return upstream_failed(&e.to_string()),
    };
    let mut res = match sender.send_request(upstream).await {
        Ok(res) => res,
        Err(e) => return upstream_failed(&e.to_string()),
    };

    if let Some(downstream) = downstream_upgrade
        && res.status() == HyperStatus::SWITCHING_PROTOCOLS
    {
        let upstream = hyper::upgrade::on(&mut res);
        tokio::spawn(async move {
            match (downstream.await, upstream.await) {
                (Ok(down), Ok(up)) => {
                    let mut down = TokioIo::new(down);
                    let mut up = TokioIo::new(up);
                    if let Err(e) = tokio::io::copy_bidirectional(&mut down, &mut up).await {
                        log::debug!("远端浏览器 WebSocket 隧道结束：{e}");
                    }
                }
                (down, up) => {
                    if let Err(e) = down {
                        log::warn!("远端浏览器 WebSocket 下游升级失败：{e}");
                    }
                    if let Err(e) = up {
                        log::warn!("远端浏览器 WebSocket 上游升级失败：{e}");
                    }
                }
            }
        });
    }

    let (parts, incoming) = res.into_parts();
    let switching = parts.status == HyperStatus::SWITCHING_PROTOCOLS;
    let mut out = if switching { Response::new(Body::empty()) } else { Response::new(Body::new(incoming)) };
    *out.status_mut() = parts.status;
    *out.headers_mut() = downstream_headers(&parts.headers, &target.id, switching);
    out
}

fn upstream_failed(why: &str) -> Response {
    ApiError::new(StatusCode::BAD_GATEWAY, format!("远端浏览器连不上：{why}")).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{TestApp, body_bytes, header};
    use crate::db::{ProjectRow, SshHostRow};
    use axum::http::Request;
    use serde_json::json;
    use std::io::Write as _;

    fn target(id: &str, host_id: Option<&str>, port: u16) -> BrowserTarget {
        BrowserTarget { id: id.into(), host_id: host_id.map(Into::into), port }
    }

    #[test]
    fn config_validation_rejects_duplicates_and_bad_ids() {
        let mut config = ConfigFile { targets: vec![target("local", None, 8080), target("x", Some("h1"), 8080)] };
        validate_config(&mut config).unwrap();
        let mut dup_id = ConfigFile { targets: vec![target("a", None, 1), target("a", Some("h"), 2)] };
        assert!(validate_config(&mut dup_id).unwrap_err().contains("重复"));
        let mut dup_host = ConfigFile { targets: vec![target("a", Some("h"), 1), target("b", Some("h"), 2)] };
        assert!(validate_config(&mut dup_host).unwrap_err().contains("多个"));
        let mut bad = ConfigFile { targets: vec![target("Open", None, 1)] };
        assert!(validate_config(&mut bad).is_err());
        let mut zero = ConfigFile { targets: vec![target("local", None, 0)] };
        assert!(validate_config(&mut zero).is_err());
    }

    #[test]
    fn open_resolution_does_not_fall_back_to_local() {
        let app = TestApp::new();
        app.state.db.insert_host(&SshHostRow {
            id: "h1".into(),
            name: "dev".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth_method: "agent".into(),
            created_at: 1,
            ..Default::default()
        });
        app.state.db.insert_project(&ProjectRow {
            id: "p".into(),
            project_type: "ssh".into(),
            host_id: Some("h1".into()),
            created_at: 1,
            ..Default::default()
        });
        let config = ConfigFile { targets: vec![target("local", None, 8080)] };
        let err = resolve_for_open(&app.state.db, &config, "p").unwrap_err();
        assert!(err.contains("这台 SSH 主机还没有远端浏览器目标"), "{err}");
        assert_eq!(resolve_for_open(&app.state.db, &config, "").unwrap().id, "local");
        let bound = ConfigFile { targets: vec![target("dev", Some("h1"), 8081)] };
        assert_eq!(resolve_for_open(&app.state.db, &bound, "p").unwrap().id, "dev");
    }

    #[test]
    fn cookies_are_scoped_and_falcon_token_is_not_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("falcon_token=secret; NEKO_SESSION=remote; theme=dark"));
        assert_eq!(upstream_cookie(&headers).as_deref(), Some("NEKO_SESSION=remote"));

        let set = HeaderValue::from_static("NEKO_SESSION=abc; Path=/; Max-Age=3600; Secure; Domain=example.com");
        let scoped = scoped_set_cookie(&set, "dev").unwrap().to_str().unwrap().to_string();
        assert!(scoped.contains("Path=/remote-browser/dev/"), "{scoped}");
        assert!(scoped.contains("Max-Age=3600"), "{scoped}");
        assert!(!scoped.contains("Domain="), "{scoped}");
        assert!(!scoped.contains("Secure"), "{scoped}");
    }

    #[test]
    fn cross_origin_ws_is_rejected_before_proxying() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("falcon.test"));
        headers.insert(header::ORIGIN, HeaderValue::from_static("https://evil.test"));
        assert!(check_origin(&Method::GET, &headers, true).is_err());
        headers.insert(header::ORIGIN, HeaderValue::from_static("https://falcon.test"));
        assert!(check_origin(&Method::GET, &headers, true).is_ok());
    }

    #[tokio::test]
    async fn entry_shows_standalone_login_then_redirects_to_target() {
        let app = TestApp::new();
        app.state.auth.set_password("secret", None);
        super::super::test_support::write(
            &app.data_dir.join(CONFIG_FILE),
            serde_json::to_vec(&json!({ "targets": [{ "id": "local", "port": 8080 }] })).unwrap(),
        );
        let res = app
            .send(Request::get("/remote-browser/open").header(header::ACCEPT, "text/html").body(Body::empty()).unwrap())
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let html = String::from_utf8(body_bytes(res).await).unwrap();
        assert!(html.contains("登录并继续"), "{html}");

        let login = app.post_json("/api/auth/login", json!({ "password": "secret" })).await;
        let cookie = header(&login, "set-cookie").split(';').next().unwrap().to_string();
        let res = app
            .send(Request::get("/remote-browser/open").header(header::COOKIE, cookie).body(Body::empty()).unwrap())
            .await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(header(&res, "location"), "/remote-browser/local/?usr=Falcon&pwd=falcon");
    }

    #[tokio::test]
    async fn http_proxy_filters_cookies_and_scopes_neko_cookie() {
        let app = TestApp::new();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = captured.clone();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut buf = vec![0; 8192];
            let n = socket.peek(&mut buf).unwrap();
            seen.lock().unwrap().push_str(&String::from_utf8_lossy(&buf[..n]));
            let response = "HTTP/1.1 200 OK
Content-Type: text/plain
Set-Cookie: NEKO_SESSION=remote; Path=/; Max-Age=60
Content-Length: 2

ok";
            socket.write_all(response.as_bytes()).unwrap();
        });
        super::super::test_support::write(
            &app.data_dir.join(CONFIG_FILE),
            serde_json::to_vec(&json!({ "targets": [{ "id": "local", "port": port }] })).unwrap(),
        );
        let res = app
            .send(
                Request::get("/remote-browser/local/assets/app.js")
                    .header(header::COOKIE, "falcon_token=secret; NEKO_SESSION=remote")
                    .header(header::CONNECTION, "keep-alive, x-neko-secret")
                    .header("x-neko-secret", "1")
                    .header(header::FORWARDED, "for=203.0.113.7")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(res.status(), StatusCode::OK);
        let set_cookie = res.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
        assert_eq!(body_bytes(res).await, b"ok");
        assert!(set_cookie.contains("Path=/remote-browser/local/"), "{set_cookie}");
        let seen = captured.lock().unwrap().clone();
        assert!(seen.contains("GET /remote-browser/local/assets/app.js HTTP/1.1"), "{seen}");
        assert!(seen.contains("cookie: NEKO_SESSION=remote"), "{seen}");
        assert!(!seen.contains("falcon_token"), "{seen}");
        assert!(!seen.contains("x-neko-secret"), "{seen}");
        assert!(!seen.contains("forwarded"), "{seen}");
    }

    #[tokio::test]
    async fn websocket_proxy_relays_frames_and_filters_falcon_cookie() {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let app = TestApp::new();
        let upstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream_port = upstream_listener.local_addr().unwrap().port();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = captured.clone();
        let upstream = tokio::spawn(async move {
            let (socket, _) = upstream_listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(
                socket,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    seen.lock().unwrap().push_str(&format!("{:?}", request.headers()));
                    Ok(response)
                },
            )
            .await
            .unwrap();
            if let Some(Ok(message)) = ws.next().await {
                ws.send(message).await.unwrap();
                // 让 echo 先过隧道；立刻 drop 会让客户端把 TCP 复位当成握手失败
                let _ = ws.next().await;
            }
        });
        super::super::test_support::write(
            &app.data_dir.join(CONFIG_FILE),
            serde_json::to_vec(&json!({ "targets": [{ "id": "local", "port": upstream_port }] })).unwrap(),
        );

        let downstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let downstream_port = downstream_listener.local_addr().unwrap().port();
        let router = app.router.clone();
        let _server = tokio::spawn(async move {
            axum::serve(downstream_listener, router).await.unwrap();
        });

        let url = format!("ws://127.0.0.1:{downstream_port}/remote-browser/local/api/ws");
        let mut request = url.into_client_request().unwrap();
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(&format!("http://127.0.0.1:{downstream_port}")).unwrap(),
        );
        request
            .headers_mut()
            .insert(header::COOKIE, HeaderValue::from_static("falcon_token=secret; NEKO_SESSION=remote"));
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        ws.send(Message::Text("signal".into())).await.unwrap();
        let next = ws.next().await.unwrap();
        let seen = captured.lock().unwrap().clone();
        assert!(next.is_ok(), "{next:?}; upstream finished={}; {seen}", upstream.is_finished());
        assert_eq!(next.unwrap(), Message::Text("signal".into()));

        assert!(seen.contains("NEKO_SESSION=remote"), "{seen}");
        assert!(seen.contains(&format!("127.0.0.1:{upstream_port}")), "{seen}");
        assert!(!seen.contains("falcon_token"), "{seen}");
    }
}
