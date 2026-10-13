//! SessionManager 的集成测试：本地会话（真 PTY + /bin/sh），不跑用户的 login shell。
//! - 非持久：后端自己开 PTY；
//! - 持久：真的起守护进程（falcon-sessiond，ADR 0020）。找不到本机目标的守护进程时
//!   （没跑 `cargo xtask sessiond`）这几个用例跳过并说明原因，`cargo test --workspace` 不因此变红。

use super::*;
use crate::sessions::local::LocalSessiond;
use crate::sessions::local::tests::test_env;
use crate::sessions::viewer::{ViewerFrame, ViewerSink};

struct Harness {
    dir: tempfile::TempDir,
    db: Arc<Db>,
    mgr: Rc<SessionManager>,
    project: ProjectRow,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open(dir.path()).unwrap());
    let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
    let mgr = SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()));
    mgr.local.set_prepared(LocalSessiond::non_durable(NonDurableReason::ArchUnsupported, None));
    mgr.local.set_base_env(test_env());
    let project = ProjectRow {
        id: "p1".into(),
        name: "p".into(),
        project_type: "local".into(),
        working_dir: Some(dir.path().to_string_lossy().into_owned()),
        shell: Some("/bin/sh".into()),
        created_at: now_ms(),
        ..Default::default()
    };
    db.insert_project(&project);
    Harness { dir, db, mgr, project }
}

fn run(f: impl Future<Output = ()>) {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    tokio::task::LocalSet::new().block_on(&rt, f);
}

/// 一条 Viewer 收到的东西，按到达顺序
#[derive(Debug)]
enum Got {
    Msg(serde_json::Value),
    Output(String),
    Replay(String),
}

struct Peer {
    viewer: Rc<Viewer>,
    sink: ViewerSink,
    seen: Vec<Got>,
}

impl Peer {
    fn new() -> Peer {
        let (viewer, sink) = Viewer::new();
        Peer { viewer: Rc::new(viewer), sink, seen: Vec::new() }
    }

    fn id(&self) -> u64 {
        self.viewer.id
    }

    fn drain(&mut self) {
        while let Ok(f) = self.sink.rx.try_recv() {
            self.sink.written(&f);
            self.seen.push(match f {
                ViewerFrame::Text(t) => Got::Msg(serde_json::from_str(&t).unwrap()),
                ViewerFrame::Binary(b) => {
                    let text = String::from_utf8(b[1..].to_vec()).unwrap();
                    if b[0] == TERM_FRAME_OUTPUT { Got::Output(text) } else { Got::Replay(text) }
                }
            });
        }
    }

    /// 等到 pred 成立（每 20ms 看一次，最多 10s）
    async fn wait(&mut self, what: &str, pred: impl Fn(&[Got]) -> bool) {
        for _ in 0..500 {
            self.drain();
            if pred(&self.seen) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等不到 {what}：{:#?}", self.seen);
    }

    fn output(&self) -> String {
        self.seen
            .iter()
            .filter_map(|g| match g {
                Got::Output(s) | Got::Replay(s) => Some(s.as_str()),
                Got::Msg(_) => None,
            })
            .collect()
    }

    fn msgs(&self, ty: &str) -> Vec<&serde_json::Value> {
        self.seen.iter().filter_map(|g| if let Got::Msg(m) = g { (m["type"] == ty).then_some(m) } else { None }).collect()
    }
}

fn has_output(needle: &'static str) -> impl Fn(&[Got]) -> bool {
    move |seen| seen.iter().any(|g| matches!(g, Got::Output(s) | Got::Replay(s) if s.contains(needle)))
}

fn has_state(state: &'static str) -> impl Fn(&[Got]) -> bool {
    move |seen| seen.iter().any(|g| matches!(g, Got::Msg(m) if m["type"] == "state" && m["state"] == state))
}

async fn new_session(h: &Harness) -> Session {
    h.mgr.create_session(&h.project, String::new(), OscColorHint::default(), None).await.unwrap()
}

#[test]
fn non_durable_session_round_trip() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        assert!(!s.durable);
        assert_eq!(s.non_durable_reason, Some(NonDurableReason::ArchUnsupported));
        assert_eq!(h.db.get_session(&s.id).unwrap().state, "active");

        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        a.drain();
        // 连上来：先 replay，再 state active
        assert!(matches!(a.seen.first(), Some(Got::Replay(_))), "{:?}", a.seen);
        assert!(has_state("active")(&a.seen));

        h.mgr.resize(&s.id, a.id(), 100, 30);
        h.mgr.input(&s.id, a.id(), "stty size; echo \"$TERM\"; echo m\u{4e2d}rk\n");
        a.wait("stty 输出", has_output("30 100")).await;
        a.wait("中文回显", has_output("m中rk")).await;
        assert!(a.output().contains("xterm-256color"));

        // 尺寸 500ms 合并后落库
        tokio::time::sleep(Duration::from_millis(700)).await;
        let row = h.db.get_session(&s.id).unwrap();
        assert_eq!((row.cols, row.rows), (Some(100), Some(30)));

        // shell 退出：非持久会话即死亡，原因 exited，标题收回
        h.mgr.input(&s.id, a.id(), "exit\n");
        a.wait("dead", has_state("dead")).await;
        let dead = a.msgs("state").into_iter().find(|m| m["state"] == "dead").unwrap().clone();
        assert_eq!(dead["deadReason"], "exited");
        assert!(!a.msgs("title").is_empty());
        assert_eq!(h.db.get_session(&s.id).unwrap().dead_reason.as_deref(), Some("exited"));

        // 死会话：新 Viewer 只收到 dead；能清除
        let mut b = Peer::new();
        h.mgr.add_viewer(&s.id, b.viewer.clone());
        b.drain();
        assert_eq!(b.seen.len(), 1);
        assert!(has_state("dead")(&b.seen));
        assert!(h.mgr.delete_dead(&s.id));
        assert!(h.db.get_session(&s.id).is_none());
        assert!(!h.mgr.delete_dead(&s.id));
        drop(h.dir);
    });
}

#[test]
fn smallest_viewer_wins_and_size_returns_when_it_leaves() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        let mut big = Peer::new();
        let mut small = Peer::new();
        h.mgr.add_viewer(&s.id, big.viewer.clone());
        h.mgr.resize(&s.id, big.id(), 200, 50);
        h.mgr.add_viewer(&s.id, small.viewer.clone());
        h.mgr.resize(&s.id, small.id(), 90, 60);
        h.mgr.input(&s.id, big.id(), "stty size\n");
        big.wait("最小格子", has_output("50 90")).await;
        // 两个 Viewer 看到的是同一份输出
        small.wait("小屏也收到", has_output("50 90")).await;

        h.mgr.remove_viewer(&s.id, small.id());
        h.mgr.input(&s.id, big.id(), "stty size\n");
        big.wait("尺寸还给大屏", has_output("50 200")).await;
        h.mgr.terminate(&s.id, false).await;
    });
}

#[test]
fn terminate_broadcasts_dead_and_forgets_the_row() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.terminate(&s.id, false).await;
        a.drain();
        assert!(has_state("dead")(&a.seen));
        assert!(h.db.get_session(&s.id).is_none());
        assert!(h.mgr.entry(&s.id).is_none());
        // 已经不存在的会话
        let mut b = Peer::new();
        h.mgr.add_viewer(&s.id, b.viewer.clone());
        b.drain();
        assert_eq!(b.msgs("error")[0]["message"], "会话不存在");
    });
}

#[test]
fn foreground_and_title_follow_the_running_command() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 80, 24);
        // 等 shell 出提示符再问：fork 之后、exec 之前前台进程还顶着测试二进制的名字
        a.wait("提示符", has_output("$")).await;
        assert!(!h.mgr.foreground(&s.id).await.busy);
        h.mgr.input(&s.id, a.id(), "sleep 30\n");
        let mut fg = SessionForeground { busy: false, command: None };
        for _ in 0..100 {
            fg = h.mgr.foreground(&s.id).await;
            if fg.busy {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(fg.command.as_deref(), Some("sleep"));
        // 有输出（回显）就排一次标题探测，2.5s 节流后推 title
        a.wait("title", |seen| seen.iter().any(|g| matches!(g, Got::Msg(m) if m["type"] == "title" && m["title"] == "sleep")))
            .await;
        assert_eq!(h.mgr.title_of(&s.id).as_deref(), Some("sleep"));
        h.mgr.terminate(&s.id, false).await;
    });
}

#[test]
fn lagged_viewer_is_resynced_with_a_replay_once_drained() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 80, 24);
        a.drain();
        // 假装它丢过帧：下一次 flush 不给增量，直接整份 replay
        a.viewer.lagged.set(true);
        h.mgr.input(&s.id, a.id(), "echo resync\n");
        a.wait("replay", |seen| seen.iter().any(|g| matches!(g, Got::Replay(s) if s.contains("resync")))).await;
        assert!(!a.viewer.lagged.get());
        h.mgr.terminate(&s.id, false).await;
    });
}

#[test]
fn askpass_prompts_reach_viewers() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        let hub = h.mgr.askpass.clone();
        let sid = s.id.clone();
        let pending = tokio::task::spawn_local(async move { hub.request("pw?", Some(&sid)).await });
        while h.mgr.askpass.pending_prompts().is_empty() {
            tokio::task::yield_now().await;
        }
        // 引擎里由 on_prompt 回调转给 broadcast_askpass；这里直接调
        let p = h.mgr.askpass.pending_prompts().pop().unwrap();
        h.mgr.broadcast_askpass(&p);
        a.drain();
        assert_eq!(a.msgs("askpass")[0]["prompt"], "pw?");
        // 后连上来的 Viewer 补发
        let mut b = Peer::new();
        h.mgr.add_viewer(&s.id, b.viewer.clone());
        b.drain();
        assert_eq!(b.msgs("askpass")[0]["id"], p.id.as_str());
        h.mgr.askpass.cancel(&p.id);
        assert!(pending.await.unwrap().is_err());
        h.mgr.terminate(&s.id, false).await;
    });
}

#[test]
fn backoff_schedule() {
    assert_eq!(backoff_ms(1), 1000);
    assert_eq!(backoff_ms(2), 2000);
    assert_eq!(backoff_ms(5), 16_000);
    assert_eq!(backoff_ms(6), 30_000);
    assert_eq!(backoff_ms(50), 30_000);
}

#[test]
fn host_rows_become_fake_projects() {
    let host = SshHostRow {
        id: "h1".into(),
        name: "box".into(),
        host: "10.0.0.2".into(),
        port: 2222,
        username: "me".into(),
        auth_method: "agent".into(),
        ..Default::default()
    };
    let p = host_as_project(&host);
    assert_eq!(p.id, "host:h1");
    assert_eq!(p.project_type, "ssh");
    assert_eq!(p.ssh_port, Some(2222));
    assert_eq!(p.host_id.as_deref(), Some("h1"));
    assert!(p.working_dir.is_none());
}

// ---------------------------------------------------------------- 守护进程会话（ADR 0020）

/// 本机目标的守护进程在不在（`cargo xtask sessiond` 编出来的，或 FALCON_SESSIOND_DIR）
fn sessiond_available() -> bool {
    let ok = crate::sessiond::command::local_target().and_then(crate::sessiond::bin::binary_for).is_some();
    if !ok {
        eprintln!("跳过：没有本机目标的会话守护进程（先跑 cargo xtask sessiond）");
    }
    ok
}

/// 持久会话的 harness：数据目录放在 /tmp 下的短路径（socket 路径有长度上限，虽然超了会落到
/// /tmp 的私有目录，短路径让用例走的是常规那条）。返回前先把守护进程部署好
async fn durable_harness(tag: &str) -> Harness {
    let dir = tempfile::Builder::new().prefix(&format!("fsd-{tag}-")).tempdir_in("/tmp").unwrap();
    let db = Arc::new(Db::open(dir.path()).unwrap());
    let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
    let mgr = SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()));
    mgr.local.set_base_env(test_env());
    let project = ProjectRow {
        id: "p1".into(),
        name: "p".into(),
        project_type: "local".into(),
        working_dir: Some(dir.path().to_string_lossy().into_owned()),
        shell: Some("/bin/sh".into()),
        created_at: now_ms(),
        ..Default::default()
    };
    db.insert_project(&project);
    let prep = mgr.prepare(&project, None, None, false).await;
    assert!(prep.durable, "本机部署守护进程失败：{:?} {:?}", prep.reason, prep.detail);
    Harness { dir, db, mgr, project }
}

/// 收掉这个数据目录下还活着的守护进程会话
async fn kill_all(h: &Harness) {
    if let Some(d) = h.mgr.local.peek().and_then(|l| l.deployed)
        && let Some(list) = crate::sessions::local::local_list_sessions(&d).await
    {
        let sids: Vec<String> = list.into_iter().filter(|e| e.alive).map(|e| e.sid).collect();
        crate::sessions::local::local_kill(&d, &sids).await;
    }
}

fn has_replay(needle: &'static str) -> impl Fn(&[Got]) -> bool {
    move |seen| seen.iter().any(|g| matches!(g, Got::Replay(s) if s.contains(needle)))
}

#[test]
fn durable_session_replays_from_the_daemon_snapshot() {
    if !sessiond_available() {
        return;
    }
    run(async {
        let h = durable_harness("a").await;
        let s = new_session(&h).await;
        assert!(s.durable, "{s:?}");
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 100, 30);
        a.wait("回放", |seen| seen.iter().any(|g| matches!(g, Got::Replay(_)))).await;
        a.wait("提示符", has_output("$")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        h.mgr.input(&s.id, a.id(), "stty size; echo d$((40+2))ne\n");
        a.wait("输出", has_output("d42ne")).await;
        a.wait("尺寸", has_output("30 100")).await;

        // 第二个 Viewer：拿到的是守护进程的快照，里面有之前的输出
        let mut b = Peer::new();
        h.mgr.add_viewer(&s.id, b.viewer.clone());
        b.wait("快照", has_replay("d42ne")).await;
        // 之后两边照常收输出
        h.mgr.input(&s.id, a.id(), "echo af$((1+1))ter\n");
        a.wait("a 收到", has_output("af2ter")).await;
        b.wait("b 收到", has_output("af2ter")).await;

        // shell 退出：守护进程报 Exit，会话 dead(exited)
        h.mgr.input(&s.id, a.id(), "exit 3\n");
        a.wait("dead", has_state("dead")).await;
        assert_eq!(h.db.get_session(&s.id).unwrap().dead_reason.as_deref(), Some("exited"));
        kill_all(&h).await;
    });
}

#[test]
fn durable_session_survives_a_backend_restart() {
    if !sessiond_available() {
        return;
    }
    run(async {
        let h = durable_harness("b").await;
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 80, 24);
        a.wait("提示符", has_output("$")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        h.mgr.input(&s.id, a.id(), "echo be$((2+2))fore\n");
        a.wait("输出", has_output("be4fore")).await;

        // "重启后端"：丢掉 manager（连接随之断开，会话在宿主机上照常），库停在 unverified
        h.db.update_session_state(&s.id, SessionState::Unverified, None);
        let Harness { dir, db, mgr, project } = h;
        drop(mgr);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
        let mgr = SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()));
        mgr.local.set_base_env(test_env());
        let h = Harness { dir, db, mgr, project };

        let mut b = Peer::new();
        h.mgr.add_viewer(&s.id, b.viewer.clone());
        h.mgr.resize(&s.id, b.id(), 80, 24);
        b.wait("接回后 active", has_state("active")).await;
        b.wait("历史还在", has_replay("be4fore")).await;
        h.mgr.input(&s.id, b.id(), "echo af$((3+3))ter\n");
        b.wait("还能用", has_output("af6ter")).await;

        // 终止：守护进程结束会话，库里的行删掉，宿主机上不再有活的 holder
        h.mgr.terminate(&s.id, true).await;
        assert!(h.db.get_session(&s.id).is_none());
        let d = h.mgr.local.peek().and_then(|l| l.deployed).unwrap();
        let list = crate::sessions::local::local_list_sessions(&d).await.unwrap();
        assert!(list.iter().all(|e| !e.alive), "{list:?}");
        kill_all(&h).await;
    });
}

#[test]
fn zellij_era_rows_end_up_session_gone() {
    if !sessiond_available() {
        return;
    }
    run(async {
        let h = durable_harness("c").await;
        // 升级前 Zellij 建的会话：库里有行，宿主机上没有任何守护进程的痕迹
        let row = SessionRow {
            id: "0a1b2c3d-0000-4000-8000-00000000beef".into(),
            project_id: h.project.id.clone(),
            name: String::new(),
            state: "unverified".into(),
            durable: 1,
            created_at: now_ms(),
            last_active_at: now_ms(),
            ..Default::default()
        };
        h.db.insert_session(&row);
        let res = h.mgr.ensure_attached(&row.id, true).await;
        assert_eq!(res.err(), Some(SessionError::Gone));
        let after = h.db.get_session(&row.id).unwrap();
        assert_eq!((after.state.as_str(), after.dead_reason.as_deref()), ("dead", Some("session-gone")));
    });
}

#[test]
fn program_status_is_pushed_to_viewers_and_listed() {
    if !sessiond_available() {
        return;
    }
    run(async {
        let h = durable_harness("d").await;
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 80, 24);
        a.wait("提示符", has_output("$")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // 程序要一直占着前台，否则守护进程按规范把 blocked 清掉（fg::at_shell）
        h.mgr.input(&s.id, a.id(), "printf '\\033]7501;state=blocked:kind=permission:app=claude-code\\033\\\\'; sleep 5\n");
        a.wait("program_status", |seen| {
            seen.iter().any(|g| matches!(g, Got::Msg(m) if m["type"] == "program_status" && m["records"][0]["state"] == "blocked"))
        })
        .await;
        let sum = h.mgr.program_status_of(&s.id).unwrap();
        assert_eq!((sum.state, sum.kind), (falcon_proto::ProgramState::Blocked, Some(falcon_proto::ProgramBlockKind::Permission)));
        h.mgr.terminate(&s.id, false).await;
        kill_all(&h).await;
    });
}

/// SSH 上的守护进程会话：推送 → bridge 建会话 → 「重启后端」后接回 → 退出判定 → 终止。
///
/// 只对**隔离的** sshd 跑（CLAUDE.md），手动：起一个容器（docs/design/session-daemon.md 第 15 节），
/// `FALCON_SSH_TEST=127.0.0.1:2299:fx:/path/to/id_ed25519 cargo test -p falcon-server --lib ssh_durable -- --ignored`
#[test]
#[ignore]
fn ssh_durable_session_round_trip() {
    let Ok(spec) = std::env::var("FALCON_SSH_TEST") else {
        eprintln!("跳过：没设 FALCON_SSH_TEST=host:port:user:key");
        return;
    };
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    let (host, port, user, key) = (parts[0], parts[1].parse::<i64>().unwrap(), parts[2], parts[3]);
    run(async {
        let dir = tempfile::Builder::new().prefix("fsd-ssh-").tempdir_in("/tmp").unwrap();
        let db = Arc::new(Db::open(dir.path()).unwrap());
        let new_mgr = |db: &Arc<Db>| {
            let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
            SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()))
        };
        let project = ProjectRow {
            id: "ssh1".into(),
            name: "remote".into(),
            project_type: "ssh".into(),
            ssh_host: Some(host.into()),
            ssh_port: Some(port),
            ssh_username: Some(user.into()),
            ssh_auth_method: Some("key".into()),
            ssh_key_path: Some(key.into()),
            created_at: now_ms(),
            ..Default::default()
        };
        db.insert_project(&project);
        let mgr = new_mgr(&db);
        // 没授权：非持久
        assert_eq!(mgr.prepare(&project, None, None, false).await.reason, Some(NonDurableReason::NotAuthorized));
        db.upsert_session_host(host, port as u16, user, crate::db::SessionHostPatch { authorized: Some(Some(1)), ..Default::default() });
        let stages = RefCell::new(Vec::new());
        let on_stage = |s: falcon_proto::InstallStage, _a: u32, _c: Option<String>| stages.borrow_mut().push(s);
        let prep = mgr.prepare(&project, Some(&on_stage), None, true).await;
        assert!(prep.durable, "{:?} {:?}", prep.reason, prep.detail);
        eprintln!("部署阶段：{:?}，二进制：{}", stages.borrow(), prep.deployed.as_ref().unwrap().bin);

        let s = mgr.create_session(&project, String::new(), OscColorHint::default(), None).await.unwrap();
        assert!(s.durable);
        let mut a = Peer::new();
        mgr.add_viewer(&s.id, a.viewer.clone());
        mgr.resize(&s.id, a.id(), 90, 25);
        a.wait("提示符", has_output("$")).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        mgr.input(&s.id, a.id(), "stty size; echo re$((5+5))mote; echo \"$TERM\"\n");
        a.wait("远端输出", has_output("re10mote")).await;
        a.wait("尺寸", has_output("25 90")).await;
        assert!(a.output().contains("xterm-256color"));
        // 拉起 bridge 的登录 shell 读 .profile 时打到 stdout 的噪音不能混进帧流（bridge 的前导标记
        // 挡掉）；会话里的交互式 bash 自己读 .bashrc 打的那行是正常的终端输出，不算
        assert!(!a.output().contains(".profile 打到"), "{}", a.output());

        // 「重启后端」：连接断开，会话在远端照常
        db.update_session_state(&s.id, SessionState::Unverified, None);
        drop(mgr);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mgr = new_mgr(&db);
        let mut b = Peer::new();
        mgr.add_viewer(&s.id, b.viewer.clone());
        mgr.resize(&s.id, b.id(), 90, 25);
        b.wait("接回", has_state("active")).await;
        b.wait("历史还在", has_replay("re10mote")).await;
        let fg = mgr.foreground(&s.id).await;
        assert!(!fg.busy, "{fg:?}");

        // shell 退出 → dead(exited)
        mgr.input(&s.id, b.id(), "exit 4\n");
        b.wait("dead", has_state("dead")).await;
        assert_eq!(db.get_session(&s.id).unwrap().dead_reason.as_deref(), Some("exited"));

        // 再建一个，终止它
        let s2 = mgr.create_session(&project, String::new(), OscColorHint::default(), None).await.unwrap();
        mgr.terminate(&s2.id, true).await;
        let link = mgr.get_link(&project);
        let list = link.list_sessions(prep.deployed.as_ref().unwrap()).await.unwrap();
        assert!(list.iter().all(|e| !e.alive), "{list:?}");
    });
}

/// 宿主机的 falcon 目录是 noexec 挂载：推得上去、跑不起来，报 verify-failed（不自动重试）。
/// 手动：`FALCON_SSH_TEST_NOEXEC=host:port:user:key cargo test -p falcon-server --lib ssh_noexec -- --ignored`
#[test]
#[ignore]
fn ssh_noexec_reports_verify_failed() {
    let Ok(spec) = std::env::var("FALCON_SSH_TEST_NOEXEC") else {
        eprintln!("跳过：没设 FALCON_SSH_TEST_NOEXEC=host:port:user:key");
        return;
    };
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    let (host, port, user, key) = (parts[0], parts[1].parse::<i64>().unwrap(), parts[2], parts[3]);
    run(async {
        let dir = tempfile::Builder::new().prefix("fsd-nx-").tempdir_in("/tmp").unwrap();
        let db = Arc::new(Db::open(dir.path()).unwrap());
        let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
        let mgr = SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()));
        let project = ProjectRow {
            id: "ssh-nx".into(),
            name: "noexec".into(),
            project_type: "ssh".into(),
            ssh_host: Some(host.into()),
            ssh_port: Some(port),
            ssh_username: Some(user.into()),
            ssh_auth_method: Some("key".into()),
            ssh_key_path: Some(key.into()),
            created_at: now_ms(),
            ..Default::default()
        };
        db.insert_project(&project);
        db.upsert_session_host(host, port as u16, user, crate::db::SessionHostPatch { authorized: Some(Some(1)), ..Default::default() });
        let prep = mgr.prepare(&project, None, None, true).await;
        assert!(!prep.durable);
        assert_eq!(prep.reason, Some(NonDurableReason::VerifyFailed), "{:?}", prep.detail);
        eprintln!("详情：{:?}", prep.detail);
        // 建会话照样能建：非持久，原因写明
        let s = mgr.create_session(&project, String::new(), OscColorHint::default(), None).await.unwrap();
        assert_eq!((s.durable, s.non_durable_reason), (false, Some(NonDurableReason::VerifyFailed)));
        mgr.terminate(&s.id, false).await;
    });
}
