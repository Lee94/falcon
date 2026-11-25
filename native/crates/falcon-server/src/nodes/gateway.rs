use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{FromRequestParts as _, Request, State, ws::WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt as _, StreamExt as _};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex as AsyncMutex;

use super::{
    LOCAL_HEADER, PROTOCOL_VERSION, TOKEN_HEADER, bootstrap, map_resource_fields, migration,
    resource_id, split_resource, transport,
};
use crate::api::AppState;
use crate::api::error::ApiError;
use crate::db::SshHostRow;

const JSON_LIMIT: usize = 32 * 1024 * 1024;
const DISCOVERY_WAIT: Duration = Duration::from_millis(1200);
const LIST_TIMEOUT: Duration = Duration::from_secs(5);
type Stream = crate::px0::manager::Px0Stream;

#[derive(Clone)]
pub(super) struct Remote {
    pub(super) host: SshHostRow,
    pub(super) node_id: String,
    pub(super) port: u16,
    pub(super) token: String,
    pub(super) version: String,
    #[cfg(test)]
    pub(super) direct: Option<std::net::SocketAddr>,
}

#[derive(Default)]
struct Cache {
    nodes: HashMap<String, Remote>,
    errors: HashMap<String, String>,
}

#[derive(Default)]
pub struct Gateway {
    cache: Mutex<Cache>,
    locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl Gateway {
    fn lock(&self, key: &str) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(key.to_string())
            .or_default()
            .clone()
    }

    fn endpoint(&self, host: &SshHostRow) -> Option<Remote> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .nodes
            .get(&host.id)
            .filter(|node| node.host == *host)
            .cloned()
    }

    pub(super) async fn remote(&self, state: &AppState, host_id: &str) -> anyhow::Result<Remote> {
        let lock = self.lock(&format!("host:{host_id}"));
        let _turn = lock.lock().await;
        self.discover(state, host_id).await
    }

    /// 发现节点可能涉及首次部署。列表只短暂等端点，超时后由同一个任务继续发现。
    async fn list_endpoint(&self, state: &AppState, host: &SshHostRow) -> Option<Remote> {
        if let Some(remote) = self.endpoint(host) {
            return Some(remote);
        }
        let lock = self.lock(&format!("host:{}", host.id));
        if let Ok(turn) = lock.clone().try_lock_owned() {
            let state = state.clone();
            let host_id = host.id.clone();
            tokio::spawn(async move {
                let _turn = turn;
                let _ = state.nodes.discover(&state, &host_id).await;
            });
        }
        let _turn = tokio::time::timeout(DISCOVERY_WAIT, lock.lock()).await.ok()?;
        self.endpoint(host)
    }

    /// 调用者持有主机锁。这里仅建立访问权威节点的连接，旧网关记录不参与发现。
    async fn discover(&self, state: &AppState, host_id: &str) -> anyhow::Result<Remote> {
        let host = state
            .db
            .get_host(host_id)
            .ok_or_else(|| anyhow::anyhow!("远端主机不存在"))?;
        if let Some(node) = self.endpoint(&host) {
            return Ok(node);
        }
        match self.discover_uncached(state, host).await {
            Ok(remote) => Ok(remote),
            Err(error) => {
                self.failed(host_id, &format!("{error:#}"));
                Err(error)
            }
        }
    }

    async fn discover_uncached(&self, state: &AppState, host: SshHostRow) -> anyhow::Result<Remote> {
        let target = host.clone();
        let endpoint = state
            .engine
            .call(move |engine| async move {
                transport::ensure_remote(engine.sessions.get_host_link(&target)).await
            })
            .await??;
        anyhow::ensure!(
            endpoint.node_id != state.db.instance_id(),
            "这个 SSH 主机指向当前节点，请直接使用本机项目"
        );
        let remote = Remote {
            host,
            node_id: endpoint.node_id,
            port: endpoint.port,
            token: endpoint.token,
            version: endpoint.version,
            #[cfg(test)]
            direct: None,
        };
        let host_id = &remote.host.id;
        state.db.set_setting(&format!("nodes.route.{}", remote.node_id), host_id);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.errors.remove(host_id);
        cache.nodes.insert(host_id.clone(), remote.clone());
        Ok(remote)
    }

    fn host_for_node(&self, state: &AppState, node_id: &str) -> anyhow::Result<String> {
        let cached = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .nodes
            .values()
            .find(|n| n.node_id == node_id)
            .map(|n| n.host.id.clone());
        cached.or_else(|| state.db.get_setting(&format!("nodes.route.{node_id}")))
            .ok_or_else(|| anyhow::anyhow!("尚未连接这个资源所属的节点，请刷新主机列表"))
    }

    async fn by_node(&self, state: &AppState, node_id: &str) -> anyhow::Result<Remote> {
        let host = self.host_for_node(state, node_id)?;
        let remote = self.remote(state, &host).await?;
        verify_node(remote, node_id)
    }

    /// 插件在入口执行，只为当前请求从拥有项目的节点取上下文。
    pub(crate) async fn live_project(
        &self, state: &AppState, id: &str,
    ) -> anyhow::Result<Option<crate::db::ProjectRow>> {
        let target = match split_resource(id) {
            Some((node, raw)) => Some((node.to_string(), raw)),
            None => state.db.migrated_project_target(id)?,
        };
        let Some((node_id, raw_id)) = target else {
            return Ok(state.db.get_project(id).filter(|project| project.project_type == "local"));
        };
        let host = state.db.get_host(&self.host_for_node(state, &node_id)?)
            .ok_or_else(|| anyhow::anyhow!("远端主机不存在"))?;
        let remote = self.list_endpoint(state, &host).await
            .ok_or_else(|| anyhow::anyhow!("远端节点当前不可用"))?;
        let remote = verify_node(remote, &node_id)?;
        // 旧节点也支持同一列表接口。仅在本次插件请求内使用结果。
        let projects: Vec<falcon_proto::Project> = remote_list(state, &remote, "projects").await?;
        let host = remote.host;
        Ok(projects.into_iter().find(|p| p.id == raw_id).map(|project| crate::db::ProjectRow {
            id: resource_id(&node_id, &project.id),
            name: project.name,
            project_type: "ssh".into(),
            working_dir: project.working_dir,
            shell: project.shell,
            created_at: project.created_at,
            host_id: Some(host.id),
            ssh_host: Some(host.host),
            ssh_port: Some(host.port),
            ssh_username: Some(host.username),
            ssh_auth_method: Some(host.auth_method),
            ssh_key_path: host.key_path,
            ssh_secret_enc: host.secret_enc,
            ..Default::default()
        }))
    }

    fn failed(&self, host_id: &str, error: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.nodes.remove(host_id);
        cache.errors.insert(host_id.to_string(), error.to_string());
    }
}

fn verify_node(remote: Remote, node_id: &str) -> anyhow::Result<Remote> {
    anyhow::ensure!(remote.node_id == node_id, "连接的节点身份已改变，请重新检查主机配置");
    Ok(remote)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/node", get(node_info))
        .route("/api/nodes", get(node_status))
        .route("/api/node/import-projects", post(import_projects))
}

async fn node_info(State(state): State<AppState>) -> Json<falcon_proto::NodeInfo> {
    Json(falcon_proto::NodeInfo {
        node_id: state.db.instance_id(), version: crate::api::VERSION.into(),
        protocol_version: PROTOCOL_VERSION, platform: std::env::consts::OS.into(), arch: std::env::consts::ARCH.into(),
        data_dir: std::fs::canonicalize(&state.config.data_dir).unwrap_or_else(|_|state.config.data_dir.clone()).to_string_lossy().into_owned(),
    })
}

async fn node_status(State(state): State<AppState>) -> Json<Vec<falcon_proto::NodeStatus>> {
    let hosts = state.db.list_host_rows();
    let cache = state.nodes.cache.lock().unwrap_or_else(|e| e.into_inner());
    Json(hosts.iter().map(|host| {
        let node = cache.nodes.get(&host.id).filter(|node| node.host == *host);
        falcon_proto::NodeStatus {
            node_id: node.map(|n|n.node_id.clone()), host_id: host.id.clone(), name: host.name.clone(),
            version: node.map(|n|n.version.clone()), online: node.is_some(), error: cache.errors.get(&host.id).cloned(),
        }
    }).collect())
}

fn internal(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(given) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Ok(expected) = bootstrap::node_token(&state.config.data_dir) else {
        return false;
    };
    use subtle::ConstantTimeEq as _;
    given.as_bytes().ct_eq(expected.as_bytes()).into()
}

async fn import_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(entries): Json<Vec<crate::host_projects::HostProjectEntry>>,
) -> Response {
    if !internal(&state, &headers) {
        return ApiError::unauthorized().into_response();
    }
    match migration::import_projects(&state.db, entries) {
        Ok(()) => Json(json!({"ok":true})).into_response(),
        Err(e) => ApiError::conflict(e.to_string()).into_response(),
    }
}

fn decode_segment(value: &str) -> String {
    url::form_urlencoded::parse(format!("id={value}").as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

fn query_pairs(uri: &Uri) -> Vec<(String, String)> {
    url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

fn project_json(mut value: Value, remote: &Remote) -> Value {
    let standalone = format!("standalone:host:{}", remote.host.id);
    map_resource_fields(&mut value, &|id| {
        if id == "standalone:local" {
            standalone.clone()
        } else {
            resource_id(&remote.node_id, id)
        }
    });
    if let Some(object) = value.as_object_mut() {
        if object.get("type").and_then(Value::as_str) == Some("local")
            && object.contains_key("createdAt")
        {
            object.insert("type".into(), json!("ssh"));
            object.insert("hostId".into(), json!(remote.host.id));
            object.insert(
                "ssh".into(),
                json!({"host":remote.host.host,"port":remote.host.port,
                "username":remote.host.username,"authMethod":remote.host.auth_method,
                "keyPath":remote.host.key_path,"hasSecret":remote.host.secret_enc.is_some()}),
            );
        }
        if object.contains_key("projectType") {
            object.insert("projectType".into(), json!("ssh"));
            object.insert("hostName".into(), json!(remote.host.name));
        }
        if let Some(base) = object
            .get("rawBase")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            object.insert("rawBase".into(), json!(resource_url(&base, remote)));
        }
    }
    value
}

fn resource_url(path: &str, remote: &Remote) -> String {
    for (prefix, index) in [("/api/projects/", 3usize), ("/px0/", 2usize)] {
        if path.starts_with(prefix) {
            let mut parts: Vec<String> = path.split('/').map(str::to_string).collect();
            if let Some(id) = parts.get_mut(index) {
                let raw = decode_segment(id);
                let mapped = if raw == "standalone:local" {
                    format!("standalone:host:{}", remote.host.id)
                } else {
                    resource_id(&remote.node_id, &raw)
                };
                *id = crate::api::encode_uri_component(&mapped);
                return parts.join("/");
            }
        }
    }
    path.to_string()
}

fn outgoing_id(state: &AppState, node_id: &str, id: &str) -> anyhow::Result<String> {
    let target = split_resource(id)
        .map(|(node, raw)| (node.to_string(), raw))
        .or(state.db.migrated_project_target(id)?);
    match target {
        Some((node, raw)) => {
            anyhow::ensure!(node == node_id, "一次操作不能混合不同节点的资源");
            Ok(raw)
        }
        None => Ok(id.to_string()),
    }
}

fn outgoing_value(mut value: Value, node_id: &str, state: &AppState) -> anyhow::Result<Value> {
    fn visit(value: &mut Value, node_id: &str, state: &AppState) -> anyhow::Result<()> {
        match value {
            Value::Array(items) => {
                for value in items {
                    visit(value, node_id, state)?;
                }
            }
            Value::Object(object) => {
                for (key, value) in object {
                    if matches!(
                        key.as_str(),
                        "projectId" | "sourceProjectId" | "sessionId" | "target"
                    ) {
                        if let Some(id) = value.as_str() {
                            *value = json!(outgoing_id(state, node_id, id)?);
                        }
                    } else {
                        visit(value, node_id, state)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(&mut value, node_id, state)?;
    if let Some(object) = value.as_object_mut() {
        if object.get("type").and_then(Value::as_str) == Some("ssh") {
            object.insert("type".into(), json!("local"));
            object.remove("ssh");
            object.remove("hostId");
        }
    }
    Ok(value)
}

async fn connect(state: &AppState, remote: &Remote) -> anyhow::Result<Stream> {
    #[cfg(test)]
    if let Some(address) = remote.direct {
        return Ok(Box::new(tokio::net::TcpStream::connect(address).await?));
    }
    let host = remote.host.clone();
    let port = remote.port;
    state
        .engine
        .call(move |engine| async move {
            let link = engine.sessions.get_host_link(&host);
            let channel = link.forward_out("127.0.0.1", port).await?;
            Ok::<Stream, anyhow::Error>(Box::new(channel.into_stream()))
        })
        .await?
}

pub(super) async fn upstream(
    state: &AppState,
    remote: &Remote,
    method: Method,
    path: &str,
    mut headers: HeaderMap,
    body: Body,
) -> anyhow::Result<Response> {
    let stream = connect(state, remote).await?;
    if path.starts_with("/px0/") {
        headers = crate::px0::proxy::upstream_request_headers(&headers, "127.0.0.1");
    }
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    for name in [
        "host",
        "connection",
        "transfer-encoding",
        "cookie",
        "authorization",
        "content-length",
    ] {
        headers.remove(name);
    }
    headers.insert("host", HeaderValue::from_static("127.0.0.1"));
    headers.insert(LOCAL_HEADER, HeaderValue::from_static("1"));
    headers.insert(TOKEN_HEADER, HeaderValue::from_str(&remote.token)?);
    if path == "/api/system/update/staged" {
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", remote.token))?,
        );
    }
    let mut request = Request::builder().method(method).uri(path).body(body)?;
    *request.headers_mut() = headers;
    let response = sender.send_request(request).await?;
    let (parts, body) = response.into_parts();
    Ok(Response::from_parts(parts, Body::new(body)))
}

pub(super) async fn buffered(
    state: &AppState,
    remote: &Remote,
    method: Method,
    path: &str,
    mut headers: HeaderMap,
    body: Body,
) -> anyhow::Result<(StatusCode, HeaderMap, Vec<u8>)> {
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    let response = upstream(state, remote, method, path, headers, body).await?;
    let (parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, JSON_LIMIT).await?;
    let version = if path == "/api/node" {
        serde_json::from_slice::<falcon_proto::NodeInfo>(&bytes).ok()
            .filter(|info|info.node_id==remote.node_id).map(|info|info.version)
    } else if path == "/api/system/update/status" {
        serde_json::from_slice::<falcon_proto::ServerUpdateTask>(&bytes).ok()
            .filter(|task|task.phase==falcon_proto::ServerUpdatePhase::Succeeded).and_then(|task|task.target_version)
    } else {None};
    if let Some(version)=version {
        for node in state.nodes.cache.lock().unwrap_or_else(|e|e.into_inner()).nodes.values_mut().filter(|node|node.node_id==remote.node_id) {
            node.version=version.clone();
        }
    }
    Ok((parts.status, parts.headers, bytes.to_vec()))
}

async fn remote_list<T: serde::de::DeserializeOwned>(
    state: &AppState, remote: &Remote, kind: &str,
) -> anyhow::Result<Vec<T>> {
    let path = format!("/api/{kind}");
    let result = tokio::time::timeout(LIST_TIMEOUT, async {
        let (status, _, bytes) = buffered(
            state, remote, Method::GET, &path, HeaderMap::new(), Body::empty(),
        ).await?;
        anyhow::ensure!(status.is_success(), "节点列表请求失败：{status}");
        Ok(serde_json::from_slice(&bytes)?)
    }).await.unwrap_or_else(|_| Err(anyhow::anyhow!("读取节点列表超时")));
    if let Err(error) = &result {
        state.nodes.failed(&remote.host.id, &format!("{error:#}"));
    }
    result
}

async fn aggregate(state: &AppState, kind: &str, mut local: Vec<Value>) -> Value {
    let lists = state.db.list_host_rows().into_iter().map(|host| async move {
        let remote = state.nodes.list_endpoint(state, &host).await?;
        let values = remote_list::<Value>(state, &remote, kind).await.ok()?;
        Some((remote, values))
    });
    // 每台主机的本轮结果独立，按保存顺序合并。同一节点可由多条 SSH 配置发现。
    for (remote, values) in futures::future::join_all(lists).await.into_iter().flatten() {
        for mut value in values {
            if kind == "askpass/pending" {
                if let Some(id) = value.get("id").and_then(Value::as_str) {
                    value["id"] = json!(resource_id(&remote.node_id, id));
                }
            } else {
                value = project_json(value, &remote);
            }
            if !local.iter().any(|v| v.get("id") == value.get("id")) {
                local.push(value);
            }
        }
    }
    Value::Array(local)
}

/// 所有路由外层统一按资源归属转发；原始文件字节和 WebSocket 不经过 JSON 变换。
pub async fn middleware(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let local_only = req.headers().contains_key(LOCAL_HEADER);
    if local_only && !internal(&state, req.headers()) {
        return ApiError::unauthorized().into_response();
    }
    if req.method() == Method::GET && path == "/api/projects" {
        let values = state
            .db
            .list_projects()
            .iter()
            .filter(|p| p.project_type == "local")
            .map(crate::db::Db::to_project)
            .filter_map(|p| serde_json::to_value(p).ok())
            .collect();
        return Json(if local_only {
            Value::Array(values)
        } else {
            aggregate(&state, "projects", values).await
        }).into_response();
    }
    if req.method() == Method::GET && path == "/api/sessions" {
        let response = next.run(req).await;
        let status = response.status();
        if !status.is_success() {
            return response;
        }
        let Ok(bytes) = axum::body::to_bytes(response.into_body(), JSON_LIMIT).await else {
            return failure("会话列表超过大小限制");
        };
        let values: Vec<Value> = serde_json::from_slice::<Vec<Value>>(&bytes)
            .unwrap_or_default()
            .into_iter()
            .filter(|v| {
                v.get("projectId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| {
                        id == "standalone:local"
                            || state
                                .db
                                .get_project(id)
                                .is_some_and(|p| p.project_type == "local")
                    })
            })
            .collect();
        return Json(if local_only {
            Value::Array(values)
        } else {
            aggregate(&state, "sessions", values).await
        })
        .into_response();
    }
    if req.method() == Method::GET && path == "/api/askpass/pending" {
        let local: Vec<Value> = state
            .askpass
            .pending_prompts()
            .into_iter()
            .map(|p| json!({"id":p.id,"prompt":p.prompt}))
            .collect();
        return Json(if local_only {
            Value::Array(local)
        } else {
            aggregate(&state, "askpass/pending", local).await
        })
        .into_response();
    }
    if local_only {
        // 节点认证不能随 px0 等第三方反代继续发送；迁移端点自己还需核验令牌。
        if path != "/api/node/import-projects" {
            req.headers_mut().remove(TOKEN_HEADER);
        }
        req.headers_mut().remove(LOCAL_HEADER);
        if let Err(error) = canonical_request(&state, &mut req).await {
            return failure(error);
        }
        return idempotent(&state, req, next).await;
    }
    let is_json = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    let mut payload = None;
    if is_json {
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, JSON_LIMIT).await {
            Ok(b) => b,
            Err(e) => return failure(e),
        };
        payload = serde_json::from_slice::<Value>(&bytes).ok();
        req = Request::from_parts(parts, Body::from(bytes));
    }
    let uri = req.uri().clone();
    if let Some((host_id, remote_path)) = host_update_request(&uri) {
        return super::update::handle_host(state, host_id, req.method().clone(), &remote_path, payload).await;
    }
    match destination(&state, &uri, payload.as_ref()).await {
        Ok(Some((remote, remote_path))) => {
            if matches!(
                remote_path.as_str(),
                "/api/system/update" | "/api/system/update/status"
            ) {
                return super::update::handle(
                    state,
                    remote,
                    req.method().clone(),
                    &remote_path,
                    payload,
                )
                .await;
            }
            // 远端节点自己准备本机 sessiond，入口不再重复弹部署会话守护进程的表单。
            if remote_path.ends_with("/host") && remote_path.starts_with("/api/projects/") {
                return Json(json!({"authorized":true,"installedVersion":falcon_proto::sessiond::FALCON_VERSION,
                    "verifiedDurable":true,"requiredVersion":falcon_proto::sessiond::FALCON_VERSION})).into_response();
            }
            if path.starts_with("/ws/") {
                return websocket(state, remote, remote_path, req).await;
            }
            let (parts, body) = req.into_parts();
            let body = match payload {
                Some(value) => match outgoing_value(value, &remote.node_id, &state)
                    .and_then(|v| Ok(serde_json::to_vec(&v)?))
                {
                    Ok(b) => Body::from(b),
                    Err(e) => return failure(e),
                },
                None => body,
            };
            match upstream(
                &state,
                &remote,
                parts.method,
                &remote_path,
                parts.headers,
                body,
            )
            .await
            {
                Ok(response) => {
                    if remote_path.starts_with("/px0/") {
                        rewrite_px0_response(response, &remote).await
                    } else if remote_path.contains("/raw/")
                        || remote_path.ends_with("/download")
                        || remote_path.contains("/browser/")
                    {
                        response
                    } else {
                        rewrite_response(response, &remote).await
                    }
                }
                Err(error) => {
                    state.nodes.failed(&remote.host.id, &error.to_string());
                    failure(error)
                }
            }
        }
        Ok(None) => idempotent(&state, req, next).await,
        Err(error) => failure(error),
    }
}

async fn canonical_request(state: &AppState, request: &mut Request) -> anyhow::Result<()> {
    let mut segments: Vec<String> = request
        .uri()
        .path()
        .split('/')
        .map(str::to_string)
        .collect();
    let index = if segments.get(1).is_some_and(|s| s == "px0") {
        Some(2)
    } else if segments.get(2).is_some_and(|s| s == "projects") {
        Some(3)
    } else {
        None
    };
    if let Some(index) = index.filter(|i| *i < segments.len()) {
        let id = decode_segment(&segments[index]);
        if state.db.get_project(&id).is_none() {
            if let Some(local) = state.db.local_project_id(&id)? {
                segments[index] = crate::api::encode_uri_component(&local);
            }
        }
    }
    let mut pairs = query_pairs(request.uri());
    for (key, value) in &mut pairs {
        if key == "projectId" && state.db.get_project(value).is_none() {
            if let Some(local) = state.db.local_project_id(value)? {
                *value = local;
            }
        }
    }
    let mut uri = segments.join("/");
    if !pairs.is_empty() {
        uri.push('?');
        uri.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(pairs)
                .finish(),
        );
    }
    *request.uri_mut() = uri.parse()?;
    if request.uri().path() != "/api/node/import-projects"
        && request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"))
    {
        let body = std::mem::replace(request.body_mut(), Body::empty());
        let bytes = axum::body::to_bytes(body, JSON_LIMIT).await?;
        if let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) {
            fn visit(value: &mut Value, state: &AppState) -> anyhow::Result<()> {
                match value {
                    Value::Array(values) => {
                        for value in values {
                            visit(value, state)?;
                        }
                    }
                    Value::Object(object) => {
                        for (key, value) in object {
                            if matches!(key.as_str(), "projectId" | "sourceProjectId") {
                                if let Some(id) = value.as_str() {
                                    if state.db.get_project(id).is_none() {
                                        if let Some(local) = state.db.local_project_id(id)? {
                                            *value = json!(local);
                                        }
                                    }
                                }
                            } else {
                                visit(value, state)?;
                            }
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            visit(&mut value, state)?;
            if request.uri().path().ends_with("/move") {
                if let Some(target) = value.get("target").and_then(Value::as_str) {
                    if state.db.get_project(target).is_none() {
                        if let Some(local) = state.db.local_project_id(target)? {
                            value["target"] = json!(local);
                        }
                    }
                }
            }
            request.headers_mut().remove("content-length");
            *request.body_mut() = Body::from(serde_json::to_vec(&value)?);
        } else {
            *request.body_mut() = Body::from(bytes);
        }
    }
    Ok(())
}

fn host_update_request(uri: &Uri) -> Option<(String, String)> {
    let segments: Vec<String> = uri.path().split('/').map(decode_segment).collect();
    if segments.get(1).is_some_and(|s| s == "api")
        && segments.get(2).is_some_and(|s| s == "hosts")
        && segments.get(4).is_some_and(|s| s == "node")
        && segments.get(5).is_some_and(|s| s == "system")
        && segments.get(6).is_some_and(|s| s == "update")
        && matches!(segments.len(), 7 | 8)
        && (segments.len() == 7 || segments.get(7).is_some_and(|s| s == "status"))
    {
        return Some((segments[3].clone(), format!("/api/{}", segments[5..].join("/"))));
    }
    None
}

async fn destination(
    state: &AppState,
    uri: &Uri,
    payload: Option<&Value>,
) -> anyhow::Result<Option<(Remote, String)>> {
    let path = uri.path();
    // 插件仍在入口服务执行；它们从只读节点缓存取得项目上下文，内置机器探测沿用 SSH 执行器。
    if path.starts_with("/api/plugins/") {
        return Ok(None);
    }
    let mut segments: Vec<String> = path.split('/').map(decode_segment).collect();
    let mut remote = None;
    if segments.get(1).is_some_and(|s| s == "api")
        && segments.get(2).is_some_and(|s| s == "hosts")
        && segments.get(4).is_some_and(|s| s == "node")
    {
        let node = state.nodes.remote(state, &segments[3]).await?;
        let rest = segments[5..].join("/");
        let suffix = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
        return Ok(Some((node, format!("/api/{rest}{suffix}"))));
    }
    let resource_index = if segments.get(1).is_some_and(|s| s == "api" || s == "ws")
        && segments
            .get(2)
            .is_some_and(|s| matches!(s.as_str(), "projects" | "sessions" | "install" | "askpass" | "browser"))
    {
        Some(3)
    } else if segments.get(1).is_some_and(|s| s == "px0") {
        Some(2)
    } else {
        None
    };
    if let Some(index) = resource_index.filter(|i| *i < segments.len()) {
        let id = segments[index].clone();
        // 本机签发的作用域 raw 令牌由本机 raw_core 核验宿主机身份与目录范围（宿主机变更时直接回 401，不触发 SSH）。
        let local_raw_token = segments.get(2).is_some_and(|s| s == "projects")
            && segments.get(4).is_some_and(|s| s == "raw")
            && segments
                .get(5)
                .is_some_and(|token| state.auth.raw_scope(Some(token), &id).is_some());
        if !local_raw_token {
            if let Some((node_id, raw)) = split_resource(&id) {
                remote = Some(state.nodes.by_node(state, node_id).await?);
                segments[index] = raw;
            } else if let Some(host) = id.strip_prefix("standalone:host:") {
                remote = Some(state.nodes.remote(state, host).await?);
                segments[index] = "standalone:local".into();
            } else if let Some((node_id, raw)) = state.db.migrated_project_target(&id)? {
                remote = Some(state.nodes.by_node(state, &node_id).await?);
                segments[index] = raw;
            } else if let Some(project) = state
                .db
                .get_project(&id)
                .filter(|p| p.project_type == "ssh")
            {
                let host = project.host_id.ok_or_else(|| {
                    anyhow::anyhow!("旧 SSH 项目尚未绑定主机，请先在主机设置中保存连接")
                })?;
                let node = state.nodes.remote(state, &host).await?;
                segments[index] = state
                    .db
                    .migrated_project_target(&id)?
                    .map(|(_, id)| id)
                    .unwrap_or(id);
                remote = Some(node);
            }
        }
    }
    let mut pairs = query_pairs(uri);
    if remote.is_none() {
        let project = pairs
            .iter()
            .find(|(k, _)| k == "projectId")
            .map(|(_, v)| v.clone())
            .or_else(|| {
                payload
                    .and_then(|v| v.get("projectId"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        if let Some(id) = project {
            if let Some((node, _)) = split_resource(&id) {
                remote = Some(state.nodes.by_node(state, node).await?);
            } else if let Some((node, _)) = state.db.migrated_project_target(&id)? {
                remote = Some(state.nodes.by_node(state, &node).await?);
            }
        }
    }
    if remote.is_none()
        && matches!(
            path,
            "/api/projects" | "/api/agents" | "/api/shells" | "/api/fs/list"
        )
    {
        let host = payload
            .and_then(|v| v.get("hostId"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                pairs
                    .iter()
                    .find(|(k, _)| k == "hostId")
                    .map(|(_, v)| v.clone())
            })
            .filter(|h| !h.is_empty());
        // 空串或本地库不存在的 hostId 交给原有路由校验，保留 400（"主机不存在"）而非 502。
        if let Some(host) = host.filter(|h| state.db.get_host(h).is_some()) {
            remote = Some(state.nodes.remote(state, &host).await?);
        }
    }
    let Some(remote) = remote else {
        return Ok(None);
    };
    for (key, value) in &mut pairs {
        if key == "projectId" {
            *value = outgoing_id(state, &remote.node_id, value)?;
        }
    }
    pairs.retain(|(key, _)| key != "hostId");
    // fs/shells 的宿主探查在 B 上就是本机，不需要再按 SSH 项目解析。
    if matches!(path, "/api/fs/list" | "/api/shells") {
        pairs.retain(|(key, _)| key != "projectId");
    }
    let original: Vec<&str> = uri.path().split('/').collect();
    let mut rewritten = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if Some(i) == resource_index {
                crate::api::encode_uri_component(s)
            } else {
                original[i].to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if !pairs.is_empty() {
        rewritten.push('?');
        rewritten.push_str(
            &url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(pairs)
                .finish(),
        );
    }
    Ok(Some((remote, rewritten)))
}

async fn rewrite_px0_response(mut response: Response, remote: &Remote) -> Response {
    if let Some(location) = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    {
        if let Ok(value) = HeaderValue::from_str(&resource_url(&location, remote)) {
            response.headers_mut().insert("location", value);
        }
    }
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"))
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, JSON_LIMIT).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let Ok(html) = std::str::from_utf8(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    // px0 的入口页内嵌自己的资源前缀；只改入口 HTML 的 URL，不将第三方 JSON 当 Falcon 协议解析。
    static PREFIX: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r#"/px0/([^/\s\"'<>?#]+)/"#).unwrap());
    let rewritten = PREFIX
        .replace_all(html, |captures: &regex::Captures<'_>| {
            resource_url(&captures[0], remote)
        })
        .into_owned();
    parts.headers.remove("content-length");
    parts.headers.remove("etag");
    Response::from_parts(parts, Body::from(rewritten))
}

async fn rewrite_response(mut response: Response, remote: &Remote) -> Response {
    if let Some(location) = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    {
        if let Ok(value) = HeaderValue::from_str(&resource_url(&location, remote)) {
            response.headers_mut().insert("location", value);
        }
    }
    if !response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, JSON_LIMIT).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let value = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Array(values)) => Value::Array(
            values
                .into_iter()
                .map(|v| project_json(v, remote))
                .collect(),
        ),
        Ok(value) => project_json(value, remote),
        Err(_) => return Response::from_parts(parts, Body::from(bytes)),
    };
    parts.headers.remove("content-length");
    match serde_json::to_vec(&value) {
        Ok(body) => Response::from_parts(parts, Body::from(body)),
        Err(e) => failure(e),
    }
}

async fn idempotent(state: &AppState, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let create = req.method() == Method::POST
        && (path == "/api/projects" || path.ends_with("/sessions") || path.ends_with("/worktrees"));
    if !create {
        return next.run(req).await;
    }
    let Some(key) = req
        .headers()
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return next.run(req).await;
    };
    if key.len() > 256 || key.is_empty() {
        return ApiError::bad_request("无效的请求标识").into_response();
    }
    let lock = state.nodes.lock(&format!("request:{path}:{key}"));
    let _turn = lock.lock().await;
    let (parts, body) = req.into_parts();
    let body = match axum::body::to_bytes(body, JSON_LIMIT).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let fingerprint = hex::encode(Sha256::digest(&body));
    match state.db.begin_node_request(&path, &key, &fingerprint) {
        Ok(Some((status, body))) => {
            return (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                [("content-type", "application/json")],
                body,
            )
                .into_response();
        }
        Ok(None) => {}
        Err(error) => return ApiError::conflict(error.to_string()).into_response(),
    }
    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    let (parts, body) = response.into_parts();
    let body = match axum::body::to_bytes(body, JSON_LIMIT).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let text = String::from_utf8_lossy(&body);
    if let Err(e) = state
        .db
        .finish_node_request(&path, &key, parts.status.as_u16(), &text)
    {
        return failure(e);
    }
    Response::from_parts(parts, Body::from(body))
}

fn viewer_control(text: &str, node_id: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(text) {
        if value.get("type").and_then(Value::as_str) == Some("askpass") {
            if let Some(id) = value.get("id").and_then(Value::as_str).map(str::to_string) {
                value["id"] = json!(resource_id(node_id, &id));
                return value.to_string();
            }
        }
    }
    // Agent 工具输入可含任意业务 projectId/sessionId；控制通道仅转换 askpass 的顶层 ID。
    text.to_string()
}

async fn websocket(state: AppState, remote: Remote, path: String, req: Request) -> Response {
    let stream = match connect(&state, &remote).await {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    let address = format!("ws://127.0.0.1:{}{path}", remote.port);
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut request = match address.into_client_request() {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    request
        .headers_mut()
        .insert(LOCAL_HEADER, HeaderValue::from_static("1"));
    let token = match HeaderValue::from_str(&remote.token) {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    request.headers_mut().insert(TOKEN_HEADER, token);
    let (upstream, _) = match tokio_tungstenite::client_async(request, stream).await {
        Ok(v) => v,
        Err(e) => return failure(e),
    };
    let (mut parts, _) = req.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    upgrade
        .max_message_size(falcon_proto::WS_MAX_PAYLOAD_BYTES)
        .on_upgrade(move |downstream| async move {
            let (mut down_tx, mut down_rx) = downstream.split();
            let (mut up_tx, mut up_rx) = upstream.split();
            let to_up = async {
                while let Some(Ok(message)) = down_rx.next().await {
                    use axum::extract::ws::Message as D;
                    use tokio_tungstenite::tungstenite::Message as U;
                    let out = match message {
                        D::Text(v) => U::Text(v.to_string().into()),
                        D::Binary(v) => U::Binary(v),
                        D::Ping(v) => U::Ping(v),
                        D::Pong(v) => U::Pong(v),
                        D::Close(_) => U::Close(None),
                    };
                    if up_tx.send(out).await.is_err() {
                        break;
                    }
                }
            };
            let to_down = async {
                while let Some(Ok(message)) = up_rx.next().await {
                    use axum::extract::ws::Message as D;
                    use tokio_tungstenite::tungstenite::Message as U;
                    let out = match message {
                        U::Text(v) => D::Text(viewer_control(&v, &remote.node_id).into()),
                        U::Binary(v) => D::Binary(v),
                        U::Ping(v) => D::Ping(v),
                        U::Pong(v) => D::Pong(v),
                        U::Close(_) => D::Close(None),
                        U::Frame(_) => continue,
                    };
                    if down_tx.send(out).await.is_err() {
                        break;
                    }
                }
            };
            tokio::select! {_=to_up=>{},_=to_down=>{}}
        })
}

fn failure(error: impl std::fmt::Display) -> Response {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        format!("宿主机节点不可用：{error}"),
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{TestApp, json_of};

    #[test]
    fn host_update_route_is_handled_before_node_discovery() {
        let uri: Uri = "/api/hosts/host%20a/node/system/update".parse().unwrap();
        assert_eq!(host_update_request(&uri), Some(("host a".into(), "/api/system/update".into())));
        let status: Uri = "/api/hosts/h/node/system/update/status".parse().unwrap();
        assert_eq!(host_update_request(&status), Some(("h".into(), "/api/system/update/status".into())));
        assert!(host_update_request(&"/api/hosts/h/node/system/update/staged".parse().unwrap()).is_none());
        assert!(host_update_request(&"/api/hosts/h/node/projects".parse().unwrap()).is_none());
    }

    fn attach(gateway: &TestApp, owner: &TestApp, address: std::net::SocketAddr, host_id: &str) {
        let host = SshHostRow {
            id: host_id.into(),
            name: host_id.into(),
            host: "isolated.invalid".into(),
            port: 22,
            username: "test".into(),
            auth_method: "agent".into(),
            created_at: 1,
            ..Default::default()
        };
        gateway.state.db.insert_host(&host);
        let node_id = owner.state.db.instance_id();
        gateway
            .state
            .db
            .set_setting(&format!("nodes.route.{node_id}"), host_id);
        gateway.state.nodes.cache.lock().unwrap().nodes.insert(
            host_id.into(),
            Remote {
                host,
                node_id,
                port: address.port(),
                token: bootstrap::node_token(&owner.data_dir).unwrap(),
                version: crate::api::VERSION.into(),
                direct: Some(address),
            },
        );
    }

    async fn serve(app: &TestApp) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = app.router.clone();
        (
            address,
            tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            }),
        )
    }

    #[tokio::test]
    async fn browser_instances_stay_on_owner_and_reconnect_through_gateway() {
        use base64::Engine as _;
        use falcon_proto::{BrowserControlHolder, BrowserServerMessage, decode_browser_frame};
        use tokio_tungstenite::tungstenite::Message;

        let gateway = TestApp::new();
        let owner = TestApp::new();
        let other_owner = TestApp::new();
        let terminal_id = "terminal-1";
        for (node, host_id) in [(&owner, "work"), (&other_owner, "lab")] {
            node.local_project("project");
            node.state.db.insert_session(&crate::db::SessionRow {
                id: terminal_id.into(),
                project_id: "project".into(),
                state: "active".into(),
                ..Default::default()
            });
            let (address, _task) = serve(node).await;
            attach(&gateway, node, address, host_id);
        }
        let (gateway_address, _gateway_task) = serve(&gateway).await;
        let public_id = resource_id(&owner.state.db.instance_id(), terminal_id);
        let encoded_id = crate::api::encode_uri_component(&public_id);
        let upstream = |node: &TestApp, name: &str, seq: u64| {
            let name = name.to_owned();
            let run = crate::agent_browser::run_dir(&node.data_dir);
            std::fs::create_dir_all(&run).unwrap();
            let policy = crate::agent_browser::policy::write_allow(&node.data_dir, terminal_id).unwrap();
            async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();
                std::fs::write(run.join(format!("{name}.config")), json!({"action_policy": policy}).to_string()).unwrap();
                std::fs::write(run.join(format!("{name}.version")), "0.43.0").unwrap();
                std::fs::write(run.join(format!("{name}.pid")), std::process::id().to_string()).unwrap();
                std::fs::write(run.join(format!("{name}.stream")), port.to_string()).unwrap();
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let task = tokio::spawn(async move {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                    let jpeg = base64::engine::general_purpose::STANDARD.encode([seq as u8]);
                    for event in [
                        json!({"type":"url","url":format!("http://localhost/{name}")}),
                        json!({"type":"result","action":"snapshot","ok":true}),
                        json!({"type":"frame","seq":seq,"data":jpeg,"metadata":{"deviceWidth":1280,"deviceHeight":720}}),
                    ] {
                        socket.send(Message::Text(event.to_string().into())).await.unwrap();
                    }
                    while let Some(Ok(Message::Text(text))) = socket.next().await {
                        let _ = tx.send(serde_json::from_str::<Value>(&text).unwrap());
                    }
                });
                (rx, task)
            }
        };
        let (mut events_a, upstream_a) = upstream(&owner, "falcon-terminal-1", 1).await;
        let (mut events_b, upstream_b) = upstream(&owner, "预览_二", 2).await;
        let (_events_c, upstream_c) = upstream(&other_owner, "预览_二", 3).await;
        for node in [&owner, &other_owner] {
            node.state.engine.call(|engine| async move { engine.browser.scan_now() }).await.unwrap();
        }

        let token = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("browserSession", "预览_二")
            .finish();
        let base = format!("ws://{gateway_address}/ws/browser/{encoded_id}");
        let (mut a, _) = tokio_tungstenite::connect_async(&base).await.unwrap();
        let (mut b, _) = tokio_tungstenite::connect_async(format!("{base}?{token}")).await.unwrap();
        let other_id = resource_id(&other_owner.state.db.instance_id(), terminal_id);
        let other_path = crate::api::encode_uri_component(&other_id);
        let (mut c, _) = tokio_tungstenite::connect_async(format!("ws://{gateway_address}/ws/browser/{other_path}?{token}")).await.unwrap();
        for (socket, seq) in [(&mut a, 1u64), (&mut b, 2), (&mut c, 3)] {
            let frame = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Message::Binary(frame) = socket.next().await.unwrap().unwrap() {
                        break frame;
                    }
                }
            }).await.unwrap();
            let (header, jpeg) = decode_browser_frame(&frame).unwrap();
            assert_eq!(header.seq, seq);
            assert_eq!(jpeg, &[seq as u8]);
        }
        let (_, sessions) = json_of(gateway.get("/api/sessions").await).await;
        let rows = sessions.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        let first = rows.iter().find(|row| row["id"] == public_id).unwrap();
        let second = rows.iter().find(|row| row["id"] == other_id).unwrap();
        assert_eq!(first["hostName"], "work");
        assert_eq!(second["hostName"], "lab");
        assert_eq!(second["browsers"][0]["browserSession"], "预览_二");
        assert_eq!(first["browser"]["url"], "http://localhost/falcon-terminal-1");
        assert_eq!(first["browsers"][1]["browserSession"], "预览_二");
        assert!(gateway.state.db.list_sessions().is_empty());

        let path = format!("/api/sessions/{encoded_id}/browser");
        let (status, archives) = json_of(gateway.get(&format!("{path}/history")).await).await;
        assert_eq!(status, StatusCode::OK);
        let named = archives.as_array().unwrap().iter().find(|row| row["name"] == "预览_二").unwrap();
        let record = named["record"].as_str().unwrap();
        let (status, page) = json_of(gateway.get(&format!("{path}/history/{record}?before=2")).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["events"][0]["url"], "http://localhost/预览_二");
        for (query, name, jpeg) in [(String::new(), "falcon-terminal-1", 1u8), (format!("?{token}"), "预览_二", 2)] {
            let (status, log) = json_of(gateway.get(&format!("{path}/log{query}")).await).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(log["events"][0]["url"], format!("http://localhost/{name}"));
            let thumb = log["events"][1]["thumbId"].as_str().unwrap();
            let response = gateway.get(&format!("{path}/thumbs/{thumb}{query}")).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(bytes.as_ref(), &[jpeg]);
        }

        a.send(Message::Text(r#"{"type":"takeover"}"#.into())).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(Ok(Message::Text(text))) = b.next().await {
                if matches!(serde_json::from_str(&text), Ok(BrowserServerMessage::Control { holder: BrowserControlHolder::Human, .. })) {
                    return;
                }
            }
            panic!("同终端的另一个浏览器没有收到接管状态");
        }).await.unwrap();
        b.send(Message::Text(r#"{"type":"text","text":"x"}"#.into())).await.unwrap();
        let input = tokio::time::timeout(Duration::from_secs(5), events_b.recv()).await.unwrap().unwrap();
        assert_eq!(input["type"], "input_keyboard");
        assert!(events_a.try_recv().is_err());

        b.close(None).await.unwrap();
        a.close(None).await.unwrap();
        // 客户端断开后还可在同一台节点接回原有浏览器和最后一帧。
        let (mut again, _) = tokio_tungstenite::connect_async(format!("{base}?{token}")).await.unwrap();
        let hello = tokio::time::timeout(Duration::from_secs(5), again.next()).await.unwrap();
        let Some(Ok(Message::Text(hello))) = hello else { panic!("重连缺少 Hello") };
        assert!(matches!(serde_json::from_str(&hello), Ok(BrowserServerMessage::Hello { gone: false, .. })));

        upstream_a.abort();
        upstream_b.abort();
        upstream_c.abort();
    }

    #[tokio::test]
    async fn two_gateways_share_owner_projects_and_sessions_without_local_session_rows() {
        let a = TestApp::new();
        let b = TestApp::new();
        let c = TestApp::new();
        let home = b.root.to_string_lossy().into_owned();
        b.state
            .engine
            .call(move |engine| async move {
                engine.sessions.local.set_base_env(vec![
                    ("PATH".into(), "/usr/bin:/bin".into()),
                    ("HOME".into(), home),
                    ("TERM".into(), "xterm-256color".into()),
                ]);
            })
            .await
            .unwrap();
        let (b_address, b_task) = serve(&b).await;
        attach(&a, &b, b_address, "a-to-b");
        attach(&c, &b, b_address, "c-to-b");
        let cwd = b.root.to_string_lossy();
        let (status, project) = json_of(a.post_json("/api/projects", json!({"name":"shared", "type":"ssh", "hostId":"a-to-b", "workingDir":cwd, "shell":"/bin/sh"})).await).await;
        assert_eq!(status, StatusCode::OK, "{project}");
        let project_id = project["id"].as_str().unwrap();
        let (node, raw) = split_resource(project_id).unwrap();
        assert_eq!(node, b.state.db.instance_id());
        assert!(b.state.db.get_project(&raw).is_some());
        assert!(a.state.db.list_projects().is_empty());
        let (_, projects_c) = json_of(c.get("/api/projects").await).await;
        assert_eq!(projects_c[0]["id"], project_id);
        assert_eq!(projects_c[0]["hostId"], "c-to-b");
        std::fs::write(b.root.join("a b.txt"), b"owner bytes").unwrap();
        let (status, file) = json_of(
            c.get(&format!(
                "/api/projects/{}/file?path=a%20b.txt",
                crate::api::encode_uri_component(project_id)
            ))
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{file}");
        let raw_base = file["rawBase"].as_str().unwrap();
        assert!(raw_base.contains("node%3A"), "{raw_base}");
        let raw = c.get(&format!("{raw_base}a%20b.txt")).await;
        assert_eq!(raw.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(raw.into_body(), 1024)
                .await
                .unwrap()
                .as_ref(),
            b"owner bytes"
        );
        let json_bytes = b"{\"id\":\"business\",\"projectId\":\"p\"}\n";
        std::fs::write(b.root.join("config.json"), json_bytes).unwrap();
        let raw = c.get(&format!("{raw_base}config.json")).await;
        assert_eq!(raw.status(), StatusCode::OK);
        assert_eq!(
            axum::body::to_bytes(raw.into_body(), 1024)
                .await
                .unwrap()
                .as_ref(),
            json_bytes,
            "原始JSON文件不能被协议资源ID改写"
        );

        let (status, session) = json_of(
            a.post_json(
                &format!(
                    "/api/projects/{}/sessions",
                    crate::api::encode_uri_component(project_id)
                ),
                json!({}),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{session}");
        let session_id = session["id"].as_str().unwrap();
        let (_, raw_session) = split_resource(session_id).unwrap();
        assert!(b.state.db.get_session(&raw_session).is_some());
        assert!(a.state.db.list_sessions().is_empty());
        assert!(c.state.db.list_sessions().is_empty());
        let (_, sessions_c) = json_of(c.get("/api/sessions").await).await;
        assert_eq!(sessions_c[0]["id"], session_id);
        assert_eq!(sessions_c[0]["projectId"], project_id);

        let (c_address, c_task) = serve(&c).await;
        let url = format!(
            "ws://{c_address}/ws/sessions/{}",
            crate::api::encode_uri_component(session_id)
        );
        let (mut viewer, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        use tokio_tungstenite::tungstenite::Message;
        viewer
            .send(Message::Text(
                json!({"type":"resize","cols":80,"rows":24})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        viewer
            .send(Message::Text(
                json!({"type":"input","data":"echo node-shared-$((3+4))\r"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let output = tokio::time::timeout(std::time::Duration::from_secs(12), async {
            let mut output = String::new();
            while let Some(Ok(message)) = viewer.next().await {
                if let Message::Binary(bytes) = message {
                    output.push_str(&String::from_utf8_lossy(bytes.get(1..).unwrap_or_default()));
                }
                if output.contains("node-shared-7") {
                    break;
                }
            }
            output
        })
        .await
        .expect("经第二入口接入终端并收到实际输出");
        assert!(output.contains("node-shared-7"), "{output}");
        let (status, _) = json_of(
            a.post_json(
                &format!(
                    "/api/sessions/{}/terminate",
                    crate::api::encode_uri_component(session_id)
                ),
                json!({}),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            use falcon_proto::{ServerMessage, SessionState};
            while let Some(Ok(message)) = viewer.next().await {
                match message {
                    Message::Text(text) => match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(event @ (ServerMessage::Terminated | ServerMessage::State {
                            state: SessionState::Dead | SessionState::Unverified, ..
                        })) => return Some(event),
                        _ => {}
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            None
        })
        .await
        .expect("经第二入口收到终止通知");
        assert_eq!(event, Some(falcon_proto::ServerMessage::Terminated));
        assert!(b.state.db.get_session(&raw_session).is_none());
        let (_, sessions_c) = json_of(c.get("/api/sessions").await).await;
        assert_eq!(sessions_c, json!([]));
        let _ = viewer.close(None).await;
        c_task.abort();
        b_task.abort();
    }

    #[tokio::test]
    async fn creation_replays_same_request_but_rejects_changed_body() {
        let app = TestApp::new();
        let body = json!({"name":"once","type":"local","workingDir":app.root});
        let make = |body: Value| {
            Request::post("/api/projects")
                .header("content-type", "application/json")
                .header("idempotency-key", "same-logical-operation")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let (first_status, first) = json_of(app.send(make(body.clone())).await).await;
        let (second_status, second) = json_of(app.send(make(body)).await).await;
        assert_eq!(first_status, StatusCode::OK);
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(first, second);
        assert_eq!(app.state.db.list_projects().len(), 1);
        let (status, _) = json_of(app.send(make(json!({"name":"different"}))).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn legacy_aliases_are_resolved_in_paths_queries_and_json_fields() {
        let a = TestApp::new();
        let b = TestApp::new();
        attach(&a, &b, "127.0.0.1:1".parse().unwrap(), "h");
        let old = crate::db::ProjectRow {
            id: "a-old".into(),
            name: "legacy".into(),
            project_type: "ssh".into(),
            host_id: Some("h".into()),
            created_at: 1,
            ..Default::default()
        };
        a.state.db.insert_synced_project(&old, "host-remote", 1);
        migration::mark_migrated(&a.state.db, "h", &b.state.db.instance_id()).unwrap();
        let owner = crate::db::ProjectRow {
            id: "b-local".into(),
            name: "legacy".into(),
            project_type: "local".into(),
            created_at: 1,
            ..Default::default()
        };
        b.state.db.insert_synced_project(&owner, "host-remote", 1);
        let uri: Uri = "/api/agents?projectId=a-old".parse().unwrap();
        let (_, path) = destination(&a.state, &uri, None).await.unwrap().unwrap();
        assert_eq!(path, "/api/agents?projectId=host-remote");
        let payload = outgoing_value(
            json!({"projectId":"a-old","target":"a-old"}),
            &b.state.db.instance_id(),
            &a.state,
        )
        .unwrap();
        assert_eq!(
            payload,
            json!({"projectId":"host-remote","target":"host-remote"})
        );
        let mut request = Request::post("/api/projects/host-remote/move?projectId=host-remote")
            .header("content-type", "application/json")
            .body(Body::from(payload.to_string()))
            .unwrap();
        canonical_request(&b.state, &mut request).await.unwrap();
        assert_eq!(
            request.uri().to_string(),
            "/api/projects/b-local/move?projectId=b-local"
        );
        let body = axum::body::to_bytes(request.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"projectId":"b-local","target":"b-local"})
        );
        let mut px0 = Request::get("/px0/host-remote/")
            .body(Body::empty())
            .unwrap();
        canonical_request(&b.state, &mut px0).await.unwrap();
        assert_eq!(px0.uri(), "/px0/b-local/");
    }

    #[tokio::test]
    async fn lists_use_owner_even_when_legacy_metadata_conflicts_and_return_no_offline_copy() {
        let gateway = TestApp::new();
        let owner = TestApp::new();
        gateway.local_project("own");
        owner.local_project("shared");
        let mut project = owner.state.db.get_project("shared").unwrap();
        project.name = "host value".into();
        owner.state.db.update_project(&project);
        owner.state.db.insert_session(&crate::db::SessionRow {
            id: "remote-session".into(),
            project_id: "shared".into(),
            state: "active".into(),
            ..Default::default()
        });
        let (address, task) = serve(&owner).await;
        attach(&gateway, &owner, address, "h");
        gateway.state.db.insert_project(&crate::db::ProjectRow {
            id: "shared".into(),
            name: "gateway value".into(),
            project_type: "ssh".into(),
            host_id: Some("h".into()),
            ..Default::default()
        });
        let project_id = resource_id(&owner.state.db.instance_id(), "shared");
        let (_, projects) = json_of(gateway.get("/api/projects").await).await;
        assert_eq!(projects.as_array().unwrap().len(), 2);
        assert_eq!(projects[1]["id"], project_id);
        assert_eq!(projects[1]["name"], "host value");
        let (_, sessions) = json_of(gateway.get("/api/sessions").await).await;
        assert_eq!(sessions[0]["projectId"], project_id);
        assert_eq!(gateway.state.nodes.live_project(&gateway.state, &project_id).await.unwrap().unwrap().name, "host value");
        assert!(!gateway.state.db.is_host_migrated("h").unwrap());
        for kind in ["projects", "sessions"] {
            assert!(gateway.state.db.get_setting(&format!("nodes.cache.h.{kind}")).is_none());
        }

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let lock = gateway.state.nodes.lock("host:h");
        let _turn = lock.lock().await; // 禁止在测试中尝试真实 SSH 重新发现。
        let (projects, sessions) = tokio::join!(gateway.get("/api/projects"), gateway.get("/api/sessions"));
        let (status, projects) = json_of(projects).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(projects.as_array().unwrap().len(), 1);
        assert_eq!(projects[0]["id"], "own");
        assert_eq!(json_of(sessions).await.1, json!([]));
        let (_, nodes) = json_of(gateway.get("/api/nodes").await).await;
        assert_eq!(nodes[0]["online"], false);
        assert!(nodes[0]["error"].is_string());
    }

    #[tokio::test]
    async fn unbound_ssh_creation_cannot_write_gateway_database() {
        let app = TestApp::new();
        let (status, _) = json_of(app.post_json("/api/projects", json!({
            "name": "legacy", "type": "ssh", "workingDir": "/tmp",
            "ssh": { "host": "isolated.invalid", "username": "test", "authMethod": "agent" }
        })).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(app.state.db.list_projects().is_empty());
    }

    #[tokio::test]
    async fn redirects_keep_owner_resource_namespace() {
        let a = TestApp::new();
        let b = TestApp::new();
        attach(&a, &b, "127.0.0.1:1".parse().unwrap(), "h");
        let remote = a.state.nodes.cache.lock().unwrap().nodes["h"].clone();
        let response = Response::builder()
            .status(302)
            .header("location", "/px0/project/")
            .body(Body::empty())
            .unwrap();
        let rewritten = rewrite_response(response, &remote).await;
        assert_eq!(
            rewritten.headers()["location"],
            format!(
                "/px0/{}/",
                crate::api::encode_uri_component(&resource_id(&remote.node_id, "project"))
            )
        );
    }

    #[test]
    fn viewer_control_preserves_agent_business_ids_and_only_scopes_askpass() {
        let entry=r#"{"type":"agent_entry","entry":{"id":"call","input":{"projectId":"business","sessionId":"conversation"}}}"#;
        assert_eq!(viewer_control(entry,"n"),entry);
        let prompt=r#"{"type":"askpass","id":"p","prompt":"Password:"}"#;
        let value:Value=serde_json::from_str(&viewer_control(prompt,"n")).unwrap();
        assert_eq!(value["id"],resource_id("n","p"));
        assert_eq!(value["prompt"],"Password:");
    }

    #[tokio::test]
    async fn project_import_requires_ssh_readable_node_token() {
        let app = TestApp::new();
        let (status, _) =
            json_of(app.post_json("/api/node/import-projects", json!([])).await).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
