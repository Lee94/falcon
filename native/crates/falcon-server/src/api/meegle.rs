//! `/api/meegle/*`：右侧「飞书项目」面板的后端（ADR 0010）。都是只读查询加一条登录。
//! 移植自 `packages/server/src/meegle/routes.ts`。
//!
//! 错误码约定：CLI 没装 / 没登录是 409（带 reason，前端据此切到安装 / 登录提示，
//! 用户动手就能过）；CLI 跑了但飞书那边报错是 502；参数不合法 / 链接不支持是 400。
//!
//! 固定列表存 SQLite（db.meegle_pins）：只读的 CLI 查询走 meegle，固定是 falcon 自己的数据。

use axum::Json;
use axum::Router;
use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use falcon_proto::{
    MeegleLogin, MeeglePage, MeeglePin, MeegleSearchResult, MeegleSpace, MeegleStatus, MeegleTodoAction, MeegleTodoItem,
    MeegleUrlTarget, MeegleWorkItem, MeegleWorkItemDetail, MeegleWorkItemType,
};
use serde_json::Value;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::askpass::hub::uuid_v4;
use crate::auth::now_ms;
use crate::db::{Db, MeeglePinRow};
use crate::meegle::client::MeegleQueryOpts;
use crate::meegle::command::{is_valid_host, is_valid_id, is_valid_key, is_valid_url};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/meegle/status", get(status))
        .route("/api/meegle/login", post(login))
        .route("/api/meegle/login/cancel", post(login_cancel))
        .route("/api/meegle/cache/clear", post(cache_clear))
        .route("/api/meegle/spaces", get(spaces))
        .route("/api/meegle/spaces/{key}/types", get(types))
        .route("/api/meegle/spaces/{key}/search", get(search))
        .route("/api/meegle/spaces/{key}/recent", get(recent))
        .route("/api/meegle/spaces/{key}/views/{view_id}/items", get(view_items))
        .route("/api/meegle/spaces/{key}/items/{id}", get(work_item))
        .route("/api/meegle/spaces/{key}/multi-views/{view_id}/items", get(multi_view_items))
        .route("/api/meegle/resolve-url", post(resolve_url))
        .route("/api/meegle/pins", get(list_pins).post(add_pin))
        .route("/api/meegle/pins/{id}", patch(rename_pin).delete(remove_pin))
        .route("/api/meegle/todo", get(todo))
}

type Q = Query<HashMap<String, String>>;

fn opts(q: &HashMap<String, String>) -> MeegleQueryOpts {
    MeegleQueryOpts::from_fresh_param(q.get("fresh").map(String::as_str))
}

/// `Number(raw ?? 1)`，1–10000 的整数，否则 1
fn page_of(raw: Option<&String>) -> u32 {
    let n = match raw.map(|s| s.trim()) {
        None => 1.0,
        Some("") => 0.0,
        Some(s) => s.parse::<f64>().unwrap_or(f64::NAN),
    };
    if n.is_finite() && n.trunc() == n && (1.0..=10_000.0).contains(&n) { n as u32 } else { 1 }
}

fn valid_key(v: &str, what: &'static str) -> ApiResult<()> {
    if is_valid_key(v) { Ok(()) } else { Err(ApiError::bad_request(what)) }
}

async fn status(State(state): State<AppState>, Query(q): Q) -> Json<MeegleStatus> {
    Json(state.meegle.status(opts(&q)).await)
}

async fn login(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<MeegleLogin>> {
    let Some(host) = body.str("host").filter(|h| is_valid_host(h)) else {
        return Err(ApiError::bad_request("站点域名不合法"));
    };
    Ok(Json(state.meegle.start_login(host).await?))
}

async fn login_cancel(State(state): State<AppState>) -> Json<Value> {
    state.meegle.cancel_login();
    ok()
}

async fn cache_clear(State(state): State<AppState>) -> Json<Value> {
    state.meegle.clear_cache();
    ok()
}

async fn spaces(State(state): State<AppState>, Query(q): Q) -> ApiResult<Json<Vec<MeegleSpace>>> {
    Ok(Json(state.meegle.spaces(q.get("q").map(String::as_str), opts(&q)).await?))
}

async fn types(State(state): State<AppState>, Path(key): Path<String>, Query(q): Q) -> ApiResult<Json<Vec<MeegleWorkItemType>>> {
    valid_key(&key, "空间 key 不合法")?;
    Ok(Json(state.meegle.types(&key, opts(&q)).await?))
}

async fn search(State(state): State<AppState>, Path(key): Path<String>, Query(q): Q) -> ApiResult<Json<MeegleSearchResult>> {
    let keyword = q.get("q").map(|s| crate::term_env::js::trim(s).to_string()).unwrap_or_default();
    valid_key(&key, "空间 key 不合法")?;
    if keyword.is_empty() {
        return Err(ApiError::bad_request("缺少关键字"));
    }
    let type_key = q.get("type").filter(|t| !t.is_empty());
    if let Some(t) = type_key {
        valid_key(t, "类型 key 不合法")?;
    }
    Ok(Json(state.meegle.search(&key, &keyword, type_key.map(String::as_str), opts(&q)).await?))
}

async fn recent(State(state): State<AppState>, Path(key): Path<String>, Query(q): Q) -> ApiResult<Json<Vec<MeegleWorkItem>>> {
    valid_key(&key, "空间 key 不合法")?;
    let type_key = q.get("type").map(String::as_str).unwrap_or("");
    valid_key(type_key, "类型 key 不合法")?;
    Ok(Json(state.meegle.recent(&key, type_key, opts(&q)).await?))
}

async fn view_items(
    State(state): State<AppState>,
    Path((key, view_id)): Path<(String, String)>,
    Query(q): Q,
) -> ApiResult<Json<MeeglePage<MeegleWorkItem>>> {
    let page = page_of(q.get("page"));
    valid_key(&key, "空间 key 不合法")?;
    valid_key(&view_id, "视图 id 不合法")?;
    Ok(Json(state.meegle.view_items(&key, &view_id, page, opts(&q)).await?))
}

async fn multi_view_items(
    State(state): State<AppState>,
    Path((key, view_id)): Path<(String, String)>,
    Query(q): Q,
) -> ApiResult<Json<MeeglePage<MeegleWorkItem>>> {
    let page = page_of(q.get("page"));
    valid_key(&key, "空间 key 不合法")?;
    valid_key(&view_id, "视图 id 不合法")?;
    Ok(Json(state.meegle.multi_view_items(&key, &view_id, page, opts(&q)).await?))
}

async fn work_item(
    State(state): State<AppState>,
    Path((key, id)): Path<(String, String)>,
    Query(q): Q,
) -> ApiResult<Json<MeegleWorkItemDetail>> {
    valid_key(&key, "空间 key 不合法")?;
    if !is_valid_id(&id) {
        return Err(ApiError::bad_request("工作项 id 不合法"));
    }
    Ok(Json(state.meegle.work_item(&key, &id, opts(&q)).await?))
}

async fn resolve_url(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<MeegleUrlTarget>> {
    let Some(url) = body.str("url").filter(|u| is_valid_url(u)) else {
        return Err(ApiError::bad_request("不是合法的链接"));
    };
    Ok(Json(state.meegle.resolve_url(url).await?))
}

async fn todo(State(state): State<AppState>, Query(q): Q) -> ApiResult<Json<MeeglePage<MeegleTodoItem>>> {
    let Some(action) = q.get("action").and_then(|a| MeegleTodoAction::from_wire(a)) else {
        return Err(ApiError::bad_request("action 不合法"));
    };
    Ok(Json(state.meegle.todo(action, page_of(q.get("page")), opts(&q)).await?))
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

    #[test]
    fn page_numbers_follow_js_number() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(page_of(None), 1);
        assert_eq!(page_of(s("3").as_ref()), 3);
        assert_eq!(page_of(s("0").as_ref()), 1);
        assert_eq!(page_of(s("2.5").as_ref()), 1);
        assert_eq!(page_of(s("10001").as_ref()), 1);
        assert_eq!(page_of(s("x").as_ref()), 1);
    }
}
