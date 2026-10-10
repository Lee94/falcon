//! `/api/system`（S5 再把 `/api/fs/*`、`/api/shells` 搬进来）

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::routing::get;
use falcon_proto::SystemInfo;

use super::error::ApiResult;
use super::{AppState, VERSION};
use crate::sessions::login_env::node_platform;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/system", get(system))
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
