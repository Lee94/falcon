//! AI 助手的 REST 与 WS 路由（docs/design/ai-assistant.md 第 8 节）。
//!
//! 一段对话 = 一个终端会话 + 一段 AI 上下文。终端会话建出来之后就是普通会话——WS 附着、resize、
//! 贴图都走 `/api/sessions/*` 与 `/ws/sessions/:id` 那一套；对话自己的条目走 `/ws/assistant/threads/:id`。

use axum::Json;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use falcon_proto::Session;
use falcon_proto::assistant::{AssistantClientMessage, AssistantSettings, AssistantSettingsPatch, ThreadSummary};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::sync::mpsc;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult, ok};
use crate::assistant::settings;
use crate::sessions::machine::Machine;
use crate::term_env::sanitize_color_hint;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/assistant/threads", get(list_threads))
        .route("/api/assistant/machines/{machine}/threads", post(create_thread))
        .route("/api/assistant/threads/{id}", axum::routing::patch(patch_thread).delete(delete_thread))
        .route("/api/assistant/threads/{id}/shell", post(thread_shell))
        .route("/api/assistant/settings", get(get_settings).put(put_settings))
}

/// 对话的 WS 自己验身份（与 `/ws/sessions/:id` 同一个口径），不挂在 require_login 后面
pub fn ws_router() -> Router<AppState> {
    Router::new().route("/ws/assistant/threads/{id}", get(thread_ws))
}

/// `:machine` 存在吗（本机永远在；SSH Host 可能刚被删）
fn check_machine(state: &AppState, machine: &str) -> ApiResult<()> {
    match Machine::from_param(machine) {
        Machine::Host(id) if state.db.get_host(&id).is_none() => Err(ApiError::not_found("主机不存在")),
        _ => Ok(()),
    }
}

/// 全部对话（跨机器），新的在前
async fn list_threads(State(state): State<AppState>) -> ApiResult<Json<Vec<ThreadSummary>>> {
    let list = state.engine.call(move |e| async move { e.assistant.list() }).await?;
    Ok(Json(list))
}

/// 新对话：在这台机器上开一个终端，配一段新的 AI 上下文。请求体带 agent 与配色的深浅
async fn create_thread(
    State(state): State<AppState>,
    Path(machine): Path<String>,
    body: LenientJson,
) -> ApiResult<Json<ThreadSummary>> {
    check_machine(&state, &machine)?;
    let agent = body.str("agent").map(str::to_string);
    let hint = sanitize_color_hint(&body.0);
    let res = state.engine.call(move |e| async move { e.assistant.create(&machine, agent.as_deref(), hint).await }).await?;
    res.map(Json).map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, e))
}

/// 对话的终端：活着就给，丢了（shell 退出了）就另开一个
async fn thread_shell(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<Session>> {
    let hint = sanitize_color_hint(&body.0);
    let res = state.engine.call(move |e| async move { e.assistant.shell(&id, hint).await }).await?;
    match res {
        Ok(session) => Ok(Json(session)),
        Err(e) if e == "对话不存在" => Err(ApiError::not_found(e)),
        Err(e) => Err(ApiError::new(StatusCode::BAD_GATEWAY, e)),
    }
}

/// 换 agent
async fn patch_thread(State(state): State<AppState>, Path(id): Path<String>, body: LenientJson) -> ApiResult<Json<ThreadSummary>> {
    let Some(agent) = body.str("agent").map(str::to_string) else { return Err(ApiError::bad_request("缺少 agent")) };
    let res = state.engine.call(move |e| async move { e.assistant.set_agent(&id, &agent) }).await?;
    res.map(Json).map_err(ApiError::bad_request)
}

/// 删对话：在跑的停下、它的终端终止
async fn delete_thread(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    state.engine.call(move |e| async move { e.assistant.delete(&id).await }).await?;
    Ok(ok())
}

async fn get_settings(State(state): State<AppState>) -> Json<AssistantSettings> {
    Json(settings::view(&state.db, &state.secrets))
}

async fn put_settings(State(state): State<AppState>, body: LenientJson) -> ApiResult<Json<AssistantSettings>> {
    // 逐字段按 JS 口径取（api/input.rs 的约定），类型不对的字段当没带
    let patch = AssistantSettingsPatch {
        base_url: body.str("baseUrl").map(str::to_string),
        model: body.str("model").map(str::to_string),
        effort: body.str("effort").map(str::to_string),
        api_key: body.str("apiKey").map(str::to_string),
        auto_approve: body.0.get("autoApprove").and_then(Value::as_bool),
        acp_agents: body.0.get("acpAgents").and_then(|v| serde_json::from_value(v.clone()).ok()),
    };
    settings::apply(&state.db, &state.secrets, &patch).map_err(ApiError::bad_request)?;
    Ok(Json(settings::view(&state.db, &state.secrets)))
}

async fn thread_ws(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let authed = state.authenticated(&headers);
    ws.max_message_size(falcon_proto::WS_MAX_PAYLOAD_BYTES).on_upgrade(move |socket| async move {
        if !authed {
            let mut socket = socket;
            let _ = socket
                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: falcon_proto::WS_CLOSE_UNAUTHORIZED,
                    reason: "unauthorized".into(),
                })))
                .await;
            return;
        }
        run_thread_ws(state, id, socket).await;
    })
}

/// 引擎推来的 JSON 原样写进 socket；客户端发来的投回引擎。应用层心跳（`{"type":"ping"}`）
/// 直接回 pong，不经引擎
async fn run_thread_ws(state: AppState, id: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let tid = id.clone();
    let viewer = state.engine.call(move |e| async move { e.assistant.add_viewer(&tid, tx) }).await.ok().flatten();
    let Some(vid) = viewer else {
        let _ = sink
            .send(Message::Text(r#"{"type":"status","status":"error"}"#.into()))
            .await;
        let _ = sink.close().await;
        return;
    };
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<()>();
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(text) => if sink.send(Message::Text(text.into())).await.is_err() { break },
                    // 对话被删了：发送端随之落下
                    None => break,
                },
                Some(()) = pong_rx.recv() => {
                    if sink.send(Message::Text(r#"{"type":"pong"}"#.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = sink.close().await;
    });
    while let Some(Ok(msg)) = stream.next().await {
        let text = match &msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        if text.contains("\"ping\"") && serde_json::from_str::<Value>(&text).is_ok_and(|v| v["type"] == "ping") {
            let _ = pong_tx.send(());
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<AssistantClientMessage>(&text) else { continue };
        let tid = id.clone();
        state.engine.send(move |e| e.assistant.handle(&tid, parsed));
    }
    let tid = id.clone();
    state.engine.send(move |e| e.assistant.remove_viewer(&tid, vid));
    writer.abort();
}

/// `:machine` 是 `local` 或 SSH Host 的 id；请求体与新建会话一样带终端配色的深浅
/// （`appearance` / `background` / `foreground`），只在真的新建时用得上
#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use crate::api::test_support::{TestApp, json_of};
    use crate::db::SessionRow;

    #[tokio::test]
    async fn unknown_host_is_404() {
        let app = TestApp::new();
        let (status, _) = json_of(app.post_json("/api/assistant/machines/no-such-host/threads", json!({})).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = json_of(app.post_json("/api/assistant/threads/no-such-thread/shell", json!({})).await).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn session_list_hides_machine_sessions() {
        let app = TestApp::new();
        app.local_project("p1");
        for (id, owner) in [("s1", "p1"), ("s2", "machine:local"), ("s3", "machine:host:h1")] {
            app.state.db.insert_session(&SessionRow {
                id: id.into(),
                project_id: owner.into(),
                state: "dead".into(),
                created_at: 1,
                last_active_at: 1,
                ..Default::default()
            });
        }
        let (status, body) = json_of(app.get("/api/sessions").await).await;
        assert_eq!(status, StatusCode::OK);
        let ids: Vec<&str> = body.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["s1"]);
    }
}
