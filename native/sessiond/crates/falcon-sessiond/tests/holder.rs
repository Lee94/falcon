//! 真 PTY 的守护进程用例（设计文档第 15 节「sessiond 真 PTY 用例」）：起真的 holder 与 VT 进程，
//! 经 socket 说真协议。只在 Unix 上跑（Windows 的部分要真机，见设计文档的清单）。
#![cfg(unix)]

use std::io::Write as _;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use falcon_proto::program_status::ProgramState;
use falcon_proto::sessiond::{
    BRIDGE_EXIT_EXITED, BRIDGE_EXIT_GONE, ClientMsg, Event, Hello, HolderMsg, PROTO_MAX, PROTO_MIN, Resize, Role, SpawnResult,
    SpawnSpec, read_frame, socket_path,
};

const BIN: &str = env!("CARGO_BIN_EXE_falcon-sessiond");

struct Root(String);

impl Root {
    fn new(tag: &str) -> Root {
        // socket 路径有长度上限：用 /tmp 下的短目录
        let dir = format!("/tmp/sdt-{}-{tag}", std::process::id());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Root(dir)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        // 把这个根下还活着的会话收掉
        let _ = Command::new(BIN).args(["list", "--json", "--root", &self.0]).output().map(|o| {
            let list: Vec<falcon_proto::sessiond::ListEntry> = serde_json::from_slice(&o.stdout).unwrap_or_default();
            for e in list.into_iter().filter(|e| e.alive) {
                let _ = Command::new(BIN).args(["kill", "--root", &self.0, &e.sid]).output();
            }
        });
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn spec(argv: &[&str]) -> SpawnSpec {
    SpawnSpec {
        session_id: "0a1b2c3d-0000-4000-8000-000000000000".into(),
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: vec![
            ("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into()),
            ("HOME".into(), std::env::var("HOME").unwrap_or_else(|_| "/tmp".into())),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "en_US.UTF-8".into()),
            ("PS1".into(), "$ ".into()),
        ],
        env_clear: true,
        cwd: Some("/tmp".into()),
        cols: 80,
        rows: 24,
        scrollback_lines: 1000,
        owner: "test".into(),
        appearance: None,
    }
}

fn spawn(root: &Root, sid: &str, s: &SpawnSpec) -> SpawnResult {
    let out = Command::new(BIN).args(["spawn", "--root", &root.0, "--id", sid, "--spec", &s.encode()]).output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("spawn: {}", String::from_utf8_lossy(&out.stderr)))
}

struct Client {
    w: std::os::unix::net::UnixStream,
    rx: mpsc::Receiver<HolderMsg>,
    /// Synced 之后收到的全部输出（按顺序拼起来）
    seen: Vec<u8>,
}

impl Client {
    fn connect(root: &Root, sid: &str) -> Client {
        let path = socket_path(&root.0, sid, unsafe { libc::getuid() });
        let s = std::os::unix::net::UnixStream::connect(&path).unwrap_or_else(|e| panic!("连不上 {path}: {e}"));
        let mut r = s.try_clone().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(Some((k, p))) = read_frame(&mut r) {
                if tx.send(HolderMsg::decode(k, p).unwrap()).is_err() {
                    return;
                }
            }
        });
        let mut c = Client { w: s, rx, seen: Vec::new() };
        c.send(ClientMsg::Hello(Hello { proto_min: PROTO_MIN, proto_max: PROTO_MAX, role: Role::Server, client: "test".into() }));
        match c.recv() {
            HolderMsg::Welcome(_) => {}
            other => panic!("没有 Welcome：{other:?}"),
        }
        c
    }

    fn send(&mut self, m: ClientMsg) {
        self.w.write_all(&m.encode()).unwrap();
    }

    fn recv(&mut self) -> HolderMsg {
        self.rx.recv_timeout(Duration::from_secs(10)).expect("10 秒内没收到")
    }

    /// 收到满足条件的消息为止（顺便记下输出）
    fn until(&mut self, what: &str, mut f: impl FnMut(&HolderMsg) -> bool) -> HolderMsg {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let Ok(m) = self.rx.recv_timeout(Duration::from_millis(200)) else { continue };
            if let HolderMsg::Output { bytes, .. } = &m {
                self.seen.extend_from_slice(bytes);
            }
            if f(&m) {
                return m;
            }
        }
        panic!("等不到 {what}；已收到的输出：{:?}", String::from_utf8_lossy(&self.seen));
    }

    fn sync(&mut self, req: u32) -> (u64, String) {
        self.send(ClientMsg::Sync { req });
        match self.until("Synced", |m| matches!(m, HolderMsg::Synced { req: r, .. } if *r == req)) {
            HolderMsg::Synced { offset, snapshot, continuation, .. } => {
                let mut s = snapshot;
                s.extend(continuation);
                (offset, String::from_utf8_lossy(&s).into_owned())
            }
            _ => unreachable!(),
        }
    }

    fn input(&mut self, s: &str) {
        // 上一条命令跑完、新提示符的 `CSI 6n` 还没答完时输入会被 ash 吞掉（见 ready）
        std::thread::sleep(Duration::from_millis(300));
        self.send(ClientMsg::Input(s.as_bytes().to_vec()));
    }

    /// 同步之后等第一个提示符再输入：busybox 的 ash（alpine 的 /bin/sh）画完提示符会发 `CSI 6n`
    /// 问光标位置，等回答期间到达的输入会被它吞掉（实测 "echo hello" 整段没了）。回答由 VT
    /// 进程给，往返要一点时间。macOS 的 bash 没有这回事
    fn ready(&mut self) {
        let (_, snapshot) = self.sync(1);
        if !snapshot.contains("$ ") {
            self.expect_output("$ ");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

impl Client {
    fn expect_output(&mut self, needle: &str) {
        let n = needle.to_string();
        if String::from_utf8_lossy(&self.seen).contains(&n) {
            return;
        }
        let mut acc = self.seen.clone();
        self.until(needle, |m| {
            if let HolderMsg::Output { bytes, .. } = m {
                acc.extend_from_slice(bytes);
            }
            String::from_utf8_lossy(&acc).contains(&n)
        });
    }
}

#[test]
fn spawn_sync_input_and_exit() {
    let root = Root::new("a");
    let sid = "0a1b2c3d00004000";
    let res = spawn(&root, sid, &spec(&["/bin/sh"]));
    assert!(res.ok && !res.existing, "{res:?}");
    // 第二次 spawn 是"已经在了"
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).existing);

    let mut c = Client::connect(&root, sid);
    c.ready();
    c.input("echo hello-$((40+2))\n");
    c.expect_output("hello-42");
    let (offset, snap) = c.sync(2);
    assert!(offset > 0);
    assert!(snap.contains("hello-42"), "{snap:?}");

    c.input("exit 7\n");
    match c.until("Exit", |m| matches!(m, HolderMsg::Exit(_))) {
        HolderMsg::Exit(s) => assert_eq!(s.code, Some(7)),
        _ => unreachable!(),
    }
    // 退出记录留着，meta 删掉；bridge 据此报"已结束"
    std::thread::sleep(Duration::from_millis(300));
    assert!(std::path::Path::new(&format!("{}/sessiond/sessions/{sid}.exit", root.0)).is_file());
    assert!(!std::path::Path::new(&format!("{}/sessiond/sessions/{sid}.json", root.0)).exists());
    let code = Command::new(BIN).args(["bridge", "--root", &root.0, "--id", sid]).status().unwrap().code();
    assert_eq!(code, Some(BRIDGE_EXIT_EXITED));
    let code = Command::new(BIN).args(["bridge", "--root", &root.0, "--id", "ffffffffffffffff"]).status().unwrap().code();
    assert_eq!(code, Some(BRIDGE_EXIT_GONE));
}

#[test]
fn queries_are_answered_by_the_vt_process_with_no_client() {
    let root = Root::new("b");
    let sid = "0a1b2c3d00004001";
    // 程序发 DA 后等回答：没有任何客户端连着，VT 进程得替它答
    let script = r#"stty raw -echo; printf '\033[c'; r=$(dd bs=1 count=9 2>/dev/null | tr -d '\033'); stty sane; echo "DA=$r" > reply.txt; sleep 30"#;
    let mut s = spec(&["/bin/sh", "-c", script]);
    s.cwd = Some(root.0.clone());
    assert!(spawn(&root, sid, &s).ok);
    let file = format!("{}/reply.txt", root.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !std::path::Path::new(&file).is_file() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let reply = std::fs::read_to_string(&file).unwrap_or_default();
    assert!(reply.starts_with("DA=[?62;"), "{reply:?}");
}

#[test]
fn kitty_keyboard_query_is_not_answered() {
    let root = Root::new("c");
    let sid = "0a1b2c3d00004002";
    // 先问 kitty 键盘，再问 DA：DA 的回答必须是第一个到的（kitty 那条被滤掉了）
    let script = r#"stty raw -echo; printf '\033[?u\033[c'; r=$(dd bs=1 count=4 2>/dev/null | tr -d '\033'); stty sane; echo "R=$r" > reply.txt; sleep 30"#;
    let mut s = spec(&["/bin/sh", "-c", script]);
    s.cwd = Some(root.0.clone());
    assert!(spawn(&root, sid, &s).ok);
    let file = format!("{}/reply.txt", root.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !std::path::Path::new(&file).is_file() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let reply = std::fs::read_to_string(&file).unwrap_or_default();
    // 读 4 个字节：ESC [ ? 6 —— 是 DA 的开头，不是 kitty 的 `ESC [ ? 0 u`
    assert!(reply.starts_with("R=[?6"), "{reply:?}");
}

#[test]
fn program_status_is_stored_and_cleared_by_a_new_prompt() {
    let root = Root::new("d");
    let sid = "0a1b2c3d00004003";
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).ok);
    let mut c = Client::connect(&root, sid);
    c.ready();
    // 发报告的程序要一直占着前台：它一退出，holder 就按规范丢掉 working / blocked（fg::at_shell）。
    // 2 秒后再由它自己发一个 OSC 133 A
    c.input("printf '\\033]7501;state=blocked:kind=permission:app=claude-code\\033\\\\'; printf '\\033]7501;id=x:state=done\\007'; sleep 2; printf '\\033]133;A\\007'; sleep 5\n");
    let records = loop {
        if let HolderMsg::Status { records } = c.until("Status", |m| matches!(m, HolderMsg::Status { .. })) {
            if records.len() == 2 {
                break records;
            }
        }
    };
    assert!(records.iter().any(|r| r.state == ProgramState::Blocked));
    // 新连接一握手就拿到全集
    let mut c2 = Client::connect(&root, sid);
    match c2.recv() {
        HolderMsg::Status { records } => assert_eq!(records.len(), 2),
        other => panic!("{other:?}"),
    }
    // OSC 133 A：丢掉 blocked，done 留着
    let records = loop {
        if let HolderMsg::Status { records } = c.until("Status", |m| matches!(m, HolderMsg::Status { .. })) {
            if records.len() == 1 {
                break records;
            }
        }
    };
    assert_eq!(records[0].state, ProgramState::Done);
    c.send(ClientMsg::StatusSeen { ids: vec![] });
    match c.until("Status", |m| matches!(m, HolderMsg::Status { .. })) {
        HolderMsg::Status { records } => assert!(records.is_empty()),
        _ => unreachable!(),
    }
}

/// VT 进程的 pid：holder 的子进程里名字是 falcon-sessiond 的那个
fn vt_pid(root: &Root, sid: &str) -> u32 {
    let meta: falcon_proto::sessiond::SessionMeta =
        serde_json::from_slice(&std::fs::read(format!("{}/sessiond/sessions/{sid}.json", root.0)).unwrap()).unwrap();
    let out = Command::new("pgrep").args(["-P", &meta.pid.to_string()]).output().unwrap();
    let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|s| s.parse().ok()).collect();
    *pids.iter().find(|p| **p != meta.shell_pid).expect("找不到 VT 进程")
}

#[test]
fn vt_crash_is_survived_and_history_rebuilt_from_the_tail() {
    let root = Root::new("e");
    let sid = "0a1b2c3d00004004";
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).ok);
    let mut c = Client::connect(&root, sid);
    c.ready();
    c.input("echo before-crash\n");
    c.expect_output("before-crash");
    let pid = vt_pid(&root, sid);
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    c.until("VtRestarted", |m| matches!(m, HolderMsg::Event(Event::VtRestarted { .. })));
    // 会话照常：输入还在，快照里还有崩溃前的输出
    c.input("echo after-crash\n");
    c.expect_output("after-crash");
    let (_, snap) = c.sync(2);
    assert!(snap.contains("before-crash") && snap.contains("after-crash"), "{snap:?}");
}

#[test]
fn three_crashes_in_a_minute_degrade_to_pass_through() {
    let root = Root::new("f");
    let sid = "0a1b2c3d00004005";
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).ok);
    let mut c = Client::connect(&root, sid);
    c.ready();
    for i in 0..3 {
        // 等新的 VT 进程起来再杀
        std::thread::sleep(Duration::from_millis(300));
        let pid = vt_pid(&root, sid);
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        if i < 2 {
            c.until("VtRestarted", |m| matches!(m, HolderMsg::Event(Event::VtRestarted { .. })));
        }
    }
    c.until("VtDegraded", |m| matches!(m, HolderMsg::Event(Event::VtDegraded)));
    // 降级后照样转发，快照退化成原始尾巴
    c.input("echo still-alive\n");
    c.expect_output("still-alive");
    let (_, snap) = c.sync(2);
    assert!(snap.contains("still-alive"), "{snap:?}");
}

#[test]
fn hot_upgrade_swaps_the_vt_process_and_keeps_state() {
    let root = Root::new("g");
    let sid = "0a1b2c3d00004006";
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).ok);
    let mut c = Client::connect(&root, sid);
    c.ready();
    c.input("printf '\\033[?1049h\\033[Hin-alt-%d' $((1+1)); echo; echo; sleep 30\n");
    c.expect_output("in-alt-2");
    let old = vt_pid(&root, sid);
    c.send(ClientMsg::UpgradeVt { req: 9, binary: BIN.into() });
    match c.until("Upgraded", |m| matches!(m, HolderMsg::Upgraded { .. })) {
        HolderMsg::Upgraded { ok, message, .. } => assert!(ok, "{message}"),
        _ => unreachable!(),
    }
    assert_ne!(vt_pid(&root, sid), old);
    // 从二进制快照恢复的：备用屏状态还在
    let (_, snap) = c.sync(2);
    assert!(snap.contains("\x1b[?1049h") && snap.contains("in-alt-2"), "{snap:?}");
}

#[test]
fn most_recently_active_connection_sets_the_size() {
    let root = Root::new("h");
    let sid = "0a1b2c3d00004007";
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).ok);
    let mut a = Client::connect(&root, sid);
    let mut b = Client::connect(&root, sid);
    a.ready();
    a.send(ClientMsg::Resize(Resize { cols: 100, rows: 30 }));
    b.send(ClientMsg::Resize(Resize { cols: 60, rows: 20 }));
    a.input("stty size\n");
    a.expect_output("30 100");
    drop(b);
    std::thread::sleep(Duration::from_millis(200));
    a.input("stty size; echo done-$((1+1))\n");
    a.expect_output("done-2");
}

#[test]
fn kill_ends_the_session() {
    let root = Root::new("i");
    let sid = "0a1b2c3d00004008";
    assert!(spawn(&root, sid, &spec(&["/bin/sh"])).ok);
    let out = Command::new(BIN).args(["kill", "--root", &root.0, sid]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = Command::new(BIN).args(["list", "--json", "--root", &root.0]).output().unwrap();
    let list: Vec<falcon_proto::sessiond::ListEntry> = serde_json::from_slice(&out.stdout).unwrap();
    assert!(list.iter().all(|e| !e.alive), "{list:?}");
    assert!(list.iter().any(|e| e.sid == sid && e.exited.is_some()), "{list:?}");
}

#[test]
fn bridge_relays_the_protocol_and_creates_on_demand() {
    use std::io::Read as _;
    let root = Root::new("j");
    let sid = "0a1b2c3d00004009";
    let mut child = Command::new(BIN)
        .args(["bridge", "--root", &root.0, "--id", sid, "--create", &spec(&["/bin/sh"]).encode()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // 先是前导标记，之后才是帧
    let mut preamble = vec![0u8; falcon_proto::sessiond::BRIDGE_PREAMBLE.len()];
    stdout.read_exact(&mut preamble).unwrap();
    assert_eq!(preamble, falcon_proto::sessiond::BRIDGE_PREAMBLE);
    stdin
        .write_all(&ClientMsg::Hello(Hello { proto_min: PROTO_MIN, proto_max: PROTO_MAX, role: Role::Server, client: "t".into() }).encode())
        .unwrap();
    stdin.write_all(&ClientMsg::Sync { req: 1 }.encode()).unwrap();
    let mut seen = Vec::new();
    let mut sent = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let Ok(Some((k, p))) = read_frame(&mut stdout) else { break };
        match HolderMsg::decode(k, p).unwrap() {
            HolderMsg::Output { bytes, .. } => seen.extend(bytes),
            HolderMsg::Synced { snapshot, .. } => seen.extend(snapshot),
            _ => {}
        }
        let text = String::from_utf8_lossy(&seen).into_owned();
        // 等提示符出来再输入（见 Client::ready）
        if !sent && text.contains("$ ") {
            std::thread::sleep(Duration::from_millis(300));
            stdin.write_all(&ClientMsg::Input(b"echo via-$((1+1))-bridge\n".to_vec()).encode()).unwrap();
            sent = true;
        }
        if text.contains("via-2-bridge") {
            break;
        }
    }
    assert!(String::from_utf8_lossy(&seen).contains("via-2-bridge"), "{:?}", String::from_utf8_lossy(&seen));
    // 关掉 stdin：bridge 退出，会话还在
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
    let mut rest = Vec::new();
    let _ = stdout.read_to_end(&mut rest);
    let code = Command::new(BIN).args(["bridge", "--root", &root.0, "--id", sid]).stdin(std::process::Stdio::null()).status().unwrap();
    assert!(code.success(), "会话应当还在");
}

/// 录接回比对语料（设计文档第 15 节）：`cargo xtask sessiond --test` 不跑它，要重录时手动
/// `cargo test -p falcon-sessiond --test holder -- --ignored record_corpus`。
/// 产物入库在 tests/corpus/<名字>.bin，是 PTY 的原始输出（100×30）。
#[test]
#[ignore]
fn record_corpus() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus");
    std::fs::create_dir_all(dir).unwrap();
    let git_repo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
    // (名字, 依次输入的内容, 每步之后等多久)
    let scenarios: Vec<(&str, Vec<String>)> = vec![
        (
            "shell-colors",
            vec![
                "ls -G /usr/bin | head -60\n".into(),
                "for i in 0 1 2 3 4 5 6 7; do printf '\\033[3%dm普通%d \\033[1;3%dm粗体%d\\033[0m \\033[9%dm亮%d\\033[0m\\n' $i $i $i $i $i $i; done\n".into(),
                "printf '中文宽字符 😀 👨‍👩‍👧 🇨🇳 e\\xcc\\x81 \\033[4;38;2;255;100;0m真彩下划线\\033[0m\\n'\n".into(),
                "printf '%0250d\\n' 7\n".into(),
                "printf '\\033]8;;https://example.com\\033\\\\链接\\033]8;;\\033\\\\ 之后\\n'\n".into(),
            ],
        ),
        ("vim", vec!["printf 'fn main() {\\n    println!(\"你好\");\\n}\\n' > /tmp/sd-corpus.rs; seq 1 80\n".into(), "vim -u NONE -N -c 'set nu' -c 'set mouse=a' /tmp/sd-corpus.rs\n".into(), "Go// 第四行 😀\x1b".into()]),
        ("less", vec!["seq 1 40\n".into(), "seq 1 500 | less\n".into(), "  ".into()]),
        ("git-log", vec![format!("git -C {git_repo} log --graph --color=always --decorate=short -60 | cat\n")]),
        ("top", vec!["top -l 0 -s 1 -n 15 -stats pid,command,cpu,mem\n".into()]),
        (
            "cleared",
            vec!["seq 1 100\n".into(), "clear\n".into(), "echo 清屏之后\n".into(), "printf '\\033[2J\\033[H覆盖\\n'\n".into()],
        ),
        ("modes", vec!["printf '\\033[?2004h\\033[?1002h\\033[?1006h\\033[?1h\\033[5 q'; echo 模式开着\n".into()]),
    ];
    for (name, steps) in scenarios {
        let root = Root::new(&format!("rec-{name}"));
        let sid = "0a1b2c3d0000ffff";
        let mut s = spec(&["/bin/sh"]);
        s.cols = 100;
        s.rows = 30;
        s.env.push(("CLICOLOR".into(), "1".into()));
        assert!(spawn(&root, sid, &s).ok);
        let mut c = Client::connect(&root, sid);
        c.ready();
        for step in steps {
            c.input(&step);
            // 每步之后让输出落定
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                if let Ok(HolderMsg::Output { bytes, .. }) = c.rx.recv_timeout(Duration::from_millis(100)) {
                    c.seen.extend(bytes);
                }
            }
        }
        // 只要 Synced 之后的输出：从会话开始的那一段已经在 sync(1) 之前收全
        std::fs::write(format!("{dir}/{name}.bin"), &c.seen).unwrap();
        println!("{name}: {} 字节", c.seen.len());
    }
}
