//! 中转（ADR 0016）：端口转发与公网发布的增删改查（routes.ts 的 `/api/relays`、`/api/forwards*`、
//! `/api/shares*`）。规则挂在机器上，不挂项目；运行时（隧道、cloudflared 进程）在会话引擎里。
//!
//! 入参原样以 JSON 交给 ForwardManager / ShareManager——校验沿用 TS 的宽松口径（端口可以是
//! 数字字符串等），先反序列化成强类型会把这些行为丢掉。

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use falcon_proto::{PortForward, PublicShare, RelayList};
use serde_json::Value;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::sessions::relay::RelayError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/relays", get(list))
        .route("/api/forwards", post(create_forward))
        .route("/api/forwards/{id}", patch(update_forward).delete(remove_forward))
        .route("/api/shares", post(create_share))
        .route("/api/shares/{id}", patch(update_share).delete(remove_share))
}

fn relay_error(e: RelayError) -> ApiError {
    ApiError::new(e.status(), e.to_string())
}

/// 设置里的「中转」页一次拉全。环境事实（连不上、端口占用、cloudflared 起不来）
/// 写在每条规则的 state/error 里，列表本身不因某条隧道失败而 5xx。
async fn list(State(state): State<AppState>) -> ApiResult<Json<RelayList>> {
    let list = state
        .engine
        .call(|e| async move { RelayList { forwards: e.sessions.forwards.list(), shares: e.sessions.shares.list() } })
        .await?;
    Ok(Json(list))
}

/// 同端口的规则可以建多条；带 enabled 的写入会顺手停掉同端口的其它规则
/// （见 relay_spec），所以前端写完要重拉整张列表，不能只替换这一行。
async fn create_forward(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<PortForward>> {
    let input = body.0;
    let res = state.engine.call(move |e| async move { e.sessions.forwards.create(&input).await }).await?;
    res.map(Json).map_err(relay_error)
}

async fn update_forward(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: LenientJson,
) -> ApiResult<Json<PortForward>> {
    let patch = body.0;
    let res = state.engine.call(move |e| async move { e.sessions.forwards.update(&id, &patch).await }).await?;
    res.map(Json).map_err(relay_error)
}

async fn remove_forward(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let res = state.engine.call(move |e| async move { e.sessions.forwards.remove(&id).await }).await?;
    res.map(|()| ok()).map_err(relay_error)
}

/// cloudflared 永远在 falcon 后端本机跑；挂 SSH Host 的先经主机链路接到本机
async fn create_share(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<PublicShare>> {
    let input = body.0;
    let res = state.engine.call(move |e| async move { e.sessions.shares.create(&input).await }).await?;
    res.map(Json).map_err(relay_error)
}

async fn update_share(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: LenientJson,
) -> ApiResult<Json<PublicShare>> {
    let patch = body.0;
    let res = state.engine.call(move |e| async move { e.sessions.shares.update(&id, &patch).await }).await?;
    res.map(Json).map_err(relay_error)
}

async fn remove_share(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let res = state.engine.call(move |e| async move { e.sessions.shares.remove(&id).await }).await?;
    res.map(|()| ok()).map_err(relay_error)
}
