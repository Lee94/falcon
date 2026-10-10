//! SessionManager 的集成测试：非持久的本地会话（真 PTY + /bin/sh），不装 Zellij、不跑
//! 用户的 login shell。持久会话（Zellij）的路径要真机验，见 docs/design/rust-unification.md。

use super::*;
use crate::sessions::local::LocalZellij;
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
    mgr.local.set_prepared(LocalZellij::non_durable(NonDurableReason::ArchUnsupported, None));
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
