//! `/api/auth/*`：登录态查询、登录、登出、设 / 改访问密码。这一组不经过登录检查。

use axum::Json;
use axum::extract::{FromRequest, Request, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, body::Bytes};
use falcon_proto::AuthStatus;
use serde_json::{Value, json};

use super::error::{ApiError, ApiResult, ok};
use super::{AppState, cookie};
use crate::auth::{Auth, COOKIE_NAME};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/auth/status", get(status))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/password", post(password))
}

/// Node 版的 `(req.body ?? {})`：没有请求体当空对象；不是 JSON 才 400（Fastify 的形状）。
/// 不看 Content-Type——客户端偶尔不带，Fastify 那边也是靠缺省兜过去的
pub struct LenientJson(pub Value);

impl<S: Send + Sync> FromRequest<S> for LenientJson {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(req, state).await.map_err(|e| ApiError::bad_request(e.to_string()))?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(LenientJson(json!({})));
        }
        serde_json::from_slice(&bytes).map(LenientJson).map_err(|e| ApiError {
            status: StatusCode::BAD_REQUEST,
            body: json!({ "statusCode": 400, "error": "Bad Request", "message": format!("Body is not valid JSON: {e}") }),
        })
    }
}

impl LenientJson {
    /// 字符串字段；缺省或类型不对都是 None（Node 版靠解构 + 真值判断，效果一样）
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }
}

async fn status(State(state): State<AppState>, headers: HeaderMap) -> Json<AuthStatus> {
    Json(AuthStatus {
        required: state.auth.required(),
        authenticated: state.authenticated(&headers),
        password_set: state.auth.password_set(),
    })
}

async fn login(State(state): State<AppState>, body: LenientJson) -> ApiResult<Response> {
    let token = body.str("password").filter(|p| !p.is_empty()).and_then(|p| state.auth.login(p));
    let Some(token) = token else {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "密码错误"));
    };
    Ok(([(SET_COOKIE, Auth::<std::sync::Arc<crate::db::Db>>::set_cookie_header(&token))], ok()).into_response())
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    state.auth.logout(cookie(&headers, COOKIE_NAME).as_deref());
    ([(SET_COOKIE, Auth::<std::sync::Arc<crate::db::Db>>::clear_cookie_header())], ok()).into_response()
}

async fn password(State(state): State<AppState>, headers: HeaderMap, body: LenientJson) -> ApiResult<Json<Value>> {
    let next = body.str("next").unwrap_or("");
    // 按 UTF-16 码元数（= TS 的 .length）
    if next.is_empty() || next.encode_utf16().count() < 6 {
        return Err(ApiError::bad_request("密码至少 6 位"));
    }
    if state.auth.password_set() && !state.authenticated(&headers) {
        return Err(ApiError::unauthorized());
    }
    if !state.auth.set_password(next, body.str("current").filter(|c| !c.is_empty())) {
        return Err(ApiError::bad_request("当前密码不正确"));
    }
    Ok(ok())
}
