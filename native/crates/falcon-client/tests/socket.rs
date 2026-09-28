//! 会话 socket 与安装通道对着脚本化的假服务端：开场时序、强发 resize、去重、分片、
//! 4401 重登、退避与立即重连、心跳判死、干净关闭。

mod support;

use std::sync::Arc;
use std::time::Duration;

use falcon_client::{InstallEvent, InstallSocket, SessionEvent, SessionSocket, SocketOptions};
use falcon_proto::{
    InstallServerMessage, SessionState, TERM_FRAME_OUTPUT, TERM_FRAME_REPLAY, TermAppearance,
    WS_MAX_PAYLOAD_BYTES, ZellijInstallStage,
};
use futures::StreamExt;
use futures::executor::block_on;
use serde_json::json;
use support::{Events, HttpReq, HttpResp, Mock, WsIn};

fn fast() -> SocketOptions {
    SocketOptions {
        backoff_base: Duration::from_millis(20),
        backoff_max: Duration::from_millis(100),
        ping_interval: Duration::from_secs(30),
        connect_timeout: Duration::from_secs(5),
        probe_timeout: Duration::from_millis(300),
    }
}

/// 服务端接受连接：推一条 state（客户端收到才报 Connected），把这两个事件吃掉。
fn accept(conn: &support::WsConn, events: &Events) -> SessionEvent {
    conn.hello();
    let ev = events.next();
    assert!(matches!(ev, SessionEvent::Connected { .. }), "{ev:?}");
    assert_eq!(events.next(), SessionEvent::State { state: SessionState::Active, dead_reason: None });
    ev
}

fn plain_server() -> Mock {
    Mock::start(|_| HttpResp::json(404, json!({ "error": "not found" })))
}

#[test]
fn every_new_socket_sends_appearance_then_forced_resize() {
    let mock = plain_server();
    let client = mock.client();
    let (sink, events) = Events::new();
    let sock = SessionSocket::open_with(&client, "s-1", sink, fast());
    // 连上之前设好的尺寸与外观：都记着，连上后按顺序补发
    sock.set_appearance(TermAppearance::Dark, Some("#0a0a0a"), Some("#fafafa"));
    sock.resize(120, 40);

    let conn = mock.next_ws();
    assert_eq!(conn.path, "/ws/sessions/s-1");
    assert_eq!(
        conn.recv_json(),
        json!({ "type": "appearance", "appearance": "dark", "background": "#0a0a0a", "foreground": "#fafafa" })
    );
    assert_eq!(conn.recv_json(), json!({ "type": "resize", "cols": 120, "rows": 40 }));
    // 升级成功还不算连上：服务端推来第一条消息才报 Connected
    assert!(events.try_next(Duration::from_millis(100)).is_none());
    assert_eq!(accept(&conn, &events), SessionEvent::Connected { reconnected: false });

    // 同一条 socket 上相同尺寸去重，变了才发
    sock.resize(120, 40);
    sock.resize(100, 30);
    assert_eq!(conn.recv_json(), json!({ "type": "resize", "cols": 100, "rows": 30 }));
    conn.assert_silent(Duration::from_millis(100));

    // 断开：新 socket 仍然先 appearance、再强发最近一次的尺寸（哪怕与上次一样）
    conn.kill();
    let ev = events.until(|e| matches!(e, SessionEvent::Disconnected { .. }));
    assert!(matches!(ev, SessionEvent::Disconnected { retry_attempt: 1, .. }), "{ev:?}");
    let conn2 = mock.next_ws();
    assert_eq!(conn2.recv_json()["type"], "appearance");
    assert_eq!(conn2.recv_json(), json!({ "type": "resize", "cols": 100, "rows": 30 }));
    assert_eq!(accept(&conn2, &events), SessionEvent::Connected { reconnected: true });
}

#[test]
fn no_size_no_resize() {
    // 格子没量好之前不发 resize
    let mock = plain_server();
    let (sink, events) = Events::new();
    let sock = SessionSocket::open_with(&mock.client(), "s-1", sink, fast());
    let conn = mock.next_ws();
    accept(&conn, &events);
    conn.assert_silent(Duration::from_millis(100));
    sock.resize(0, 10); // 非法尺寸忽略
    conn.assert_silent(Duration::from_millis(100));
}

#[test]
fn frames_and_control_messages_arrive_in_order() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let _sock = SessionSocket::open_with(&mock.client(), "s-1", sink, fast());
    let conn = mock.next_ws();

    // 第一条消息是二进制回放：先报 Connected，紧接着就是它
    let mut replay = vec![TERM_FRAME_REPLAY];
    replay.extend_from_slice("\x1b[2J你好".as_bytes());
    conn.send_binary(replay);
    conn.send_text(r#"{"type":"state","state":"active"}"#);
    let mut out = vec![TERM_FRAME_OUTPUT];
    out.extend_from_slice(b"$ ");
    conn.send_binary(out);
    conn.send_binary(vec![0x07, b'x']); // 认不出的帧类型：丢掉
    conn.send_text(r#"{"type":"bell"}"#); // 认不出的控制消息：丢掉
    conn.send_text("not json");
    conn.send_text(r#"{"type":"title","title":"vim a.rs"}"#);
    conn.send_text(r#"{"type":"title","title":null}"#);
    conn.send_text(r#"{"type":"reconnecting","attempt":2}"#);
    conn.send_text(r#"{"type":"error","message":"接回失败：boom"}"#);
    conn.send_text(r#"{"type":"askpass","id":"ap1","prompt":"Password:"}"#);
    conn.send_text(r#"{"type":"state","state":"dead","deadReason":"exited"}"#);

    let got: Vec<SessionEvent> = (0..10).map(|_| events.next()).collect();
    assert_eq!(
        got,
        vec![
            SessionEvent::Connected { reconnected: false },
            SessionEvent::Replay("\x1b[2J你好".as_bytes().to_vec().into()),
            SessionEvent::State { state: SessionState::Active, dead_reason: None },
            SessionEvent::Output(b"$ ".to_vec().into()),
            SessionEvent::Title(Some("vim a.rs".into())),
            SessionEvent::Title(None),
            SessionEvent::Reconnecting { attempt: 2 },
            SessionEvent::Error("接回失败：boom".into()),
            SessionEvent::Askpass { id: "ap1".into(), prompt: "Password:".into() },
            SessionEvent::State {
                state: SessionState::Dead,
                dead_reason: Some(falcon_proto::DeadReason::Exited)
            },
        ]
    );
}

#[test]
fn big_input_is_split_under_max_payload() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let sock = SessionSocket::open_with(&mock.client(), "s-1", sink, fast());
    let conn = mock.next_ws();
    accept(&conn, &events);

    let data = "粘贴\"一大段\"\u{1b}[0m\r\n".repeat(120_000); // ~3MB，含转义膨胀
    sock.send_input(data.clone());
    let mut back = String::new();
    let mut frames = 0;
    while back.len() < data.len() {
        let text = conn.recv_text();
        assert!(text.len() <= WS_MAX_PAYLOAD_BYTES, "帧 {} 字节", text.len());
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "input");
        back.push_str(v["data"].as_str().unwrap());
        frames += 1;
    }
    assert!(frames >= 3);
    assert_eq!(back, data);
}

#[test]
fn input_while_disconnected_is_dropped() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let opts = SocketOptions { backoff_base: Duration::from_millis(300), ..fast() };
    let sock = SessionSocket::open_with(&mock.client(), "s-1", sink, opts);
    let conn = mock.next_ws();
    accept(&conn, &events);
    conn.kill();
    events.until(|e| matches!(e, SessionEvent::Disconnected { .. }));
    sock.send_input("typed while offline\r");
    let conn2 = mock.next_ws();
    accept(&conn2, &events);
    sock.send_input("after\r");
    assert_eq!(conn2.recv_json(), json!({ "type": "input", "data": "after\r" }));
}

/// 登录接口 + WS：只认最新签发的 token，模拟"后端重启后内存 token 全丢"。
fn auth_server() -> Mock {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let n = Arc::new(AtomicUsize::new(0));
    Mock::start(move |req: &HttpReq| {
        if req.path_only() == "/api/auth/login" && req.json()["password"] == "pw-123456" {
            let tok = format!("tok-{}", n.fetch_add(1, Ordering::SeqCst) + 1);
            return HttpResp::json(200, json!({ "ok": true }))
                .header("Set-Cookie", &format!("falcon_token={tok}; Path=/; HttpOnly"));
        }
        HttpResp::json(401, json!({ "error": "未认证" }))
    })
}

#[test]
fn close_4401_relogs_in_and_reconnects_with_the_new_cookie() {
    let mock = auth_server();
    let client = mock.client();
    client.set_relogin_password(Some("pw-123456".into()));
    block_on(client.login("pw-123456")).unwrap();
    let (sink, events) = Events::new();
    let sock = SessionSocket::open_with(&client, "s-1", sink, fast());
    sock.resize(80, 24);

    let conn = mock.next_ws();
    assert_eq!(conn.cookie.as_deref(), Some("falcon_token=tok-1"));
    conn.recv_json();
    // 服务端重启过：旧 token 不认了
    conn.close(4401);
    let conn2 = mock.next_ws();
    assert_eq!(conn2.cookie.as_deref(), Some("falcon_token=tok-2"), "应当先重登再重连");
    assert_eq!(conn2.recv_json(), json!({ "type": "resize", "cols": 80, "rows": 24 }));
    conn2.hello();
    // 重登是即时的，不经退避；被 4401 拒掉的那条连接既不报 Connected，也不报
    // Disconnected / Unauthorized——app 只看到一次干干净净的"连上了"
    let evs: Vec<_> = std::iter::from_fn(|| events.try_next(Duration::from_millis(200))).collect();
    assert_eq!(
        evs,
        vec![
            SessionEvent::Connected { reconnected: false },
            SessionEvent::State { state: SessionState::Active, dead_reason: None },
        ]
    );
}

#[test]
fn close_4401_without_password_parks_until_someone_logs_in() {
    let mock = auth_server();
    let client = mock.client();
    let (sink, events) = Events::new();
    let _sock = SessionSocket::open_with(&client, "s-1", sink, fast());
    let conn = mock.next_ws();
    conn.close(4401);
    events.until(|e| *e == SessionEvent::Unauthorized);
    // 停住了：不再重连
    assert!(mock.next_ws_within(Duration::from_millis(300)).is_none());
    // 用户在登录框里登好 → socket 自己接着连，带上新 cookie
    block_on(client.login("pw-123456")).unwrap();
    let conn2 = mock.next_ws();
    assert_eq!(conn2.cookie.as_deref(), Some("falcon_token=tok-1"));
    accept(&conn2, &events);
}

#[test]
fn repeated_4401_after_relogin_does_not_loop() {
    let mock = auth_server();
    let client = mock.client();
    client.set_relogin_password(Some("pw-123456".into()));
    let (sink, events) = Events::new();
    let _sock = SessionSocket::open_with(&client, "s-1", sink, fast());
    mock.next_ws().close(4401);
    // 重登成功，但新连接还是 4401（比如 token 被别处立刻作废了）：别死循环
    mock.next_ws().close(4401);
    events.until(|e| *e == SessionEvent::Unauthorized);
    assert!(mock.next_ws_within(Duration::from_millis(300)).is_none());
}

#[test]
fn reconnect_now_skips_the_backoff() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let opts = SocketOptions { backoff_base: Duration::from_secs(20), backoff_max: Duration::from_secs(20), ..fast() };
    let sock = SessionSocket::open_with(&mock.client(), "s-1", sink, opts);
    let conn = mock.next_ws();
    accept(&conn, &events);
    conn.kill();
    let ev = events.until(|e| matches!(e, SessionEvent::Disconnected { .. }));
    assert_eq!(ev, SessionEvent::Disconnected { retry_attempt: 1, retry_in: Duration::from_secs(20) });
    sock.reconnect_now();
    let conn2 = mock.next_ws_within(Duration::from_secs(2)).expect("应当立刻重连");
    accept(&conn2, &events);
}

#[test]
fn reconnect_now_while_connected_probes_a_half_open_connection() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let sock = SessionSocket::open_with(&mock.client(), "s-1", sink, fast());
    let conn = mock.next_ws();
    accept(&conn, &events);
    // 活着的连接：探一下，服务端回 pong，什么都不发生
    sock.reconnect_now();
    assert_eq!(conn.recv_raw(Duration::from_secs(2)), Some(WsIn::Ping));
    assert!(events.try_next(Duration::from_millis(500)).is_none());
    // 睡眠醒来后的半开连接：不回 pong → probe_timeout 后断开重连
    conn.freeze();
    sock.reconnect_now();
    events.until(|e| matches!(e, SessionEvent::Disconnected { .. }));
    assert!(mock.next_ws_within(Duration::from_secs(2)).is_some());
}

#[test]
fn heartbeat_pings_and_detects_a_dead_peer() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let opts = SocketOptions { ping_interval: Duration::from_millis(100), ..fast() };
    let _sock = SessionSocket::open_with(&mock.client(), "s-1", sink, opts);
    let conn = mock.next_ws();
    accept(&conn, &events);
    assert_eq!(conn.recv_raw(Duration::from_secs(2)), Some(WsIn::Ping));
    assert!(events.try_next(Duration::from_millis(350)).is_none(), "有 pong 就不该断");
    conn.freeze();
    events.until(|e| matches!(e, SessionEvent::Disconnected { .. }));
    assert!(mock.next_ws_within(Duration::from_secs(2)).is_some());
}

#[test]
fn close_is_clean_and_final() {
    let mock = plain_server();
    let (sink, events) = Events::new();
    let sock = SessionSocket::open_with(&mock.client(), "s-1", sink, fast());
    let conn = mock.next_ws();
    accept(&conn, &events);
    drop(sock);
    // 发了 Close 帧（服务端看到的是正常关闭，不是 TCP 被掐）
    assert_eq!(conn.recv_raw(Duration::from_secs(2)), Some(WsIn::Closed));
    assert!(mock.next_ws_within(Duration::from_millis(300)).is_none(), "关了就不再重连");
    assert!(events.try_next(Duration::from_millis(100)).is_none());
}

#[test]
fn install_socket_streams_until_done_and_closed() {
    let mock = plain_server();
    let client = mock.client();
    let mut install = InstallSocket::open(&client, "p 1");
    let conn = mock.next_ws();
    assert_eq!(conn.path, "/ws/install/p%201");
    conn.send_text(r#"{"type":"stage","stage":"probing","attempt":1}"#);
    conn.send_text(r#"{"type":"stage","stage":"downloading","attempt":2,"command":"curl -fsSL x"}"#);
    conn.send_text(r#"{"type":"log","line":"ignored"}"#);
    conn.send_text(r#"{"type":"done"}"#);
    conn.close(1000);
    let evs: Vec<InstallEvent> = block_on(async { (&mut install).collect::<Vec<_>>().await });
    assert_eq!(
        evs,
        vec![
            InstallEvent::Message(InstallServerMessage::Stage {
                stage: ZellijInstallStage::Probing,
                attempt: 1,
                command: None
            }),
            InstallEvent::Message(InstallServerMessage::Stage {
                stage: ZellijInstallStage::Downloading,
                attempt: 2,
                command: Some("curl -fsSL x".into())
            }),
            InstallEvent::Message(InstallServerMessage::Done),
            InstallEvent::Closed,
        ]
    );
}

#[test]
fn install_cancel_sends_cancel_then_closes() {
    let mock = plain_server();
    let mut install = InstallSocket::open(&mock.client(), "p1");
    let conn = mock.next_ws();
    conn.send_text(r#"{"type":"stage","stage":"probing","attempt":1}"#);
    assert!(matches!(block_on(install.next()), Some(InstallEvent::Message(_))));
    install.cancel();
    assert_eq!(conn.recv_json(), json!({ "type": "cancel" }));
    assert_eq!(conn.recv_raw(Duration::from_secs(2)), Some(WsIn::Closed));
    assert_eq!(block_on(install.next()), Some(InstallEvent::Closed));
    assert_eq!(block_on(install.next()), None);
}

#[test]
fn install_channel_error_and_unauthorized() {
    // 连不上后端
    let dead = falcon_client::FalconClient::new(url::Url::parse("http://127.0.0.1:9").unwrap());
    let evs: Vec<_> = block_on(InstallSocket::open(&dead, "p1").collect::<Vec<_>>());
    assert!(matches!(evs.as_slice(), [InstallEvent::ChannelError(_), InstallEvent::Closed]), "{evs:?}");
    // 4401 且没设密码
    let mock = auth_server();
    let client = mock.client();
    let install = InstallSocket::open(&client, "p1");
    mock.next_ws().close(4401);
    let evs: Vec<_> = block_on(install.collect::<Vec<_>>());
    assert_eq!(evs, vec![InstallEvent::Unauthorized, InstallEvent::Closed]);
}
