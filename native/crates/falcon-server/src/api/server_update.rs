//! 当前服务端升级的 HTTP 接口；节点网关可向节点转发同一组请求。

use axum::{Json, Router, extract::{Request, State}, http::{Method, StatusCode}, middleware::Next, response::{IntoResponse, Response}, routing::get};
use falcon_proto::{ServerUpdateCheck, ServerUpdateTask};

use super::{AppState, auth_routes::LenientJson, error::{ApiError, ApiResult}};
use crate::server_update;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/system/update", get(check).post(start))
        .route("/api/system/update/status", get(status))
        .route("/api/system/update/staged", axum::routing::post(staged))
        .route("/api/system/update/health", get(health))
}

async fn check() -> Json<ServerUpdateCheck> { Json(server_update::check().await) }

async fn start(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<ServerUpdateTask>> {
    let version = body.0.get("version").and_then(|v| v.as_str()).filter(|v| !v.is_empty())
        .ok_or_else(|| ApiError::bad_request("version 必须是稳定 Release 的版本号"))?;
    server_update::start(state, version).map(Json).map_err(|err| ApiError::conflict(format!("{err:#}")))
}

fn staged_headers(state: &AppState, headers: &axum::http::HeaderMap) -> ApiResult<(String, String)> {
    use subtle::ConstantTimeEq as _;
    let authorization = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ")).ok_or_else(ApiError::unauthorized)?;
    let expected = crate::nodes::bootstrap::node_token(&state.config.data_dir)?;
    if !bool::from(authorization.as_bytes().ct_eq(expected.as_bytes())) { return Err(ApiError::unauthorized()); }
    let header = |name: &str| -> ApiResult<String> {
        headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
            .ok_or_else(|| ApiError::bad_request(format!("缺少升级头 {name}")))
    };
    Ok((header("x-falcon-update-version")?, header("x-falcon-update-sha256")?))
}

async fn staged(State(state): State<AppState>, req: Request) -> ApiResult<Json<ServerUpdateTask>> {
    let (version, checksum) = staged_headers(&state, req.headers())?;
    let bytes = axum::body::to_bytes(req.into_body(), 512 * 1024 * 1024).await
        .map_err(|err| ApiError::bad_request(format!("读取升级产物失败：{err}")))?;
    server_update::stage_verified_update(state, &version, bytes.to_vec(), &checksum).await
        .map(Json).map_err(|err| ApiError::bad_request(format!("{err:#}")))
}

async fn status(State(state): State<AppState>) -> ApiResult<Json<ServerUpdateTask>> {
    Ok(Json(server_update::status(&state.config.data_dir)?))
}

async fn health(State(state): State<AppState>) -> ApiResult<Json<server_update::UpdateHealth>> {
    Ok(Json(server_update::health(&state.config, &state.db)?))
}

/// 覆盖整个路由树，包括新建 WS 与公开写接口；存量连接由紧随其后的停服关闭。
/// 数据库备份在旧进程完全退出后才执行，因此维护响应之前已经确认的写入仍进入备份。
pub async fn maintenance(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let status_request = req.method() == Method::GET && matches!(path, "/api/system/update/status" | "/api/system/update/health");
    let restarting_during_update = path == "/api/system/restart"
        && server_update::status(&state.config.data_dir).is_ok_and(|task| task.phase.is_running());
    if !status_request && (server_update::maintenance(&state.config.data_dir) || restarting_during_update) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error":"服务端升级维护中，暂不接受业务请求"}))).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{TestApp, json_of};
    use axum::http::StatusCode;

    #[tokio::test]
    async fn staged_install_requires_node_token_and_matching_checksum() {
        use axum::{body::Body, http::Request};
        let app = TestApp::new();
        assert_eq!(json_of(app.post_json("/api/system/update/staged", serde_json::json!({})).await).await.0, StatusCode::UNAUTHORIZED);
        let token = crate::nodes::bootstrap::node_token(&app.data_dir).unwrap();
        let request = Request::post("/api/system/update/staged")
            .header("authorization", format!("Bearer {token}"))
            .header("x-falcon-update-version", "9.9.9")
            .header("x-falcon-update-sha256", "0".repeat(64))
            .body(Body::from("invalid-artifact")).unwrap();
        assert_eq!(json_of(app.send(request).await).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(crate::server_update::status(&app.data_dir).unwrap().phase, falcon_proto::ServerUpdatePhase::Idle);
    }

    #[tokio::test]
    async fn maintenance_blocks_business_and_ws_but_status_remains_readable() {
        let app = TestApp::new();
        let dir = app.data_dir.join("server-update");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("task.json"), r#"{"phase":"installing","targetVersion":"9.9.9","error":null}"#).unwrap();
        assert_eq!(json_of(app.get("/api/system/update/status").await).await.0, StatusCode::OK);
        assert_eq!(json_of(app.get("/api/projects").await).await.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json_of(app.get("/ws/sessions/test").await).await.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json_of(app.post_json("/api/projects", serde_json::json!({"name":"blocked"})).await).await.0, StatusCode::SERVICE_UNAVAILABLE);
        assert!(app.state.db.list_projects().is_empty());
    }

    #[tokio::test]
    async fn update_status_contract_and_invalid_input() {
        let app = TestApp::new();
        let (code, body) = json_of(app.get("/api/system/update/status").await).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"phase":"idle","targetVersion":null,"error":null}));
        for body in [serde_json::json!({}), serde_json::json!({"version":5})] {
            assert_eq!(json_of(app.post_json("/api/system/update", body).await).await.0, StatusCode::BAD_REQUEST);
        }
        for version in ["1.2.3-rc.1", "https://example.com/a", "0.0.1"] {
            assert_eq!(json_of(app.post_json("/api/system/update", serde_json::json!({"version":version})).await).await.0, StatusCode::CONFLICT);
        }
    }
}
