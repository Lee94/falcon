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

/// 收到的最后一条 size：(cols, rows, owner)
fn last_size(p: &Peer) -> Option<(u64, u64, bool)> {
    let m = p.msgs("size").into_iter().last()?;
    Some((m["cols"].as_u64()?, m["rows"].as_u64()?, m["owner"].as_bool()?))
}

#[test]
fn size_follows_the_latest_visible_typist_and_is_pushed_to_every_viewer() {
    run(async {
        let h = harness();
        let s = new_session(&h).await;
        let mut big = Peer::new();
        let mut small = Peer::new();
        h.mgr.add_viewer(&s.id, big.viewer.clone());
        h.mgr.resize(&s.id, big.id(), 200, 50);
        h.mgr.add_viewer(&s.id, small.viewer.clone());
        h.mgr.resize(&s.id, small.id(), 90, 60);
        // 后连上来的只是看：尺寸还是先到的那扇的，两边看到的是同一份输出
        h.mgr.input(&s.id, big.id(), "stty size\n");
        big.wait("先到的那扇定尺寸", has_output("50 200")).await;
        small.wait("小屏也收到", has_output("50 200")).await;
        assert_eq!(last_size(&big), Some((200, 50, true)));
        // 小屏一连上就知道生效尺寸（回放之前），自己不是定尺寸的那扇
        assert!(matches!(small.seen.first(), Some(Got::Msg(m)) if m["type"] == "size"), "{:?}", small.seen);
        assert_eq!(last_size(&small), Some((200, 50, false)));

        // 小屏一敲键尺寸就换过去，两边都被告知
        big.seen.clear();
        small.seen.clear();
        h.mgr.input(&s.id, small.id(), "stty size\n");
        small.wait("敲键的那扇定尺寸", has_output("60 90")).await;
        big.drain();
        assert_eq!(last_size(&small), Some((90, 60, true)));
        assert_eq!(last_size(&big), Some((90, 60, false)));

        // 小屏到了后台：尺寸还给看得见的大屏；后台窗口里的输入不再抢回去
        small.seen.clear();
        h.mgr.set_visibility(&s.id, small.id(), false);
        h.mgr.input(&s.id, small.id(), "stty size\n");
        small.wait("后台不算活跃", has_output("50 200")).await;
        assert_eq!(last_size(&small), Some((200, 50, false)));

        // 大屏走了，只剩后台的小屏：谁都看不见时跟剩下的人
        small.seen.clear();
        h.mgr.remove_viewer(&s.id, big.id());
        h.mgr.input(&s.id, small.id(), "stty size\n");
        small.wait("只剩后台的人", has_output("60 90")).await;
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

/// 「升级后重启」的接回撞上一次失败的部署（缓存在那儿的判定）：会话照样接得回来。
/// 以前部署失败里"稳定的那几种"直接判 session-gone——2026-10-14 两条推送踩了同一个临时文件，
/// 报错被认成目录写不了，活着的会话就这样被标成没了
#[test]
fn a_cached_deploy_failure_does_not_bury_a_live_session() {
    if !sessiond_available() {
        return;
    }
    run(async {
        let h = durable_harness("f").await;
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 80, 24);
        a.wait("提示符", has_output("$")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        h.mgr.input(&s.id, a.id(), "echo st$((3+4))ill\n");
        a.wait("输出", has_output("st7ill")).await;

        h.db.update_session_state(&s.id, SessionState::Unverified, None);
        let Harness { dir, db, mgr, project } = h;
        drop(mgr);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
        let mgr = SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()));
        mgr.local.set_base_env(test_env());
        mgr.local.set_prepared(LocalSessiond::non_durable(
            NonDurableReason::DirNotWritable,
            Some("chmod: cannot access '….partial': No such file or directory".into()),
        ));
        let h = Harness { dir, db, mgr, project };

        let mut b = Peer::new();
        h.mgr.add_viewer(&s.id, b.viewer.clone());
        h.mgr.resize(&s.id, b.id(), 80, 24);
        b.wait("接回后 active", has_state("active")).await;
        b.wait("历史还在", has_replay("st7ill")).await;
        assert!(h.mgr.local.peek().is_some_and(|l| l.durable), "接回时重新部署过，新会话也该是持久的");
        h.mgr.terminate(&s.id, true).await;
        kill_all(&h).await;
    });
}

fn ssh_project(id: &str) -> ProjectRow {
    ProjectRow {
        id: id.into(),
        name: "remote".into(),
        project_type: "ssh".into(),
        // TEST-NET-1：用例走不到连接那一步，万一走到了也连不上任何真机器
        ssh_host: Some("192.0.2.1".into()),
        ssh_port: Some(22),
        ssh_username: Some("fx".into()),
        ssh_auth_method: Some("key".into()),
        created_at: now_ms(),
        ..Default::default()
    }
}

/// 接回时宿主机上的守护进程没就位（这里是没授权）：说明不了会话没了，留在 unverified 等下一轮
#[test]
fn reattach_keeps_waiting_when_the_daemon_is_not_ready() {
    run(async {
        let h = harness();
        let project = ssh_project("ssh-na");
        h.db.insert_project(&project);
        let row = SessionRow {
            id: "1667c495-0ab4-4a6f-b096-ec458f0af07a".into(),
            project_id: project.id.clone(),
            state: "unverified".into(),
            durable: 1,
            created_at: now_ms(),
            last_active_at: now_ms(),
            ..Default::default()
        };
        h.db.insert_session(&row);
        let res = h.mgr.ensure_attached(&row.id, true).await;
        assert!(matches!(&res, Err(SessionError::Failed(m)) if m.contains("not-authorized")), "{res:?}");
        let after = h.db.get_session(&row.id).unwrap();
        assert_eq!((after.state.as_str(), after.dead_reason.as_deref()), ("unverified", None));
    });
}

/// 缓存的部署失败：放凉之前接回照用（同一轮里排队接回的几个会话别各推一遍），放凉之后丢掉重推；
/// 成功的结论不丢
#[test]
fn stale_deploy_failures_are_retried_after_the_cooldown() {
    use crate::sessions::ssh_host::RemoteSessiond;
    use std::time::Instant;
    run(async {
        let h = harness();
        let link = h.mgr.get_link(&ssh_project("ssh-cd"));
        let failed = RemoteSessiond {
            durable: false,
            reason: Some(NonDurableReason::DirNotWritable),
            detail: None,
            deployed: None,
            kind: HostKind::Posix,
        };
        let long_ago = Instant::now().checked_sub(REDEPLOY_COOLDOWN + Duration::from_secs(1)).unwrap();
        let set = |state: &RemoteSessiond, at: Instant| {
            let mut r = link.remote.borrow_mut();
            r.sessiond = Some(state.clone());
            r.sessiond_at = Some(at);
        };

        set(&failed, Instant::now());
        link.forget_failed_sessiond(REDEPLOY_COOLDOWN);
        assert!(link.cached_sessiond().is_some(), "刚失败：同一轮里接着用");

        set(&failed, long_ago);
        link.forget_failed_sessiond(REDEPLOY_COOLDOWN);
        assert!(link.cached_sessiond().is_none(), "放凉了：下次重推");

        set(&RemoteSessiond { durable: true, reason: None, ..failed.clone() }, long_ago);
        link.forget_failed_sessiond(REDEPLOY_COOLDOWN);
        assert!(link.cached_sessiond().is_some_and(|s| s.durable), "成功的结论不丢");
    });
}

/// 等到会话的自动标题是 want（每 50ms 看一次，最多 15s）
async fn wait_title(h: &Harness, id: &str, want: &str) {
    for _ in 0..300 {
        if h.mgr.title_of(id).as_deref() == Some(want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等不到标题 {want:?}，现在是 {:?}", h.mgr.title_of(id));
}

#[test]
fn auto_title_follows_the_program_even_with_nobody_watching() {
    if !sessiond_available() {
        return;
    }
    run(async {
        let h = durable_harness("t").await;
        let s = new_session(&h).await;
        let mut a = Peer::new();
        h.mgr.add_viewer(&s.id, a.viewer.clone());
        h.mgr.resize(&s.id, a.id(), 80, 24);
        a.wait("提示符", has_output("$")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        // 一条命令走完全程：shell 在提示符写的标题（不算）→ sleep → 没人看着时换成 awk → awk 报 7501
        // （身份）→ awk 写标题（带 Claude Code 的状态符号）。报状态、写标题的得是 shell 之外的进程：
        // shell 自己写的标题守护进程不收，`sh -c` 又会被当成 shell
        h.mgr.input(
            &s.id,
            a.id(),
            "printf '\\033]2;fay@mac: ~\\007'; sleep 4; echo go; awk 'BEGIN{system(\"sleep 4\"); \
             printf \"\\033]7501;state=working:app=claude-code\\033\\\\\"; fflush(); system(\"sleep 3\"); \
             printf \"\\033]2;\u{2733} 修复登录 bug\\007\"; fflush(); system(\"sleep 30\")}'\n",
        );
        a.wait("前台命令", |seen| seen.iter().any(|g| matches!(g, Got::Msg(m) if m["type"] == "title" && m["title"] == "sleep")))
            .await;
        assert!(a.msgs("title").iter().all(|m| m["title"] != "fay@mac: ~"), "{:?}", a.msgs("title"));
        // 走开：之后的变化照样探得到（侧栏上的后台会话不再停在最后一次有人看时）
        h.mgr.remove_viewer(&s.id, a.id());
        wait_title(&h, &s.id, "awk").await;
        wait_title(&h, &s.id, "claude").await;
        wait_title(&h, &s.id, "修复登录 bug").await;
        h.mgr.terminate(&s.id, false).await;
        kill_all(&h).await;
    });
}

/// SSH 上的守护进程会话：推送 → bridge 建会话 → 「重启后端」后接回 → 退出判定 → 终止。
///
/// 只对**隔离的** sshd 跑（CLAUDE.md），手动：起一个容器（docs/design/session-daemon.md 第 15 节），
/// `FALCON_SSH_TEST=127.0.0.1:2299:fx:/path/to/id_ed25519 cargo test -p falcon-server --lib ssh_durable -- --ignored --test-threads=1`
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

/// 升级后重启：同一台机器上两个项目各有一个会话等着接回，宿主机上没有这个版本的守护进程
/// （删掉二进制来模拟换了版本号），两条链路都要推。两个会话都得接回来、宿主机上不留临时文件——
/// 2026-10-14 两条推送踩了同一个 `.partial`，报错被认成目录写不了，两个活着的会话被判 session-gone。
/// 手动：同 ssh_durable_session_round_trip。两个用例共用远端家目录、这个会删远端的二进制，
/// 要 `--test-threads=1`
#[test]
#[ignore]
fn ssh_durable_two_projects_on_one_host_resume_together() {
    use crate::exec::Exec as _;
    let Ok(spec) = std::env::var("FALCON_SSH_TEST") else {
        eprintln!("跳过：没设 FALCON_SSH_TEST=host:port:user:key");
        return;
    };
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    let (host, port, user, key) = (parts[0], parts[1].parse::<i64>().unwrap(), parts[2], parts[3]);
    run(async {
        let dir = tempfile::Builder::new().prefix("fsd-ssh2-").tempdir_in("/tmp").unwrap();
        let db = Arc::new(Db::open(dir.path()).unwrap());
        let new_mgr = |db: &Arc<Db>| {
            let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
            SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()))
        };
        let projects: Vec<ProjectRow> = ["ssh-a", "ssh-b"]
            .into_iter()
            .map(|id| ProjectRow {
                id: id.into(),
                name: id.into(),
                project_type: "ssh".into(),
                ssh_host: Some(host.into()),
                ssh_port: Some(port),
                ssh_username: Some(user.into()),
                ssh_auth_method: Some("key".into()),
                ssh_key_path: Some(key.into()),
                created_at: now_ms(),
                ..Default::default()
            })
            .collect();
        for p in &projects {
            db.insert_project(p);
        }
        db.upsert_session_host(host, port as u16, user, crate::db::SessionHostPatch { authorized: Some(Some(1)), ..Default::default() });

        let mgr = new_mgr(&db);
        let marks = [("echo a$((1+1))lpha\n", "a2lpha"), ("echo b$((2+2))eta\n", "b4eta")];
        let mut ids = Vec::new();
        for (p, (cmd, mark)) in projects.iter().zip(marks) {
            let s = mgr.create_session(p, String::new(), OscColorHint::default(), None).await.unwrap();
            assert!(s.durable, "{s:?}");
            let mut a = Peer::new();
            mgr.add_viewer(&s.id, a.viewer.clone());
            mgr.resize(&s.id, a.id(), 80, 24);
            a.wait("提示符", has_output("$")).await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            mgr.input(&s.id, a.id(), cmd);
            a.wait("输出", has_output(mark)).await;
            mgr.remove_viewer(&s.id, a.id());
            ids.push(s.id);
        }
        let deployed = mgr.prepare(&projects[0], None, None, false).await.deployed.unwrap();

        // 「升级后重启」：库停在 unverified，宿主机上这个版本的二进制还没有
        for id in &ids {
            db.update_session_state(id, SessionState::Unverified, None);
        }
        let rm = mgr.get_link(&projects[0]).exec(&format!("rm -f {}", crate::host::quote_posix(&deployed.bin)), None).await.unwrap();
        assert!(rm.ok(), "{rm:?}");
        drop(mgr);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mgr = new_mgr(&db);
        mgr.resume_unverified();
        for _ in 0..1000 {
            if ids.iter().all(|id| db.get_session(id).is_some_and(|r| r.state != "unverified")) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for id in &ids {
            let row = db.get_session(id).unwrap();
            assert_eq!((row.state.as_str(), row.dead_reason.as_deref()), ("active", None), "{id}");
        }
        for (id, (_, mark)) in ids.iter().zip(marks) {
            let mut b = Peer::new();
            mgr.add_viewer(id, b.viewer.clone());
            mgr.resize(id, b.id(), 80, 24);
            b.wait("历史还在", has_replay(mark)).await;
        }
        let ls = mgr.get_link(&projects[0]).exec("ls -a ~/.falcon/bin", None).await.unwrap();
        assert!(!ls.stdout.contains(".partial"), "宿主机上留了临时文件：{}", ls.stdout);
        for id in &ids {
            mgr.terminate(id, true).await;
        }
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
