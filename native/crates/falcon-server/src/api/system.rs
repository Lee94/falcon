//! `/api/system`、`/api/fs/*`、`/api/shells`（routes.ts 的 `---- system / fs ----` 一段，
//! askpass 那两条在 askpass.rs）。

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::Uri;
use axum::routing::{get, post};
use falcon_proto::{FsListing, FsValidateResult, ShellsInfo, SystemInfo};
use serde_json::Value;

use super::auth_routes::LenientJson;
use super::body::query_param;
use super::error::{ApiError, ApiResult};
use super::{AppState, VERSION};
use crate::db::{ProjectRow, SshHostRow};
use crate::fs::{list_directories, list_remote_directories};
use crate::sessions::local::{default_local_shell, local_kind};
use crate::sessions::login_env::node_platform;
use crate::shells::{detect_shells, shells_from_probe, shells_probe};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/system", get(system))
        .route("/api/fs/validate", post(fs_validate))
        .route("/api/fs/list", get(fs_list))
        .route("/api/shells", get(shells))
}

async fn system(State(state): State<AppState>) -> ApiResult<Json<SystemInfo>> {
    // 只报已知状态，不触发探测/下载——那是首次创建本地会话时才做的事
    let local = state.engine.call(|engine| async move { engine.sessions.local_durable_state() }).await?;
    Ok(Json(SystemInfo {
        platform: node_platform().into(),
        local_durable: local.as_ref().map(|l| l.durable),
        local_durable_reason: local.and_then(|l| l.reason),
        version: VERSION.into(),
    }))
}

/// 后端本机上这个路径是不是一个能进的文件夹。相对路径按后端 cwd 解析（同 Node 的 statSync）。
/// 不是文件夹 / 不存在同样是 200 + ok:false
async fn fs_validate(body: LenientJson) -> Json<FsValidateResult> {
    let fail = |error: &str| Json(FsValidateResult { ok: false, error: Some(error.into()) });
    // `if (!p)`：缺省、null、空串（以及 0 / false）都是"路径为空"
    let p = match body.0.get("path") {
        None | Some(Value::Null | Value::Bool(false)) => return fail("路径为空"),
        Some(Value::String(s)) if s.is_empty() => return fail("路径为空"),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => return fail("路径为空"),
        Some(Value::String(s)) => s.clone(),
        // 别的类型 statSync 会抛 TypeError，落进同一个 catch
        Some(_) => return fail("路径不存在或不可访问"),
    };
    match tokio::task::spawn_blocking(move || std::fs::metadata(p)).await {
        Ok(Ok(m)) if m.is_dir() => Json(FsValidateResult { ok: true, error: None }),
        Ok(Ok(_)) => fail("不是文件夹"),
        _ => fail("路径不存在或不可访问"),
    }
}

/// `hostId` / `projectId` 指向的远端（TS 的 `resolveLink`，两个只读探查路由共用）。
/// 这里只做查库那一半；拿链路要进会话引擎
enum LinkTarget {
    Project(ProjectRow),
    Host(SshHostRow),
}

fn resolve_link_target(
    state: &AppState,
    host_id: Option<&str>,
    project_id: Option<&str>,
) -> Result<LinkTarget, String> {
    if let Some(project_id) = project_id.filter(|p| !p.is_empty()) {
        let row = state.db.get_project(project_id).ok_or("项目不存在")?;
        if row.project_type != "ssh" {
            return Err("只有 SSH 项目能访问远端".into());
        }
        return Ok(LinkTarget::Project(row));
    }
    let host = state.db.get_host(host_id.unwrap_or("")).ok_or("主机不存在")?;
    Ok(LinkTarget::Host(host))
}

/// 列子目录。不带 hostId/projectId 时列后端本机；带了就走 SSH 列远端。
/// query 缺省是家目录；`path=` 空字符串是 Windows 盘符列表。
/// 读失败回 400，不回 500——路径不存在或 SSH 连不上都是调用方能处理的。
async fn fs_list(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<FsListing>> {
    let p = query_param(&uri, "path");
    let host_id = query_param(&uri, "hostId").filter(|s| !s.is_empty());
    let project_id = query_param(&uri, "projectId").filter(|s| !s.is_empty());
    if host_id.is_none() && project_id.is_none() {
        return list_directories(p).await.map(Json).map_err(|e| ApiError::bad_request(e.to_string()));
    }
    let target =
        resolve_link_target(&state, host_id.as_deref(), project_id.as_deref()).map_err(ApiError::bad_request)?;
    let res = state
        .engine
        .call(move |engine| async move {
            let link = match target {
                LinkTarget::Project(row) => engine.sessions.get_link(&row),
                LinkTarget::Host(host) => engine.sessions.get_host_link(&host),
            };
            let facts = link.host_facts().await.map_err(|e| e.to_string())?;
            list_remote_directories(&*link, facts.kind, &facts.home, p.as_deref()).await.map_err(|e| e.to_string())
        })
        .await?;
    res.map(Json).map_err(ApiError::bad_request)
}

/// 侦测宿主机上可用的 shell，供项目表单的 shell 选择。
/// 不带 hostId/projectId 时侦测后端本机；带了就经 SSH 侦测远端。
/// 探测命令本身失败不算错（至少有默认项），连不上远端才回 400。
async fn shells(State(state): State<AppState>, uri: Uri) -> ApiResult<Json<ShellsInfo>> {
    let host_id = query_param(&uri, "hostId").filter(|s| !s.is_empty());
    let project_id = query_param(&uri, "projectId").filter(|s| !s.is_empty());
    if host_id.is_none() && project_id.is_none() {
        // 本机执行器永远不报错（spawn 失败也收成 code: None），直接跑，不必进引擎
        let kind = local_kind();
        let res = crate::exec::local_exec(shells_probe(kind), None).await;
        return Ok(Json(shells_from_probe(kind, &default_local_shell(), Some(&res))));
    }
    let target =
        resolve_link_target(&state, host_id.as_deref(), project_id.as_deref()).map_err(ApiError::bad_request)?;
    let res = state
        .engine
        .call(move |engine| async move {
            let link = match target {
                LinkTarget::Project(row) => engine.sessions.get_link(&row),
                LinkTarget::Host(host) => engine.sessions.get_host_link(&host),
            };
            let facts = link.host_facts().await.map_err(|e| e.to_string())?;
            Ok::<_, String>(detect_shells(&*link, facts.kind, &facts.shell).await)
        })
        .await?;
    res.map(Json).map_err(ApiError::bad_request)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{TestApp, json_of, write};
    use axum::http::StatusCode;
    use serde_json::json;

    #[tokio::test]
    async fn fs_validate_shapes() {
        let app = TestApp::new();
        let dir = app.root.join("d");
        std::fs::create_dir(&dir).unwrap();
        write(&app.root.join("f"), "x");
        let check = |body: serde_json::Value| {
            let app = &app;
            async move { json_of(app.post_json("/api/fs/validate", body).await).await }
        };
        assert_eq!(check(json!({ "path": dir })).await, (StatusCode::OK, json!({ "ok": true })));
        assert_eq!(
            check(json!({ "path": app.root.join("f") })).await,
            (StatusCode::OK, json!({ "ok": false, "error": "不是文件夹" }))
        );
        assert_eq!(
            check(json!({ "path": app.root.join("nope") })).await,
            (StatusCode::OK, json!({ "ok": false, "error": "路径不存在或不可访问" }))
        );
        assert_eq!(check(json!({ "path": "" })).await, (StatusCode::OK, json!({ "ok": false, "error": "路径为空" })));
        assert_eq!(check(json!({})).await, (StatusCode::OK, json!({ "ok": false, "error": "路径为空" })));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fs_list_local_and_link_errors() {
        let app = TestApp::new();
        std::fs::create_dir_all(app.root.join("b/inner")).unwrap();
        std::fs::create_dir(app.root.join(".hidden")).unwrap();
        std::fs::create_dir(app.root.join("A")).unwrap();
        write(&app.root.join("file.txt"), "x");
        std::os::unix::fs::symlink(app.root.join("b"), app.root.join("link-to-b")).unwrap();
        let root = app.root.to_string_lossy().into_owned();

        // `..` 按字面消掉（path.resolve），只列文件夹、跟符号链接、点开头的沉底
        let uri = format!("/api/fs/list?path={}", crate::api::encode_uri_component(&format!("{root}/b/inner/../..")));
        let (status, body) = json_of(app.get(&uri).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["path"], root.as_str());
        assert_eq!(body["parent"], app.root.parent().unwrap().to_string_lossy().as_ref());
        assert_eq!(body["roots"], json!(["/"]));
        let names: Vec<&str> =
            body["entries"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["A", "b", "link-to-b", ".hidden"]);
        assert_eq!(body["entries"][1]["path"], format!("{root}/b"));

        // 空串在 POSIX 上是根
        let (status, body) = json_of(app.get("/api/fs/list?path=").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!((body["path"].as_str(), body["parent"].is_null()), (Some("/"), true));

        let uri = format!("/api/fs/list?path={}", crate::api::encode_uri_component(&format!("{root}/file.txt")));
        let (status, body) = json_of(app.get(&uri).await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "不是文件夹" })));
        let (status, body) = json_of(app.get("/api/fs/list?path=%2Fno%2Fsuch%2Fdir").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "路径不存在或不可访问" })));

        // 远端：查不到主机 / 不是 SSH 项目，都是 400（不连网）
        let (status, body) = json_of(app.get("/api/fs/list?hostId=nope").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "主机不存在" })));
        app.local_project("p1");
        let (status, body) = json_of(app.get("/api/fs/list?projectId=p1").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "只有 SSH 项目能访问远端" })));
        let (status, body) = json_of(app.get("/api/shells?projectId=zz").await).await;
        assert_eq!((status, body), (StatusCode::BAD_REQUEST, json!({ "error": "项目不存在" })));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shells_local_default_first() {
        let app = TestApp::new();
        let (status, body) = json_of(app.get("/api/shells").await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["kind"], "posix");
        let default = crate::sessions::local::default_local_shell();
        assert_eq!(body["default"], default.as_str());
        assert_eq!(body["shells"][0], default.as_str());
        // 本机上至少有 /bin/sh
        assert!(body["shells"].as_array().unwrap().iter().any(|s| s.as_str().is_some_and(|s| s.starts_with('/'))));
    }
}
