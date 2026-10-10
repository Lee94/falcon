//! `/api/meegle/*`：右侧「飞书项目」面板的后端（ADR 0010）。都是只读查询加一条登录。
//! 移植自 `packages/server/src/meegle/routes.ts`。
//!
//! 错误码约定：CLI 没装 / 没登录是 409（带 reason，前端据此切到安装 / 登录提示，
//! 用户动手就能过）；CLI 跑了但飞书那边报错是 502；参数不合法 / 链接不支持是 400。
//!
//! 固定列表存 SQLite（db.meegle_pins）：只读的 CLI 查询走 meegle，固定是 falcon 自己的数据。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, patch};
use falcon_proto::MeeglePin;
use serde_json::Value;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::askpass::hub::uuid_v4;
use crate::auth::now_ms;
use crate::db::{Db, MeeglePinRow};
use crate::meegle::command::{is_valid_id, is_valid_key, is_valid_url};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/meegle/pins", get(list_pins).post(add_pin))
        .route("/api/meegle/pins/{id}", patch(rename_pin).delete(remove_pin))
}

const PIN_KINDS: [&str; 3] = ["view", "multiProjectView", "workitem"];

/// 显示名：去首尾空白后非空、不超过 200 个 UTF-16 码元（TS 的 .length）
fn pin_label(v: Option<&Value>) -> Option<String> {
    let s = crate::term_env::js::trim(v?.as_str()?);
    (!s.is_empty() && s.encode_utf16().count() <= 200).then(|| s.to_string())
}

/// 可选文本：缺省 / null / 空串 = Ok(None)；类型不对或超长 = Err
fn optional_text(v: Option<&Value>, max: usize) -> Result<Option<String>, ()> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) if s.encode_utf16().count() <= max => Ok(Some(s.clone())),
        _ => Err(()),
    }
}

fn key_of(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str).filter(|s| is_valid_key(s))
}

/// "空着"：null / undefined / 空串（TS 的 `v != null && v !== ""` 取反）
fn blank(v: Option<&Value>) -> bool {
    matches!(v, None | Some(Value::Null)) || v.and_then(Value::as_str) == Some("")
}

/// 固定项各字段都过白名单：它们之后会原样进 CLI 的 argv
fn parse_pin_input(body: &Value) -> Option<MeeglePinRow> {
    let b = body.as_object()?;
    let kind = b.get("kind")?.as_str().filter(|k| PIN_KINDS.contains(k))?;
    let space_key = key_of(b.get("spaceKey"))?;
    let target_id = key_of(b.get("targetId"))?;
    if kind == "workitem" && !is_valid_id(target_id) {
        return None;
    }
    let type_key = b.get("typeKey");
    if !blank(type_key) && key_of(type_key).is_none() {
        return None;
    }
    let label = pin_label(b.get("label"))?;
    let space_name = optional_text(b.get("spaceName"), 200).ok()?;
    let url = b.get("url");
    if !blank(url) && !url.and_then(Value::as_str).is_some_and(is_valid_url) {
        return None;
    }
    Some(MeeglePinRow {
        id: uuid_v4(),
        kind: kind.to_string(),
        space_key: space_key.to_string(),
        space_name,
        target_id: target_id.to_string(),
        type_key: type_key.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
        label,
        url: url.and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
        created_at: now_ms(),
    })
}

async fn list_pins(State(state): State<AppState>) -> Json<Vec<MeeglePin>> {
    Json(state.db.list_meegle_pins().iter().map(Db::to_meegle_pin).collect())
}

/// 同一个东西再固定一次就回已有的那条，前端不用先查
async fn add_pin(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<MeeglePin>> {
    let row = parse_pin_input(&body.0).ok_or_else(|| ApiError::bad_request("固定项不合法"))?;
    if let Some(existing) = state.db.find_meegle_pin(&row.kind, &row.space_key, &row.target_id) {
        return Ok(Json(Db::to_meegle_pin(&existing)));
    }
    state.db.insert_meegle_pin(&row);
    Ok(Json(Db::to_meegle_pin(&row)))
}

async fn rename_pin(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<MeeglePin>> {
    let label = pin_label(body.0.get("label")).ok_or_else(|| ApiError::bad_request("名称不能为空"))?;
    let row = state.db.get_meegle_pin(&id).ok_or_else(|| ApiError::not_found("固定项不存在"))?;
    state.db.rename_meegle_pin(&id, &label);
    Ok(Json(Db::to_meegle_pin(&MeeglePinRow { label, ..row })))
}

async fn remove_pin(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    if state.db.get_meegle_pin(&id).is_none() {
        return Err(ApiError::not_found("固定项不存在"));
    }
    state.db.delete_meegle_pin(&id);
    Ok(ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pin_input_whitelist() {
        let ok = json!({ "kind": "view", "spaceKey": "proj", "targetId": "v1", "label": " 我的视图 " });
        let row = parse_pin_input(&ok).unwrap();
        assert_eq!((row.kind.as_str(), row.label.as_str(), row.type_key, row.url), ("view", "我的视图", None, None));
        assert!(parse_pin_input(&json!({ "kind": "nope", "spaceKey": "p", "targetId": "v", "label": "x" })).is_none());
        assert!(parse_pin_input(&json!({ "kind": "view", "spaceKey": "p", "targetId": "v", "label": "  " })).is_none());
        assert!(parse_pin_input(&json!({ "kind": "view", "spaceKey": "p", "targetId": "v", "label": "x", "spaceName": 3 }))
            .is_none());
        assert!(parse_pin_input(&json!([1])).is_none());
        let long = "字".repeat(201);
        assert!(pin_label(Some(&json!(long))).is_none());
    }
}
