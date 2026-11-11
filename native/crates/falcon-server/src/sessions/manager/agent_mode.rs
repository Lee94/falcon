//! 官方终端的只读图形镜像。显示切换不替换 Backend、PTY 或 CLI，也不向它们发送控制字节。

use super::*;
use falcon_proto::{AgentEntry, AgentNoticeLevel, AgentViewState, ProgramState};
use crate::host::quote_posix;
use super::super::backend::ForegroundProcess;
use super::super::agent::{AGENT_NATIVE_ENV, AGENT_RESUME_ENV, AGENT_RESUME_ONLY_ENV, AGENT_SESSION_ENV};
use super::super::agent_transcript::{ClaudeTranscript, LocalTranscriptSource, TranscriptCursor};
use super::transcript_remote::{RemoteTranscriptCursor, RemoteTranscriptSource};

const TRANSCRIPT_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Clone)]
enum TranscriptSource {
    Local(LocalTranscriptSource, TranscriptCursor),
    Remote(RemoteTranscriptSource, RemoteTranscriptCursor),
}

pub(super) struct TranscriptMirror {
    transcript: ClaudeTranscript,
    source: TranscriptSource,
    /// 每次重新开启都使用独立标记，防止旧读取结果覆盖新的镜像。
    identity: Rc<()>,
    paused: bool,
    last_error: Option<String>,
}

impl TranscriptMirror {
    pub(super) fn snapshot(&self) -> AgentViewState {
        let mut state = self.transcript.snapshot();
        if let Some(error) = &self.last_error {
            state.upsert_entry(mirror_error_entry(error));
        }
        state
    }

    pub(super) fn update_program_status(&mut self, records: &[ProgramStatusRecord]) -> Vec<ServerMessage> {
        if self.transcript.update_program_status(records).is_empty() { Vec::new() }
        else { vec![ServerMessage::AgentSnapshot { state: self.snapshot() }] }
    }
}

impl SessionManager {
    pub fn set_agent_view_capabilities(&self, session_id: &str, viewer_id: u64, terminal_backed: bool) {
        let Some(entry) = self.entry(session_id) else { return };
        let Some(viewer) = entry.viewers().into_iter().find(|v| v.id == viewer_id) else { return };
        viewer.terminal_backed_agent_view.set(terminal_backed);
        if terminal_backed {
            let state = entry.s.borrow().transcript_mirror.as_ref().map(TranscriptMirror::snapshot);
            if let Some(state) = state { viewer.send(&ServerMessage::AgentSnapshot { state }); }
        }
    }

    pub(super) fn broadcast_transcript(&self, entry: &LiveEntry, message: &ServerMessage) {
        self.flush_output(entry);
        for viewer in entry.viewers() {
            if viewer.terminal_backed_agent_view.get() { viewer.send(message); }
        }
    }

    /// 三类回调共用代次检查。只比较 Backend Rc 不够：握手期间 backend 还没有赋值，旧连接也可能
    /// 在新一代创建之后才送来最后一块 JSON、快照或 Exit。
    pub(super) fn backend_callbacks(
        &self,
        entry: &Rc<LiveEntry>,
        slot: &Rc<RefCell<(Option<Rc<dyn Backend>>, Vec<String>)>>,
    ) -> BackendCallbacks {
        let epoch = {
            let mut s = entry.s.borrow_mut();
            s.backend_epoch += 1;
            s.closed_epoch = None;
            s.backend_epoch
        };
        let weak = Rc::downgrade(entry);
        let weak_event = weak.clone();
        let weak_exit = weak.clone();
        let me = self.me.clone();
        let me_event = me.clone();
        let me_exit = me.clone();
        let slot_data = slot.clone();
        let slot_exit = slot.clone();
        BackendCallbacks {
            on_data: Rc::new(move |data| {
                let (Some(me), Some(e)) = (me.upgrade(), weak.upgrade()) else { return };
                if !me.is_current(&e) || e.s.borrow().backend_epoch != epoch { return; }
                me.on_backend_data(&e, &slot_data, &data);
            }),
            on_event: Rc::new(move |event| {
                let (Some(me), Some(e)) = (me_event.upgrade(), weak_event.upgrade()) else { return };
                if !me.is_current(&e) || e.s.borrow().backend_epoch != epoch { return; }
                me.on_backend_event(&e, event);
            }),
            on_exit: Rc::new(move || {
                let (Some(me), Some(e)) = (me_exit.upgrade(), weak_exit.upgrade()) else { return };
                if !me.is_current(&e) || e.s.borrow().backend_epoch != epoch { return; }
                e.s.borrow_mut().closed_epoch = Some(epoch);
                let ended = slot_exit.borrow().0.clone();
                tokio::task::spawn_local(async move { me.handle_backend_exit(&e.session_id, ended, epoch).await });
            }),
        }
    }

    /// 显式 GUI 转换与接回/重开所选运行协议共用；紧凑/图形排版切换不调用它。
    pub(super) async fn mode_launcher(
        &self, project: &ProjectRow, agent: SessionAgent, graphical: bool,
    ) -> Result<String, SessionError> {
        let path = if project.project_type == "local" {
            if cfg!(windows) { return Err(SessionError::Failed("Windows 宿主机暂不支持旧结构化 Agent 运行模式".into())); }
            let shell = project.shell.clone().unwrap_or_else(default_local_shell);
            let result = if graphical {
                super::super::agent::write_local_native_launcher(&self.data_dir, agent, &shell)
            } else { write_local_launcher(&self.data_dir, agent, &shell) };
            result.ok().map(|p| p.to_string_lossy().into_owned())
        } else {
            let link = self.get_link(project);
            let facts = link.host_facts().await.map_err(|e| SessionError::Failed(e.to_string()))?;
            if facts.kind != HostKind::Posix {
                return Err(SessionError::Failed("Windows 宿主机暂不支持旧结构化 Agent 运行模式".into()));
            }
            if graphical { link.ensure_native_agent_launcher(agent, project.shell.as_deref()).await }
            else { link.ensure_agent_launcher(agent, project.shell.as_deref()).await }
        };
        path.ok_or_else(|| SessionError::Failed("无法准备 Agent 启动脚本".into()))
    }

    /// 保留旧客户端调用的入口，但彻底删除 terminate/resume 转换路径。
    /// 两个模式请求都只能改变镜像；旧流协议不能变成官方 TUI，明确拒绝而不结束它。
    pub async fn switch_agent_mode(&self, session_id: &str, mode: &str) -> Result<(), SessionError> {
        if !matches!(mode, "gui" | "terminal" | "gui-control") { return Err(SessionError::Failed("mode 必须是 gui、terminal 或 gui-control".into())); }
        let row = self.db.get_session(session_id).ok_or_else(|| SessionError::NotFound("会话不存在".into()))?;
        if row.agent.as_deref() != Some("claude") {
            return Err(SessionError::Failed("目前仅 Claude Code 支持图形对话镜像".into()));
        }
        let entry = self.entry(session_id).ok_or_else(|| SessionError::Failed("会话尚未连接".into()))?;
        if row.state != "active" || entry.backend().is_none() || entry.terminating() {
            return Err(SessionError::Failed("请先接回 Claude 会话".into()));
        }
        if entry.s.borrow().agent_converting { return Err(SessionError::Failed("正在转换为图形界面，请稍后重试".into())); }
        if mode == "gui-control" {
            let project = owner_row(&self.db, &row.project_id).ok_or_else(|| SessionError::NotFound("项目不存在".into()))?;
            return self.enable_interactive_gui(&entry, &project).await;
        }
        if entry.s.borrow().agent_driver.is_some() || self.db.session_agent_mode(session_id).as_deref() == Some("gui") {
            return Err(SessionError::Failed("此会话使用旧结构化协议，只能切换紧凑/图形样式；为保留 shell 和后台任务，不再重启为官方终端".into()));
        }
        entry.s.borrow_mut().transcript_generation += 1;
        if mode == "terminal" {
            entry.s.borrow_mut().transcript_mirror = None;
            self.db.update_session_agent_mode(session_id, "terminal");
            let mut state = AgentViewState::new(SessionAgent::Claude, false);
            state.terminal_backed = true;
            self.broadcast_transcript(&entry, &ServerMessage::AgentSnapshot { state });
            return Ok(());
        }
        let project = owner_row(&self.db, &row.project_id).ok_or_else(|| SessionError::NotFound("项目不存在".into()))?;
        self.enable_transcript_mirror(&entry, &project).await?;
        self.db.update_session_agent_mode(session_id, "transcript");
        Ok(())
    }

    /// 用户已明确确认的协议转换。只退出经核实的 CLI 单进程，绝不结束 holder、PTY 或 shell。
    async fn enable_interactive_gui(&self, entry: &Rc<LiveEntry>, project: &ProjectRow) -> Result<(), SessionError> {
        let snapshot = entry.s.borrow().agent_driver.as_ref().filter(|d| d.is_active()).map(|d| d.state.clone());
        if let Some(state) = snapshot {
            self.broadcast(entry, &ServerMessage::AgentSnapshot { state });
            return Ok(());
        }
        if !entry.durable { return Err(SessionError::Failed("无法核实非持久会话的前台进程，未转换图形界面".into())); }
        require_idle(entry)?;
        entry.s.borrow_mut().agent_converting = true;
        let _guard = GuiConversionGuard(entry.clone());
        let epoch = entry.s.borrow().backend_epoch;
        let backend = entry.backend().ok_or_else(|| SessionError::Failed("会话尚未连接".into()))?;
        let source = self.prepare_transcript_source(entry, project).await?;
        let (history, fresh) = read_gui_history(source, &entry.session_id, project.working_dir.clone()).await?;
        let launcher = self.mode_launcher(project, SessionAgent::Claude, true).await?;
        self.check_gui_conversion(entry, &backend, epoch)?;
        require_idle(entry)?;
        let original = query_foreground(&backend).await?;
        let pid = verified_claude_pid(&original, &entry.session_id, fresh)?;
        // ps 的父 pid 与启动时间作为额外身份依据；最终再查一次前台，不能把历史中的 Claude 当成目标。
        let identity = self.gui_host_command(project, &process_identity_command(pid)).await?;
        let identity = identity.trim().to_string();
        let parent = identity.split_whitespace().next().and_then(|s| s.parse::<u32>().ok())
            .filter(|p| *p > 1 && *p != pid).ok_or_else(|| SessionError::Failed("无法核实 Claude 的父 shell，未退出原进程".into()))?;
        self.check_gui_conversion(entry, &backend, epoch)?;
        require_idle(entry)?;
        let current = query_foreground(&backend).await?;
        if current.pid != original.pid || current.name != original.name || current.argv != original.argv {
            return Err(SessionError::Failed("前台进程已经改变，未退出原进程".into()));
        }
        self.check_gui_conversion(entry, &backend, epoch)?;
        require_idle(entry)?;
        let signal = format!("{}; [ \"$*\" = {} ] || exit 75; kill -TERM {pid}", process_identity_setup(pid), quote_posix(&identity));
        self.gui_host_command(project, &signal).await?;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.check_gui_conversion(entry, &backend, epoch)?;
            let foreground = query_foreground(&backend).await?;
            self.check_gui_conversion(entry, &backend, epoch)?;
            if foreground.pid == Some(parent) && verified_idle_shell(&foreground, project.shell.as_deref()) { break; }
            if foreground.pid.is_some_and(|p| p != pid && p != parent) {
                return Err(SessionError::Failed("退出 Claude 后前台变成了其他进程，未注入启动命令".into()));
            }
            if Instant::now() >= deadline {
                return Err(SessionError::Failed("等待 Claude 退出并返回原 shell 超时；未启动新进程，也未结束 shell 或后台任务".into()));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // 输入锁仍在，确认 shell 的 pid 与 argv 后才发送一条完整、经过引用的启动命令。
        let foreground = query_foreground(&backend).await?;
        self.check_gui_conversion(entry, &backend, epoch)?;
        if foreground.pid != Some(parent) || !verified_idle_shell(&foreground, project.shell.as_deref()) {
            return Err(SessionError::Failed("原 shell 的前台身份发生变化，未注入启动命令".into()));
        }
        let mut driver = AgentDriver::new(SessionAgent::Claude, project.working_dir.clone());
        driver.pin_session_id(&entry.session_id);
        driver.state.entries = history;
        // 所有 Viewer 都必须等控制握手完成，不能只禁用发起转换的窗口。
        driver.state.status = falcon_proto::AgentTurnStatus::Unknown;
        let snapshot = driver.state.clone();
        {
            let mut s = entry.s.borrow_mut();
            s.transcript_generation += 1;
            s.transcript_mirror = None;
            s.agent_driver = Some(driver);
        }
        self.db.update_session_agent_mode(&entry.session_id, "gui");
        self.flush_output(entry);
        self.broadcast(entry, &ServerMessage::AgentSnapshot { state: snapshot });
        let resume = if fresh { "" } else { "1" };
        backend.write(&format!("{AGENT_SESSION_ENV}={} {AGENT_RESUME_ENV}={} {AGENT_RESUME_ONLY_ENV}=1 {AGENT_NATIVE_ENV}=1 {}\n",
            quote_posix(&entry.session_id), quote_posix(resume), quote_posix(&launcher)));
        // 新脚本 READY → CONNECT → START 后才允许 JSON 握手；启动失败也要经过 EXIT/ACK 后才回 shell。
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.check_gui_conversion(entry, &backend, epoch)?;
            let (active, ready) = {
                let s = entry.s.borrow();
                let driver = s.agent_driver.as_ref().unwrap();
                (driver.is_active(), driver.control_ready())
            };
            if !active {
                self.db.update_session_agent_mode(&entry.session_id, "terminal");
                return Err(SessionError::Failed("Claude 图形进程启动或续接失败；未自动新建对话，原 shell 和独立后台任务仍然保留".into()));
            }
            if ready {
                let state = {
                    let mut s = entry.s.borrow_mut();
                    s.agent_converting = false;
                    let driver = s.agent_driver.as_mut().unwrap();
                    driver.state.status = falcon_proto::AgentTurnStatus::Idle;
                    driver.state.clone()
                };
                self.broadcast(entry, &ServerMessage::AgentSnapshot { state });
                return Ok(());
            }
            if Instant::now() >= deadline {
                let reason = "等待 Claude 图形控制握手超时；未继续注入输入。原 shell 和独立后台任务仍在，控制通道需要重新核实。";
                let out = entry.s.borrow_mut().agent_driver.as_mut().unwrap().initialization_failed(reason);
                self.apply_agent_output(entry, None, out);
                return Err(SessionError::Failed(reason.into()));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    fn check_gui_conversion(&self, entry: &Rc<LiveEntry>, backend: &Rc<dyn Backend>, epoch: u64) -> Result<(), SessionError> {
        if !self.is_current(entry) || entry.terminating() || entry.s.borrow().backend_epoch != epoch
            || !entry.backend().is_some_and(|b| Rc::ptr_eq(&b, backend)) {
            return Err(SessionError::Failed("转换期间会话连接已经改变，未向新连接写入控制协议".into()));
        }
        Ok(())
    }

    async fn gui_host_command(&self, project: &ProjectRow, command: &str) -> Result<String, SessionError> {
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            if project.project_type == "local" { crate::exec::LocalExec.exec(command, None).await }
            else { self.get_link(project).exec(command, None).await }
        }).await.map_err(|_| SessionError::Failed("核实 Agent 进程的宿主机命令超时".into()))?
            .map_err(|e| SessionError::Failed(format!("核实 Agent 进程失败：{e}")))?;
        if !result.ok() {
            return Err(SessionError::Failed(format!("核实或退出 Claude 失败（exit {:?}）：{}", result.code, result.stderr.trim())));
        }
        Ok(result.stdout)
    }

    async fn prepare_transcript_source(&self, entry: &Rc<LiveEntry>, project: &ProjectRow) -> Result<TranscriptSource, SessionError> {
        if project.project_type == "local" {
            if cfg!(windows) { return Err(SessionError::Failed("Windows 宿主机暂不支持图形控制模式".into())); }
            let env = self.local.base_env().await;
            let get = |key| super::super::login_env::env_get(&env, key).filter(|s| !s.is_empty()).map(PathBuf::from);
            Ok(TranscriptSource::Local(LocalTranscriptSource::new(get("CLAUDE_CONFIG_DIR"), get("HOME"), &entry.session_id)
                .map_err(|e| SessionError::Failed(e.to_string()))?, TranscriptCursor::default()))
        } else {
            let link = self.get_link(project);
            let facts = link.host_facts().await.map_err(|e| SessionError::Failed(e.to_string()))?;
            if facts.kind != HostKind::Posix {
                return Err(SessionError::Failed("Windows 远端暂不支持图形对话镜像或控制模式；原终端继续运行".into()));
            }
            let shell = project.shell.clone().unwrap_or(facts.shell);
            Ok(TranscriptSource::Remote(RemoteTranscriptSource::new(link, shell, &entry.session_id)
                .map_err(SessionError::Failed)?, RemoteTranscriptCursor::default()))
        }
    }

    pub(super) async fn enable_transcript_mirror(&self, entry: &Rc<LiveEntry>, project: &ProjectRow) -> Result<(), SessionError> {
        let generation = entry.s.borrow().transcript_generation;
        let snapshot = entry.s.borrow().transcript_mirror.as_ref().map(TranscriptMirror::snapshot);
        if let Some(state) = snapshot {
            self.broadcast_transcript(entry, &ServerMessage::AgentSnapshot { state });
            return Ok(());
        }
        let source = self.prepare_transcript_source(entry, project).await?;
        // 准备读取源期间仍可输入、切换和退出；await 后重新核实 entry，不能复活已关闭的会话。
        if !self.is_current(entry) || entry.terminating() || entry.backend().is_none() {
            return Err(SessionError::Failed("会话已经关闭，未开启图形镜像".into()));
        }
        if entry.s.borrow().transcript_generation != generation {
            return Err(SessionError::Failed("显示方式已被后续请求更新，未覆盖当前选择".into()));
        }
        let identity = Rc::new(());
        let snapshot = {
            let mut s = entry.s.borrow_mut();
            if let Some(mirror) = &s.transcript_mirror {
                Some(mirror.snapshot())
            } else {
                let mut transcript = ClaudeTranscript::new(&entry.session_id, project.working_dir.clone())
                    .map_err(|e| SessionError::Failed(e.to_string()))?;
                transcript.update_program_status(&s.program_status);
                s.transcript_mirror = Some(TranscriptMirror { transcript, source, identity: identity.clone(), paused: false, last_error: None });
                None
            }
        };
        if let Some(state) = snapshot {
            self.broadcast_transcript(entry, &ServerMessage::AgentSnapshot { state });
            return Ok(());
        }
        let state = entry.s.borrow().transcript_mirror.as_ref().unwrap().snapshot();
        self.broadcast_transcript(entry, &ServerMessage::AgentSnapshot { state });
        self.poll_transcript_mirror(entry, identity);
        Ok(())
    }

    fn poll_transcript_mirror(&self, entry: &Rc<LiveEntry>, identity: Rc<()>) {
        let weak = Rc::downgrade(entry);
        let me = self.me.clone();
        tokio::task::spawn_local(async move {
            loop {
                let (Some(me), Some(entry)) = (me.upgrade(), weak.upgrade()) else { return };
                if !me.is_current(&entry) || entry.terminating() { return; }
                let source = {
                    let s = entry.s.borrow();
                    let Some(mirror) = s.transcript_mirror.as_ref().filter(|m| Rc::ptr_eq(&m.identity, &identity)) else { return };
                    (!mirror.paused && s.viewers.iter().any(|v| v.terminal_backed_agent_view.get())).then(|| mirror.source.clone())
                };
                let mut has_more = false;
                if let Some(source) = source {
                    let result = match source {
                        TranscriptSource::Local(source, cursor) => {
                            let read_source = source.clone();
                            tokio::task::spawn_blocking(move || read_source.read_chunk(&cursor, TRANSCRIPT_CHUNK_BYTES))
                                .await.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string()))
                                .map(|c| (c.bytes, c.reset, c.found, c.has_more, TranscriptSource::Local(source, c.cursor)))
                        }
                        TranscriptSource::Remote(source, cursor) => {
                            let next = source.read_chunk(&cursor, TRANSCRIPT_CHUNK_BYTES).await;
                            next.map(|c| (c.bytes, c.reset, c.found, c.has_more, TranscriptSource::Remote(source, c.cursor)))
                        }
                    };
                    if !me.is_current(&entry) || entry.terminating() { return; }
                    let messages = {
                        let mut s = entry.s.borrow_mut();
                        let Some(mirror) = s.transcript_mirror.as_mut().filter(|m| Rc::ptr_eq(&m.identity, &identity)) else { return };
                        match result {
                            Ok((bytes, reset, _found, more, source)) => {
                                has_more = more;
                                match mirror.transcript.ingest(&bytes, reset) {
                                    Ok(mut messages) => {
                                        mirror.source = source;
                                        if mirror.last_error.take().is_some() {
                                            messages.push(ServerMessage::AgentSnapshot { state: mirror.snapshot() });
                                        }
                                        messages
                                    }
                                    Err(error) => {
                                        mirror.paused = true;
                                        mirror_error(mirror, error.to_string())
                                    }
                                }
                            }
                            Err(error) => mirror_error(mirror, error),
                        }
                    };
                    for message in messages { me.broadcast_transcript(&entry, &message); }
                }
                // 只保留 Weak 跨等待，避免镜像任务让已销毁的会话/manager 无法释放。
                drop(entry);
                drop(me);
                tokio::time::sleep(if has_more { Duration::from_millis(10) } else { Duration::from_secs(1) }).await;
            }
        });
    }
}

/// 转换锁跨 await 持有；无论在哪一步失败，都要恢复其他 Viewer 的输入。
struct GuiConversionGuard(Rc<LiveEntry>);
impl Drop for GuiConversionGuard {
    fn drop(&mut self) { self.0.s.borrow_mut().agent_converting = false; }
}

fn require_idle(entry: &LiveEntry) -> Result<(), SessionError> {
    let s = entry.s.borrow();
    if s.program_status.is_empty() || s.program_status.iter().any(|r| {
        !matches!(r.state, ProgramState::Idle | ProgramState::Done)
            || !matches!(r.app.as_deref(), Some("claude" | "claude-code" | "Claude Code"))
    }) {
        return Err(SessionError::Failed("仅能在已核实 Claude 空闲时转换；执行中、等待审批或状态未知时请先完成当前操作".into()));
    }
    Ok(())
}

async fn query_foreground(backend: &Rc<dyn Backend>) -> Result<ForegroundProcess, SessionError> {
    let query = backend.foreground_process().ok_or_else(|| SessionError::Failed("守护进程没有提供真实前台身份，未转换图形界面".into()))?;
    tokio::time::timeout(Duration::from_secs(3), query).await
        .map_err(|_| SessionError::Failed("等待真实前台进程信息超时".into()))
}

fn verified_claude_pid(process: &ForegroundProcess, session_id: &str, fresh: bool) -> Result<u32, SessionError> {
    let argv = process.argv.as_deref().unwrap_or_default();
    let has_id = argv.windows(2).any(|args| {
        (args[0] == "--session-id" || (!fresh && args[0] == "--resume")) && args[1] == session_id
    });
    // 原生安装的可执行文件可能按版本命名；登录 shell 传给它的 argv[0] 仍是 claude。
    let is_claude = process.name.as_deref() == Some("claude")
        || argv.first().is_some_and(|arg| arg.rsplit('/').next() == Some("claude"));
    if !is_claude || !has_id
        || argv.iter().any(|a| matches!(a.as_str(), "-p" | "--print" | "--input-format" | "--output-format")) {
        return Err(SessionError::Failed("真实前台不是使用固定对话 UUID 的 Claude 交互进程，未退出任何进程".into()));
    }
    process.pid.filter(|p| *p > 1).ok_or_else(|| SessionError::Failed("守护进程没有提供 Claude 的 pid，未退出任何进程".into()))
}

fn verified_idle_shell(process: &ForegroundProcess, shell: Option<&str>) -> bool {
    let Some(argv) = process.argv.as_ref().filter(|v| !v.is_empty() && !v[0].is_empty()) else { return false };
    process.name.as_ref().is_some_and(|name| !name.is_empty() && is_shell_command(name, shell))
        && is_shell_command(&argv.join(" "), shell)
}

fn process_identity_setup(pid: u32) -> String {
    // lstart 的六个字段只有 pid、日期和时间；不读取环境、命令参数或其他敏感值。
    format!("set -- $(ps -p {pid} -o ppid= -o lstart=); [ \"$#\" -eq 6 ] || exit 74")
}

fn process_identity_command(pid: u32) -> String {
    format!("{}; printf '%s\\n' \"$*\"", process_identity_setup(pid))
}

async fn read_gui_history(mut source: TranscriptSource, session_id: &str, cwd: Option<String>) -> Result<(Vec<AgentEntry>, bool), SessionError> {
    let mut transcript = ClaudeTranscript::new(session_id, cwd).map_err(|e| SessionError::Failed(e.to_string()))?;
    let mut total_bytes = 0usize;
    let mut complete_line = true;
    loop {
        let (bytes, reset, more, next) = match source {
            TranscriptSource::Local(source, cursor) => {
                let reader = source.clone();
                let chunk = tokio::task::spawn_blocking(move || reader.read_chunk(&cursor, TRANSCRIPT_CHUNK_BYTES)).await
                    .map_err(|e| SessionError::Failed(e.to_string()))?
                    .map_err(|e| SessionError::Failed(format!("无法核实原对话历史：{e}")))?;
                (chunk.bytes, chunk.reset, chunk.has_more, TranscriptSource::Local(source, chunk.cursor))
            }
            TranscriptSource::Remote(source, cursor) => {
                let chunk = source.read_chunk(&cursor, TRANSCRIPT_CHUNK_BYTES).await
                    .map_err(|e| SessionError::Failed(format!("无法核实原对话历史：{e}")))?;
                (chunk.bytes, chunk.reset, chunk.has_more, TranscriptSource::Remote(source, chunk.cursor))
            }
        };
        if reset && total_bytes != 0 { return Err(SessionError::Failed("核实期间对话文件被替换，未转换图形界面".into())); }
        total_bytes += bytes.len();
        if let Some(last) = bytes.last() { complete_line = *last == b'\n'; }
        transcript.ingest(&bytes, reset).map_err(|e| SessionError::Failed(format!("无法核实原对话历史：{e}")))?;
        source = next;
        if !more { break; }
    }
    if !complete_line { return Err(SessionError::Failed("原对话历史仍有未写完的记录，未退出原 Claude 进程".into())); }
    let entries = transcript.snapshot().entries;
    if total_bytes == 0 { return Ok((entries, true)); }
    if !entries.iter().any(|e| matches!(e, AgentEntry::User { .. })) {
        return Err(SessionError::Failed("无法证明原对话可续接或从未创建历史，未退出原 Claude 进程".into()));
    }
    Ok((entries, false))
}

fn mirror_error(mirror: &mut TranscriptMirror, error: String) -> Vec<ServerMessage> {
    if mirror.last_error.as_ref() == Some(&error) { return Vec::new(); }
    mirror.last_error = Some(error.clone());
    vec![ServerMessage::AgentEntry { entry: mirror_error_entry(&error) }]
}

fn mirror_error_entry(error: &str) -> AgentEntry {
    AgentEntry::Notice {
        id: "falcon-transcript-error".into(), level: AgentNoticeLevel::Warning,
        text: format!("无法更新图形对话记录：{error}。原终端和后台任务继续运行，请在终端中操作。"),
    }
}
