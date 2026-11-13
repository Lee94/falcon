//! 网关替目标节点下载官方更新：产物只在入口下载并校验，目标节点可以完全不访问公网。
//!
//! 安装请求发送前持久化交接记录。入口断线或重启后只查询目标任务，不重发安装请求；
//! 两个入口同时升级同一节点时，以目标节点的数据目录任务锁为准。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use axum::{Json, body::Body, http::{HeaderMap, HeaderValue, Method, StatusCode}, response::{IntoResponse, Response}};
use falcon_core::app_update;
use falcon_proto::{ServerUpdateCheck, ServerUpdatePhase as Phase, ServerUpdateTask};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::gateway::{Remote, buffered, upstream};
use crate::{api::{AppState, error::ApiError}, server_update};

static RUNNING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stage { Downloading, Submitted, Finished }

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Record {
    task: ServerUpdateTask,
    stage: Stage,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    node_id: String,
    version: String,
    platform: String,
    arch: String,
}

fn record_path(state: &AppState, node_id: &str) -> PathBuf {
    state.config.data_dir.join("node-updates").join(format!("{}.json", hex::encode(Sha256::digest(node_id.as_bytes()))))
}

fn load(path: &Path) -> Result<Option<Record>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn save(path: &Path, record: &Record) -> Result<()> { server_update::atomic_json(path, record) }
fn busy(path: &Path) -> bool { RUNNING.lock().unwrap_or_else(|e| e.into_inner()).contains(path) }

async fn remote_json<T: for<'de> Deserialize<'de>>(state: &AppState, remote: &Remote, path: &str) -> Result<T> {
    let (status, _, bytes) = tokio::time::timeout(Duration::from_secs(15),
        buffered(state, remote, Method::GET, path, HeaderMap::new(), Body::empty())).await.context("目标节点响应超时")??;
    ensure!(status.is_success(), "目标节点返回 {status}：{}", String::from_utf8_lossy(&bytes));
    Ok(serde_json::from_slice(&bytes)?)
}

async fn identity(state: &AppState, remote: &Remote) -> Result<Identity> {
    let info: Identity = remote_json(state, remote, "/api/node").await?;
    ensure!(info.node_id == remote.node_id, "升级目标节点身份已改变");
    Ok(info)
}

async fn check(state: &AppState, remote: &Remote) -> ServerUpdateCheck {
    let mut result = ServerUpdateCheck { current_version: remote.version.clone(), ..Default::default() };
    let check = async {
        let info = identity(state, remote).await?;
        result.current_version = info.version;
        result.platform = server_update::platform(&info.platform, &info.arch).context("目标节点没有此平台的官方服务端产物")?;
        let release = server_update::fetch_release(None).await?;
        result.latest_version = Some(release.version.clone());
        result.available = server_update::release_available(&result.current_version, &release, &result.platform)?;
        Ok::<_, anyhow::Error>(())
    }.await;
    if let Err(error) = check { result.error = Some(format!("检查目标节点更新失败：{error:#}")); }
    result
}

async fn status(state: &AppState, remote: &Remote) -> Result<ServerUpdateTask> {
    let path = record_path(state, &remote.node_id);
    let Some(mut record) = load(&path)? else { return remote_json(state, remote, "/api/system/update/status").await };
    if record.stage == Stage::Finished { return Ok(record.task); }
    if record.stage == Stage::Downloading {
        if busy(&path) { return Ok(record.task); }
        // 尚未发送安装请求，重启后只记录下载中断；目标节点不受影响。
        record.task.phase = Phase::Failed;
        record.task.error = Some("入口在下载期间重新启动，下载已中断，尚未提交目标节点安装".into());
        record.stage = Stage::Finished;
        save(&path, &record)?;
        return Ok(record.task);
    }
    if busy(&path) { return Ok(record.task); }
    match remote_json::<ServerUpdateTask>(state, remote, "/api/system/update/status").await {
        Ok(task) if task.target_version == record.task.target_version && task.phase != Phase::Idle => {
            record.task = task;
            if !record.task.phase.is_running() { record.stage = Stage::Finished; }
        }
        Ok(_) if busy(&path) => return Ok(record.task),
        Ok(_) | Err(_) => {
            // 不能把丢失的响应解释成请求未执行。目标可能已经换程序，此处绝不自动重发 POST。
            record.task.phase = Phase::Restarting;
            record.task.error = Some("安装交接结果尚未确认；正在查询目标节点，不会重复提交安装请求".into());
        }
    }
    save(&path, &record)?;
    Ok(record.task)
}

async fn start(state: AppState, remote: Remote, version: &str) -> Result<ServerUpdateTask> {
    let parsed = app_update::parse_version(version).context("升级版本必须是三段稳定版本号")?;
    let version = app_update::version_string(parsed);
    ensure!(app_update::is_newer(&version, &remote.version), "目标版本必须高于当前节点版本");
    let path = record_path(&state, &remote.node_id);
    if let Some(record) = load(&path)? {
        if record.stage == Stage::Submitted {
            let _ = status(&state, &remote).await?;
            ensure!(load(&path)?.is_some_and(|r| r.stage == Stage::Finished), "上次安装结果尚未确认，请先查询目标节点状态");
        }
    }
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    ensure!(!running.contains(&path), "此入口已有目标节点升级任务");
    let record = Record { task: ServerUpdateTask { phase: Phase::Downloading, target_version: Some(version.clone()), error: None }, stage: Stage::Downloading };
    save(&path, &record)?;
    running.insert(path.clone());
    drop(running);
    let task = record.task.clone();
    tokio::spawn(async move {
        let result = download_and_submit(&state, &remote, &version, &path).await;
        if let Err(error) = result {
            if let Ok(Some(mut record)) = load(&path) {
                if record.stage == Stage::Submitted {
                    record.task.phase = Phase::Restarting;
                    record.task.error = Some(format!("安装响应未确认，后续仅查询目标节点：{error:#}"));
                } else {
                    record.stage = Stage::Finished;
                    record.task.phase = Phase::Failed;
                    record.task.error = Some(format!("目标节点升级下载失败：{error:#}"));
                }
                let _ = save(&path, &record);
            }
        }
        RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(&path);
    });
    Ok(task)
}

async fn download_and_submit(state: &AppState, remote: &Remote, version: &str, path: &Path) -> Result<()> {
    let info = identity(state, remote).await?;
    ensure!(app_update::is_newer(version, &info.version), "目标节点已经运行相同或更高版本");
    let platform = server_update::platform(&info.platform, &info.arch).context("目标节点平台没有官方服务端产物")?;
    let release = server_update::fetch_release(Some(version)).await?;
    let work = path.parent().context("缺少节点升级目录")?.join(format!("download-{}", hex::encode(rand::random::<[u8; 16]>())));
    std::fs::create_dir(&work)?;
    let result = async {
        let candidate = work.join("candidate");
        server_update::download_verified_release(&release, &platform, &candidate).await?;
        let mut record = load(path)?.context("节点升级任务丢失")?;
        record.task.phase = Phase::Verifying;
        save(path, &record)?;
        let bytes = tokio::fs::read(candidate).await?;
        let checksum = hex::encode(Sha256::digest(&bytes));
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("application/octet-stream"));
        headers.insert("x-falcon-update-version", HeaderValue::from_str(version)?);
        headers.insert("x-falcon-update-sha256", HeaderValue::from_str(&checksum)?);
        // 在第一次网络发送之前记录，崩溃恢复不能据本地没有响应推断 B 没有开始安装。
        record.stage = Stage::Submitted;
        record.task.phase = Phase::Installing;
        save(path, &record)?;
        let response = tokio::time::timeout(Duration::from_secs(300),
            upstream(state, remote, Method::POST, "/api/system/update/staged", headers, Body::from(bytes))).await.context("传输升级产物超时")??;
        let code = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
        if code.is_success() {
            record.task = serde_json::from_slice(&bytes).context("目标节点升级响应无效")?;
            ensure!(record.task.target_version.as_deref() == Some(version), "目标节点接受了不同升级版本");
            if !record.task.phase.is_running() { record.stage = Stage::Finished; }
        } else {
            record.stage = Stage::Finished;
            record.task.phase = Phase::Failed;
            let error = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned));
            record.task.error = Some(error.unwrap_or_else(|| format!("目标节点拒绝安装：{code}")));
        }
        save(path, &record)
    }.await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

pub(super) async fn handle(state: AppState, remote: Remote, method: Method, path: &str, payload: Option<Value>) -> Response {
    if method == Method::GET && path == "/api/system/update" { return Json(check(&state, &remote).await).into_response(); }
    if method == Method::GET && path == "/api/system/update/status" {
        return match status(&state, &remote).await { Ok(task) => Json(task).into_response(), Err(err) => ApiError::new(StatusCode::BAD_GATEWAY, err.to_string()).into_response() };
    }
    if method == Method::POST && path == "/api/system/update" {
        let version = payload.as_ref().and_then(|body| body.get("version")).and_then(Value::as_str);
        let Some(version) = version else { return ApiError::bad_request("version 必须是稳定 Release 的版本号").into_response() };
        return match start(state, remote, version).await { Ok(task) => Json(task).into_response(), Err(err) => ApiError::conflict(err.to_string()).into_response() };
    }
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::TestApp;

    #[tokio::test]
    async fn submitted_tasks_only_query_node_and_persist_its_final_result() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        use axum::{Router, routing::{get, post}};
        let app = TestApp::new();
        let target = Arc::new(Mutex::new(ServerUpdateTask::default()));
        let installs = Arc::new(AtomicUsize::new(0));
        let task = target.clone();
        let count = installs.clone();
        let router = Router::new()
            .route("/api/system/update/status", get(move || { let task = task.lock().unwrap().clone(); async move { Json(task) } }))
            .route("/api/system/update/staged", post(move || { count.fetch_add(1, Ordering::SeqCst); async { StatusCode::OK } }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        let remote = Remote { host: crate::db::SshHostRow::default(), node_id: "isolated-node".into(), port: address.port(), token: "test-token".into(), version: "1.0.0".into(), direct: Some(address) };
        let path = record_path(&app.state, &remote.node_id);
        save(&path, &Record { task: ServerUpdateTask { phase: Phase::Installing, target_version: Some("1.2.3".into()), error: None }, stage: Stage::Submitted }).unwrap();
        let uncertain = status(&app.state, &remote).await.unwrap();
        assert_eq!(uncertain.phase, Phase::Restarting);
        assert!(uncertain.error.is_some());
        assert_eq!(load(&path).unwrap().unwrap().stage, Stage::Submitted);
        *target.lock().unwrap() = ServerUpdateTask { phase: Phase::Succeeded, target_version: Some("1.2.3".into()), error: None };
        let done = status(&app.state, &remote).await.unwrap();
        assert_eq!(done.phase, Phase::Succeeded);
        assert_eq!(load(&path).unwrap().unwrap().stage, Stage::Finished);
        assert_eq!(installs.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[test]
    fn records_survive_restart_and_node_ids_cannot_escape_the_directory() {
        let app = TestApp::new();
        let path = record_path(&app.state, "../../other-node");
        assert_eq!(path.parent(), Some(app.data_dir.join("node-updates").as_path()));
        let record = Record { task: ServerUpdateTask { phase: Phase::Installing, target_version: Some("1.2.3".into()), error: None }, stage: Stage::Submitted };
        save(&path, &record).unwrap();
        let restored = load(&path).unwrap().unwrap();
        assert_eq!(restored.stage, Stage::Submitted);
        assert_eq!(restored.task, record.task);
    }
}
