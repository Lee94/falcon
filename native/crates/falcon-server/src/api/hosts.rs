//! 已保存的 SSH 主机（routes.ts 的 `---- saved SSH hosts ----` 一段）。
//!
//! 中转挂在主机上（ADR 0016）：改连接配置要换掉主机链路并把启用中的中转重新拉起来，
//! 删主机要先停隧道再连同规则一起删。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use falcon_proto::SshHost;
use serde_json::{Value, json};

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use super::input::{field, integer, present, str_field, trimmed, trimmed_non_empty, truthy};
use crate::askpass::hub::uuid_v4;
use crate::auth::now_ms;
use crate::db::{Db, SshHostRow};
use crate::sessions::manager::host_as_project;
use crate::zellij::host::HostKind;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/hosts", get(list).post(create))
        .route("/api/hosts/test", post(test_draft))
        .route("/api/hosts/{id}", axum::routing::put(update).delete(remove))
        .route("/api/hosts/{id}/test", post(test_saved))
}

/// SSH 连接字段的校验（项目里手写的 ssh 与已保存主机共用）
pub fn validate_ssh_fields(ssh: &Value) -> Option<&'static str> {
    if trimmed(ssh, "host").is_none_or(str::is_empty) {
        return Some("SSH 主机不能为空");
    }
    if trimmed(ssh, "username").is_none_or(str::is_empty) {
        return Some("SSH 用户名不能为空");
    }
    let port = field(ssh, "port");
    if present(port) && !integer(port).is_some_and(|p| (1..=65535).contains(&p)) {
        return Some("端口无效");
    }
    let method = str_field(ssh, "authMethod");
    if method == Some("key") && trimmed(ssh, "keyPath").is_none_or(str::is_empty) {
        return Some("密钥认证必须指定私钥路径");
    }
    if !matches!(method, Some("key" | "password" | "agent")) {
        return Some("未知认证方式");
    }
    None
}

fn validate_host_input(input: &Value) -> Option<&'static str> {
    if trimmed(input, "name").is_none_or(str::is_empty) {
        return Some("主机名称不能为空");
    }
    validate_ssh_fields(input)
}

/// `input.port || 22`（校验过之后 port 要么缺省要么是 1–65535 的整数）
pub fn port_or_default(input: &Value) -> i64 {
    let port = field(input, "port");
    if truthy(port) { integer(port).unwrap_or(22) } else { 22 }
}

/// 主机的连接配置变没变（显示名不算）。变了才需要换掉它的 SshLink
fn conn_changed(a: &SshHostRow, b: &SshHostRow) -> bool {
    a.host != b.host
        || a.port != b.port
        || a.username != b.username
        || a.auth_method != b.auth_method
        || a.key_path != b.key_path
        || a.secret_enc != b.secret_enc
}

async fn list(State(state): State<AppState>) -> Json<Vec<SshHost>> {
    Json(state.db.list_hosts())
}

async fn create(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<SshHost>> {
    let input = &body.0;
    if let Some(err) = validate_host_input(input) {
        return Err(ApiError::bad_request(err));
    }
    let name = trimmed(input, "name").unwrap_or_default().to_string();
    if state.db.find_host_by_name(&name, None).is_some() {
        return Err(ApiError::conflict("已有同名主机"));
    }
    let method = str_field(input, "authMethod").unwrap_or_default().to_string();
    let row = SshHostRow {
        id: uuid_v4(),
        name,
        host: trimmed(input, "host").unwrap_or_default().to_string(),
        port: port_or_default(input),
        username: trimmed(input, "username").unwrap_or_default().to_string(),
        key_path: if method == "key" { trimmed_non_empty(input, "keyPath") } else { None },
        auth_method: method,
        secret_enc: str_field(input, "secret").filter(|s| !s.is_empty()).map(|s| state.secrets.encrypt(s)),
        created_at: now_ms(),
    };
    state.db.insert_host(&row);
    Ok(Json(Db::to_ssh_host(&row, 0)))
}

async fn update(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<SshHost>> {
    let existing = state.db.get_host(&id).ok_or_else(|| ApiError::not_found("主机不存在"))?;
    let input = &body.0;
    if let Some(err) = validate_host_input(input) {
        return Err(ApiError::bad_request(err));
    }
    let name = trimmed(input, "name").unwrap_or_default().to_string();
    if state.db.find_host_by_name(&name, Some(&id)).is_some() {
        return Err(ApiError::conflict("已有同名主机"));
    }
    let method = str_field(input, "authMethod").unwrap_or_default().to_string();
    let row = SshHostRow {
        name,
        host: trimmed(input, "host").unwrap_or_default().to_string(),
        port: port_or_default(input),
        username: trimmed(input, "username").unwrap_or_default().to_string(),
        key_path: if method == "key" {
            trimmed_non_empty(input, "keyPath").or_else(|| existing.key_path.clone())
        } else {
            None
        },
        auth_method: method,
        secret_enc: match str_field(input, "secret").filter(|s| !s.is_empty()) {
            Some(s) => Some(state.secrets.encrypt(s)),
            None => existing.secret_enc.clone(),
        },
        ..existing.clone()
    };
    state.db.update_host(&row);
    state.db.update_projects_from_host(&row);
    // 连接配置变了才换链路：只改显示名也拆一遍会让这台主机上的中转无端断一下
    if conn_changed(&existing, &row) {
        let host_id = id.clone();
        state.engine.call(move |e| async move { e.sessions.dispose_host_link(&host_id, true).await }).await?;
    }
    Ok(Json(Db::to_ssh_host(&row, state.db.count_projects_by_host(&id) as u32)))
}

async fn remove(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if state.db.get_host(&id).is_none() {
        return Err(ApiError::not_found("主机不存在"));
    }
    let n = state.db.count_projects_by_host(&id);
    if n > 0 {
        return Err(ApiError::conflict(format!("有 {n} 个项目正在使用该主机，请先改绑或删除这些项目")));
    }
    // 中转挂在主机上（ADR 0016），主机没了规则无处可挂：先停隧道再连同规则一起删
    let host_id = id.clone();
    state.engine.call(move |e| async move { e.sessions.dispose_host_link(&host_id, false).await }).await?;
    state.db.delete_relays_of_host(&id);
    state.db.delete_host(&id);
    Ok(ok())
}

/// 试连一台已保存主机。连不上是环境事实，200 + ok:false，不 4xx。
async fn test_saved(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let host = state.db.get_host(&id).ok_or_else(|| ApiError::not_found("主机不存在"))?;
    probe_host(&state, host).await
}

/// 试连表单里这组还没保存（或正在改）的凭据。
/// 编辑已有主机时带 hostId：secret / keyPath 留空则沿用已保存的。
async fn test_draft(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<Value>> {
    let input = &body.0;
    let host_id = str_field(input, "hostId").filter(|s| !s.is_empty());
    let existing = host_id.and_then(|id| state.db.get_host(id));
    if host_id.is_some() && existing.is_none() {
        return Err(ApiError::not_found("主机不存在"));
    }
    if let Some(err) = validate_ssh_fields(input) {
        return Ok(Json(json!({ "ok": false, "error": err })));
    }
    let method = str_field(input, "authMethod").unwrap_or_default().to_string();
    let row = SshHostRow {
        id: existing.as_ref().map(|e| e.id.clone()).unwrap_or_else(|| format!("draft:{}", uuid_v4())),
        name: trimmed_non_empty(input, "name")
            .or_else(|| existing.as_ref().map(|e| e.name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(|| "test".into()),
        host: trimmed(input, "host").unwrap_or_default().to_string(),
        port: port_or_default(input),
        username: trimmed(input, "username").unwrap_or_default().to_string(),
        key_path: if method == "key" {
            trimmed_non_empty(input, "keyPath").or_else(|| existing.as_ref().and_then(|e| e.key_path.clone()))
        } else {
            None
        },
        auth_method: method,
        secret_enc: match str_field(input, "secret").filter(|s| !s.is_empty()) {
            Some(s) => Some(state.secrets.encrypt(s)),
            None => existing.as_ref().and_then(|e| e.secret_enc.clone()),
        },
        created_at: existing.as_ref().map(|e| e.created_at).unwrap_or_else(now_ms),
    };
    probe_host(&state, row).await
}

async fn probe_host(state: &AppState, host: SshHostRow) -> ApiResult<Json<Value>> {
    let res = state
        .engine
        .call(move |e| async move {
            e.sessions.probe_ssh(&host_as_project(&host)).await.map(|f| (f.kind, f.home)).map_err(|e| e.to_string())
        })
        .await?;
    Ok(Json(match res {
        Ok((kind, home)) => {
            json!({ "ok": true, "kind": if kind == HostKind::Windows { "windows" } else { "posix" }, "home": home })
        }
        Err(error) => json!({ "ok": false, "error": error }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_field_validation_matches_node() {
        let ok = json!({ "host": "h", "username": "u", "authMethod": "agent" });
        assert_eq!(validate_ssh_fields(&ok), None);
        assert_eq!(validate_ssh_fields(&json!({ "host": " ", "username": "u" })), Some("SSH 主机不能为空"));
        assert_eq!(validate_ssh_fields(&json!({ "host": "h" })), Some("SSH 用户名不能为空"));
        let bad_port = json!({ "host": "h", "username": "u", "authMethod": "agent", "port": 0 });
        assert_eq!(validate_ssh_fields(&bad_port), Some("端口无效"));
        let str_port = json!({ "host": "h", "username": "u", "authMethod": "agent", "port": "22" });
        assert_eq!(validate_ssh_fields(&str_port), Some("端口无效"));
        let null_port = json!({ "host": "h", "username": "u", "authMethod": "agent", "port": null });
        assert_eq!(validate_ssh_fields(&null_port), None);
        assert_eq!(port_or_default(&null_port), 22);
        let key = json!({ "host": "h", "username": "u", "authMethod": "key" });
        assert_eq!(validate_ssh_fields(&key), Some("密钥认证必须指定私钥路径"));
        let what = json!({ "host": "h", "username": "u", "authMethod": "kerberos" });
        assert_eq!(validate_ssh_fields(&what), Some("未知认证方式"));
        assert_eq!(validate_host_input(&ok), Some("主机名称不能为空"));
        assert_eq!(port_or_default(&json!({ "port": 2222 })), 2222);
    }
}
