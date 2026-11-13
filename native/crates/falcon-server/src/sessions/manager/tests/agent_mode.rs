use super::*;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 真 PTY 中的 CLI 替身，另起持有变量/目录的 shell 与持续写入的后台任务。
/// 不调用真实 Claude、认证或网络；通过进程标识和持续输出验收，不只比较 UUID。
fn fake_claude() -> &'static Path {
    static BIN: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    &BIN.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("fake.rs");
        let bin = dir.path().join("claude");
        std::fs::write(&source, r##"
use std::io::{BufRead, Write};
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = std::env::var("FAKE_AGENT_ROOT").unwrap();
    let sid = args.windows(2).find(|a| matches!(a[0].as_str(), "--session-id" | "--resume")).unwrap()[1].clone();
    let pid = std::process::id();
    writeln!(std::fs::OpenOptions::new().append(true).create(true).open(format!("{root}/cli.log")).unwrap(), "{pid}|{}", args.join(" ")).unwrap();
    if args.iter().any(|a| a == "-p") {
        if std::path::Path::new(&format!("{root}/gui-ignore-term")).exists() {
            unsafe extern "C" { fn signal(sig: i32, handler: usize) -> usize; }
            unsafe { signal(15, 1); }
        }
        if std::path::Path::new(&format!("{root}/gui-fail")).exists() { println!("No conversation found"); return; }
        let mut model = "sonnet";
        let mut effort = "high";
        let mut turn = 0;
        let field = |line: &str, name: &str| -> String {
            line.split(&format!("\"{name}\":\"")).nth(1).unwrap_or_default().split('"').next().unwrap_or_default().to_string()
        };
        let mut log = std::fs::OpenOptions::new().append(true).create(true).open(format!("{root}/protocol.log")).unwrap();
        for line in std::io::stdin().lock().lines() {
            let line = line.unwrap(); writeln!(log, "{line}").unwrap(); log.flush().unwrap();
            if std::path::Path::new(&format!("{root}/gui-hang")).exists() { continue; }
            let request_id = field(&line, "request_id");
            if line.contains("\"subtype\":\"initialize\"") {
                println!("{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"{request_id}\",\"response\":{{\"models\":[{{\"value\":\"sonnet\",\"displayName\":\"Sonnet\",\"supportsEffort\":true,\"supportedEffortLevels\":[\"low\",\"high\",\"max\"]}},{{\"value\":\"plain\",\"supportsEffort\":false}}]}}}}}}");
            } else if line.contains("\"subtype\":\"get_settings\"") {
                println!("{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"{request_id}\",\"response\":{{\"applied\":{{\"model\":\"{model}\",\"effort\":\"{effort}\"}}}}}}}}");
            } else if line.contains("\"subtype\":\"set_model\"") || line.contains("\"subtype\":\"apply_flag_settings\"") {
                if line.contains("\"model\":\"plain\"") { model = "plain"; }
                if line.contains("\"model\":\"sonnet\"") { model = "sonnet"; }
                if line.contains("\"effortLevel\":\"max\"") { effort = "max"; }
                println!("{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"{request_id}\",\"response\":{{}}}}}}");
            } else if line.contains("\"subtype\":\"interrupt\"") || line.contains("\"type\":\"control_response\"") {
                println!("{{\"type\":\"result\",\"is_error\":false}}");
            } else if line.contains("\"type\":\"user\"") {
                turn += 1;
                let mut transcript = std::fs::OpenOptions::new().append(true).create(true).open(format!("{root}/.claude/projects/test/{sid}.jsonl")).unwrap();
                writeln!(transcript, "{{\"type\":\"user\",\"sessionId\":\"{sid}\",\"uuid\":\"gui-{pid}-{turn}\",\"message\":{{\"content\":\"GUI prompt\"}}}}").unwrap();
                transcript.flush().unwrap();
                println!("{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{sid}\",\"model\":\"{model}\"}}");
                if line.contains("permission") {
                    println!("{{\"type\":\"control_request\",\"request_id\":\"ask-1\",\"request\":{{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tool-1\",\"input\":{{\"command\":\"true\"}}}}}}");
                } else if !line.contains("wait") {
                    println!("{{\"type\":\"assistant\",\"message\":{{\"id\":\"reply-1\",\"content\":[{{\"type\":\"text\",\"text\":\"GUI reply\"}}]}}}}");
                    println!("{{\"type\":\"result\",\"is_error\":false}}");
                }
            }
            std::io::stdout().flush().unwrap();
        }
        return;
    }
    if std::path::Path::new(&format!("{root}/terminal-fail")).exists() { return; }
    if std::path::Path::new(&format!("{root}/terminal-slow")).exists() {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    let parent = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "ppid="]).output().unwrap();
    let parent = String::from_utf8(parent.stdout).unwrap().trim().to_string();
    std::fs::write(format!("{root}/original-shell"), &parent).unwrap();
    let _independent = std::process::Command::new("/bin/sh").args(["-c", r#"while kill -0 "$FAKE_PARENT" 2>/dev/null; do printf '%s\n' "$$" >> "$FAKE_AGENT_ROOT/independent-heartbeat"; sleep 0.05; done"#]).env("FAKE_PARENT", parent).spawn().unwrap();
    let shell = std::process::Command::new("/bin/sh").args(["-c", r#"export SHELL_MARKER=kept; cd "$FAKE_AGENT_ROOT" || exit; printf '%s:%s:%s' "$$" "$SHELL_MARKER" "$PWD" > shell-state; while kill -0 "$FAKE_PARENT" 2>/dev/null; do sleep 0.05; done"#]).env("FAKE_PARENT", pid.to_string()).spawn().unwrap();
    let task = std::process::Command::new("/bin/sh").args(["-c", r#"while kill -0 "$FAKE_PARENT" 2>/dev/null; do printf '%s\n' "$$" >> "$FAKE_AGENT_ROOT/heartbeat"; sleep 0.05; done"#]).env("FAKE_PARENT", pid.to_string()).spawn().unwrap();
    std::fs::write(format!("{root}/children"), format!("{}:{}", shell.id(), task.id())).unwrap();
    let project = format!("{root}/.claude/projects/test");
    std::fs::create_dir_all(&project).unwrap();
    let mut transcript = std::fs::OpenOptions::new().append(true).create(true).open(format!("{project}/{sid}.jsonl")).unwrap();
    let status = |s: &str| { print!("\x1b]7501;state={s}:app=claude-code\x1b\\"); std::io::stdout().flush().unwrap(); };
    status("idle"); println!("CLI_READY"); std::io::stdout().flush().unwrap();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        match line.as_str() {
            "done" => {
                writeln!(transcript, "{{\"type\":\"user\",\"sessionId\":\"{sid}\",\"uuid\":\"u1\",\"message\":{{\"content\":\"hello\"}}}}").unwrap();
                writeln!(transcript, "{{\"type\":\"assistant\",\"sessionId\":\"{sid}\",\"uuid\":\"a1\",\"message\":{{\"id\":\"m1\",\"content\":[{{\"type\":\"text\",\"text\":\"**mirror ready**\"}}]}}}}").unwrap();
                transcript.flush().unwrap(); status("done");
            }
            "quit" => return,
            "busy" => status("working"), "blocked" => status("blocked"),
            "ignore-term" => {
                unsafe extern "C" { fn signal(sig: i32, handler: usize) -> usize; }
                unsafe { signal(15, 1); }
                status("idle");
            }
            _ => {}
        }
        println!("CLI_INPUT:{line}"); std::io::stdout().flush().unwrap();
    }
}
"##).unwrap();
        let out = std::process::Command::new("rustc").arg("--edition=2024").arg(&source).arg("-o").arg(&bin).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        (dir, bin)
    }).1
}

async fn mode_harness(tag: &str) -> (Harness, Session, Peer, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let mut h = durable_harness(tag).await;
    let bin = h.dir.path().join("fake-bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::copy(fake_claude(), bin.join("claude")).unwrap();
    let shell = h.dir.path().join("login-sh");
    std::fs::write(&shell, "#!/bin/sh\nexport ORIGINAL_MARKER=kept\ncd \"$FAKE_AGENT_ROOT\" || exit\nif [ \"$3\" = -c ]; then exec /bin/bash --noprofile --norc -i -c \"$4\"; fi\nexec /bin/sh\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = h.dir.path().join("cli.log");
    let mut env = test_env();
    env[0].1 = format!("{}:{}", bin.display(), env[0].1);
    env[1].1 = h.dir.path().to_string_lossy().into_owned();
    env.push(("FAKE_AGENT_ROOT".into(), h.dir.path().to_string_lossy().into_owned()));
    h.mgr.local.set_base_env(env);
    h.project.shell = Some(shell.to_string_lossy().into_owned());
    h.db.insert_project(&h.project);
    let session = h.mgr.create_session(&h.project, String::new(), OscColorHint::default(), Some(SessionAgent::Claude), false).await.unwrap();
    let mut peer = mirror_peer();
    h.mgr.add_viewer(&session.id, peer.viewer.clone());
    h.mgr.resize(&session.id, peer.id(), 100, 30);
    peer.wait("真实 CLI 就绪", has_output("CLI_READY")).await;
    (h, session, peer, log)
}

async fn terminal_status(h: &Harness, session: &Session, peer: &mut Peer, input: &str, state: &str) {
    peer.seen.clear();
    h.mgr.input(&session.id, peer.id(), &format!("{input}\n"));
    peer.wait("程序状态", |seen| seen.iter().any(|g| matches!(g, Got::Msg(m) if m["type"] == "program_status" && m["records"][0]["state"] == state))).await;
}

fn mirror_peer() -> Peer {
    let peer = Peer::new();
    peer.viewer.terminal_backed_agent_view.set(true);
    peer
}

fn file_text(root: &Path, name: &str) -> String { std::fs::read_to_string(root.join(name)).unwrap_or_default() }

async fn wait_file(root: &Path, name: &str) -> String {
    for _ in 0..100 {
        let text = file_text(root, name);
        if !text.is_empty() { return text; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("等待测试文件 {name} 超时");
}

async fn wait_protocol(h: &Harness, needle: &str) {
    for _ in 0..200 {
        if file_text(h.dir.path(), "protocol.log").contains(needle) { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("未收到图形控制 {needle}：{}", file_text(h.dir.path(), "protocol.log"));
}

async fn wait_gui_idle(h: &Harness, session: &Session) {
    for _ in 0..200 {
        if h.mgr.entry(&session.id).unwrap().s.borrow().agent_driver.as_ref().is_some_and(|d| d.ready_to_switch()) { return; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("图形 Agent 没有进入已确认的空闲状态");
}

#[test]
fn fresh_claude_can_enter_complete_gui_before_the_first_prompt() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("gui-fresh").await;
        let mut observer = Peer::new();
        h.mgr.add_viewer(&session.id, observer.viewer.clone());
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        let epoch = entry.s.borrow().backend_epoch;
        let shell = wait_file(h.dir.path(), "original-shell").await;
        let before = wait_file(h.dir.path(), "independent-heartbeat").await.lines().count();
        peer.drain(); observer.drain(); peer.seen.clear(); observer.seen.clear();
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        wait_gui_idle(&h, &session).await;
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        assert_eq!(entry.s.borrow().backend_epoch, epoch);
        assert!(!entry.s.borrow().agent_driver.as_ref().unwrap().state.terminal_backed);
        assert!(entry.s.borrow().agent_driver.as_ref().unwrap().state.entries.is_empty());
        for viewer in [&mut peer, &mut observer] {
            viewer.drain();
            let snapshots = viewer.msgs("agent_snapshot");
            assert_eq!(snapshots.first().unwrap()["state"]["status"], "unknown", "所有 Viewer 在握手完成前均不可提交输入");
            assert_eq!(snapshots.last().unwrap()["state"]["status"], "idle", "解锁输入后再发布可交互快照");
        }
        let starts = std::fs::read_to_string(log).unwrap();
        let lines: Vec<_> = starts.lines().collect();
        assert_eq!(lines.len(), 2, "{starts}");
        assert!(lines[1].contains(&format!("--session-id {}", session.id)), "{starts}");
        assert!(!lines[1].contains("--resume"));
        assert_ne!(lines[0].split('|').next(), lines[1].split('|').next(), "首次转换确实重启 CLI，不应声称同一进程");
        assert_eq!(unsafe { libc::kill(shell.trim().parse::<i32>().unwrap(), 0) }, 0, "原 shell 必须继续存在");
        for _ in 0..100 {
            if file_text(h.dir.path(), "independent-heartbeat").lines().count() > before { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(file_text(h.dir.path(), "independent-heartbeat").lines().count() > before);
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn complete_gui_resumes_same_uuid_and_controls_prompts_permissions_models_effort_and_interrupts() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("gui-control").await;
        terminal_status(&h, &session, &mut peer, "done", "done").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        wait_gui_idle(&h, &session).await;
        let state = entry.s.borrow().agent_driver.as_ref().unwrap().state.clone();
        assert!(!state.terminal_backed);
        assert!(state.entries.iter().any(|e| matches!(e, falcon_proto::AgentEntry::Message { text, .. } if text.contains("mirror ready"))), "转换不能丢失旧历史");
        assert!(std::fs::read_to_string(&log).unwrap().lines().nth(1).unwrap().contains(&format!("--resume {}", session.id)));
        h.mgr.agent_set_model(&session.id, "plain"); wait_protocol(&h, "\"model\":\"plain\"").await;
        wait_gui_idle(&h, &session).await;
        h.mgr.agent_set_model(&session.id, "sonnet"); wait_gui_idle(&h, &session).await;
        h.mgr.agent_set_reasoning_effort(&session.id, Some("max")); wait_protocol(&h, "\"effortLevel\":\"max\"").await;
        wait_gui_idle(&h, &session).await;
        assert_eq!(entry.s.borrow().agent_driver.as_ref().unwrap().state.reasoning_effort.as_deref(), Some("max"));
        h.mgr.agent_prompt(&session.id, "permission please");
        for _ in 0..200 {
            if entry.s.borrow().agent_driver.as_ref().unwrap().state.status == falcon_proto::AgentTurnStatus::AwaitingPermission { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(entry.s.borrow().agent_driver.as_ref().unwrap().state.status, falcon_proto::AgentTurnStatus::AwaitingPermission);
        h.mgr.agent_permission(&session.id, "tool-1", falcon_proto::AgentPermissionDecision::Allow, None);
        wait_protocol(&h, "\"behavior\":\"allow\"").await; wait_gui_idle(&h, &session).await;
        h.mgr.agent_prompt(&session.id, "wait for interrupt"); wait_protocol(&h, "wait for interrupt").await;
        h.mgr.agent_interrupt(&session.id); wait_protocol(&h, "\"subtype\":\"interrupt\"").await;
        wait_gui_idle(&h, &session).await;
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2, "重复请求不能重启图形进程");
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn official_terminal_round_trips_gui_with_same_uuid_backend_shell_and_independent_task() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("tui-roundtrip").await;
        terminal_status(&h, &session, &mut peer, "done", "done").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        let epoch = entry.s.borrow().backend_epoch;
        let shell = wait_file(h.dir.path(), "original-shell").await;
        let heartbeat = wait_file(h.dir.path(), "independent-heartbeat").await.lines().count();
        for _ in 0..2 {
            h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
            h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap();
            assert!(entry.s.borrow().agent_driver.is_none());
            assert_eq!(h.db.session_agent_mode(&session.id).as_deref(), Some("terminal"));
            assert!(same_backend(&backend, &entry.backend().unwrap()));
            assert_eq!(entry.s.borrow().backend_epoch, epoch);
            peer.seen.clear();
            h.mgr.input(&session.id, peer.id(), "terminal-prompt\n");
            peer.wait("官方 CLI 能接收原始终端输入", has_output("CLI_INPUT:terminal-prompt")).await;
        }
        let starts = std::fs::read_to_string(log).unwrap();
        let starts: Vec<_> = starts.lines().collect();
        assert_eq!(starts.len(), 5);
        for (index, start) in starts.iter().enumerate().skip(1) {
            assert!(start.contains(&format!("--resume {}", session.id)), "{start}");
            assert_eq!(start.split('|').nth(1).unwrap().starts_with("-p "), index % 2 == 1, "真实 TUI 必须运行官方交互 CLI：{start}");
        }
        assert_eq!(unsafe { libc::kill(shell.trim().parse::<i32>().unwrap(), 0) }, 0);
        assert!(file_text(h.dir.path(), "independent-heartbeat").lines().count() > heartbeat);
        peer.seen.clear();
        h.mgr.input(&session.id, peer.id(), "quit\n");
        for _ in 0..100 {
            let fg = entry.backend().unwrap().foreground_process().unwrap().await;
            if fg.pid == shell.trim().parse().ok() && fg.name.as_deref().is_some_and(|n| is_shell_command(n, h.project.shell.as_deref())) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        h.mgr.input(&session.id, peer.id(), "printf 'ORIGINAL_STATE:%s:%s:%s\\n' \"$$\" \"$ORIGINAL_MARKER\" \"$PWD\"\n");
        let expected = format!("ORIGINAL_STATE:{}:kept:{}", shell.trim(), h.dir.path().display());
        peer.wait("原 shell 的环境与当前目录仍在", move |seen| seen.iter().any(|g| matches!(g, Got::Output(s) if s.contains(&expected)))).await;
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn terminal_conversion_keeps_all_viewers_unknown_and_blocks_inputs_until_real_cli_is_ready() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut first, _log) = mode_harness("tui-viewers").await;
        terminal_status(&h, &session, &mut first, "done", "done").await;
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        let entry = h.mgr.entry(&session.id).unwrap();
        let mut second = Peer::new();
        h.mgr.add_viewer(&session.id, second.viewer.clone());
        first.drain(); second.drain(); first.seen.clear(); second.seen.clear();
        std::fs::write(h.dir.path().join("terminal-slow"), "1").unwrap();
        let mgr = h.mgr.clone();
        let id = session.id.clone();
        let converting = tokio::task::spawn_local(async move { mgr.switch_agent_mode(&id, "terminal-control").await });
        for _ in 0..300 {
            if file_text(h.dir.path(), "cli.log").lines().count() == 3 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(file_text(h.dir.path(), "cli.log").lines().count(), 3);
        assert!(entry.s.borrow().agent_converting);
        assert!(!converting.is_finished(), "不能在官方 CLI 报告真实 idle 之前应答转换成功");
        let mut late = Peer::new();
        h.mgr.add_viewer(&session.id, late.viewer.clone());
        for peer in [&mut first, &mut second, &mut late] {
            peer.drain();
            for snapshot in peer.msgs("agent_snapshot") {
                assert_eq!(snapshot["state"]["active"], true);
                assert_eq!(snapshot["state"]["status"], "unknown");
            }
            assert!(peer.msgs("agent_meta").iter().all(|m| m["active"] != false), "EXIT 不能提前让 Viewer 切回可输入终端");
            h.mgr.input(&session.id, peer.id(), "must-not-reach-cli\n");
        }
        h.mgr.agent_prompt(&session.id, "must-not-reach-gui");
        h.mgr.agent_interrupt(&session.id);
        assert!(h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap_err().to_string().contains("正在转换"));
        assert!(h.mgr.switch_agent_mode(&session.id, "terminal-control").await.is_err());
        converting.await.unwrap().unwrap();
        assert!(!entry.s.borrow().agent_converting);
        for peer in [&mut first, &mut second, &mut late] {
            peer.drain();
            let snapshots = peer.msgs("agent_snapshot");
            let final_state = &snapshots.last().unwrap()["state"];
            assert_eq!(final_state["active"], false);
            assert_eq!(final_state["terminalBacked"], true);
            assert!(!peer.output().contains("CLI_INPUT:must-not-reach-cli"));
        }
        assert!(!file_text(h.dir.path(), "protocol.log").contains("must-not-reach-gui"));
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn terminal_conversion_refuses_changed_shell_and_handles_cli_exit_timeout_or_resume_failure_without_rebuilding_backend() {
    if !sessiond_available() { return; }
    run(async {
        for failure in ["changed-shell", "gui-ignore-term", "terminal-fail"] {
            let (h, session, mut peer, _log) = mode_harness(failure).await;
            terminal_status(&h, &session, &mut peer, "done", "done").await;
            if failure == "gui-ignore-term" { std::fs::write(h.dir.path().join(failure), "1").unwrap(); }
            h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
            let entry = h.mgr.entry(&session.id).unwrap();
            let backend = entry.backend().unwrap();
            if failure == "changed-shell" { entry.s.borrow_mut().agent_terminal_return.as_mut().unwrap().1 = "replaced".into(); }
            if failure == "terminal-fail" { std::fs::write(h.dir.path().join(failure), "1").unwrap(); }
            let error = h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap_err().to_string();
            let expected = match failure { "changed-shell" => "核实", "gui-ignore-term" => "超时", _ => "续接失败" };
            assert!(error.contains(expected), "{failure}: {error}");
            let starts = file_text(h.dir.path(), "cli.log");
            assert_eq!(starts.lines().count(), if failure == "terminal-fail" { 3 } else { 2 }, "失败不能重新创建对话或启动未核实的进程：{starts}");
            assert!(same_backend(&backend, &entry.backend().unwrap()));
            assert!(!entry.s.borrow().agent_converting);
            if failure != "terminal-fail" {
                let pid = starts.lines().nth(1).unwrap().split('|').next().unwrap().parse::<i32>().unwrap();
                assert_eq!(unsafe { libc::kill(pid, 0) }, 0, "拒绝或超时后原图形 CLI 应保留");
            }
            if failure == "changed-shell" {
                assert!(entry.s.borrow().agent_driver.as_ref().unwrap().ready_to_switch());
            } else if failure == "terminal-fail" {
                assert!(entry.s.borrow().agent_driver.is_none());
                assert!(entry.s.borrow().agent_terminal_return.is_none());
                assert_eq!(h.db.session_agent_mode(&session.id).as_deref(), Some("terminal"));
                peer.drain();
                let snapshots = peer.msgs("agent_snapshot");
                assert_eq!(snapshots.last().unwrap()["state"]["active"], false);
                assert_eq!(snapshots.last().unwrap()["state"]["terminalBacked"], true);
                peer.seen.clear();
                h.mgr.input(&session.id, peer.id(), "printf 'SHELL_%s\\n' recovered\n");
                peer.wait("官方续接失败后仍可操作原 shell", has_output("SHELL_recovered")).await;
            } else {
                assert_eq!(entry.s.borrow().agent_driver.as_ref().unwrap().state.status, falcon_proto::AgentTurnStatus::Unknown);
                assert!(entry.s.borrow().agent_driver.as_ref().unwrap().state.active, "未确认新 CLI 可用时 Viewer 保持禁用");
            }
            h.mgr.terminate(&session.id, true).await;
            kill_all(&h).await;
        }
    });
}

#[test]
fn terminal_conversion_rejects_active_turns_permissions_unknown_and_missing_history() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, _log) = mode_harness("tui-reject").await;
        terminal_status(&h, &session, &mut peer, "done", "done").await;
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        h.mgr.agent_prompt(&session.id, "wait for interrupt");
        wait_protocol(&h, "wait for interrupt").await;
        assert!(h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap_err().to_string().contains("空闲"));
        h.mgr.agent_interrupt(&session.id); wait_gui_idle(&h, &session).await;
        h.mgr.agent_prompt(&session.id, "permission please");
        for _ in 0..100 {
            if entry.s.borrow().agent_driver.as_ref().unwrap().state.status == falcon_proto::AgentTurnStatus::AwaitingPermission { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap_err().to_string().contains("空闲"));
        h.mgr.agent_permission(&session.id, "tool-1", falcon_proto::AgentPermissionDecision::Allow, None); wait_gui_idle(&h, &session).await;
        std::fs::write(h.dir.path().join(format!(".claude/projects/test/{}.jsonl", session.id)), "not json\n").unwrap();
        assert!(h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap_err().to_string().contains("历史"));
        entry.s.borrow_mut().agent_driver.as_mut().unwrap().state.status = falcon_proto::AgentTurnStatus::Unknown;
        assert!(h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap_err().to_string().contains("空闲"));
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        assert_eq!(file_text(h.dir.path(), "cli.log").lines().count(), 2, "拒绝转换不能启动新 CLI");
        assert!(!entry.s.borrow().agent_converting);
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn fresh_gui_can_return_to_official_terminal_before_the_first_prompt() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, _peer, log) = mode_harness("tui-fresh").await;
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap();
        let starts = std::fs::read_to_string(log).unwrap();
        let last = starts.lines().last().unwrap();
        assert!(last.contains(&format!("--session-id {}", session.id)), "{starts}");
        assert!(!last.contains(" -p ") && !last.contains("--resume"));
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn fresh_gui_can_return_to_official_terminal_after_the_first_prompt() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("tui-first-turn").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap();
        h.mgr.agent_prompt(&session.id, "first GUI prompt");
        wait_protocol(&h, "first GUI prompt").await;
        wait_gui_idle(&h, &session).await;
        let history = file_text(h.dir.path(), &format!(".claude/projects/test/{}.jsonl", session.id));
        assert!(history.contains("GUI prompt"), "首轮图形对话必须生成可续接历史");
        h.mgr.switch_agent_mode(&session.id, "terminal-control").await.unwrap();
        let starts = std::fs::read_to_string(log).unwrap();
        let starts: Vec<_> = starts.lines().collect();
        assert_eq!(starts.len(), 3);
        assert!(starts[1].contains(&format!("--session-id {}", session.id)), "图形进程的启动参数不会随历史生成而变化");
        assert!(starts[2].contains(&format!("--resume {}", session.id)), "官方终端必须续接首轮图形对话");
        assert!(entry.s.borrow().agent_driver.is_none());
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        peer.seen.clear();
        h.mgr.input(&session.id, peer.id(), "after-first-turn\n");
        peer.wait("首轮图形对话后官方终端仍能接收输入", has_output("CLI_INPUT:after-first-turn")).await;
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn complete_gui_rejects_busy_permission_and_unknown_states_without_stopping_the_cli() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("gui-reject").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        for (command, state) in [("busy", "working"), ("blocked", "blocked")] {
            terminal_status(&h, &session, &mut peer, command, state).await;
            assert!(h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap_err().to_string().contains("空闲"));
        }
        h.mgr.on_backend_event(&entry, BackendEvent::ProgramStatus(Vec::new()));
        assert!(h.mgr.switch_agent_mode(&session.id, "gui-control").await.is_err());
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
        assert!(!entry.s.borrow().agent_converting);
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn failed_gui_resume_returns_to_shell_without_executing_late_protocol_lines() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("gui-fail").await;
        terminal_status(&h, &session, &mut peer, "done", "done").await;
        std::fs::write(h.dir.path().join("gui-fail"), "1").unwrap();
        let error = h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap_err();
        assert!(error.to_string().contains("续接失败"), "{error}");
        assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 2, "续接失败不能自动创建新对话");
        peer.seen.clear();
        h.mgr.input(&session.id, peer.id(), "printf 'SHELL_%s\\n' usable\n");
        peer.wait("失败后的 shell 可用", has_output("SHELL_usable")).await;
        assert!(!peer.output().contains("command not found"), "迟到 JSON 不得交给 shell：{}", peer.output());
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn cli_exit_timeout_does_not_launch_gui_or_rebuild_the_backend() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("gui-exit-timeout").await;
        terminal_status(&h, &session, &mut peer, "ignore-term", "idle").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        let error = h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap_err();
        assert!(error.to_string().contains("退出并返回原 shell 超时"), "{error}");
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
        assert!(entry.s.borrow().agent_driver.is_none());
        assert!(!entry.s.borrow().agent_converting);
        h.mgr.input(&session.id, peer.id(), "still-alive\n");
        peer.wait("退出超时的原 CLI 继续可用", has_output("CLI_INPUT:still-alive")).await;
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn gui_handshake_timeout_blocks_late_controls_and_preserves_the_backend() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, _peer, _log) = mode_harness("gui-handshake-timeout").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        std::fs::write(h.dir.path().join("gui-hang"), "1").unwrap();
        let error = h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap_err();
        assert!(error.to_string().contains("握手超时"), "{error}");
        let before = file_text(h.dir.path(), "protocol.log");
        h.mgr.agent_prompt(&session.id, "must not send");
        h.mgr.agent_interrupt(&session.id);
        h.mgr.agent_set_model(&session.id, "sonnet");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(file_text(h.dir.path(), "protocol.log"), before);
        assert_eq!(entry.s.borrow().agent_driver.as_ref().unwrap().state.status, falcon_proto::AgentTurnStatus::Unknown);
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        assert!(!entry.s.borrow().agent_converting);
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn invalid_history_is_rejected_before_exiting_the_original_cli() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("gui-invalid-history").await;
        terminal_status(&h, &session, &mut peer, "done", "done").await;
        std::fs::write(h.dir.path().join(format!(".claude/projects/test/{}.jsonl", session.id)), "not json\n").unwrap();
        assert!(h.mgr.switch_agent_mode(&session.id, "gui-control").await.unwrap_err().to_string().contains("历史"));
        assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
        assert!(!h.mgr.entry(&session.id).unwrap().s.borrow().agent_converting);
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn claude_view_round_trip_preserves_cli_shell_and_background_task() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut first, log) = mode_harness("mirror").await;
        let entry = h.mgr.entry(&session.id).unwrap();
        let backend = entry.backend().unwrap();
        let epoch = entry.s.borrow().backend_epoch;
        let original = h.db.get_session(&session.id).unwrap();
        let shell_state = wait_file(h.dir.path(), "shell-state").await;
        let children = wait_file(h.dir.path(), "children").await;
        let heartbeat = wait_file(h.dir.path(), "heartbeat").await.lines().count();
        let mut second = mirror_peer();
        h.mgr.add_viewer(&session.id, second.viewer.clone());
        let mut legacy = Peer::new();
        h.mgr.add_viewer(&session.id, legacy.viewer.clone());
        let size = entry.size();
        // 无需先完成一轮：只开启旁观界面，所有输入仍交给原终端。
        h.mgr.switch_agent_mode(&session.id, "gui").await.unwrap();
        h.mgr.switch_agent_mode(&session.id, "gui").await.unwrap();
        assert!(entry.s.borrow().agent_driver.is_none());
        for peer in [&mut first, &mut second] {
            peer.drain();
            assert!(peer.msgs("agent_snapshot").iter().any(|m| m["state"]["terminalBacked"] == true));
        }
        terminal_status(&h, &session, &mut first, "done", "done").await;
        first.wait("图形镜像正文", |seen| seen.iter().any(|g| matches!(g, Got::Msg(m) if m.to_string().contains("mirror ready")))).await;
        for (command, state) in [("busy", "working"), ("blocked", "blocked")] {
            terminal_status(&h, &session, &mut first, command, state).await;
            h.mgr.switch_agent_mode(&session.id, "terminal").await.unwrap();
            h.mgr.switch_agent_mode(&session.id, "gui").await.unwrap();
            assert_eq!(entry.s.borrow().program_status[0].state.as_str(), state);
        }
        first.seen.clear();
        h.mgr.input(&session.id, first.id(), "still-live\n");
        first.wait("镜像中的真实终端输入", has_output("CLI_INPUT:still-live")).await;
        legacy.drain();
        assert!(legacy.msgs("agent_snapshot").iter().all(|m| m["state"]["active"] == false && m["state"]["terminalBacked"] == true), "旧客户端只收到权威终端模式，不能接收未协商的图形镜像");
        let mut reconnected = mirror_peer();
        h.mgr.add_viewer(&session.id, reconnected.viewer.clone());
        reconnected.wait("镜像和终端共同回放", |seen| {
            seen.iter().any(|g| matches!(g, Got::Msg(m) if m["type"] == "agent_snapshot" && m["state"]["terminalBacked"] == true))
                && seen.iter().any(|g| matches!(g, Got::Output(_) | Got::Replay(_)))
        }).await;
        assert!(same_backend(&backend, &entry.backend().unwrap()));
        assert_eq!(entry.s.borrow().backend_epoch, epoch);
        assert_eq!(entry.size(), size);
        assert_eq!(file_text(h.dir.path(), "shell-state"), shell_state);
        assert_eq!(file_text(h.dir.path(), "children"), children);
        for _ in 0..100 {
            if file_text(h.dir.path(), "heartbeat").lines().count() > heartbeat { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let beats = file_text(h.dir.path(), "heartbeat");
        assert!(beats.lines().count() > heartbeat);
        assert!(beats.lines().all(|pid| Some(pid) == children.split(':').nth(1)));
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 1, "切换不能重启 CLI");
        let row = h.db.get_session(&session.id).unwrap();
        assert_eq!((row.id, row.created_at, row.name), (original.id, original.created_at, original.name));
        assert_eq!(h.db.session_agent_mode(&session.id).as_deref(), Some("transcript"));
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[test]
fn terminal_mirror_rebuilds_after_server_restart_without_restarting_the_cli() {
    if !sessiond_available() { return; }
    run(async {
        let (h, session, mut peer, log) = mode_harness("mirror-restart").await;
        terminal_status(&h, &session, &mut peer, "done", "done").await;
        h.mgr.switch_agent_mode(&session.id, "gui").await.unwrap();
        let shell_state = wait_file(h.dir.path(), "shell-state").await;
        let children = wait_file(h.dir.path(), "children").await;
        let env = h.mgr.local.base_env().await.as_ref().clone();
        h.db.update_session_state(&session.id, SessionState::Unverified, None);
        let Harness { dir, db, mgr, project } = h;
        drop(mgr);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let secrets = Arc::new(SecretBox::open(dir.path()).unwrap());
        let mgr = SessionManager::new(db.clone(), secrets, dir.path().to_path_buf(), Arc::new(AskpassHub::new()));
        mgr.local.set_base_env(env);
        let h = Harness { dir, db, mgr, project };
        let mut peer = mirror_peer();
        h.mgr.add_viewer(&session.id, peer.viewer.clone());
        h.mgr.resize(&session.id, peer.id(), 100, 30);
        peer.wait("重启后接回原终端", has_state("active")).await;
        peer.wait("重建图形历史", |seen| seen.iter().any(|g| matches!(g, Got::Msg(m) if m.to_string().contains("mirror ready")))).await;
        h.mgr.input(&session.id, peer.id(), "after-restart\n");
        peer.wait("重启后的原 CLI 仍可输入", has_output("CLI_INPUT:after-restart")).await;
        assert_eq!(file_text(h.dir.path(), "shell-state"), shell_state);
        assert_eq!(file_text(h.dir.path(), "children"), children);
        assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(), 1);
        h.mgr.terminate(&session.id, true).await;
        kill_all(&h).await;
    });
}

#[derive(Default)]
struct RecordedBackend {
    writes: RefCell<Vec<String>>,
    terminated: Cell<usize>,
    foreground: RefCell<Option<crate::sessions::backend::ForegroundProcess>>,
}
impl Backend for RecordedBackend {
    fn write(&self, data: &str) { self.writes.borrow_mut().push(data.into()); }
    fn resize(&self, _: u16, _: u16) {}
    fn destroy(&self) {}
    fn terminate(&self) -> bool { self.terminated.set(self.terminated.get() + 1); true }
    fn foreground_process(&self) -> Option<LocalBoxFuture<'static, crate::sessions::backend::ForegroundProcess>> {
        let foreground = self.foreground.borrow().clone()?;
        Some(Box::pin(async move { foreground }))
    }
}

fn recorded_session(h: &Harness) -> (SessionRow, Rc<LiveEntry>, Rc<RecordedBackend>) {
    let mut env = test_env();
    env[1].1 = h.dir.path().to_string_lossy().into_owned();
    h.mgr.local.set_base_env(env);
    let row = SessionRow { id: uuid_v4(), project_id: h.project.id.clone(), agent: Some("claude".into()), state: "active".into(), durable: 1, ..Default::default() };
    h.db.insert_session(&row);
    let entry = h.mgr.hydrate_entry(&row);
    let backend = Rc::new(RecordedBackend::default());
    entry.s.borrow_mut().backend = Some(backend.clone());
    (row, entry, backend)
}

#[test]
fn new_viewers_always_receive_authoritative_default_terminal_converting_and_unknown_gui_snapshots() {
    run(async {
        let h = harness();
        let (row, entry, _backend) = recorded_session(&h);
        let mut terminal = Peer::new();
        h.mgr.add_viewer(&row.id, terminal.viewer.clone());
        terminal.drain();
        let snapshots = terminal.msgs("agent_snapshot");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0]["state"]["active"], false);
        assert_eq!(snapshots[0]["state"]["terminalBacked"], true);
        entry.s.borrow_mut().agent_converting = true;
        let mut converting = Peer::new();
        h.mgr.add_viewer(&row.id, converting.viewer.clone());
        converting.drain();
        assert_eq!(converting.msgs("agent_snapshot")[0]["state"]["status"], "unknown");
        assert_eq!(converting.msgs("agent_snapshot")[0]["state"]["active"], true);
        entry.s.borrow_mut().agent_converting = false;
        entry.s.borrow_mut().backend = None;
        h.db.update_session_agent_mode(&row.id, "gui");
        let mut reconnecting = Peer::new();
        h.mgr.add_viewer(&row.id, reconnecting.viewer.clone());
        reconnecting.drain();
        assert_eq!(reconnecting.msgs("agent_snapshot")[0]["state"]["status"], "unknown");
        assert_eq!(reconnecting.msgs("agent_snapshot")[0]["state"]["active"], true);
        assert_ne!(reconnecting.msgs("agent_snapshot")[0]["state"]["terminalBacked"], true);
    });
}

#[test]
fn conversion_lock_blocks_other_viewers_and_foreground_mismatch_never_writes() {
    run(async {
        let h = harness();
        let (row, entry, backend) = recorded_session(&h);
        entry.s.borrow_mut().agent_converting = true;
        h.mgr.input(&row.id, 0, "must not reach shell\n");
        h.mgr.agent_prompt(&row.id, "must not reach CLI");
        assert!(h.mgr.switch_agent_mode(&row.id, "gui-control").await.unwrap_err().to_string().contains("正在转换"));
        assert!(h.mgr.switch_agent_mode(&row.id, "terminal-control").await.unwrap_err().to_string().contains("正在转换"));
        assert!(h.mgr.ensure_attached(&row.id, true).await.unwrap_err().to_string().contains("正在转换"));
        entry.s.borrow_mut().agent_converting = false;
        entry.s.borrow_mut().program_status = vec![serde_json::from_value(serde_json::json!({"id":"","state":"idle","app":"claude-code","updatedAt":1})).unwrap()];
        *backend.foreground.borrow_mut() = Some(crate::sessions::backend::ForegroundProcess {
            name: Some("vim".into()), pid: Some(999_999), argv: Some(vec!["vim".into()]),
        });
        let error = h.mgr.switch_agent_mode(&row.id, "gui-control").await.unwrap_err();
        assert!(error.to_string().contains("真实前台"), "{error}");
        assert!(backend.writes.borrow().is_empty());
        assert_eq!(backend.terminated.get(), 0);
        assert!(!entry.s.borrow().agent_converting);
    });
}

#[test]
fn view_requests_never_terminate_or_write_even_when_busy_blocked_or_unknown() {
    run(async {
        let h = harness();
        let (row, entry, backend) = recorded_session(&h);
        for state in ["working", "blocked", "unknown"] {
            entry.s.borrow_mut().program_status = vec![serde_json::from_value(serde_json::json!({"id":"","state":state,"app":"claude-code","updatedAt":1})).unwrap()];
            h.mgr.switch_agent_mode(&row.id, "gui").await.unwrap();
            h.mgr.switch_agent_mode(&row.id, "terminal").await.unwrap();
            assert_eq!(entry.s.borrow().program_status[0].state.as_str(), state);
        }
        assert_eq!(backend.terminated.get(), 0);
        assert!(backend.writes.borrow().is_empty());
        assert!(same_backend(&entry.backend().unwrap(), &(backend as Rc<dyn Backend>)));
    });
}

#[test]
fn legacy_structured_process_is_never_restarted_for_a_view_request() {
    run(async {
        let h = harness();
        let (row, entry, backend) = recorded_session(&h);
        entry.s.borrow_mut().agent_driver = Some(AgentDriver::new(SessionAgent::Claude, None));
        h.db.update_session_agent_mode(&row.id, "gui");
        for mode in ["gui", "terminal"] {
            let error = h.mgr.switch_agent_mode(&row.id, mode).await.unwrap_err();
            assert!(error.to_string().contains("不再重启"), "{error}");
        }
        assert_eq!(backend.terminated.get(), 0);
        assert!(backend.writes.borrow().is_empty());
        assert_eq!(h.db.session_agent_mode(&row.id).as_deref(), Some("gui"));
    });
}

#[test]
fn view_requests_validate_connection_and_agent_without_touching_the_backend() {
    run(async {
        let h = harness();
        let (row, entry, backend) = recorded_session(&h);
        assert!(h.mgr.switch_agent_mode(&row.id, "invalid").await.is_err());
        h.db.update_session_agent(&row.id, Some("codex"));
        assert!(h.mgr.switch_agent_mode(&row.id, "gui").await.is_err());
        h.db.update_session_agent(&row.id, Some("claude"));
        entry.s.borrow_mut().backend = None;
        assert!(h.mgr.switch_agent_mode(&row.id, "gui").await.is_err());
        assert_eq!(backend.terminated.get(), 0);
        assert!(backend.writes.borrow().is_empty());
    });
}

#[test]
fn old_callback_generation_cannot_change_output_driver_status_or_exit_state() {
    run(async {
        let h = harness();
        let (_, entry, backend) = recorded_session(&h);
        let slot = Rc::new(RefCell::new((Some(backend.clone() as Rc<dyn Backend>), Vec::new())));
        let old = h.mgr.backend_callbacks(&entry, &slot);
        let current = h.mgr.backend_callbacks(&entry, &slot);
        let mut driver = AgentDriver::new(SessionAgent::Claude, None);
        driver.on_data(&format!("{}claude\n", super::super::super::agent::AGENT_START_SENTINEL_PREFIX));
        entry.s.borrow_mut().agent_driver = Some(driver);
        (old.on_data)("{\"type\":\"system\",\"subtype\":\"init\"}\n".into());
        (old.on_event)(BackendEvent::Title("obsolete".into()));
        (old.on_event)(BackendEvent::Exit(Default::default()));
        (old.on_exit)();
        tokio::task::yield_now().await;
        assert!(!entry.s.borrow().agent_driver.as_ref().unwrap().is_initialized());
        assert!(!entry.s.borrow().exit_seen);
        assert!(entry.s.borrow().closed_epoch.is_none());
        (current.on_event)(BackendEvent::Synced("__FALCON_AGENT_START__:claude\n{\"type\":\"system\",\"subtype\":\"init\"}\n".into()));
        assert!(backend.writes.borrow().is_empty());
    });
}

#[test]
fn persisted_legacy_gui_refuses_unknown_transport_and_does_not_parse_vt_as_json() {
    run(async {
        let h = harness();
        let (row, entry, _) = recorded_session(&h);
        entry.s.borrow_mut().backend = None;
        h.db.update_session_agent_mode(&row.id, "gui");
        let error = h.mgr.ensure_attached(&row.id, true).await.unwrap_err();
        assert!(error.to_string().contains("暂不支持自动重附着"), "{error}");
        assert!(entry.backend().is_none());
        assert_eq!(entry.s.borrow().agent_driver.as_ref().unwrap().state.status, falcon_proto::AgentTurnStatus::Unknown);
    });
}
