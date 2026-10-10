//! 会话与宿主机状态的 REST 路由（routes.ts 的 `---- sessions ----` 一段与 `/api/projects/:id/host`）。
//!
//! 碰会话状态的一律投进会话引擎（engine.rs）执行；只读写 DB 的（改名、宿主机授权）直接做——
//! 与 Node 版一样，每条 SQL 自己是原子的。

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use falcon_proto::{
    HostZellijStatus, PASTE_IMAGE_MAX_BYTES, ProjectType, Session, SessionAgent, SessionForeground, SessionWithProject,
};
use serde_json::{Value, json};

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::db::{Db, ZellijHostPatch};
use crate::exec::ExecResult;
use crate::git::path::join_path;
use crate::paste::{image_ext, paste_dir, paste_file_name, posix_write_command, windows_write_command, write_local_paste_file};
use crate::term_env::js;
use crate::term_env::sanitize_color_hint;
use crate::zellij::host::HostKind;
use crate::zellij::version::{DEFAULT_BASE_URL, ZELLIJ_VERSION};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/host", get(host_status).post(host_update))
        .route("/api/sessions", get(list))
        .route("/api/projects/{id}/sessions", post(create))
        .route("/api/sessions/{id}/reattach", post(reattach))
        .route("/api/sessions/{id}/foreground", get(foreground))
        .route("/api/sessions/{id}/terminate", post(terminate))
        .route("/api/sessions/{id}", axum::routing::delete(delete_dead).patch(rename))
        .route("/api/sessions/{id}/paste-image", post(paste_image))
}

/// JS 的真值判断（`authorized ? 1 : 0`）
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn ssh_host_key(project: &crate::db::ProjectRow) -> (String, u16, String) {
    (
        project.ssh_host.clone().unwrap_or_default(),
        project.ssh_port.unwrap_or(22) as u16,
        project.ssh_username.clone().unwrap_or_default(),
    )
}

/// 安装授权按主机记（host+port+username），不按项目：
/// 同一台机器上的第二个项目不该再问一遍，二进制本来就已经装好了。
async fn host_status(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<HostZellijStatus>> {
    let project = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    if project.project_type != "ssh" {
        return Err(ApiError::bad_request("仅 SSH 项目有宿主机状态"));
    }
    let (host, port, user) = ssh_host_key(&project);
    let row = state.db.get_zellij_host(&host, port, &user);
    Ok(Json(HostZellijStatus {
        authorized: row.as_ref().and_then(|r| r.authorized).map(|a| a == 1),
        installed_version: row.as_ref().and_then(|r| r.installed_version.clone()),
        base_url: row.as_ref().and_then(|r| r.base_url.clone()),
        verified_durable: row.as_ref().and_then(|r| r.verified_durable).map(|v| v == 1),
        default_base_url: DEFAULT_BASE_URL.into(),
        required_version: ZELLIJ_VERSION.into(),
    }))
}

async fn host_update(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Value>> {
    let project = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    if project.project_type != "ssh" {
        return Err(ApiError::bad_request("仅 SSH 项目有宿主机状态"));
    }
    let mut patch = ZellijHostPatch::default();
    match body.0.get("authorized") {
        None | Some(Value::Null) => {}
        Some(v) => patch.authorized = Some(Some(i64::from(truthy(v)))),
    }
    match body.0.get("baseUrl") {
        None => {}
        // 换了下载源就作废之前的安装记录，下次重新走一遍安装流程
        Some(Value::String(url)) => {
            let url = js::trim(url);
            patch.base_url = Some((!url.is_empty()).then(|| url.to_string()));
            patch.installed_version = Some(None);
        }
        Some(_) => return Err(ApiError::internal("baseUrl.trim is not a function")),
    }
    let (host, port, user) = ssh_host_key(&project);
    state.db.upsert_zellij_host(&host, port, &user, patch);
    state.engine.send(move |engine| engine.sessions.reset_prepare(&id));
    Ok(ok())
}

async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<SessionWithProject>>> {
    let out = state
        .engine
        .call(|engine| async move {
            let db = engine.db.clone();
            let projects: std::collections::HashMap<_, _> =
                db.list_projects().into_iter().map(|p| (p.id.clone(), p)).collect();
            db.list_sessions()
                .into_iter()
                .map(|row| {
                    let p = projects.get(&row.project_id);
                    let mut session = Db::to_session(&row);
                    // 自动标题不入库，只有还活着的 entry 手上有（可能是陈旧值，见 title_of）
                    session.title = engine.sessions.title_of(&row.id);
                    SessionWithProject {
                        session,
                        project_name: p.map(|p| p.name.clone()).unwrap_or_else(|| "?".into()),
                        project_type: p
                            .and_then(|p| ProjectType::from_wire(&p.project_type))
                            .unwrap_or(ProjectType::Local),
                    }
                })
                .collect::<Vec<_>>()
        })
        .await?;
    Ok(Json(out))
}

async fn create(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Session>> {
    let project = state.db.get_project(&id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    // 存档的项目到期会连目录一起删掉，不能再往里开终端
    if project.worktree_archived_at.is_some() {
        return Err(ApiError::conflict("项目已存档，请先恢复再新建终端"));
    }
    // 认不出的 agent 一律当普通终端：宁可开出一个 shell，也不要 400 一个新会话
    let agent = body.str("agent").and_then(SessionAgent::from_wire).filter(|a| *a != SessionAgent::Unknown);
    // 不起名（空串）是常态：UI 显示的是前台命令 / agent / 工作目录，
    // 编号名（"Terminal 3"）既没信息量，序号还会随删除重号
    let name = js::trim(body.str("name").unwrap_or("")).to_string();
    let hint = sanitize_color_hint(&body.0);
    let res = state
        .engine
        .call(move |engine| async move { engine.sessions.create_session(&project, name, hint, agent).await })
        .await?;
    res.map(Json).map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("创建会话失败：{e}")))
}

async fn reattach(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Session>> {
    let sid = id.clone();
    let res = state.engine.call(move |engine| async move { engine.sessions.ensure_attached(&sid, true).await }).await?;
    match res {
        Ok(row) => Ok(Json(Db::to_session(&row))),
        Err(e) => match state.db.get_session(&id) {
            Some(row) if row.state == "dead" => Ok(Json(Db::to_session(&row))),
            _ => Err(ApiError::new(StatusCode::BAD_GATEWAY, format!("接回失败：{e}"))),
        },
    }
}

/// 关 tab 前问一嘴前台有没有程序在跑。会话不存在也答"空闲"——
/// 这条路径上前端接下来就是终止，404 只会让它多走一个错误分支
async fn foreground(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<SessionForeground>> {
    let fg = state.engine.call(move |engine| async move { engine.sessions.foreground(&id).await }).await?;
    Ok(Json(fg))
}

async fn terminate(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state.engine.call(move |engine| async move { engine.sessions.terminate(&id, false).await }).await?;
    Ok(ok())
}

async fn delete_dead(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let deleted = state.engine.call(move |engine| async move { engine.sessions.delete_dead(&id) }).await?;
    if !deleted {
        return Err(ApiError::conflict("只能清除已丢失（dead）的会话"));
    }
    Ok(ok())
}

async fn rename(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Value>> {
    let Some(name) = body.str("name") else { return Err(ApiError::bad_request("缺少名称")) };
    if state.db.get_session(&id).is_none() {
        return Err(ApiError::not_found("会话不存在"));
    }
    // 空串是合法的：清掉名字就回到自动标题，这是"取消重命名"的唯一出口
    state.db.rename_session(&id, js::trim(name));
    Ok(ok())
}

/// 粘贴图片：写进会话宿主机的 <falcon 根>/paste，返回绝对路径。
/// 前端把路径粘进终端输入——Claude Code 认输入框里的图片路径，
/// 拖拽文件进原生终端就是同一个机制。
async fn paste_image(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Json<Value>> {
    let content_type = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string);
    // Fastify 先按 Content-Type 收请求体（image/* 限 20MB），再进路由
    let data = axum::body::to_bytes(body, PASTE_IMAGE_MAX_BYTES as usize).await.map_err(|_| ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        body: json!({
            "statusCode": 413,
            "code": "FST_ERR_CTP_BODY_TOO_LARGE",
            "error": "Payload Too Large",
            "message": "Request body is too large",
        }),
    })?;

    let row = state.db.get_session(&id).ok_or_else(|| ApiError::not_found("会话不存在"))?;
    let project = state.db.get_project(&row.project_id).ok_or_else(|| ApiError::not_found("项目不存在"))?;
    let Some(ext) = image_ext(content_type.as_deref()) else {
        return Err(ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "不支持的图片类型"));
    };
    if data.is_empty() {
        return Err(ApiError::bad_request("图片内容为空"));
    }

    let fail = |e: String| ApiError::new(StatusCode::BAD_GATEWAY, format!("图片上传失败：{e}"));
    if project.project_type == "local" {
        let data_dir = state.config.data_dir.clone();
        let path = tokio::task::spawn_blocking(move || write_local_paste_file(&data_dir, ext, &data))
            .await
            .map_err(|e| fail(e.to_string()))?
            .map_err(|e| fail(e.to_string()))?;
        return Ok(Json(json!({ "path": path.to_string_lossy() })));
    }

    let res = state
        .engine
        .call(move |engine| async move {
            let link = engine.sessions.get_link(&project);
            let facts = link.host_facts().await.map_err(|e| e.to_string())?;
            let dir = paste_dir(facts.kind, &facts.root);
            let file = join_path(facts.kind, &[dir.as_str(), &paste_file_name(ext)]);
            let res: anyhow::Result<ExecResult> = if facts.kind == HostKind::Windows {
                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &data);
                link.exec_with_input(&windows_write_command(&dir, &file), b64.as_bytes()).await
            } else {
                link.exec_with_input(&posix_write_command(&dir, &file), &data).await
            };
            let res = res.map_err(|e| e.to_string())?;
            if !res.ok() {
                let stderr = js::trim(&res.stderr);
                return Err(if stderr.is_empty() {
                    format!("远端写入失败（exit {}）", res.code.map(|c| c.to_string()).unwrap_or_else(|| "null".into()))
                } else {
                    stderr.to_string()
                });
            }
            Ok(file)
        })
        .await?;
    let file = res.map_err(fail)?;
    Ok(Json(json!({ "path": file })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_truthiness() {
        assert!(!truthy(&json!(false)));
        assert!(!truthy(&json!(0)));
        assert!(!truthy(&json!("")));
        assert!(truthy(&json!(true)));
        assert!(truthy(&json!("no")));
        assert!(truthy(&json!([])));
    }
}
