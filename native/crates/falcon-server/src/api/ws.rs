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
use falcon_proto::{InstallServerMessage, NonDurableReason, ServerMessage, WS_CLOSE_UNAUTHORIZED, WS_MAX_PAYLOAD_BYTES};
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

/// 写端一次 flush 最多攒几帧
const WRITE_BATCH: usize = 64;

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
    // 应用层心跳（`{"type":"ping"}`，浏览器版发不了 ping 帧）的回话直接交给写端，不经引擎：
    // 与 ping 帧由协议层自动回 pong 一样，不看会话在不在、Viewer 还挂没挂着
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<()>();

    // 写端：引擎投来的帧依次写进 socket，写完一帧减一次计数（= bufferedAmount 的等价物）。
    //
    // 连着到的几帧攒成一次 flush（Node 的 ws 也是同一轮事件循环里的 send 合成一次写）：
    // 少几次 syscall，也让"state dead + title null"这种成对的控制消息落在同一段 TCP 里
    //
    // Viewer 被丢掉之后写端还留着回 pong，直到读端结束（`pong_tx` 随之落下）才把 sink 还回来
    let writer = tokio::spawn(async move {
        let to_msg = |frame: &ViewerFrame| match frame {
            ViewerFrame::Text(t) => Message::Text(t.clone().into()),
            ViewerFrame::Binary(b) => Message::Binary(b.clone()),
        };
        let pong = serde_json::to_string(&ServerMessage::Pong).unwrap_or_default();
        let mut viewer_open = true;
        'outer: loop {
            let first = tokio::select! {
                frame = out.rx.recv(), if viewer_open => match frame {
                    Some(frame) => frame,
                    None => {
                        viewer_open = false;
                        continue 'outer;
                    }
                },
                asked = pong_rx.recv() => match asked {
                    Some(()) => {
                        if sink.send(Message::Text(pong.clone().into())).await.is_err() {
                            break 'outer;
                        }
                        continue 'outer;
                    }
                    None => break 'outer,
                },
            };
            let mut batch = vec![first];
            while batch.len() < WRITE_BATCH {
                match out.rx.try_recv() {
                    Ok(f) => batch.push(f),
                    Err(_) => break,
                }
            }
            let mut ok = true;
            for frame in &batch {
                if sink.feed(to_msg(frame)).await.is_err() {
                    ok = false;
                    break;
                }
            }
            if ok {
                ok = sink.flush().await.is_ok();
            }
            // 写完（或 socket 已死、丢掉了）才减计数：这就是 bufferedAmount 的口径
            for frame in &batch {
                out.written(frame);
            }
            if !ok {
                break 'outer;
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
            Some("ping") => {
                let _ = pong_tx.send(());
            }
            _ => {}
        }
    }
    drop(pong_tx);

    // Detach：仅断开视图，会话继续运行
    state.engine.send(move |engine| engine.sessions.remove_viewer(&id, viewer_id));
    // 读端结束、`pong_tx` 落下后写端就收工，把 sink 还回来，这里补上关闭握手（不补的话对端
    // 看到的是 1006 异常断开）。会话不存在时 Viewer 早就被丢了，socket 照 Node 版一直开着
    // （写端还在回 pong），等对端自己关
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

    /// 应用层心跳（浏览器版拿它代替 ping 帧）：会话不存在、Viewer 已被引擎丢掉时照样回 pong，
    /// 与协议层的 ping 帧一样——否则浏览器版在"会话不存在"的页面上会每个心跳周期重连一次
    #[tokio::test]
    async fn app_level_ping_gets_a_pong_even_without_a_session() {
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        let app = crate::api::test_support::TestApp::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router.clone();
        tokio::spawn(async move { axum::serve(listener, router).await });
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async(format!("ws://{addr}/ws/sessions/no-such-session"), tcp).await.unwrap();
        ws.send(WsMessage::Text(r#"{"type":"ping"}"#.into())).await.unwrap();
        let got_pong = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(Ok(msg)) = ws.next().await {
                if let WsMessage::Text(text) = msg
                    && serde_json::from_str::<ServerMessage>(text.as_str()).ok() == Some(ServerMessage::Pong)
                {
                    return true;
                }
            }
            false
        })
        .await;
        assert_eq!(got_pong, Ok(true));
    }

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
