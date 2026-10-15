//! 内置 agent 的循环对一个模拟的 Messages API（本机 axum，按请求顺序回放事先写好的 SSE）跑。
//! 不碰真 API、不起 shell（只用读写文件这两个工具），会话引擎那一套照样是真的。

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use falcon_proto::assistant::{
    AssistantClientMessage, AssistantServerMessage, AssistantSettingsPatch, Entry, ThreadStatus, ToolContent, ToolStatus,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::AssistantHub;
use crate::askpass::hub::AskpassHub;
use crate::assistant::settings;
use crate::crypto::SecretBox;
use crate::db::Db;
use crate::sessions::manager::SessionManager;

/// 模拟服务端：第 n 个请求回第 n 个应答（状态码 + SSE 正文），并记下每个请求体
#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<Vec<(u16, String)>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}

async fn messages(State(m): State<Mock>, body: String) -> Response {
    m.requests.lock().unwrap().push(serde_json::from_str(&body).unwrap_or(Value::Null));
    let (status, text) = {
        let mut r = m.replies.lock().unwrap();
        if r.is_empty() { (500, "{\"error\":{\"message\":\"no more replies\"}}".to_string()) } else { r.remove(0) }
    };
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap())
        .header("content-type", if status == 200 { "text/event-stream" } else { "application/json" })
        .body(Body::from(text))
        .unwrap()
}

async fn start_mock(replies: Vec<(u16, String)>) -> (String, Mock) {
    let mock = Mock { replies: Arc::new(Mutex::new(replies)), requests: Arc::default() };
    let app = Router::new().route("/v1/messages", post(messages)).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, mock)
}

/// 把事件写成 SSE 正文
fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap_or("x"), e))
        .collect()
}

fn tool_use_reply(id: &str, name: &str, input: Value) -> String {
    sse(&[
        json!({"type":"message_start","message":{"model":"mock"}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我来看一下"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        json!({"type":"message_stop"}),
    ])
}

fn text_reply(text: &str) -> String {
    sse(&[
        json!({"type":"message_start","message":{"model":"mock"}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
        json!({"type":"message_stop"}),
    ])
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    hub: std::rc::Rc<AssistantHub>,
}

fn fixture(base_url: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let db = Arc::new(Db::open(&data).unwrap());
    let secrets = Arc::new(SecretBox::open(&data).unwrap());
    settings::apply(
        &db,
        &secrets,
        &AssistantSettingsPatch {
            base_url: Some(base_url.into()),
            api_key: Some("test-key".into()),
            model: Some("mock".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let sessions = SessionManager::new(db.clone(), secrets.clone(), data, Arc::new(AskpassHub::new()));
    // 对话会开一个本机终端：起在临时目录、读不到用户真实的 rc，也不部署守护进程
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let home = home.to_string_lossy().into_owned();
    crate::sessions::machine::TEST_HOME.with(|h| *h.borrow_mut() = Some(home.clone()));
    sessions.local.set_prepared(crate::sessions::local::LocalSessiond::non_durable(
        falcon_proto::NonDurableReason::ArchUnsupported,
        None,
    ));
    sessions.local.set_base_env(vec![
        ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        ("HOME".into(), home),
        ("LANG".into(), "en_US.UTF-8".into()),
    ]);
    let hub = AssistantHub::new(db, secrets, sessions);
    Fixture { _dir: dir, root, hub }
}

/// 收观看者的消息，直到对话停在 `want`（或等太久）
async fn wait_status(rx: &mut mpsc::UnboundedReceiver<String>, want: ThreadStatus) -> Vec<AssistantServerMessage> {
    let mut seen = Vec::new();
    loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("等不到状态 {want:?}，收到 {seen:#?}"))
            .expect("观看者被丢了");
        let m: AssistantServerMessage = serde_json::from_str(&msg).unwrap();
        let hit = matches!(&m, AssistantServerMessage::Status { status } if *status == want);
        seen.push(m);
        if hit {
            return seen;
        }
    }
}

fn entries(hub: &AssistantHub, id: &str) -> Vec<Entry> {
    hub.thread(id).unwrap().s.borrow().entries.clone()
}

#[tokio::test(flavor = "current_thread")]
async fn read_file_round_trip_feeds_the_result_back() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, mock) = start_mock(vec![]).await;
            let f = fixture(&url);
            let file = f.root.join("notes.txt");
            std::fs::write(&file, "第一行\nsecond\n").unwrap();
            mock.replies.lock().unwrap().extend([
                (200, tool_use_reply("toolu_1", "read_file", json!({ "path": file.to_string_lossy() }))),
                (200, text_reply("文件里有两行")),
            ]);

            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "看看 notes.txt".into(), context: None });
            wait_status(&mut rx, ThreadStatus::Running).await;
            wait_status(&mut rx, ThreadStatus::Idle).await;

            let es = entries(&f.hub, &t.id);
            assert!(matches!(&es[0], Entry::User { text, .. } if text == "看看 notes.txt"));
            let call = es.iter().find_map(|e| if let Entry::ToolCall(c) = e { Some(c.clone()) } else { None }).unwrap();
            assert_eq!(call.status, ToolStatus::Completed);
            assert_eq!(call.title, file.to_string_lossy());
            assert!(matches!(es.last(), Some(Entry::Agent { text, .. }) if text == "文件里有两行"));
            assert_eq!(f.hub.thread(&t.id).unwrap().summary().title, "看看 notes.txt");

            // 第二个请求带着工具结果：带行号的文件内容
            let reqs = mock.requests.lock().unwrap().clone();
            assert_eq!(reqs.len(), 2);
            let last = reqs[1]["messages"].as_array().unwrap().last().unwrap().clone();
            assert_eq!(last["role"], "user");
            assert_eq!(last["content"][0]["type"], "tool_result");
            assert_eq!(last["content"][0]["tool_use_id"], "toolu_1");
            assert!(last["content"][0]["content"].as_str().unwrap().contains("     1\t第一行"));
            // 网关地址：不带 eager / fallbacks
            assert!(reqs[0].get("fallbacks").is_none());
            // 历史落了库，重读回来一样
            let row = f.hub.db.get_assistant_thread(&t.id).unwrap();
            assert!(row.provider.contains("toolu_1"));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn writing_asks_first_and_a_rejection_stops_the_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, mock) = start_mock(vec![]).await;
            let f = fixture(&url);
            let file = f.root.join("out.txt");
            mock.replies.lock().unwrap().push((
                200,
                tool_use_reply("toolu_w", "write_file", json!({ "path": file.to_string_lossy(), "content": "hi" })),
            ));
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "写个文件".into(), context: None });
            wait_status(&mut rx, ThreadStatus::AwaitingPermission).await;
            let call = entries(&f.hub, &t.id)
                .into_iter()
                .find_map(|e| if let Entry::ToolCall(c) = e { Some(c) } else { None })
                .unwrap();
            assert!(call.permission.as_ref().is_some_and(|p| p.len() == 3));
            assert!(!file.exists());

            f.hub.handle(&t.id, AssistantClientMessage::Permission { tool_call_id: "toolu_w".into(), option_id: "reject".into(), command: None });
            wait_status(&mut rx, ThreadStatus::Idle).await;
            assert!(!file.exists());
            // 没有再请求模型；历史末尾是 is_error 的结果，下一句话会并进这条 user 消息
            assert_eq!(mock.requests.lock().unwrap().len(), 1);
            let th = f.hub.thread(&t.id).unwrap();
            let last = th.s.borrow().messages.last().unwrap().clone();
            assert_eq!(last["content"][0]["is_error"], true);

            // 下一句：并进同一条 user 消息，然后「本对话内总是允许」之后真的写了
            mock.replies.lock().unwrap().extend([
                (200, tool_use_reply("toolu_w2", "write_file", json!({ "path": file.to_string_lossy(), "content": "hi" }))),
                (200, text_reply("写好了")),
            ]);
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "可以写".into(), context: Some("$ ls\r\n".into()) });
            wait_status(&mut rx, ThreadStatus::AwaitingPermission).await;
            f.hub.handle(&t.id, AssistantClientMessage::Permission { tool_call_id: "toolu_w2".into(), option_id: "always".into(), command: None });
            wait_status(&mut rx, ThreadStatus::Idle).await;
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi");
            assert!(th.s.borrow().always_allow.contains("write_file"));
            let reqs = mock.requests.lock().unwrap().clone();
            let msgs = reqs[1]["messages"].as_array().unwrap();
            // user, assistant(tool_use), user(tool_result + 新的话 + 终端输出)
            assert_eq!(msgs.len(), 3);
            let merged = msgs[2]["content"].as_array().unwrap();
            assert_eq!(merged[0]["type"], "tool_result");
            assert!(merged.iter().any(|b| b["text"] == "可以写"));
            assert!(merged.iter().any(|b| b["text"].as_str().is_some_and(|t| t.contains("<terminal_output>"))));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn api_errors_end_the_turn_with_a_notice() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, _mock) =
                start_mock(vec![(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#.into())])
                    .await;
            let f = fixture(&url);
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "hi".into(), context: None });
            wait_status(&mut rx, ThreadStatus::Error).await;
            let es = entries(&f.hub, &t.id);
            assert!(matches!(es.last(), Some(Entry::Notice { text, .. }) if text.contains("invalid x-api-key")));
            // 出错之后还能接着说话（不是卡死在 running）
            assert_eq!(f.hub.thread(&t.id).unwrap().status(), ThreadStatus::Error);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn snapshot_first_then_deltas_and_delete_closes_viewers() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, _mock) = start_mock(vec![]).await;
            let f = fixture(&url);
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            let first: AssistantServerMessage = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
            assert!(matches!(first, AssistantServerMessage::Snapshot { .. }));
            assert_eq!(f.hub.list().len(), 1);
            f.hub.delete(&t.id).await;
            assert!(rx.recv().await.is_none());
            assert!(f.hub.list().is_empty());
        })
        .await;
}

// ---------------------------------------------------------------- ACP

/// 测试二进制自己扮演 ACP agent：设了 FALCON_TEST_ACP_AGENT 时这个「测试」读 stdin、写 stdout，
/// 说一段固定的剧本（一句话 → 一个命令卡片 → 要权限 → 输出 → 结束）。测试框架自己往 stdout 打的
/// 「running 1 test」不是 JSON，客户端照协议跳过
#[test]
fn mock_acp_agent() {
    use std::io::{BufRead as _, Write as _};
    if std::env::var("FALCON_TEST_ACP_AGENT").is_err() {
        return;
    }
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut out = std::io::stdout();
    let mut send = |v: Value| {
        writeln!(out, "{v}").unwrap();
        out.flush().unwrap();
    };
    // 测试框架在 stdout 上先打了不带换行的「test … ... 」：先换一行，别让第一条应答粘在它后面
    send(Value::Null);
    let update = |u: Value| json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":u}});
    while let Some(Ok(line)) = lines.next() {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let id = msg["id"].clone();
        match msg["method"].as_str().unwrap_or("") {
            "initialize" => send(json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"authMethods":[]}})),
            "session/new" => send(json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"s1"}})),
            "session/prompt" => {
                let text = msg["params"]["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                send(update(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("收到：{text}")}})));
                send(update(json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"`echo hello`","kind":"execute","status":"pending",
                    "_meta":{"terminal_info":{"terminal_id":"t1"}}})));
                send(json!({"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"sessionId":"s1",
                    "toolCall":{"toolCallId":"t1"},
                    "options":[{"optionId":"ok","name":"Allow","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]}}));
                // 等权限的回话
                let outcome = loop {
                    let Some(Ok(l)) = lines.next() else { return };
                    let Ok(r) = serde_json::from_str::<Value>(&l) else { continue };
                    if r["id"] == "p1" {
                        break r["result"]["outcome"].clone();
                    }
                };
                if outcome["optionId"] == "ok" {
                    send(update(json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"in_progress",
                        "_meta":{"terminal_output_delta":{"terminal_id":"t1","data":"hello\n"}}})));
                    send(update(json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed",
                        "_meta":{"terminal_exit":{"terminal_id":"t1","exit_code":0,"signal":null}}})));
                    send(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
                } else {
                    send(update(json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"failed"})));
                    send(json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}}));
                }
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn acp_agent_turn_maps_updates_and_permissions() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let f = fixture("http://127.0.0.1:9");
            f.hub.sessions.local.set_base_env(vec![
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("HOME".into(), f.root.to_string_lossy().into_owned()),
            ]);
            let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
            let mut agents = settings::acp_agents(&f.hub.db);
            agents.push(falcon_proto::assistant::AcpAgentConfig {
                id: "mock".into(),
                name: "Mock".into(),
                command: exe,
                args: ["--exact", "assistant::hub::tests::mock_acp_agent", "--nocapture", "--test-threads=1"]
                    .map(String::from)
                    .to_vec(),
                env: vec![("FALCON_TEST_ACP_AGENT".into(), "1".into())],
                enabled: true,
            });
            settings::apply(&f.hub.db, &f.hub.secrets, &AssistantSettingsPatch { acp_agents: Some(agents), ..Default::default() })
                .unwrap();

            // ACP agent 只在本机
            assert!(f.hub.create("some-host", Some("acp:mock"), Default::default()).await.is_err());
            let t = f.hub.create("local", Some("acp:mock"), Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "hi".into(), context: None });
            wait_status(&mut rx, ThreadStatus::AwaitingPermission).await;
            let call = entries(&f.hub, &t.id).into_iter().find_map(|e| if let Entry::ToolCall(c) = e { Some(c) } else { None }).unwrap();
            assert_eq!(call.title, "echo hello");
            assert_eq!(call.permission.as_ref().map(|p| p.len()), Some(2));

            f.hub.handle(&t.id, AssistantClientMessage::Permission { tool_call_id: "t1".into(), option_id: "ok".into(), command: None });
            wait_status(&mut rx, ThreadStatus::Idle).await;
            let es = entries(&f.hub, &t.id);
            assert!(es.iter().any(|e| matches!(e, Entry::Agent { text, .. } if text == "收到：hi")));
            let call = es.iter().find_map(|e| if let Entry::ToolCall(c) = e { Some(c.clone()) } else { None }).unwrap();
            assert_eq!(call.status, ToolStatus::Completed);
            assert_eq!(
                call.content,
                [falcon_proto::assistant::ToolContent::Terminal { output: "hello\n".into(), exit_code: Some(0) }]
            );
            // 会话 id 落了库：服务端重启后能试着接回
            assert!(f.hub.db.get_assistant_thread(&t.id).unwrap().provider.contains("\"acpSessionId\":\"s1\""));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn missing_acp_command_is_a_readable_error() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let f = fixture("http://127.0.0.1:9");
            f.hub.sessions.local.set_base_env(vec![("PATH".into(), "/nonexistent".into())]);
            let mut agents = settings::acp_agents(&f.hub.db);
            agents.push(falcon_proto::assistant::AcpAgentConfig {
                id: "gone".into(),
                name: "Gone".into(),
                command: "falcon-no-such-agent".into(),
                enabled: true,
                ..Default::default()
            });
            settings::apply(&f.hub.db, &f.hub.secrets, &AssistantSettingsPatch { acp_agents: Some(agents), ..Default::default() })
                .unwrap();
            let t = f.hub.create("local", Some("acp:gone"), Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "hi".into(), context: None });
            wait_status(&mut rx, ThreadStatus::Error).await;
            assert!(matches!(entries(&f.hub, &t.id).last(), Some(Entry::Notice { text, .. }) if text.contains("找不到命令")));
        })
        .await;
}

/// 在跑的一轮里删掉对话：那一轮收尾时不能把它写回库里
#[tokio::test(flavor = "current_thread")]
async fn deleting_a_running_thread_keeps_it_deleted() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, mock) = start_mock(vec![]).await;
            let f = fixture(&url);
            let file = f.root.join("x.txt");
            mock.replies.lock().unwrap().push((
                200,
                tool_use_reply("toolu_d", "write_file", json!({ "path": file.to_string_lossy(), "content": "x" })),
            ));
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "写".into(), context: None });
            wait_status(&mut rx, ThreadStatus::AwaitingPermission).await;
            f.hub.delete(&t.id).await;
            // 让那一轮收完尾
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(f.hub.db.get_assistant_thread(&t.id).is_none());
            assert!(f.hub.list().is_empty());
            assert!(!file.exists());
        })
        .await;
}

// ---------------------------------------------------------------- 一段对话 = 一个终端

/// 本机的登录 shell 有没有集成（zsh / bash / fish）；没有就跳过这些要真 shell 的用例
fn integrated_shell() -> bool {
    crate::sessions::shell_integration::detect(&crate::sessions::local::default_local_shell()).is_some()
}

/// 等对话里出现满足条件的条目
async fn wait_entry(hub: &AssistantHub, id: &str, pred: impl Fn(&Entry) -> bool) -> Entry {
    for _ in 0..200 {
        if let Some(e) = entries(hub, id).into_iter().find(|e| pred(e)) {
            return e;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("等不到条目，现在有 {:#?}", entries(hub, id));
}

/// 等终端停在提示符上（集成打出了第一个 A）
async fn wait_prompt(hub: &AssistantHub, session: &str) {
    for _ in 0..200 {
        if hub.sessions.session_shell_info(session).is_some_and(|(cwd, _, _)| cwd.is_some()) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("终端一直没到提示符");
}

#[tokio::test(flavor = "current_thread")]
async fn user_commands_become_blocks_and_ride_along_to_the_agent() {
    if !integrated_shell() {
        return;
    }
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, mock) = start_mock(vec![(200, text_reply("看到了"))]).await;
            let f = fixture(&url);
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            let sid = t.session_id.clone().expect("建对话就有终端");
            wait_prompt(&f.hub, &sid).await;
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();

            // 用户在终端里敲一条命令
            f.hub.sessions.input(&sid, 0, "echo block-$((20+1))\r");
            let block = wait_entry(&f.hub, &t.id, |e| matches!(e, Entry::Command { .. })).await;
            let Entry::Command { command, output, exit_code, .. } = block else { unreachable!() };
            assert_eq!(command, "echo block-$((20+1))");
            assert!(output.contains("block-21"), "{output:?}");
            assert_eq!(exit_code, Some(0));
            // 还没说过话：标题取命令行，并推给在看的（面板的对话列表靠它改名）
            assert_eq!(f.hub.thread(&t.id).unwrap().summary().title, "echo block-$((20+1))");
            let mut named = None;
            while let Ok(text) = rx.try_recv() {
                if let Ok(AssistantServerMessage::Thread { thread }) = serde_json::from_str(&text) {
                    named = Some(thread.title);
                }
            }
            assert_eq!(named.as_deref(), Some("echo block-$((20+1))"));

            // 接着跟 agent 说：请求里带上了这条命令与它的输出
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "刚才那条什么意思".into(), context: None });
            wait_status(&mut rx, ThreadStatus::Running).await;
            wait_status(&mut rx, ThreadStatus::Idle).await;
            let req = mock.requests.lock().unwrap()[0].clone();
            let user = req["messages"][0]["content"].to_string();
            assert!(user.contains("<terminal_activity>"), "{user}");
            assert!(user.contains("$ echo block-$((20+1))"), "{user}");
            assert!(user.contains("block-21"), "{user}");
            f.hub.delete(&t.id).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn agent_commands_run_in_the_threads_shell_and_land_as_one_block() {
    if !integrated_shell() {
        return;
    }
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, mock) = start_mock(vec![]).await;
            let f = fixture(&url);
            settings::apply(&f.hub.db, &f.hub.secrets, &AssistantSettingsPatch { auto_approve: Some(true), ..Default::default() })
                .unwrap();
            mock.replies.lock().unwrap().extend([
                (
                    200,
                    tool_use_reply("toolu_r", "run_command", json!({ "command": "echo agent-$((1+1))", "description": "打个招呼" })),
                ),
                (200, text_reply("跑完了")),
                (200, text_reply("好")),
            ]);
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            wait_prompt(&f.hub, t.session_id.as_deref().unwrap()).await;
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "跑一下".into(), context: None });
            wait_status(&mut rx, ThreadStatus::Running).await;
            wait_status(&mut rx, ThreadStatus::Idle).await;

            let reqs = mock.requests.lock().unwrap().clone();
            let result = reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"][0]["content"].as_str().unwrap().to_string();
            assert!(result.contains("exit code: 0"), "{result}");
            assert!(result.contains("agent-2"), "{result}");
            let es = entries(&f.hub, &t.id);
            // 说话时的目录记在这句话上
            assert!(es.iter().any(|e| matches!(e, Entry::User { cwd: Some(_), .. })), "{es:#?}");
            // 卡片带着「这一步要做什么」，输出不在卡片上
            let call = es.iter().find_map(|e| if let Entry::ToolCall(c) = e { Some(c.clone()) } else { None }).unwrap();
            assert_eq!(call.description.as_deref(), Some("打个招呼"));
            assert!(!call.content.iter().any(|c| matches!(c, ToolContent::Terminal { .. })), "{call:#?}");
            // 输出记成卡片后面的一个命令块（只一个：shell 那边不再另记一个「你敲的」）
            let blocks: Vec<&Entry> = es.iter().filter(|e| matches!(e, Entry::Command { .. })).collect();
            assert_eq!(blocks.len(), 1, "{es:#?}");
            let Entry::Command { command, output, exit_code, by_agent, .. } = blocks[0] else { unreachable!() };
            assert_eq!(command, "echo agent-$((1+1))");
            assert!(output.contains("agent-2"), "{output:?}");
            assert_eq!(*exit_code, Some(0));
            assert!(by_agent);
            let card_at = es.iter().position(|e| matches!(e, Entry::ToolCall(_))).unwrap();
            let block_at = es.iter().position(|e| matches!(e, Entry::Command { .. })).unwrap();
            assert!(card_at < block_at);

            // 下一句话不把 agent 自己跑的命令当成「你在终端里干了什么」带回去
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "然后呢".into(), context: None });
            wait_status(&mut rx, ThreadStatus::Running).await;
            wait_status(&mut rx, ThreadStatus::Idle).await;
            let req = mock.requests.lock().unwrap()[2].clone();
            let last = req["messages"].as_array().unwrap().last().unwrap().to_string();
            assert!(!last.contains("<terminal_activity>"), "{last}");
            f.hub.delete(&t.id).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_edited_command_runs_instead_and_the_model_is_told() {
    if !integrated_shell() {
        return;
    }
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (url, mock) = start_mock(vec![]).await;
            let f = fixture(&url);
            mock.replies.lock().unwrap().extend([
                (200, tool_use_reply("toolu_e", "run_command", json!({ "command": "free -h", "description": "看内存" }))),
                (200, text_reply("好的")),
            ]);
            let t = f.hub.create("local", None, Default::default()).await.unwrap();
            wait_prompt(&f.hub, t.session_id.as_deref().unwrap()).await;
            let (tx, mut rx) = mpsc::unbounded_channel();
            f.hub.add_viewer(&t.id, tx).unwrap();
            f.hub.handle(&t.id, AssistantClientMessage::Prompt { text: "看看内存".into(), context: None });
            wait_status(&mut rx, ThreadStatus::AwaitingPermission).await;
            f.hub.handle(
                &t.id,
                AssistantClientMessage::Permission {
                    tool_call_id: "toolu_e".into(),
                    option_id: "allow".into(),
                    command: Some("echo edited-$((2+3))".into()),
                },
            );
            wait_status(&mut rx, ThreadStatus::Idle).await;

            let es = entries(&f.hub, &t.id);
            let call = es.iter().find_map(|e| if let Entry::ToolCall(c) = e { Some(c.clone()) } else { None }).unwrap();
            assert_eq!(call.title, "echo edited-$((2+3))");
            assert_eq!(call.status, ToolStatus::Completed);
            let out = es.iter().find_map(|e| if let Entry::Command { output, .. } = e { Some(output.clone()) } else { None });
            assert!(out.is_some_and(|o| o.contains("edited-5")));
            let reqs = mock.requests.lock().unwrap().clone();
            let result = reqs[1]["messages"].as_array().unwrap().last().unwrap()["content"][0]["content"].as_str().unwrap().to_string();
            assert!(result.contains("edited the command") && result.contains("echo edited-$((2+3))"), "{result}");
            assert!(result.contains("edited-5"), "{result}");
            f.hub.delete(&t.id).await;
        })
        .await;
}
