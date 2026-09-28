//! REST 客户端对着脚本化的假服务端：与执行器无关、错误形状、cookie、401 自动重登与
//! 重放、流式上传下载。

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use falcon_client::{
    ApiErrorKind, AuthEvent, AuthState, FalconClient, GitLogQuery, ProbeTarget, runtime,
};
use falcon_proto::{MeegleTodoAction, OkResponse};
use futures::executor::block_on;
use serde_json::json;
use support::{HttpReq, HttpResp, Mock};

fn auth_status_json() -> serde_json::Value {
    json!({ "required": false, "authenticated": true, "passwordSet": false })
}

#[test]
fn block_on_outside_tokio_gets_a_result() {
    let mock = Mock::start(|_| HttpResp::json(200, auth_status_json()));
    let client = mock.client();
    // 测试线程不在任何 tokio 上下文里：futures 自带的 block_on 只认 Waker
    let status = block_on(client.auth_status()).unwrap();
    assert!(!status.required && status.authenticated);
    // 别的普通线程里也一样
    let c = client.clone();
    let status = std::thread::spawn(move || block_on(c.auth_status())).join().unwrap().unwrap();
    assert!(status.authenticated);
    // 返回的 future 是 'static + Send，丢进别的运行时 spawn 也行
    let handle = runtime().spawn(client.auth_status());
    assert!(runtime().block_on(handle).unwrap().is_ok());
}

#[test]
fn futures_are_lazy() {
    let mock = Mock::start(|_| HttpResp::json(200, json!([])));
    let client = mock.client();
    drop(client.list_projects());
    std::thread::sleep(Duration::from_millis(100));
    assert!(mock.requests().is_empty(), "不 poll 就不该发请求");
}

#[test]
fn error_shape_matches_api_request_error() {
    let mock = Mock::start(|req| match req.path_only() {
        "/api/projects" => HttpResp::json(409, json!({ "error": "项目仍有 2 个未终止的会话" })),
        _ => HttpResp::bytes(502, "text/html", b"<html>bad gateway</html>".to_vec()),
    });
    let client = mock.client();
    let e = block_on(client.list_projects()).unwrap_err();
    assert_eq!(e.status(), Some(409));
    assert_eq!(e.message, "项目仍有 2 个未终止的会话");
    assert_eq!(e.kind, ApiErrorKind::Http);
    assert!(e.is_conflict());
    assert_eq!(e.body.unwrap()["error"], "项目仍有 2 个未终止的会话");

    let e = block_on(client.list_hosts()).unwrap_err();
    assert_eq!((e.status(), e.message.as_str()), (Some(502), "HTTP 502"));

    // 2xx 但形状不对
    let mock = Mock::start(|_| HttpResp::json(200, json!({ "nope": 1 })));
    let e = block_on(mock.client().system()).unwrap_err();
    assert_eq!(e.kind, ApiErrorKind::Decode);
    assert_eq!(e.status(), Some(200));

    // 连不上：status 为 None
    let dead = FalconClient::new(url::Url::parse("http://127.0.0.1:9").unwrap());
    let e = block_on(dead.auth_status()).unwrap_err();
    assert_eq!(e.status(), None);
    assert!(e.is_network(), "{e:?}");
}

/// 一个带密码的假服务端：登录拿 token，其余接口只认最新签发的那枚。
fn authed_server(password: &'static str) -> (Mock, Arc<Mutex<Option<String>>>) {
    let current: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let issued = Arc::new(AtomicUsize::new(0));
    let cur = current.clone();
    let mock = Mock::start(move |req: &HttpReq| {
        if req.path_only() == "/api/auth/login" {
            if req.json()["password"] == password {
                let tok = format!("tok-{}", issued.fetch_add(1, Ordering::SeqCst) + 1);
                *cur.lock().unwrap() = Some(tok.clone());
                return HttpResp::json(200, json!({ "ok": true })).header(
                    "Set-Cookie",
                    &format!("falcon_token={tok}; Max-Age=2592000; Path=/; HttpOnly; SameSite=Lax"),
                );
            }
            return HttpResp::json(401, json!({ "error": "密码错误" }));
        }
        let want = cur.lock().unwrap().clone().map(|t| format!("falcon_token={t}"));
        if want.is_none() || req.cookie() != want.as_deref() {
            return HttpResp::json(401, json!({ "error": "未认证" }));
        }
        HttpResp::json(200, json!([]))
    });
    (mock, current)
}

#[test]
fn login_stores_the_cookie_and_sends_it() {
    let (mock, _) = authed_server("pw-123456");
    let client = mock.client();
    let mut events = client.subscribe_auth();
    let e = block_on(client.login("wrong")).unwrap_err();
    assert!(e.is_unauthorized());
    assert_eq!(e.message, "密码错误");
    block_on(client.login("pw-123456")).unwrap();
    assert_eq!(client.token().as_deref(), Some("tok-1"));
    assert_eq!(client.auth_state(), AuthState::LoggedIn);
    assert_eq!(block_on(events.next_event()), Some(AuthEvent::LoggedIn));
    block_on(client.list_projects()).unwrap();
    let last = mock.requests().pop().unwrap();
    assert_eq!(last.cookie(), Some("falcon_token=tok-1"));
}

#[test]
fn server_restart_is_healed_by_one_relogin_and_one_replay() {
    let (mock, current) = authed_server("pw-123456");
    let client = mock.client();
    client.set_relogin_password(Some("pw-123456".into()));
    block_on(client.login("pw-123456")).unwrap();
    // "后端重启"：内存里的 token 全没了
    *current.lock().unwrap() = Some("after-restart-nobody-has-this".into());

    let mut events = client.subscribe_auth();
    let before = mock.requests().len();
    block_on(client.list_projects()).expect("自动重登后应当成功");
    let reqs: Vec<String> = mock.requests()[before..].iter().map(|r| r.path_only().to_owned()).collect();
    assert_eq!(reqs, ["/api/projects", "/api/auth/login", "/api/projects"]);
    assert_eq!(client.token().as_deref(), Some("tok-2"));
    assert_eq!(block_on(events.next_event()), Some(AuthEvent::LoggedIn));
}

#[test]
fn concurrent_401s_share_a_single_relogin() {
    let (mock, current) = authed_server("pw-123456");
    let client = mock.client();
    client.set_relogin_password(Some("pw-123456".into()));
    block_on(client.login("pw-123456")).unwrap();
    *current.lock().unwrap() = Some("gone".into());
    let futs: Vec<_> = (0..8).map(|_| client.list_projects()).collect();
    for r in block_on(futures::future::join_all(futs)) {
        r.unwrap();
    }
    let logins = mock.requests().iter().filter(|r| r.path_only() == "/api/auth/login").count();
    // 第一次手动登录 + 恰好一次自动重登
    assert_eq!(logins, 2);
}

#[test]
fn without_password_401_surfaces_and_login_required_fires_once() {
    let (mock, _) = authed_server("pw-123456");
    let client = mock.client();
    let mut events = client.subscribe_auth();
    let futs: Vec<_> = (0..5).map(|_| client.list_projects()).collect();
    for r in block_on(futures::future::join_all(futs)) {
        assert!(r.unwrap_err().is_unauthorized());
    }
    assert_eq!(block_on(events.next_event()), Some(AuthEvent::LoginRequired));
    std::thread::sleep(Duration::from_millis(50));
    assert!(events.try_recv().is_err(), "LoginRequired 只推一次");
    assert_eq!(client.auth_state(), AuthState::LoginRequired);
    assert!(
        mock.requests().iter().all(|r| r.path_only() != "/api/auth/login"),
        "没设密码就不该去登"
    );
}

#[test]
fn rejected_relogin_password_is_forgotten() {
    let (mock, _) = authed_server("pw-123456");
    let client = mock.client();
    client.set_relogin_password(Some("stale-password".into()));
    let e = block_on(client.list_projects()).unwrap_err();
    assert!(e.is_unauthorized());
    assert!(!client.has_relogin_password(), "被拒的密码不该留着反复试");
    assert_eq!(client.auth_state(), AuthState::LoginRequired);
}

#[test]
fn logout_forgets_token_and_password() {
    let (mock, _) = authed_server("pw-123456");
    let client = mock.client();
    block_on(client.login("pw-123456")).unwrap();
    client.set_relogin_password(Some("pw-123456".into()));
    let mut events = client.subscribe_auth();
    let _ = block_on(client.logout());
    assert_eq!(client.token(), None);
    assert!(!client.has_relogin_password());
    assert_eq!(block_on(events.next_event()), Some(AuthEvent::LoggedOut));
}

#[test]
fn paths_queries_and_bodies_follow_the_server() {
    let mock = Mock::start(|req| match req.path_only() {
        p if p.ends_with("/git/log") => {
            HttpResp::json(200, json!({ "available": true, "commits": [], "hasMore": false }))
        }
        "/api/fs/list" => HttpResp::json(
            200,
            json!({ "path": "", "parent": null, "home": "C:\\", "roots": [], "entries": [] }),
        ),
        p if p.starts_with("/api/projects/") && req.method == "DELETE" => {
            HttpResp::json(200, json!({ "ok": true }))
        }
        "/api/meegle/todo" => HttpResp::json(200, json!({ "items": [], "page": 2, "hasMore": false })),
        "/api/auth/password" => HttpResp::json(200, json!({ "ok": true })),
        _ => HttpResp::json(404, json!({ "error": "?" })),
    });
    let client = mock.client();
    let q = GitLogQuery {
        branch: Some("origin/main".into()),
        author: Some("".into()),
        q: Some("fix 滚轮".into()),
        skip: 50,
        repo: None,
    };
    block_on(client.git_log("p/1", &q)).unwrap();
    // path="" 是 Windows 盘符列表，必须带上；hostId 带上
    block_on(client.list_dir(Some(""), &ProbeTarget::SshHost("h1".into()))).unwrap();
    block_on(client.delete_project("p1", true)).unwrap();
    block_on(client.meegle_todo(MeegleTodoAction::ThisWeek, 2, true)).unwrap();
    let _: OkResponse = block_on(client.set_password("abcdef", None)).unwrap();

    let reqs = mock.requests();
    let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/projects/p%2F1/git/log?branch=origin%2Fmain&q=fix+%E6%BB%9A%E8%BD%AE&skip=50",
            "/api/fs/list?path=&hostId=h1",
            "/api/projects/p1?force=true",
            "/api/meegle/todo?action=this_week&page=2&fresh=1",
            "/api/auth/password",
        ]
    );
    // current 缺省时不写出来（与 JSON.stringify 丢掉 undefined 一致）
    assert_eq!(reqs[4].json(), json!({ "next": "abcdef" }));
    assert_eq!(reqs[4].header("content-type"), Some("application/json"));
    // 没有请求体的 DELETE 不带 Content-Type
    assert_eq!(reqs[2].header("content-type"), None);
}

#[test]
fn upload_streams_with_exact_content_length_and_409_is_returned_as_is() {
    let dir = scratch("upload");
    let src = dir.join("data.bin");
    let payload: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&src, &payload).unwrap();

    let mock = Mock::start(|req| {
        if req.path.contains("overwrite=1") {
            HttpResp::json(200, json!({ "path": "docs/data.bin", "size": req.body.len() }))
        } else {
            HttpResp::json(409, json!({ "error": "同名文件已存在" }))
        }
    });
    let client = mock.client();
    let e = block_on(client.upload_file("p1", "docs", "data.bin", src.clone(), false, |_, _| {})).unwrap_err();
    assert!(e.is_conflict());
    assert_eq!(e.message, "同名文件已存在");

    let progress = Arc::new(Mutex::new(Vec::new()));
    let p = progress.clone();
    let res = block_on(client.upload_file("p1", "docs", "data.bin", src, true, move |d, t| {
        p.lock().unwrap().push((d, t))
    }))
    .unwrap();
    assert_eq!(res.size, payload.len() as u64);

    let req = mock.requests().pop().unwrap();
    assert_eq!(req.method, "PUT");
    assert_eq!(req.path, "/api/projects/p1/upload?path=docs&name=data.bin&overwrite=1");
    assert_eq!(req.header("content-length"), Some("300000"));
    assert_eq!(req.header("content-type"), Some("application/octet-stream"));
    assert!(req.header("transfer-encoding").is_none(), "不能是 chunked");
    assert_eq!(req.body, payload);
    let progress = progress.lock().unwrap();
    assert_eq!(progress.first(), Some(&(0, Some(300_000))));
    assert_eq!(progress.last(), Some(&(300_000, Some(300_000))));
}

#[test]
fn download_lands_atomically_and_failures_leave_nothing() {
    let dir = scratch("download");
    let payload: Vec<u8> = (0..500_000u32).map(|i| (i % 253) as u8).collect();
    let body = payload.clone();
    let mock = Mock::start(move |req| {
        if req.path.contains("missing") {
            HttpResp::json(404, json!({ "error": "路径不存在或不可访问" }))
        } else {
            HttpResp::bytes(200, "application/octet-stream", body.clone())
        }
    });
    let client = mock.client();
    let dest = dir.join("out.bin");
    let last = Arc::new(Mutex::new(None));
    let l = last.clone();
    let n = block_on(client.download_to_file("p1", "big/file.bin", dest.clone(), move |d, t| {
        *l.lock().unwrap() = Some((d, t))
    }))
    .unwrap();
    assert_eq!(n, payload.len() as u64);
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert_eq!(*last.lock().unwrap(), Some((500_000, Some(500_000))));
    assert_eq!(mock.requests()[0].path, "/api/projects/p1/download?path=big%2Ffile.bin");

    let dest2 = dir.join("missing.bin");
    let e = block_on(client.download_to_file("p1", "missing", dest2.clone(), |_, _| {})).unwrap_err();
    assert!(e.is_not_found());
    assert!(!dest2.exists());
    // 目录里只剩那个下好的文件，没有残留的临时文件
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("out.bin")]);
}

#[test]
fn paste_image_and_raw_bytes() {
    let mock = Mock::start(|req| match req.path_only() {
        "/api/sessions/s1/paste-image" => {
            assert_eq!(req.header("content-type"), Some("image/png"));
            assert_eq!(req.body, b"\x89PNG fake");
            HttpResp::json(200, json!({ "path": "/Users/fay/.falcon/paste/a.png" }))
        }
        _ => HttpResp::bytes(200, "image/svg+xml", b"<svg/>".to_vec()),
    });
    let client = mock.client();
    let path = block_on(client.paste_image("s1", b"\x89PNG fake".to_vec(), "image/png")).unwrap();
    assert_eq!(path, "/Users/fay/.falcon/paste/a.png");

    let raw = block_on(client.raw_bytes("/api/projects/p1/raw/tok/a%20b.svg")).unwrap();
    assert_eq!(raw.content_type.as_deref(), Some("image/svg+xml"));
    assert_eq!(&raw.bytes[..], b"<svg/>");
    // 以本服务端基址开头的完整地址也认
    let full = format!("{}/api/projects/p1/raw/tok/x.svg", client.base_url());
    block_on(client.raw_bytes(&full)).unwrap();
    // 别的主机不给（不把登录 cookie 送出去）
    let e = block_on(client.raw_bytes("https://evil.example/api/x")).unwrap_err();
    assert_eq!(e.kind, ApiErrorKind::Internal);
    assert_eq!(mock.requests().len(), 3);
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("falcon-client-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

trait NextEvent {
    fn next_event(&mut self) -> futures::future::BoxFuture<'_, Option<AuthEvent>>;
}

impl NextEvent for falcon_client::AuthEvents {
    fn next_event(&mut self) -> futures::future::BoxFuture<'_, Option<AuthEvent>> {
        use futures::StreamExt;
        Box::pin(async move {
            let timeout = futures_timer(Duration::from_secs(5));
            futures::pin_mut!(timeout);
            match futures::future::select(self.next(), timeout).await {
                futures::future::Either::Left((ev, _)) => ev,
                futures::future::Either::Right(_) => None,
            }
        })
    }
}

/// 不依赖 tokio 上下文的定时器：在网络运行时上睡，外面等 JoinHandle。
fn futures_timer(d: Duration) -> impl std::future::Future<Output = ()> {
    // Sleep 在构造时就要 reactor，所以放进 async 块里、到网络线程上再建
    let h = runtime().spawn(async move { tokio::time::sleep(d).await });
    async move {
        let _ = h.await;
    }
}
