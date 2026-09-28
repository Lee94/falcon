//! 一台 falcon 服务端的工作区：web 那个 zustand store（`packages/web/src/store.ts`）的原生版。
//!
//! 两层：
//! - **排布与选择**（开着哪些窗口、列怎么排、焦点在哪、侧栏展开到哪）是纯状态，全部在
//!   [`falcon_core::workspace::WorkspaceState`] 里——那是 store.ts 纯函数部分的移植，带着与
//!   web 同一组测试。这里只负责"调它、落盘、通知视图"，不再自己写一遍规则；
//! - **服务端数据**（项目 / 主机 / 会话）、登录态、轮询、活着的终端视图，是 app 自己的事。
//!
//! 侧栏、画布、右侧栏、命令面板都 observe 这个 Entity。浮层（菜单 / 确认框 / 表单）不进
//! store：GPUI 里它们由 `window.open_dialog` 等命令式打开，生命周期归 gpui-component 的 Root。

pub mod actions;
pub mod persist;

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use falcon_client::FalconClient;
use falcon_core::layout::ColumnLayout;
pub use falcon_core::workspace::{
    ActiveView, DiffCommit, DiffTabTarget, FileTabTarget, PendingSession, RightPanelId,
    WorkspaceState, is_pending_id,
};
use falcon_proto::{AuthStatus, Project, SessionAgent, SessionWithProject, SshHost, SystemInfo};
use gpui_kit::{AppContext, Context, Entity, EventEmitter, Task, Window};

use crate::profiles::ServerProfile;
use crate::terminal::view::{TerminalView, TerminalViewEvent};

pub use actions::ConfirmSpec;

/// 源项目的 HEAD（侧栏检出行的分支名），附属项目用自己的 worktree.branch
pub use falcon_core::project_tree::ProjectHead;

/// 工作区行数计数（侧栏 +N −M）；干净的项目不在表里
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProjectChanges {
    pub added: u32,
    pub deleted: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AskpassSpec {
    pub id: String,
    pub prompt: String,
}

/// 登录态：web 里 `auth.required && !auth.authenticated` 时整页换成 Login
#[derive(Clone, Debug, PartialEq)]
pub enum AuthPhase {
    /// 还没问过服务端
    Checking,
    /// 服务端连不上（本机服务没起来 / 网络）
    Unreachable(String),
    NeedLogin,
    Ready,
}

/// 工作区对外的事件（窗口据此开浮层）
pub enum WorkspaceEvent {
    /// 有新的 sudo 提示排到了队头
    Askpass,
    /// 某扇窗口要拿焦点
    FocusPane(String),
    /// SSH 项目首次建会话前要先走 Zellij 授权 / 安装
    NeedsInstall { project_id: String, agent: Option<SessionAgent> },
    Toast(Toast),
    /// 要用户确认的动作（关掉一个前台还在跑程序的终端等）
    Confirm(ConfirmSpec),
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub body: Option<String>,
    /// 要人读完的（git 失败的原话）：不自动消失。web 是停留 12s，通知组件只有"5s"与
    /// "不自动关"两档，取后者
    pub sticky: bool,
}

#[derive(Clone, Copy, Debug)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Danger,
}

pub struct Workspace {
    pub profile: ServerProfile,
    pub client: FalconClient,
    pub auth_phase: AuthPhase,
    pub auth: Option<AuthStatus>,
    pub system: Option<SystemInfo>,
    pub projects: Vec<Project>,
    pub hosts: Vec<SshHost>,
    pub sessions: Vec<SessionWithProject>,
    /// 排布与选择（store.ts 纯函数部分的移植）
    pub state: WorkspaceState,
    pub heads: HashMap<String, ProjectHead>,
    pub changes: HashMap<String, ProjectChanges>,
    pub askpass: VecDeque<AskpassSpec>,
    /// 总览按状态筛选；None = 全部
    pub overview_filter: Option<falcon_proto::SessionState>,
    /// 总览按项目筛选；None = 全部项目
    pub overview_project: Option<String>,
    /// 总览里勾选的会话（批量终止）
    pub selected: Vec<String>,

    /// 活着的终端视图：在 tabs 里就常驻（web 同样不卸载——卸载 = 关 WS，再挂要重连 + 回放）
    pub terminals: HashMap<String, Entity<TerminalView>>,
    changes_in_flight: bool,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub fn new(profile: ServerProfile, cx: &mut Context<Self>) -> Self {
        let client = FalconClient::new(profile.url.parse().expect("服务端基址在配置里校验过"));
        if let Some(pw) = profile.password() {
            client.set_relogin_password(Some(pw));
        }
        let state = WorkspaceState::from_persisted(&persist::load(&profile));
        let mut this = Self {
            profile,
            client,
            auth_phase: AuthPhase::Checking,
            auth: None,
            system: None,
            projects: Vec::new(),
            hosts: Vec::new(),
            sessions: Vec::new(),
            state,
            heads: HashMap::new(),
            changes: HashMap::new(),
            askpass: VecDeque::new(),
            overview_filter: None,
            overview_project: None,
            selected: Vec::new(),
            terminals: HashMap::new(),
            changes_in_flight: false,
            _tasks: Vec::new(),
        };
        this.start(cx);
        this
    }

    /// 轮询在这里起；认证 / 首次加载由窗口触发 [`Self::init`]（本机服务要先确认起来了）
    fn start(&mut self, cx: &mut Context<Self>) {
        // 会话列表 5s、askpass 1.5s、侧栏 git 计数 8s：与 web 同一套节奏
        self._tasks.push(Self::poll(cx, Duration::from_secs(5), |this, cx| this.refresh_sessions(cx)));
        self._tasks.push(Self::poll(cx, Duration::from_millis(1500), |this, cx| this.pull_askpass(cx)));
        self._tasks.push(Self::poll(cx, Duration::from_secs(8), |this, cx| this.refresh_changes(cx)));
        self._tasks.push(Self::watch_wake(cx));
        // 自动重登成功后，停在"需要登录"的界面自己恢复
        let mut events = self.client.subscribe_auth();
        self._tasks.push(cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(ev) = events.next().await {
                let keep = this
                    .update(cx, |this, cx| {
                        if matches!(ev, falcon_client::AuthEvent::LoggedIn) && this.auth_phase == AuthPhase::NeedLogin {
                            this.auth_phase = AuthPhase::Checking;
                            this.init(cx);
                        }
                    })
                    .is_ok();
                if !keep {
                    break;
                }
            }
        }));
    }

    /// 睡眠唤醒后立即重连（设计文档 §3.2）。拿不到系统的唤醒通知，改看墙钟：定时器在睡眠期间
    /// 不走，醒来后第一跳的墙钟一下子跨过去一大截。退避中的 socket 此刻可能还要等十几秒，
    /// 连着的也可能早被代理 / NAT 掐成半开，所以一律 `reconnect_now`（连着的只发 ping 探活）。
    /// 墙钟被 NTP 往前拨也会误触发一次，代价只是一轮探活 + 刷新
    fn watch_wake(cx: &mut Context<Self>) -> Task<()> {
        const TICK: Duration = Duration::from_secs(5);
        cx.spawn(async move |this, cx| {
            let mut last = std::time::SystemTime::now();
            loop {
                cx.background_executor().timer(TICK).await;
                let now = std::time::SystemTime::now();
                let gap = now.duration_since(last).unwrap_or_default();
                last = now;
                if gap < TICK + Duration::from_secs(10) {
                    continue;
                }
                let ok = this
                    .update(cx, |this, cx| {
                        for view in this.terminals.values() {
                            view.read(cx).reconnect_now();
                        }
                        if this.auth_phase == AuthPhase::Ready {
                            this.refresh_sessions(cx);
                            this.refresh_changes(cx);
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
    }

    fn poll(cx: &mut Context<Self>, every: Duration, f: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let ok = this
                    .update(cx, |this, cx| {
                        if this.auth_phase == AuthPhase::Ready {
                            f(this, cx);
                        }
                    })
                    .is_ok();
                if !ok {
                    break;
                }
            }
        })
    }

    pub fn ready(&self) -> bool {
        self.auth_phase == AuthPhase::Ready
    }

    /// 启动 / 登录之后：认证状态 → 系统信息 + 项目 + 主机 + 会话
    pub fn init(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let password = self.profile.password();
        cx.spawn(async move |this, cx| {
            let status = match client.auth_status().await {
                Ok(s) => s,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.auth_phase = AuthPhase::Unreachable(err.to_string());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let mut need_login = status.required && !status.authenticated;
            // 钥匙串里有密码就先静默登一次
            if need_login && let Some(pw) = password {
                need_login = client.login(&pw).await.is_err();
            }
            this.update(cx, |this, cx| {
                this.auth = Some(status);
                if need_login {
                    this.auth_phase = AuthPhase::NeedLogin;
                    cx.notify();
                } else {
                    this.auth_phase = AuthPhase::Ready;
                    this.load_all(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn load_all(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let (system, projects, hosts, sessions) =
                futures::join!(client.system(), client.list_projects(), client.list_hosts(), client.list_sessions());
            this.update(cx, |this, cx| {
                if let Ok(s) = system {
                    this.system = Some(s);
                }
                if let Ok(h) = hosts {
                    this.hosts = h;
                }
                if let Ok(p) = projects {
                    this.set_projects(p, cx);
                }
                match sessions {
                    Ok(s) => this.apply_sessions(s, cx),
                    Err(err) => this.handle_error(&err, cx),
                }
                // 选中的项目还在就重新选一次（把它的会话补进 tabs），不在了就回总览
                if let Some(sel) = this.state.selected_project_id.clone() {
                    if this.projects.iter().any(|p| p.id == sel) {
                        this.select_project(&sel, cx);
                    } else {
                        this.state.selected_project_id = None;
                    }
                }
                this.refresh_changes(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 登录框提交
    pub fn login(&mut self, password: String, remember: bool, cx: &mut Context<Self>) -> Task<Result<(), String>> {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| match client.login(&password).await {
            Ok(()) => {
                this.update(cx, |this, cx| {
                    if remember {
                        this.profile.store_password(Some(&password));
                        this.client.set_relogin_password(Some(password.clone()));
                    }
                    this.auth_phase = AuthPhase::Checking;
                    this.init(cx);
                    // 停在 Unauthorized 的终端 socket 登录成功后会自己接着连；保险起见叫醒一次
                    for view in this.terminals.values() {
                        view.read(cx).reconnect_now();
                    }
                })
                .ok();
                Ok(())
            }
            Err(err) => Err(err.to_string()),
        })
    }

    pub fn logout(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.profile.store_password(None);
        self.client.set_relogin_password(None);
        cx.spawn(async move |this, cx| {
            let _ = client.logout().await;
            this.update(cx, |this, cx| {
                this.auth_phase = AuthPhase::NeedLogin;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// REST 失败的统一出口：401 回登录页（web 的 handleApiError）
    pub fn handle_error(&mut self, err: &falcon_client::ApiError, cx: &mut Context<Self>) {
        if err.is_unauthorized() && self.auth_phase == AuthPhase::Ready {
            self.auth_phase = AuthPhase::NeedLogin;
            cx.notify();
        }
    }

    pub fn toast(&mut self, kind: ToastKind, title: impl Into<String>, body: Option<String>, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::Toast(Toast {
            kind,
            title: title.into(),
            body,
            sticky: false,
        }));
    }

    /// 同 [`Self::toast`]，但不自动消失（web 的 `sticky: true`）
    pub fn toast_sticky(&mut self, kind: ToastKind, title: impl Into<String>, body: Option<String>, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::Toast(Toast {
            kind,
            title: title.into(),
            body,
            sticky: true,
        }));
    }

    // ---------------- 数据刷新 ----------------

    pub fn refresh_projects(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.list_projects().await;
            this.update(cx, |this, cx| match result {
                Ok(p) => this.set_projects(p, cx),
                Err(err) => this.handle_error(&err, cx),
            })
            .ok();
        })
        .detach();
    }

    fn set_projects(&mut self, projects: Vec<Project>, cx: &mut Context<Self>) {
        self.state.apply_projects(&projects);
        self.projects = projects;
        self.refresh_heads(cx);
        cx.notify();
    }

    pub fn refresh_hosts(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.list_hosts().await;
            this.update(cx, |this, cx| match result {
                Ok(h) => {
                    this.hosts = h;
                    cx.notify();
                }
                Err(err) => this.handle_error(&err, cx),
            })
            .ok();
        })
        .detach();
    }

    pub fn refresh_sessions(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let result = client.list_sessions().await;
            this.update(cx, |this, cx| match result {
                Ok(s) => this.apply_sessions(s, cx),
                Err(err) => this.handle_error(&err, cx),
            })
            .ok();
        })
        .detach();
    }

    fn apply_sessions(&mut self, sessions: Vec<SessionWithProject>, cx: &mut Context<Self>) {
        // 内容没变就整个跳过（web 的 sameFlatArray）：tabs / active 在上一轮已经收敛
        if self.sessions == sessions {
            return;
        }
        self.sessions = sessions;
        self.state.apply_sessions(&self.sessions);
        let alive: HashSet<&str> = self.sessions.iter().map(|s| s.id.as_str()).collect();
        self.selected.retain(|id| alive.contains(id.as_str()));
        self.sync_terminals();
        self.persist();
        cx.notify();
    }

    /// WS 推来的状态立刻写进列表，不等 5s 轮询——否则接回后侧栏仍显示「待接回」
    pub fn apply_session_state(
        &mut self,
        id: &str,
        state: falcon_proto::SessionState,
        dead_reason: Option<falcon_proto::DeadReason>,
        cx: &mut Context<Self>,
    ) {
        falcon_core::workspace::apply_session_state(&mut self.sessions, id, state, dead_reason);
        cx.notify();
    }

    pub fn apply_session_title(&mut self, id: &str, title: Option<String>, cx: &mut Context<Self>) {
        falcon_core::workspace::apply_session_title(&mut self.sessions, id, title.as_deref());
        cx.notify();
    }

    fn pull_askpass(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(list) = client.pending_askpass().await {
                this.update(cx, |this, cx| {
                    for p in list {
                        this.push_askpass(p.id, p.prompt, cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn push_askpass(&mut self, id: String, prompt: String, cx: &mut Context<Self>) {
        if self.askpass.iter().any(|a| a.id == id) {
            return;
        }
        let was_empty = self.askpass.is_empty();
        self.askpass.push_back(AskpassSpec { id, prompt });
        if was_empty {
            cx.emit(WorkspaceEvent::Askpass);
        }
        cx.notify();
    }

    pub fn shift_askpass(&mut self, cx: &mut Context<Self>) {
        self.askpass.pop_front();
        if !self.askpass.is_empty() {
            cx.emit(WorkspaceEvent::Askpass);
        }
        cx.notify();
    }

    /// 源项目的分支名（侧栏检出行）。附属项目自己带 worktree.branch，不用问
    fn refresh_heads(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let ids: Vec<String> = self
            .projects
            .iter()
            .filter(|p| p.worktree.is_none() && p.multi.is_none())
            .map(|p| p.id.clone())
            .collect();
        cx.spawn(async move |this, cx| {
            let mut heads = HashMap::new();
            for id in ids {
                if let Ok(info) = client.repo_info(&id).await
                    && (info.head_branch.is_some() || info.head_sha.is_some())
                {
                    heads.insert(id, ProjectHead { branch: info.head_branch, sha: info.head_sha });
                }
            }
            this.update(cx, |this, cx| {
                if this.heads != heads {
                    this.heads = heads;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// 侧栏 +N −M：一次批量请求，服务端按主机分组；上一轮没回来不叠加
    pub fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        if self.changes_in_flight || self.projects.is_empty() {
            return;
        }
        self.changes_in_flight = true;
        let client = self.client.clone();
        let ids: Vec<String> = self
            .projects
            .iter()
            .filter(|p| p.worktree.as_ref().is_none_or(|w| w.archived_at.is_none()))
            .map(|p| p.id.clone())
            .collect();
        cx.spawn(async move |this, cx| {
            let result = client.git_changes_batch(&ids).await;
            this.update(cx, |this, cx| {
                this.changes_in_flight = false;
                if let Ok(counts) = result {
                    let next: HashMap<String, ProjectChanges> = counts
                        .into_iter()
                        .filter(|(_, c)| c.available && (c.added > 0 || c.deleted > 0))
                        .map(|(id, c)| (id, ProjectChanges { added: c.added, deleted: c.deleted }))
                        .collect();
                    if this.changes != next {
                        this.changes = next;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    // ---------------- 查询 ----------------

    /// 画布此刻要画的列（别的项目的窗口过滤掉，空列不占位）
    pub fn visible_columns(&self) -> Vec<ColumnLayout> {
        self.state.layout_columns(&self.sessions)
    }

    /// 某个终端窗口属于哪个项目
    pub fn tab_project_id(&self, id: &str) -> Option<String> {
        falcon_core::workspace::tab_project_id(id, &self.sessions, &self.state.pending).map(str::to_string)
    }

    /// 右侧各面板跟谁走：侧栏选中的项目优先，否则当前窗口所属项目；总览且没选项目时为 None
    pub fn focus_project_id(&self) -> Option<String> {
        self.state.focus_project_id(&self.sessions)
    }

    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn session(&self, id: &str) -> Option<&SessionWithProject> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// 当前活动视图对应的窗口 key；总览 / 项目空页没有窗口
    pub fn active_key(&self) -> Option<String> {
        falcon_core::workspace::active_key(&self.state.active)
    }

    /// ⌘T / ＋：侧栏选中的项目优先，否则当前会话所属项目，再否则第一个项目
    pub fn current_project_id(&self) -> Option<String> {
        if let Some(sel) = &self.state.selected_project_id {
            return Some(sel.clone());
        }
        if let ActiveView::Terminal { session_id } = &self.state.active
            && let Some(p) = self.tab_project_id(session_id)
        {
            return Some(p);
        }
        self.projects.first().map(|p| p.id.clone())
    }

    // ---------------- 终端视图 ----------------

    /// 不在 tabs 里的视图丢掉（关 WS）；缺的由窗口在 [`Self::ensure_terminal_views`] 里补建
    pub fn sync_terminals(&mut self) {
        let wanted: HashSet<&String> = self.state.tabs.iter().filter(|t| !is_pending_id(t)).collect();
        self.terminals.retain(|id, _| wanted.contains(id));
    }

    /// 在窗口上下文里补建缺的终端视图（Workspace 本身拿不到 Window）
    pub fn ensure_terminal_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let local = self.profile.is_loopback();
        let ids: Vec<String> = self
            .state
            .tabs
            .iter()
            .filter(|t| !is_pending_id(t) && !self.terminals.contains_key(*t))
            .cloned()
            .collect();
        for id in ids {
            let client = self.client.clone();
            let sid = id.clone();
            let view = cx.new(|cx| TerminalView::new(sid, client, local, window, cx));
            let sid = id.clone();
            cx.subscribe(&view, move |this, view, ev: &TerminalViewEvent, cx| match ev {
                TerminalViewEvent::TitleChanged => {
                    let title = view.read(cx).server_title.clone();
                    this.apply_session_title(&sid, title, cx);
                }
                TerminalViewEvent::StateChanged => cx.notify(),
                TerminalViewEvent::State { state, dead_reason } => this.apply_session_state(&sid, *state, *dead_reason, cx),
                TerminalViewEvent::Reconnected => this.refresh_sessions(cx),
                TerminalViewEvent::NewTerminal => {
                    if let Some(pid) = this.tab_project_id(&sid) {
                        this.new_terminal(&pid, None, None, cx);
                    }
                }
                TerminalViewEvent::ClearRecord => this.clear_dead(&sid, cx),
                TerminalViewEvent::Askpass { id, prompt } => this.push_askpass(id.clone(), prompt.clone(), cx),
                TerminalViewEvent::Unauthorized => {
                    if this.auth_phase == AuthPhase::Ready {
                        this.auth_phase = AuthPhase::NeedLogin;
                        cx.notify();
                    }
                }
            })
            .detach();
            self.terminals.insert(id, view);
        }
    }

    pub fn persist(&self) {
        persist::save(&self.profile, &self.state);
    }
}
