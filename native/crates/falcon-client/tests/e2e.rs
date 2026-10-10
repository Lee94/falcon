//! 对真服务端的端到端测试（设计文档 §6.3）：会话链路以前没有任何自动化测试，这条补上。
//!
//! 跑法（默认 `#[ignore]`，还要显式打开开关——它会起服务端进程、建 zellij 会话）：
//!
//! ```text
//! cd native && cargo build -p falcon-server
//! FALCON_E2E=1 cargo test -p falcon-client --test e2e -- --ignored --nocapture
//! ```
//!
//! 起的是 `native/target/debug/falcon-server`，`FALCON_E2E_SERVER_BIN` 可改指（比如发布产物）。
//!
//! 流程：拉起服务端（空数据目录、4940–4999 的空闲端口）→ 认证状态 → 建本地项目 → 建会话
//! → 开 SessionSocket（连上前就设好外观与尺寸）→ 收到回放与 state active → 敲
//! `echo falcon-e2e-$((6*7))-<随机数>` 在输出里看到 shell 算出来的 42 → resize 后
//! `stty size` 对得上 → 设访问密码并登录 → **重启 #1**（socket 先 Detach）：REST 撞 401
//! 自动重登 → 重新打开 socket，新 socket 强发 resize 触发接回，会话（zellij）活过了
//! 后端重启 → **重启 #2**（socket 开着）：socket 撞 4401 自动重登并重连，REST 随后
//! 不用再登 → Terminate → socket 收到 dead → 清理。
//!
//! 几个坑（与 `cargo xtask fixtures` 同一套）：数据目录必须是短路径（zellij socket 路径 macOS
//! 上限 104 字节）；全新数据目录首次建会话会去下载 zellij，先从 ~/.falcon/bin 或
//! ~/.mojito/bin 拷一份锁定版本进去；绝不碰 4923；启动子服务端前去掉环境里的
//! FALCON_* / MOJITO_*。
//!
//! 最近一次跑通：2026-10-10，macOS 27（Apple Silicon），Rust 服务端（debug 构建），zellij
//! 0.45.1（从 ~/.falcon/bin 拷入），本地项目（shell 用 /bin/sh，不吃本机 zsh 配置）+ 持久
//! 会话，全程约 3s。两次重启后同一个 zellij 会话都接得回来，`$((6*7))` 的回显与
//! `stty size` 的结果都对得上；重启 #1 由 REST 的 401 触发重登，重启 #2 由 socket 的
//! 4401 触发重登，各只登了一次；被 4401 拒掉的那条连接没有报 Connected，全程没有
//! 冒出 Unauthorized。
//!
//! 踩过的坑：会话刚建好就敲命令会丢——zellij 接管 PTY 之前敲的字只被 tty 回显一下，
//! 没人读。所以先等 shell 提示符（`Terminal::wait_prompt`）。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use falcon_client::{AuthEvent, AuthEvents, FalconClient, SessionEvent, SessionSocket};
use falcon_proto::{CreateSessionRequest, DeadReason, ProjectInput, ProjectType, SessionState, TermAppearance};
use futures::executor::block_on;

const PASSWORD: &str = "e2e-pass-1234";

#[test]
#[ignore = "起真服务端与 zellij；FALCON_E2E=1 cargo test -p falcon-client --test e2e -- --ignored"]
fn session_survives_backend_restarts_with_auto_relogin() {
    if std::env::var("FALCON_E2E").as_deref() != Ok("1") {
        eprintln!("跳过：设 FALCON_E2E=1 才跑");
        return;
    }
    let t0 = Instant::now();
    let step = |msg: &str| eprintln!("[{:>6.2}s] {msg}", t0.elapsed().as_secs_f64());

    let root = repo_root();
    let entry = server_entry(&root);
    let tag = std::process::id();
    let data = PathBuf::from(format!("/private/tmp/fal-e2e-{tag}"));
    let work = PathBuf::from(format!("/private/tmp/fal-e2e-{tag}-w"));
    let _cleanup = Cleanup(vec![data.clone(), work.clone()]);
    std::fs::create_dir_all(&work).unwrap();
    seed_zellij(&root, &data);

    let port = free_port();
    let mut server = Server::start(&entry, &data, port);
    let client = FalconClient::new(url::Url::parse(&format!("http://127.0.0.1:{port}")).unwrap());
    wait_ready(&client, &mut server);
    step(&format!("服务端起来了：{}", client.base_url()));

    // ---- 认证状态：绑回环、没设密码，不需要登录 ----
    let status = block_on(client.auth_status()).unwrap();
    assert!(!status.required && status.authenticated && !status.password_set, "{status:?}");

    // ---- 项目与会话 ----
    let project = block_on(client.create_project(&ProjectInput {
        name: "e2e".into(),
        project_type: ProjectType::Local,
        working_dir: Some(work.to_string_lossy().into_owned()),
        shell: Some("/bin/sh".into()),
        host_id: None,
        ssh: None,
        repos: None,
        default_worktree_branch: None,
    }))
    .unwrap();
    let session = block_on(client.create_session(&project.id, &CreateSessionRequest {
        appearance: Some(TermAppearance::Dark),
        background: Some("#0a0a0a".into()),
        foreground: Some("#fafafa".into()),
        ..Default::default()
    }))
    .unwrap();
    assert_eq!(session.state, SessionState::Active);
    assert!(session.durable, "本地会话应当是持久的（zellij 没准备好？看 {}/server.log）", data.display());
    step(&format!("建好项目与会话 {}", session.id));

    // ---- 连会话：连上之前就把外观与尺寸交代好 ----
    let (sink, mut term) = Terminal::new();
    let sock = SessionSocket::open(&client, &session.id, sink.clone());
    sock.set_appearance(TermAppearance::Dark, Some("#0a0a0a"), Some("#fafafa"));
    sock.resize(120, 40);
    term.wait("Connected", |e| matches!(e, SessionEvent::Connected { reconnected: false }));
    term.wait("回放", |e| matches!(e, SessionEvent::Replay(_)));
    term.wait("state active", |e| matches!(e, SessionEvent::State { state: SessionState::Active, .. }));
    step("socket 连上：收到回放与 state active");
    term.wait_prompt();

    term.echo(&sock, "首次连接");
    term.run(&sock, "stty size", "40 120");
    sock.resize(100, 30);
    term.run(&sock, "stty size", "30 100");
    step("回显与 resize 都对得上");

    // ---- 设访问密码并登录；记住密码供自动重登 ----
    block_on(client.set_password(PASSWORD, None)).unwrap();
    block_on(client.login(PASSWORD)).unwrap();
    client.set_relogin_password(Some(PASSWORD.into()));
    let mut auth = client.subscribe_auth();
    let token0 = client.token().unwrap();
    assert!(block_on(client.auth_status()).unwrap().password_set);
    step("设了密码并登录");

    // ---- 重启 #1：socket 先 Detach，重启后由 REST 撞上 401 ----
    sock.close();
    drop(sock);
    server.restart();
    wait_ready(&client, &mut server);
    step("重启 #1 完成（服务端内存里的 token 全没了）");
    let sessions = block_on(client.list_sessions()).expect("REST 应当自动重登后成功");
    assert!(sessions.iter().any(|s| s.id == session.id), "会话不见了：{sessions:?}");
    assert_eq!(drain(&mut auth), vec![AuthEvent::LoggedIn], "REST 触发恰好一次重登");
    let token1 = client.token().unwrap();
    assert_ne!(token0, token1);
    step("REST 撞 401 → 自动重登 → 重放成功");

    // 新 socket：强发 resize 是持久会话懒惰接回的信号
    let (sink, mut term) = Terminal::new();
    let sock = SessionSocket::open(&client, &session.id, sink.clone());
    sock.set_appearance(TermAppearance::Dark, Some("#0a0a0a"), Some("#fafafa"));
    sock.resize(100, 30);
    term.wait("Connected", |e| matches!(e, SessionEvent::Connected { .. }));
    term.wait("state active", |e| matches!(e, SessionEvent::State { state: SessionState::Active, .. }));
    term.echo(&sock, "重启 #1 之后");
    term.run(&sock, "stty size", "30 100");
    step("新 socket 接回同一个 zellij 会话");

    // ---- 重启 #2：socket 开着，由它撞上 4401 ----
    server.stop();
    term.wait("Disconnected", |e| matches!(e, SessionEvent::Disconnected { .. }));
    server.start_again();
    wait_ready(&client, &mut server); // auth/status 不需要认证，不会动到 token
    step("重启 #2 完成");
    sock.reconnect_now();
    let ev = term.wait("重连（或未认证）", |e| {
        matches!(e, SessionEvent::Connected { .. } | SessionEvent::Unauthorized)
    });
    assert_eq!(ev, SessionEvent::Connected { reconnected: true }, "socket 应当自己重登后连上");
    assert_eq!(drain(&mut auth), vec![AuthEvent::LoggedIn], "socket 触发恰好一次重登");
    let token2 = client.token().unwrap();
    assert_ne!(token1, token2);
    term.wait("state active", |e| matches!(e, SessionEvent::State { state: SessionState::Active, .. }));
    term.echo(&sock, "重启 #2 之后");
    step("socket 撞 4401 → 自动重登 → 重连成功，会话还在");
    block_on(client.list_sessions()).expect("token 已经是新的，REST 不用再登");
    assert!(drain(&mut auth).is_empty());
    assert_eq!(client.token().unwrap(), token2);

    // ---- Terminate ----
    block_on(client.terminate_session(&session.id)).unwrap();
    let ev = term.wait("state dead", |e| matches!(e, SessionEvent::State { state: SessionState::Dead, .. }));
    assert_eq!(ev, SessionEvent::State { state: SessionState::Dead, dead_reason: Some(DeadReason::Exited) });
    sock.close();
    let left = block_on(client.list_sessions()).unwrap();
    assert!(left.iter().all(|s| s.id != session.id));
    block_on(client.delete_project(&project.id, true)).unwrap();
    step("Terminate 完成，收尾");
    server.stop();
}

// ---------------- 终端事件 ----------------

/// 收会话事件，并把输出拼成一段"屏幕文本"（粗略去掉 ANSI）方便找回显。
struct Terminal {
    rx: mpsc::Receiver<SessionEvent>,
    screen: String,
    nonce: u64,
}

impl Terminal {
    fn new() -> (Arc<dyn falcon_client::SessionSink>, Terminal) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: Arc<dyn falcon_client::SessionSink> = Arc::new(move |ev: SessionEvent| {
            let _ = tx.lock().unwrap().send(ev);
        });
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
        (sink, Terminal { rx, screen: String::new(), nonce })
    }

    fn wait(&mut self, what: &str, mut pred: impl FnMut(&SessionEvent) -> bool) -> SessionEvent {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = self.rx.recv_timeout(left).unwrap_or_else(|_| {
                panic!("等「{what}」超时；屏幕末尾：{:?}", tail(&strip_ansi(&self.screen)))
            });
            self.absorb(&ev);
            if matches!(ev, SessionEvent::Unauthorized) && !what.contains("未认证") {
                panic!("等「{what}」时收到了 Unauthorized");
            }
            if pred(&ev) {
                return ev;
            }
        }
    }

    fn absorb(&mut self, ev: &SessionEvent) {
        match ev {
            SessionEvent::Output(b) | SessionEvent::Replay(b) => self.screen.push_str(&String::from_utf8_lossy(b)),
            _ => {}
        }
    }

    /// 等 shell 提示符出现在屏幕上。会话刚建好时 zellij 还在起、shell 还没跑起来，
    /// 这时敲的字会被 PTY 回显一下然后丢掉（zellij 接管终端之前的输入没人读）——
    /// 真人手速碰不上，测试会。
    fn wait_prompt(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !strip_ansi(&self.screen).trim_end().ends_with('$') {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = self.rx.recv_timeout(left).unwrap_or_else(|_| {
                panic!("等 shell 提示符超时；屏幕末尾：{:?}", tail(strip_ansi(&self.screen).trim_end()))
            });
            self.absorb(&ev);
        }
    }

    /// 敲一条命令，等输出里出现 `expect`（只看敲之后的输出）。
    fn run(&mut self, sock: &SessionSocket, cmd: &str, expect: &str) {
        self.screen.clear();
        sock.send_input(format!("{cmd}\r"));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !strip_ansi(&self.screen).contains(expect) {
            let left = deadline.saturating_duration_since(Instant::now());
            let ev = self.rx.recv_timeout(left).unwrap_or_else(|_| {
                let dump = std::env::temp_dir().join("falcon-e2e-screen.txt");
                let _ = std::fs::write(&dump, &self.screen);
                panic!(
                    "`{cmd}` 之后没看到 {expect:?}；原始输出在 {}；去掉转义后的末尾：{:?}",
                    dump.display(),
                    tail(strip_ansi(&self.screen).trim_end())
                )
            });
            self.absorb(&ev);
        }
    }

    /// `echo falcon-e2e-$((6*7))-<n>`：看到 42 说明是 shell 真执行了，不只是回显了敲的字。
    fn echo(&mut self, sock: &SessionSocket, label: &str) {
        self.nonce += 1;
        let n = self.nonce % 1_000_000;
        self.run(sock, &format!("echo falcon-e2e-$((6*7))-{n}"), &format!("falcon-e2e-42-{n}"));
        eprintln!("         回显 OK（{label}）");
    }
}

fn drain(rx: &mut AuthEvents) -> Vec<AuthEvent> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn tail(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    chars[chars.len().saturating_sub(300)..].iter().collect()
}

/// 粗略去掉 ANSI 转义：zellij 重绘时会在字符之间插光标移动与 SGR。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                // CSI：参数与中间字节，直到 0x40–0x7e 的终止字节
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC：到 BEL 或 ST
                while let Some(c) = it.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && it.peek() == Some(&'\\')) {
                        if c == '\u{1b}' {
                            it.next();
                        }
                        break;
                    }
                }
            }
            Some('(' | ')') => {
                it.next();
            }
            _ => {}
        }
    }
    out
}

// ---------------- 服务端进程 ----------------

/// 被测的服务端可执行文件
fn server_entry(root: &Path) -> PathBuf {
    let bin = std::env::var_os("FALCON_E2E_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("native/target/debug/falcon-server"));
    assert!(bin.exists(), "没有 {}：先 cargo build -p falcon-server", bin.display());
    bin
}

struct Server {
    child: Option<Child>,
    entry: PathBuf,
    data: PathBuf,
    port: u16,
}

impl Server {
    fn start(entry: &Path, data: &Path, port: u16) -> Server {
        let mut s = Server { child: None, entry: entry.to_owned(), data: data.to_owned(), port };
        s.start_again();
        s
    }

    fn start_again(&mut self) {
        assert!(self.child.is_none());
        std::fs::create_dir_all(&self.data).unwrap();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.data.join("server.log"))
            .unwrap();
        let mut cmd = Command::new(&self.entry);
        cmd.args(["--host", "127.0.0.1", "--port", &self.port.to_string(), "--data-dir"])
            .arg(&self.data)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log);
        // 从 falcon 终端里跑时环境里带着父实例的变量（FALCON_WEB_DIST、FALCON_PORT…）
        for (k, _) in std::env::vars() {
            if k.starts_with("FALCON_") || k.starts_with("MOJITO_") {
                cmd.env_remove(k);
            }
        }
        if std::env::var_os("LANG").is_none() {
            cmd.env("LANG", "en_US.UTF-8");
        }
        self.child = Some(cmd.spawn().expect("起服务端失败"));
    }

    /// SIGTERM（服务端会优雅退出：会话在库里保持 active，zellij 继续跑），等它真退了。
    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    fn restart(&mut self) {
        self.stop();
        self.start_again();
    }

    fn assert_alive(&mut self) {
        if let Some(child) = &mut self.child
            && let Ok(Some(status)) = child.try_wait()
        {
            let log = std::fs::read_to_string(self.data.join("server.log")).unwrap_or_default();
            panic!("服务端退出了（{status}）：\n{}", tail(&log));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_ready(client: &FalconClient, server: &mut Server) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        server.assert_alive();
        if block_on(client.auth_status()).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("服务端 30s 没起来，看 {}/server.log", server.data.display());
}

/// 收尾：删临时目录。测试中途 panic 时会话没来得及 Terminate，zellij server 是独立
/// 进程、不随后端退出——按二进制路径把数据目录里起的那些收掉（只匹配自己的目录）。
struct Cleanup(Vec<PathBuf>);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for d in &self.0 {
            let _ = Command::new("pkill")
                .args(["-f", &format!("{}/bin/zellij", d.display())])
                .stderr(Stdio::null())
                .status();
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap()
}

/// 4940–4999 里挑一个空闲端口。绝不碰 4923（日常在用、设了密码的实例）。
fn free_port() -> u16 {
    (4940..=4999)
        .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("4940–4999 没有空闲端口")
}

/// 把锁定版本的 zellij 拷进数据目录（服务端安装流程会先检查 binDir 里现成的），
/// 省掉全新数据目录第一次建会话时的 14MB 下载。找不到就让服务端自己下。
fn seed_zellij(root: &Path, data: &Path) {
    let version_rs = std::fs::read_to_string(root.join("native/crates/falcon-server/src/zellij/version.rs")).unwrap();
    let version = version_rs
        .split("ZELLIJ_VERSION: &str = \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("version.rs 里找不到 ZELLIJ_VERSION");
    let name = format!("zellij-{version}");
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let found = std::env::var_os("FALCON_E2E_ZELLIJ")
        .map(PathBuf::from)
        .into_iter()
        .chain([home.join(".falcon/bin").join(&name), home.join(".mojito/bin").join(&name)])
        .find(|p| p.exists());
    let Some(src) = found else {
        eprintln!("没找到现成的 {name}，服务端会自己下载（慢）");
        return;
    };
    let bin = data.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // macOS 上 std::fs::copy 走 clonefile，33MB 也是瞬间
    std::fs::copy(&src, bin.join(&name)).unwrap();
}
