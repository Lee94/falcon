//! sudo askpass 的三条路由（routes.ts 的 `/api/askpass*`）。helper 的长轮询不走登录 cookie，
//! 验的是写进宿主机 conf 的专用 Bearer 令牌——helper 不是浏览器。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use serde_json::{Value, json};

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::askpass::hub::AskpassError;

/// 挂在公开路由上（自己验 Bearer）
pub fn helper_router() -> Router<AppState> {
    Router::new().route("/api/askpass", post(request))
}

/// 要登录：网页对话框列出 / 答复
pub fn router() -> Router<AppState> {
    Router::new().route("/api/askpass/pending", get(pending)).route("/api/askpass/{id}/answer", post(answer))
}

/// sudo askpass helper 长轮询。
async fn request(State(state): State<AppState>, headers: HeaderMap, body: LenientJson) -> ApiResult<Json<Value>> {
    let header = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok());
    if !state.askpass.token_matches(header) {
        return Err(ApiError::unauthorized());
    }
    let prompt = body.str("prompt").filter(|p| !p.is_empty()).unwrap_or("Password:");
    match state.askpass.request(prompt, body.str("sessionId")).await {
        Ok(password) => Ok(Json(json!({ "password": password }))),
        Err(AskpassError::Cancelled) => Err(ApiError::conflict("cancelled")),
        Err(AskpassError::Timeout) => Err(ApiError::new(StatusCode::GATEWAY_TIMEOUT, "timeout")),
    }
}

async fn pending(State(state): State<AppState>) -> Json<Value> {
    Json(Value::Array(
        state.askpass.pending_prompts().into_iter().map(|p| json!({ "id": p.id, "prompt": p.prompt })).collect(),
    ))
}

async fn answer(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Value>> {
    if body.0.get("cancel") == Some(&Value::Bool(true)) {
        if !state.askpass.cancel(&id) {
            return Err(ApiError::not_found("不存在"));
        }
        return Ok(ok());
    }
    let Some(password) = body.str("password") else { return Err(ApiError::bad_request("missing password")) };
    if !state.askpass.answer(&id, password.to_string()) {
        return Err(ApiError::not_found("不存在"));
    }
    Ok(ok())
}
