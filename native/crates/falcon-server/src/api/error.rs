//! 错误形状，照 Node 版：
//! - 路由自己回的错是 `{ "error": "<给人看的话>" }`，有的带 `reason` / `code` 等字段
//!   （客户端读 `.error`，按需收窄额外字段——web 的 ApiRequestError、原生的 ApiError）；
//! - 没被路由接住的异常，Fastify 回 `{ statusCode, error: "Internal Server Error", message }`，
//!   这里的 [`ApiError::internal`] 产出同一形状。

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    /// `{ error: message }`
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        ApiError { status, body: json!({ "error": message.into() }) }
    }

    /// `{ error: message, ...extra }`。extra 的键排在 error 后面（与 Node 版对象字面量的顺序一致）
    pub fn with(status: StatusCode, message: impl Into<String>, extra: Value) -> Self {
        let mut body = Map::new();
        body.insert("error".into(), Value::String(message.into()));
        if let Value::Object(more) = extra {
            body.extend(more);
        }
        ApiError { status, body: Value::Object(body) }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "未认证")
    }

    /// 没被路由接住的异常（Fastify 的默认 500 形状）
    pub fn internal(message: impl std::fmt::Display) -> Self {
        let message = message.to_string();
        log::error!("内部错误：{message}");
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: json!({ "statusCode": 500, "error": "Internal Server Error", "message": message }),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        ApiError::internal(format!("{err:#}"))
    }
}

impl From<crate::engine::EngineGone> for ApiError {
    fn from(err: crate::engine::EngineGone) -> Self {
        ApiError::internal(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// `{ ok: true }`
pub fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}
