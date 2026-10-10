//! WebSocket：会话通道 `/ws/sessions/:id` 与 Zellij 安装通道 `/ws/install/:projectId`。
//! 移植自 `packages/server/src/ws.ts`。
//!
//! 未认证照 Node 版先升级、再以 4401 关闭（客户端认这个关闭码去重新登录，不重连）。
//! 单帧上限 1MB（@fastify/websocket 的 maxPayload），超过的帧直接断开。

use std::rc::Rc;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use falcon_proto::{InstallServerMessage, NonDurableReason, WS_CLOSE_UNAUTHORIZED, WS_MAX_PAYLOAD_BYTES};
use futures::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::AppState;
use crate::sessions::viewer::{Viewer, ViewerFrame};
use crate::term_env::sanitize_color_hint;

pub fn router() -> Router<AppState> {
    Router::new().route("/ws/install/{project_id}", get(install)).route("/ws/sessions/{id}", get(session))
}

fn upgrade(ws: WebSocketUpgrade) -> WebSocketUpgrade {
    ws.max_message_size(WS_MAX_PAYLOAD_BYTES).max_frame_size(WS_MAX_PAYLOAD_BYTES)
}

async fn close_unauthorized(mut socket: WebSocket) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame { code: WS_CLOSE_UNAUTHORIZED, reason: Utf8Bytes::from_static("unauthorized") })))
        .await;
}

/// 客户端消息的文本。Node 版对文本帧与二进制帧一视同仁（`raw.toString()` 再 JSON.parse）
fn message_json(msg: &Message) -> Option<Value> {
    match msg {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        Message::Binary(b) => serde_json::from_str(&String::from_utf8_lossy(b)).ok(),
        _ => None,
    }
}

/// Zellij 安装通道。
///
/// 安装是主机级操作、可能耗时数十秒（宿主机自己下载 14 MiB 压缩包再解压），
/// 塞进创建会话的 REST 请求里会让前端只能干等。这里推分阶段状态，
/// 并支持中途取消——取消只影响本次，不写入拒绝状态：取消按钮出现在进度条上，
/// 语境是"我不想等这次传输"，而不是"我撤销授权"。
///
/// 每次连上来都当作用户显式发起的安装（首次或重试），因此一律 fresh：
/// 上一次的失败判定是缓存在 SshLink 上的，不清掉的话重试只会秒回同一个错误。
async fn install(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let authed = state.authenticated(&headers);
    upgrade(ws).on_upgrade(move |socket| async move {
        if !authed {
            return close_unauthorized(socket).await;
        }
        run_install(state, project_id, socket).await;
    })
}

async fn run_install(state: AppState, project_id: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let send = |msg: &InstallServerMessage| Message::Text(serde_json::to_string(msg).unwrap_or_default().into());

    let Some(project) = state.db.get_project(&project_id) else {
        let failed = InstallServerMessage::Failed {
            reason: NonDurableReason::VerifyFailed,
            detail: Some("项目不存在".into()),
            attempts: None,
        };
        let _ = sink.send(send(&failed)).await;
        let _ = sink.close().await;
        return;
    };

    let cancel = CancellationToken::new();
    {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
                // 忽略无法解析的消息
                if message_json(&msg).is_some_and(|v| v["type"] == "cancel") {
                    cancel.cancel();
                }
            }
            cancel.cancel();
        });
    }

    let (stage_tx, mut stage_rx) = mpsc::unbounded_channel::<InstallServerMessage>();
    let job = state.engine.call(move |engine| async move {
        // 后端自动重试的轮次，失败时一并告诉用户——"已经替你试过 3 次"
        // 与"一次都没试"是完全不同的处境，前者说明该去查网络而不是傻点重试。
        let attempts = Rc::new(std::cell::Cell::new(1u32));
        let a2 = attempts.clone();
        let on_stage = move |stage, attempt: u32, command: Option<String>| {
            a2.set(attempt);
            let _ = stage_tx.send(InstallServerMessage::Stage { stage, attempt, command });
        };
        let res = engine.sessions.prepare(&project, Some(&on_stage), Some(&cancel), true).await;
        if res.durable {
            InstallServerMessage::Done
        } else {
            InstallServerMessage::Failed {
                reason: res.reason.unwrap_or(NonDurableReason::VerifyFailed),
                detail: res.detail,
                attempts: Some(attempts.get()),
            }
        }
    });
    tokio::pin!(job);
    let last = loop {
        tokio::select! {
            Some(stage) = stage_rx.recv() => {
                let _ = sink.send(send(&stage)).await;
            }
            res = &mut job => break res,
        }
    };
    // 结果先到、阶段消息还排在后面的情况：先把阶段发完
    while let Ok(stage) = stage_rx.try_recv() {
        let _ = sink.send(send(&stage)).await;
    }
    let last = last.unwrap_or_else(|e| InstallServerMessage::Failed {
        reason: NonDurableReason::VerifyFailed,
        detail: Some(e.to_string()),
        attempts: None,
    });
    let _ = sink.send(send(&last)).await;
    let _ = sink.close().await;
}

async fn session(State(state): State<AppState>, Path(id): Path<String>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    let authed = state.authenticated(&headers);
    upgrade(ws).on_upgrade(move |socket| async move {
        if !authed {
            return close_unauthorized(socket).await;
        }
        run_session(state, id, socket).await;
    })
}

async fn run_session(state: AppState, id: String, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (viewer, mut out) = Viewer::new();
    let viewer_id = viewer.id;

    // 写端：引擎投来的帧依次写进 socket，写完一帧减一次计数（= bufferedAmount 的等价物）。
    // Viewer 被引擎丢掉（会话不存在 / 连接已断）时 channel 关闭，写端随之结束
    let writer = tokio::spawn(async move {
        while let Some(frame) = out.rx.recv().await {
            let msg = match &frame {
                ViewerFrame::Text(t) => Message::Text(t.clone().into()),
                ViewerFrame::Binary(b) => Message::Binary(b.clone()),
            };
            let res = sink.send(msg).await;
            out.written(&frame);
            if res.is_err() {
                break;
            }
        }
        sink
    });

    {
        let id = id.clone();
        state.engine.send(move |engine| engine.sessions.add_viewer(&id, Rc::new(viewer)));
    }

    while let Some(Ok(msg)) = stream.next().await {
        if matches!(msg, Message::Close(_)) {
            break;
        }
        let Some(msg) = message_json(&msg) else { continue };
        let id = id.clone();
        match msg["type"].as_str() {
            Some("input") => {
                if let Some(data) = msg["data"].as_str() {
                    let data = data.to_string();
                    state.engine.send(move |engine| engine.sessions.input(&id, viewer_id, &data));
                }
            }
            Some("resize") => {
                if let (Some(cols), Some(rows)) = (positive_int(&msg["cols"]), positive_int(&msg["rows"])) {
                    state.engine.send(move |engine| engine.sessions.resize(&id, viewer_id, cols, rows));
                }
            }
            Some("appearance") => {
                let hint = sanitize_color_hint(&msg);
                if hint.appearance.is_some() {
                    state.engine.send(move |engine| engine.sessions.set_appearance(&id, viewer_id, hint));
                }
            }
            Some("scroll") => {
                // 认不出的 seek 当成只问位置：宁可多回一次位置，也不要乱滚
                let seek = msg["seek"].as_f64().filter(|s| s.is_finite() && *s >= 0.0);
                state.engine.send(move |engine| engine.sessions.scroll(&id, seek));
            }
            _ => {}
        }
    }

    // Detach：仅断开视图，会话继续运行
    state.engine.send(move |engine| engine.sessions.remove_viewer(&id, viewer_id));
    // 写端在引擎丢掉 Viewer 后自己结束，把 sink 还回来，这里补上关闭握手（不补的话对端
    // 看到的是 1006 异常断开）。会话不存在时 Viewer 早就被丢了，socket 照 Node 版一直开着，
    // 等对端自己关
    if let Ok(Ok(mut sink)) = tokio::time::timeout(std::time::Duration::from_secs(5), writer).await {
        let _ = sink.close().await;
    }
}

/// `Number.isInteger(x) && x > 0`，且装得进 PTY 的 16 位 winsize
fn positive_int(v: &Value) -> Option<u16> {
    let f = v.as_f64()?;
    (f.is_finite() && f.trunc() == f && f > 0.0 && f <= f64::from(u16::MAX)).then_some(f as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resize_wants_positive_integers() {
        assert_eq!(positive_int(&json!(80)), Some(80));
        assert_eq!(positive_int(&json!(80.0)), Some(80));
        assert_eq!(positive_int(&json!(80.5)), None);
        assert_eq!(positive_int(&json!(0)), None);
        assert_eq!(positive_int(&json!(-3)), None);
        assert_eq!(positive_int(&json!("80")), None);
        assert_eq!(positive_int(&json!(70000)), None);
    }

    #[test]
    fn binary_frames_carry_json_too() {
        let v = message_json(&Message::Binary(bytes::Bytes::from_static(br#"{"type":"cancel"}"#))).unwrap();
        assert_eq!(v["type"], "cancel");
        assert!(message_json(&Message::Text("nope".into())).is_none());
    }
}
