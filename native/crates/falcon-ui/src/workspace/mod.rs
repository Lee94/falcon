//! 一台 falcon 服务端的工作区：旧 React 版那个 zustand store（`packages/web/src/store.ts`）的移植。
//!
//! 两层：
//! - **排布与选择**（开着哪些窗口、列怎么排、焦点在哪、侧栏展开到哪）是纯状态，全部在
//!   [`falcon_core::workspace::WorkspaceState`] 里——那是 store.ts 纯函数部分的移植，带着从
//!   React 版搬来的那组测试。这里只负责"调它、落盘、通知视图"，不再自己写一遍规则；
//! - **服务端数据**（项目 / 主机 / 会话）、登录态、轮询、活着的终端视图，是 app 自己的事。
//!
//! 侧栏、画布、右侧栏、命令面板都 observe 这个 Entity。浮层（菜单 / 确认框 / 表单）不进
//! store：GPUI 里它们由 `window.open_dialog` 等命令式打开，生命周期归 gpui-component 的 Root。

pub mod actions;
pub mod app_icon;
pub mod persist;
pub mod plugins;

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use falcon_client::{FalconClient, ProbeTarget};
use falcon_core::layout::{self as layout, ColumnLayout};
use falcon_core::pane_key::{BrowserTarget, PaneItem, parse_pane_key};
use falcon_platform::Platform;
pub use falcon_core::workspace::{
    ActiveView, DiffTabTarget, FileTabTarget, NewTerminalPlace, PendingSession, RightPanelId,
    WorkspaceState, is_pending_id,
};
use falcon_proto::{AppIconState, AuthStatus, NodeStatus, PluginList, Project, SessionAgent, SessionWithProject, SshHost, SystemInfo};
use gpui_kit::{App, AppContext, Context, Entity, EventEmitter, Task, Window};

use crate::panels::browser_view::{BrowserView, BrowserViewEvent, auto_open_enabled};
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

/// 连上服务端没有：能连上就进入 Ready，不再有登录页。
#[derive(Clone, Debug, PartialEq)]
pub enum AuthPhase {
    /// 还没问过服务端
    Checking,
    /// 服务端连不上（本机服务没起来 / 网络）
    Unreachable(String),
    Ready,
}

/// 工作区对外的事件（窗口据此开浮层）
pub enum WorkspaceEvent {
    /// 有新的 sudo 提示排到了队头
    Askpass,
    /// 某扇窗口要拿焦点
    FocusPane(String),
    /// SSH 项目首次建会话前要先走授权 / 把会话守护进程部署到宿主机（ADR 0020）
    NeedsInstall { project_id: String, agent: Option<SessionAgent> },
    /// 程序状态（OSC 7501）的提醒：`Some` = 某个会话刚卡在 blocked、人又不在那扇窗口里，弹一条；
    /// `None` = 它不卡了（或会话没了），撤掉先前那条。同一会话的提醒原地替换，不叠
    ProgramAlert { session_id: String, alert: Option<ProgramAlertSpec> },
    Toast(Toast),
    /// 要用户确认的动作（关掉一个前台还在跑程序的终端等）
    Confirm(ConfirmSpec),
}

/// 一条 blocked 提醒的内容。标题里总带着会话的显示名：程序给的文字是不可信的，规范要求标明
/// 来源，免得一个程序冒充别的会话（ADR 0020 决定六）
#[derive(Clone, Debug)]
pub struct ProgramAlertSpec {
    pub title: String,
    /// 程序那半句（已消毒、截断）
    pub body: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub body: Option<String>,
    /// 要人读完的（git 失败的原话）：不自动消失。React 版是停留 12s，通知组件只有"5s"与
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
    pub platform: Rc<dyn Platform>,
    pub auth_phase: AuthPhase,
    pub auth: Option<AuthStatus>,
    pub system: Option<SystemInfo>,
    /// /api/system/restart 正在途：挡连点，服务端那边也有幂等
    pub restarting: bool,
    pub projects: Vec<Project>,
    pub hosts: Vec<SshHost>,
    pub sessions: Vec<SessionWithProject>,
    // 旧服务端没有 /api/nodes，继续使用原有列表。
    nodes: Option<HashMap<String, NodeStatus>>,
    inventory_in_flight: bool,
    inventory_requested: bool,
    inventory_loaded: bool,
    session_revision: u64,
    /// 正在终止时保存会话以便失败后恢复。确认后只保留 ID（值为 None），避免迟到的轮询或网关缓存重新显示它。
    hidden_sessions: HashMap<String, Option<SessionWithProject>>,
    /// 后端本机检测到的 CLI；首次检测完成前只显示普通终端。
    pub local_agents: Vec<SessionAgent>,
    agents_in_flight: bool,
    remote_agents: HashMap<String, Vec<SessionAgent>>,
    remote_agents_in_flight: HashSet<String>,
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

    /// 应用图标（ADR 0018）：这台服务端上的选择；None = 还没取到
    pub app_icon: Option<AppIconState>,
    /// 已取回并套好 Dock 版式的自定义图
    pub custom_icon: Option<app_icon::CustomIcon>,
    /// 换 / 传 / 删应用图标的请求在路上
    pub app_icon_busy: bool,

    /// 右侧栏插件（ADR 0025）：这台服务端装了哪些；None = 还没取到
    pub plugins: Option<PluginList>,

    /// 活着的终端视图：在 tabs 里就常驻（不卸载，沿用 React 版——卸载 = 关 WS，再挂要重连 + 回放）
    pub terminals: HashMap<String, Entity<TerminalView>>,
    /// 打开着的远端浏览器视图，按完整 pane key（终端 id + browserSession）索引
    pub browsers: HashMap<String, Entity<BrowserView>>,
    /// 已经处理过「首次出现浏览器」自动弹出的完整 pane key；用户关掉后不再自动弹
    auto_opened_browsers: HashSet<String>,
    /// blocked 提醒的去重（同一条只提醒一次，规则在 falcon-core）
    program_alerts: falcon_core::program_status::BlockedAlerts,
    /// 当前已弹出的提醒：会话回到可视区域或窗口被收起时撤掉，不等程序退出 blocked
    program_alert_sessions: HashSet<String>,
    /// 「跑完了」提示音的判定（进入 done / error 才响，规则在 falcon-core）
    finish_chimes: falcon_core::program_status::FinishChimes,
    /// 终端标题栏上的计时（已运行 / 已等待 / 用时），时刻是相对 `clock_epoch` 的毫秒
    program_clock: falcon_core::program_status::StateClock,
    clock_epoch: web_time::Instant,
    /// 窗口在不在前台（ServerWindow 跟着 GPUI 的窗口激活写进来）。终端的键盘焦点在窗口切到
    /// 后台时并不丢，光看它会把"人去别的 App 了"当成还看着
    pub window_active: bool,
    /// 窗口在不在屏幕上（GPUI 的窗口可见性：最小化、整个被盖住、切到别的桌面、浏览器后台标签页
    /// 都是 false）。看不见时所有终端都算在后台（[`Self::sync_terminal_visibility`]）
    pub window_visible: bool,
    /// 文件窗口里有没存的修改（FileView 经 [`Self::set_file_dirty`] 写进来）。换文件 / 关文件
    /// 窗口 / 关窗口 / 退出之前据此先问一句；只在变的时候写、不触发重画——它不画在别处
    pub file_dirty: bool,
    /// 文件面板刚把正开着的文件改了名（[`Self::follow_file_rename`]）：画布据此让文件窗口
    /// 只换路径、不重读（没存的修改跟过去）。下一次 open / close 文件时清掉
    pub file_renamed: Option<FileRename>,
    changes_in_flight: bool,
    _tasks: Vec<Task<()>>,
}

/// [`Workspace::file_renamed`]
#[derive(Clone, Debug, PartialEq)]
pub struct FileRename {
    pub project_id: String,
    pub from: String,
    pub to: String,
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub fn new(profile: ServerProfile, cx: &mut Context<Self>) -> Self {
        let platform = falcon_platform::get(cx);
        let client = FalconClient::new_with_route(
            profile.url.parse().expect("服务端基址在配置里校验过"),
            profile.route,
        );
        let state = WorkspaceState::from_persisted(&persist::load(&*platform, &profile));
        let auto_opened_browsers: HashSet<String> = state.browsers.iter().map(BrowserTarget::key).collect();
        let mut this = Self {
            profile,
            client,
            platform,
            auth_phase: AuthPhase::Checking,
            auth: None,
            system: None,
            restarting: false,
            projects: Vec::new(),
            hosts: Vec::new(),
            sessions: Vec::new(),
            nodes: None,
            inventory_in_flight: false,
            inventory_requested: false,
            inventory_loaded: false,
            session_revision: 0,
            hidden_sessions: HashMap::new(),
            local_agents: Vec::new(),
            agents_in_flight: false,
            remote_agents: HashMap::new(),
            remote_agents_in_flight: HashSet::new(),
            state,
            heads: HashMap::new(),
            changes: HashMap::new(),
            askpass: VecDeque::new(),
            overview_filter: None,
            overview_project: None,
            selected: Vec::new(),
            app_icon: None,
            custom_icon: None,
            app_icon_busy: false,
            plugins: None,
            terminals: HashMap::new(),
            browsers: HashMap::new(),
            auto_opened_browsers,
            program_alerts: Default::default(),
            program_alert_sessions: HashSet::new(),
            finish_chimes: Default::default(),
            program_clock: Default::default(),
            clock_epoch: web_time::Instant::now(),
            window_active: true,
            window_visible: true,
            file_dirty: false,
            file_renamed: None,
            changes_in_flight: false,
            _tasks: Vec::new(),
        };
        // 画布的收尾挂在自己的每次通知上（React 版挂在 store 订阅上的 settleCanvases）：会改排布 /
        // 焦点 / 可见性的动作有十几处，每处各调一遍迟早漏一处，新列就一直没有画布
        cx.observe_self(|this, cx| {
            this.settle_canvases(cx);
            this.sync_terminal_visibility(cx);
        })
        .detach();
        let tailnet = crate::tailnet::entity(cx);
        let was_running = std::cell::Cell::new(
            tailnet.read(cx).status().state == falcon_tailnet::State::Running
                && tailnet.read(cx).proxy().is_some(),
        );
        cx.observe(&tailnet, move |this, tn, cx| {
            let tn_ref = tn.read(cx);
            let now_running =
                tn_ref.status().state == falcon_tailnet::State::Running && tn_ref.proxy().is_some();
            let became_ready = !was_running.replace(now_running) && now_running;
            if this.profile.is_local() {
                return;
            }
            if became_ready && this.profile.route != falcon_client::ServerRoute::Direct {
                if matches!(this.auth_phase, AuthPhase::Unreachable(_)) {
                    this.auth_phase = AuthPhase::Checking;
                    this.init(cx);
                } else if this.auth_phase == AuthPhase::Ready {
                    this.resume(cx);
                }
            }
            cx.notify();
        })
        .detach();
        this.start(cx);
        this
    }

    /// 切换这扇窗口对应服务端的网络路由，并按需要立即重连。
    pub fn set_route(&mut self, route: falcon_client::ServerRoute, cx: &mut Context<Self>) {
        if self.profile.route == route {
            return;
        }
        self.profile.route = route;
        self.client.set_route(route);
        cx.notify();
        if matches!(self.auth_phase, AuthPhase::Unreachable(_)) {
            self.auth_phase = AuthPhase::Checking;
            self.init(cx);
        } else if self.auth_phase == AuthPhase::Ready {
            self.resume(cx);
        }
    }

    /// 还没分画布的列分好（排不下就自动另起一块），记下当前画布与每块的焦点。分了画布就
    /// 落盘；有任何变化就再通知一次（下一轮 settle 收敛成无事可做，不会转圈）
    pub(crate) fn settle_canvases(&mut self, cx: &mut Context<Self>) {
        let settled = self.state.settle_canvases(&self.sessions);
        if settled.columns {
            self.persist();
        }
        if settled.changed {
            cx.notify();
        }
    }

    /// 轮询在这里起；认证 / 首次加载由窗口触发 [`Self::init`]（本机服务要先确认起来了）
    fn start(&mut self, cx: &mut Context<Self>) {
        // 会话列表与项目/主机列表 5s、askpass 1.5s、侧栏 git 计数 8s：沿用 React 版的节奏
        self._tasks.push(Self::poll(cx, Duration::from_secs(5), |this, cx| {
            if !this.inventory_in_flight {
                this.refresh_inventory(cx);
            }
        }));
        self._tasks.push(Self::poll(cx, Duration::from_millis(1500), |this, cx| this.pull_askpass(cx)));
        self._tasks.push(Self::poll(cx, Duration::from_secs(8), |this, cx| this.refresh_changes(cx)));
        self._tasks.push(Self::poll(cx, Duration::from_secs(30), |this, cx| this.refresh_agents(cx)));
        self._tasks.push(Self::watch_wake(cx));
    }

    /// 睡眠唤醒后立即重连（设计文档 §3.2）。拿不到系统的唤醒通知，改看墙钟：定时器在睡眠期间
    /// 不走，醒来后第一跳的墙钟一下子跨过去一大截。退避中的 socket 此刻可能还要等十几秒，
    /// 连着的也可能早被代理 / NAT 掐成半开，所以一律 `reconnect_now`（连着的只发 ping 探活）。
    /// 墙钟被 NTP 往前拨也会误触发一次，代价只是一轮探活 + 刷新
    fn watch_wake(cx: &mut Context<Self>) -> Task<()> {
        const TICK: Duration = Duration::from_secs(5);
        cx.spawn(async move |this, cx| {
            let mut last = web_time::SystemTime::now();
            loop {
                cx.background_executor().timer(TICK).await;
                let now = web_time::SystemTime::now();
                let gap = now.duration_since(last).unwrap_or_default();
                last = now;
                if gap < TICK + Duration::from_secs(10) {
                    continue;
                }
                if this.update(cx, |this, cx| this.resume(cx)).is_err() {
                    break;
                }
            }
        })
    }

    /// 醒来 / 回到前台：每个终端 `reconnect_now`（退避中的立刻连，连着的发 ping 探活——
    /// 睡眠或后台期间可能早被代理 / NAT 掐成半开），已登录就顺手补拉会话列表与改动计数
    fn resume(&mut self, cx: &mut Context<Self>) {
        for view in self.terminals.values() {
            view.read(cx).reconnect_now();
        }
        if self.auth_phase == AuthPhase::Ready {
            self.refresh_inventory(cx);
            self.refresh_changes(cx);
        }
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

    /// 启动之后：能问到认证状态就进入工作区，拉系统信息 + 项目 + 主机 + 会话。
    /// 问不到就是连不上，不再因为 `required` 停在登录页。
    pub fn init(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
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
            this.update(cx, |this, cx| {
                this.auth = Some(status);
                this.auth_phase = AuthPhase::Ready;
                // 连上了才记作下次启动的默认服务端
                cx.global_mut::<crate::profiles::Profiles>().mark_connected(&this.profile.id);
                this.load_all(cx);
            })
            .ok();
        })
        .detach();
    }

    fn load_all(&mut self, cx: &mut Context<Self>) {
        self.refresh_inventory(cx);
        self.load_app_icon(cx);
        self.refresh_plugins(cx);
        self.refresh_agents(cx);
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(system) = client.system().await {
                this.update(cx, |this, cx| {
                    this.system = Some(system);
                    cx.notify();
                }).ok();
            }
        }).detach();
    }

    /// 重启连着的服务端：POST /api/system/restart 返回后它就会退出重起，连接走自动重连。
    /// 失败统一交 handle_error。
    pub fn restart_server(&mut self, cx: &mut Context<Self>) {
        if self.restarting {
            return;
        }
        self.restarting = true;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| match client.restart_server().await {
            Ok(_) => this
                .update(cx, |this, cx| {
                    this.restarting = false;
                    this.toast(ToastKind::Info, rust_i18n::t!("settings.restart_started").to_string(), None, cx);
                })
                .ok(),
            Err(err) => {
                let text = err.to_string();
                this.update(cx, |this, cx| {
                    this.restarting = false;
                    this.toast(ToastKind::Warning, rust_i18n::t!("settings.restart_failed").to_string(), Some(text), cx);
                    this.handle_error(&err, cx);
                })
                .ok()
            }
        })
        .detach();
    }

    /// REST 失败的统一出口。401 不再切到登录页（没有账户门禁），只提示一下。
    pub fn handle_error(&mut self, err: &falcon_client::ApiError, cx: &mut Context<Self>) {
        if err.is_unauthorized() {
            self.toast(ToastKind::Warning, err.to_string(), None, cx);
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

    /// 同 [`Self::toast`]，但不自动消失（React 版的 `sticky: true`）
    pub fn toast_sticky(&mut self, kind: ToastKind, title: impl Into<String>, body: Option<String>, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::Toast(Toast {
            kind,
            title: title.into(),
            body,
            sticky: true,
        }));
    }

    // ---------------- 数据刷新 ----------------

    /// 两个列表都会更新网关观测到的节点状态。等列表回来再读状态，并在同一次 UI 更新中应用，
    /// 避免迟到的旧列表把已离线主机的子项放回来。用户动作发生在途中时，完成后补拉一次。
    pub fn refresh_inventory(&mut self, cx: &mut Context<Self>) {
        if self.inventory_in_flight {
            self.inventory_requested = true;
            return;
        }
        self.inventory_in_flight = true;
        let session_revision = self.session_revision;
        let client = self.client.clone();
        cx.spawn(async move |this, cx| {
            let (hosts, projects, sessions) =
                futures::join!(client.list_hosts(), client.list_projects(), client.list_sessions());
            let nodes = client.list_nodes().await;
            this.update(cx, |this, cx| {
                this.inventory_in_flight = false;
                let hosts_changed = match hosts {
                    Ok(hosts) if this.hosts != hosts => {
                        this.hosts = hosts;
                        true
                    }
                    Ok(_) => false,
                    Err(err) => {
                        this.handle_error(&err, cx);
                        false
                    }
                };
                let status_current = match &nodes {
                    Ok(_) => true,
                    Err(err) => matches!(err.status(), Some(404 | 405)),
                };
                let mut stable_nodes = HashSet::new();
                let nodes_changed = match nodes {
                    Ok(nodes) => {
                        // 列表截止后，后台发现仍可能恰好成功。刚变为在线的节点这一轮未必列到，
                        // 下一轮它仍在线时才能用列表里的缺席判定会话已删除。
                        for node in &nodes {
                            if let Some(id) = node.node_id.as_ref().filter(|_| node.online)
                                && this.node_status(&node.host_id).is_some_and(|old| {
                                    old.online && old.node_id.as_ref() == Some(id)
                                })
                            {
                                stable_nodes.insert(id.clone());
                            }
                        }
                        let next = Some(nodes.into_iter().map(|node| (node.host_id.clone(), node)).collect());
                        let changed = this.nodes != next;
                        this.nodes = next;
                        changed
                    }
                    // 服务端可能刚升级，每轮重试能力探测。其它错误保留最近一次状态，不当成兼容模式。
                    Err(err) if matches!(err.status(), Some(404 | 405)) => this.nodes.take().is_some(),
                    Err(err) => {
                        this.handle_error(&err, cx);
                        false
                    }
                };
                let fresh_projects = projects.is_ok();
                let mut projects = match projects {
                    Ok(projects) => projects,
                    Err(err) => {
                        this.handle_error(&err, cx);
                        this.projects.clone()
                    }
                };
                projects.retain(|project| {
                    project.project_type != falcon_proto::ProjectType::Ssh
                        || status_current && this.nodes.as_ref().is_none_or(|_| {
                            fresh_projects && project.host_id.as_deref().is_none_or(|host| this.host_lists_available(host))
                        })
                });
                this.set_projects(projects, cx);
                // 独立会话的 owner 不在项目列表中，需另行清理已经看不见的主机选择。
                if let Some(id) = this.state.selected_project_id.as_deref().and_then(falcon_proto::standalone_host_id)
                    && !this.host_lists_available(id)
                {
                    this.state.selected_project_id = None;
                    if this.state.active == ActiveView::Project {
                        this.state.active = ActiveView::Overview;
                    }
                    cx.notify();
                }
                let (mut sessions, fresh_sessions) = match sessions {
                    Ok(sessions) if this.session_revision == session_revision => (sessions, true),
                    Ok(_) => (this.sessions.clone(), false),
                    Err(err) => {
                        this.handle_error(&err, cx);
                        let sessions = this.sessions.iter().filter(|session| {
                            this.nodes.is_none() || session.project_type != falcon_proto::ProjectType::Ssh
                        }).cloned().collect();
                        (sessions, false)
                    }
                };
                let lists_current = status_current && fresh_sessions;
                if !status_current {
                    sessions.retain(|session| session.project_type != falcon_proto::ProjectType::Ssh);
                }
                this.apply_sessions(sessions, fresh_projects, lists_current, &stable_nodes, cx);
                if !this.inventory_loaded && fresh_projects && lists_current {
                    this.inventory_loaded = true;
                    if let Some(sel) = this.state.selected_project_id.clone() {
                        if this.owner_project(&sel).is_some() {
                            this.select_project(&sel, cx);
                        } else {
                            this.state.selected_project_id = None;
                        }
                    }
                    this.refresh_changes(cx);
                    cx.notify();
                }
                if hosts_changed || nodes_changed {
                    this.refresh_remote_agents(cx);
                    cx.notify();
                }
                if std::mem::take(&mut this.inventory_requested) {
                    this.refresh_inventory(cx);
                }
            }).ok();
        }).detach();
    }

    pub fn node_status(&self, host_id: &str) -> Option<&NodeStatus> {
        self.nodes.as_ref()?.get(host_id)
    }

    fn host_lists_available(&self, host_id: &str) -> bool {
        self.nodes.as_ref().is_none_or(|nodes| nodes.get(host_id).is_some_and(|node| node.online))
    }

    /// 列表里的 SSH 会话从项目或独立会话 owner 判归属。旧服务端没有节点状态时沿用原列表。
    fn session_visible(&self, session: &SessionWithProject) -> bool {
        if self.nodes.is_none() || session.project_type != falcon_proto::ProjectType::Ssh {
            return true;
        }
        if let Some(host) = falcon_proto::standalone_host_id(&session.project_id) {
            return self.host_lists_available(host);
        }
        self.project(&session.project_id).is_some()
    }

    fn set_projects(&mut self, projects: Vec<Project>, cx: &mut Context<Self>) {
        if self.projects == projects {
            return;
        }
        self.state.apply_projects(&projects);
        self.projects = projects;
        self.refresh_remote_agents(cx);
        self.refresh_heads(cx);
        cx.notify();
    }

    /// CLI 清单来自会话实际所在的机器；没有检测结果时不显示候选项。
    pub fn session_agents(&self, project_id: &str) -> &[SessionAgent] {
        let Some(project) = self.owner_project(project_id) else { return &self.local_agents };
        if project.project_type != falcon_proto::ProjectType::Ssh {
            return &self.local_agents;
        }
        let key = if let Some(host_id) = project.host_id {
            format!("host:{host_id}")
        } else {
            format!("project:{}", project.id)
        };
        self.remote_agents.get(&key).map(Vec::as_slice).unwrap_or_default()
    }

    fn refresh_remote_agents(&mut self, cx: &mut Context<Self>) {
        if self.auth_phase != AuthPhase::Ready {
            return;
        }
        // 同一主机的项目共用一份清单；未绑定已保存主机的旧 SSH 项目按项目探测。
        let mut targets: Vec<_> = self.hosts.iter().filter(|host| self.host_lists_available(&host.id)).map(|host| {
            (format!("host:{}", host.id), ProbeTarget::SshHost(host.id.clone()))
        }).collect();
        targets.extend(self.projects.iter().filter(|p| {
            p.project_type == falcon_proto::ProjectType::Ssh && p.host_id.is_none()
                && p.worktree.as_ref().is_none_or(|w| w.archived_at.is_none())
        }).map(|p| (format!("project:{}", p.id), ProbeTarget::Project(p.id.clone()))));
        for (key, target) in targets {
            if !self.remote_agents_in_flight.insert(key.clone()) {
                continue;
            }
            let fut = self.client.agents(&target);
            cx.spawn(async move |this, cx| {
                let result = fut.await;
                this.update(cx, |this, cx| {
                    this.remote_agents_in_flight.remove(&key);
                    let agents: Vec<_> = match result {
                        Ok(agents) => agents.into_iter().filter(|a| *a != SessionAgent::Unknown).collect(),
                        Err(err) => {
                            log::warn!("检测远端 agent CLI 失败（{key}）：{err}");
                            this.handle_error(&err, cx);
                            Vec::new()
                        }
                    };
                    if this.remote_agents.get(&key) != Some(&agents) {
                        this.remote_agents.insert(key, agents);
                        cx.notify();
                    }
                }).ok();
            }).detach();
        }
    }

    fn refresh_agents(&mut self, cx: &mut Context<Self>) {
        self.refresh_remote_agents(cx);
        if self.auth_phase != AuthPhase::Ready || self.agents_in_flight {
            return;
        }
        self.agents_in_flight = true;
        let fut = self.client.local_agents();
        cx.spawn(async move |this, cx| {
            let result = fut.await;
            this.update(cx, |this, cx| {
                this.agents_in_flight = false;
                let agents = match result {
                    Ok(agents) => agents.into_iter().filter(|a| *a != SessionAgent::Unknown).collect(),
                    Err(err) => {
                        log::warn!("检测本地 agent CLI 失败：{err}");
                        this.handle_error(&err, cx);
                        Vec::new()
                    }
                };
                if this.local_agents != agents {
                    this.local_agents = agents;
                    cx.notify();
                }
            }).ok();
        }).detach();
    }

    fn filter_hidden_sessions(&mut self, sessions: &mut Vec<SessionWithProject>) {
        sessions.retain(|session| match self.hidden_sessions.get_mut(&session.id) {
            None => true,
            Some(snapshot) => {
                if snapshot.is_some() {
                    *snapshot = Some(session.clone());
                }
                false
            }
        });
    }

    fn apply_sessions(
        &mut self,
        mut sessions: Vec<SessionWithProject>,
        fresh_projects: bool,
        fresh_sessions: bool,
        stable_nodes: &HashSet<String>,
        cx: &mut Context<Self>,
    ) {
        // 项目和会话请求之间可能跨过一次断线。已返回但此刻不可见的会话不能作为删除布局的依据。
        let mut unavailable = HashSet::new();
        sessions.retain(|session| {
            if self.session_visible(session) {
                true
            } else {
                unavailable.insert(session.id.clone());
                false
            }
        });
        self.filter_hidden_sessions(&mut sessions);
        // 开着窗口、WS 推过程序状态的会话以推来的全集为准：这次轮询的请求可能发在推送之前、
        // 响应却晚到，拿它覆盖会让刚清掉的 blocked 徽标又冒出来，一直挂到下一轮轮询
        for s in sessions.iter_mut() {
            let Some(view) = self.terminals.get(&s.id) else { continue };
            if let Some(records) = view.read(cx).program_records() {
                s.session.program_status = falcon_core::program_status::summarize(records);
            }
            // 窗口可能比列表先建出来（启动时恢复的、刚建好的会话），那时持久性还不知道。
            // 放在下面「列表没变就跳过」之前：视图建出来之后列表可能就再没变过
            let durable = s.durable;
            view.update(cx, |v, _| v.set_durable(durable));
        }
        // 程序状态的提醒先对账：哪怕列表没变也要过一遍——头一次拿到列表（哪怕是空的）才算
        // 「启动时已经卡着的都记下了」，之后的新 blocked 才提醒
        let changes = self
            .program_alerts
            .observe_all(sessions.iter().map(|s| (s.id.as_str(), s.program_status.as_ref())));
        let finished = self
            .finish_chimes
            .observe_all(sessions.iter().map(|s| (s.id.as_str(), s.program_status.as_ref())));
        let now = self.clock_now();
        self.program_clock.observe_all(sessions.iter().map(|s| (s.id.as_str(), s.program_status.as_ref())), now);
        // 提醒读的是换上之后的列表。即使列表没变，也要按最新的节点状态对账排布。
        let changed = self.sessions != sessions;
        if changed {
            self.sessions = sessions;
        }
        for (id, change) in changes {
            self.emit_program_alert(&id, change, cx);
        }
        self.chime_finished(&finished, cx);
        let preserve = |id: &str| {
            if !fresh_sessions || unavailable.contains(id) {
                return true;
            }
            let Some(node_id) = id.strip_prefix("node:").and_then(|id| id.split_once(':').map(|(node, _)| node)) else {
                return false;
            };
            // 离线状态可能不带节点 ID，冷启动也没有旧列表可供对照。无法确认归属时宁可留下
            // 布局 ID，等所属节点重新出现再对账。同一节点也可能通过其它在线别名列出。
            !fresh_projects || self.nodes.as_ref().is_none_or(|nodes| {
                !stable_nodes.contains(node_id) && nodes.values().any(|node| {
                    !node.online || node.node_id.is_none() || node.node_id.as_deref() == Some(node_id)
                })
            })
        };
        let mut layout_changed = self.state.apply_sessions(&self.sessions, &preserve);
        if auto_open_enabled(cx) {
            // 终端第一次出现在工作区时也接入已经在节点上运行的浏览器。
            let open_sessions: HashSet<_> = self.state.tabs.iter().cloned().collect();
            for session in self.sessions.iter().filter(|s| open_sessions.contains(&s.id)) {
                for brief in falcon_core::workspace::browser_briefs(&session.session) {
                    let target = falcon_core::workspace::browser_target_for_brief(&session.id, brief);
                    if self.auto_opened_browsers.insert(target.key()) {
                        layout_changed |= self.state.open_browser(target, false, &self.sessions);
                    }
                }
            }
        }
        if changed {
            let alive: HashSet<&str> = self.sessions.iter().map(|s| s.id.as_str()).collect();
            self.selected.retain(|id| alive.contains(id.as_str()));
            self.auto_opened_browsers.retain(|key| {
                matches!(parse_pane_key(key), Some(PaneItem::Browser { session_id, .. })
                    if alive.contains(session_id.as_str()) || preserve(&session_id))
            });
        }
        if changed || layout_changed {
            self.sync_terminals();
            cx.notify();
        }
        if layout_changed {
            self.persist();
        }
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

    /// WS 推来的程序状态全集：自己算摘要写进列表（侧栏、标题栏只认 `Session.program_status`），
    /// 不等 5s 轮询；再看要不要提醒
    pub fn apply_program_records(&mut self, id: &str, records: &[falcon_proto::ProgramStatusRecord], cx: &mut Context<Self>) {
        let summary = falcon_core::program_status::summarize(records);
        falcon_core::workspace::apply_session_program_status(&mut self.sessions, id, summary.as_ref());
        let now = self.clock_now();
        self.program_clock.observe(id, summary.as_ref(), now);
        // 摘要没变也要重画：终端标题栏还列着子任务，它们只在记录全集里
        cx.notify();
        let change = self.program_alerts.observe(id, summary.as_ref());
        self.emit_program_alert(id, change, cx);
        if self.finish_chimes.observe(id, summary.as_ref()) {
            self.chime_finished(&[id.to_string()], cx);
        }
    }

    /// 这几个会话刚跑完：响一声（一批里有几个都只响一声）。人正看着的那个不算——窗口在前台、
    /// 活动视图就是它、键盘焦点在它的终端上，标题栏的徽标就在眼前
    fn chime_finished(&self, ids: &[String], cx: &mut Context<Self>) {
        if ids.is_empty() || !crate::chime::enabled(cx) {
            return;
        }
        if ids.iter().all(|id| self.watching(id, cx)) {
            return;
        }
        log::debug!("程序跑完了，响提示音：{}", ids.join(", "));
        crate::chime::play(cx);
    }

    fn clock_now(&self) -> u64 {
        self.clock_epoch.elapsed().as_millis() as u64
    }

    /// 终端标题栏上的计时读数（本客户端没亲眼看到这一轮开始时为 None）
    pub fn program_clock(&self, id: &str) -> Option<falcon_core::program_status::ClockReading> {
        self.program_clock.reading(id, self.clock_now())
    }

    /// 侧栏会话行尾巴上的那一格：哪一档、进这一档多久了
    pub fn row_status(&self, s: &falcon_proto::Session) -> falcon_core::program_status::RowReading {
        let wall_now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.program_clock.row(&s.id, s.program_status.as_ref(), s.last_active_at, self.clock_now(), wall_now)
    }

    fn watching(&self, id: &str, cx: &App) -> bool {
        self.window_active
            && matches!(&self.state.active, ActiveView::Terminal { session_id } if session_id == id)
            && self.terminals.get(id).is_some_and(|v| v.read(cx).is_focused())
    }

    /// 刚卡住且不在可视区域的会话才提醒；并排可见但没有键盘焦点的终端也能直接看见徽标。
    /// 可见性与尺寸仲裁共用当前画布、最大化和窗口遮挡的判断，不能用活动窗口代替。
    fn emit_program_alert(&mut self, id: &str, change: falcon_core::program_status::AlertChange, cx: &mut Context<Self>) {
        use falcon_core::program_status::{AlertChange, badge};
        match change {
            AlertChange::Unchanged => {}
            AlertChange::Resolved => self.dismiss_program_alert(id, cx),
            AlertChange::Blocked => {
                if self.visible_terminal_ids().contains(id) {
                    self.dismiss_program_alert(id, cx);
                    return;
                }
                let Some(session) = self.session(id) else { return };
                let Some(b) = session.program_status.as_ref().and_then(badge) else { return };
                let name = crate::labels::session_label(session, &self.projects);
                let what = rust_i18n::t!(b.label_key).to_string();
                let title = rust_i18n::t!("programStatus.toastTitle", name = name, what = what).to_string();
                let alert = ProgramAlertSpec { title, body: b.detail };
                self.program_alert_sessions.insert(id.to_string());
                cx.emit(WorkspaceEvent::ProgramAlert { session_id: id.to_string(), alert: Some(alert) });
            }
        }
    }

    fn dismiss_program_alert(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.program_alert_sessions.remove(id) {
            cx.emit(WorkspaceEvent::ProgramAlert { session_id: id.to_string(), alert: None });
        }
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

    /// 当前视图的画布、正在显示哪块、这块上画哪些列。最大化时只留活动那一扇（它不在场就当
    /// 没最大化）。画布照它画，终端的可见性（[`Self::sync_terminal_visibility`]）也照它算
    pub fn shown_canvas(&self) -> (Vec<layout::CanvasGroup>, usize, Vec<ColumnLayout>) {
        let groups = layout::canvas_groups(&self.visible_columns());
        let active = self.active_key();
        let current = layout::canvas_index(&groups, active.as_deref(), self.state.canvas_id.as_deref());
        let shown = groups.get(current).map(|g| g.columns.clone()).unwrap_or_default();
        let columns = match active.as_deref().filter(|_| self.state.term_zoomed).and_then(|k| layout::find_pane(&shown, k)) {
            Some(at) => {
                let col = &shown[at.col];
                vec![ColumnLayout { basis: None, panes: vec![col.panes[at.index].clone()], ..col.clone() }]
            }
            None => shown,
        };
        (groups, current, columns)
    }

    /// 告诉每扇终端它此刻在不在屏幕上：正在显示的画布上（最大化时只算那一扇）、没被总览盖住、
    /// 窗口本身看得见（没最小化 / 整个被盖住 / 在后台标签页）。看不见的不参与定 PTY 的尺寸
    /// （ADR 0022）。挂在工作区的每次通知上：切画布、开合总览、窗口可见性变化都会经过这里
    pub(crate) fn sync_terminal_visibility(&mut self, cx: &mut Context<Self>) {
        let shown = self.visible_terminal_ids();
        for (id, view) in &self.terminals {
            let visible = shown.contains(id);
            view.update(cx, |v, _| v.set_visible(visible));
        }
        let shown_browsers = self.visible_browser_ids();
        for (id, view) in &self.browsers {
            let visible = shown_browsers.contains(id);
            view.update(cx, |v, _| v.set_visible(visible));
        }
        // 切回会话所在画布、退出最大化或重新显示窗口后，已有的「切过去」提醒也不再需要。
        let seen: Vec<String> = self.program_alert_sessions.intersection(&shown).cloned().collect();
        for id in seen {
            self.dismiss_program_alert(&id, cx);
        }
    }

    fn visible_browser_ids(&self) -> HashSet<String> {
        if !self.window_visible || self.state.active == ActiveView::Overview {
            return HashSet::new();
        }
        let (_, _, columns) = self.shown_canvas();
        columns
            .iter()
            .flat_map(|c| c.panes.iter())
            .filter_map(|p| match parse_pane_key(&p.key) {
                Some(PaneItem::Browser { key, .. }) => Some(key),
                _ => None,
            })
            .collect()
    }

    fn visible_terminal_ids(&self) -> HashSet<String> {
        if !self.window_visible || self.state.active == ActiveView::Overview {
            return HashSet::new();
        }
        let (_, _, columns) = self.shown_canvas();
        columns
            .iter()
            .flat_map(|c| c.panes.iter())
            .filter_map(|p| match parse_pane_key(&p.key) {
                Some(PaneItem::Terminal { id, .. }) => Some(id),
                _ => None,
            })
            .collect()
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

    /// 按 owner id 解析项目元数据：真实项目直接克隆返回；`standalone:local` 与
    /// `standalone:host:<id>` 合成一条不带 `working_dir` 的虚拟 `Project`，使标题栏连接串、
    /// 主机身份色、SSH 守护进程安装引导、文件面板等逻辑无需分支即可复用。
    pub fn owner_project(&self, id: &str) -> Option<Project> {
        if id == falcon_proto::STANDALONE_LOCAL {
            return Some(Project {
                id: id.to_string(),
                name: rust_i18n::t!("sidebar.standalone").to_string(),
                project_type: falcon_proto::ProjectType::Local,
                working_dir: None,
                shell: None,
                ssh: None,
                host_id: None,
                worktree: None,
                default_worktree_branch: None,
                multi: None,
                created_at: 0,
            });
        }
        if let Some(hid) = falcon_proto::standalone_host_id(id) {
            let host = self.hosts.iter().find(|h| h.id == hid)?;
            return Some(Project {
                id: id.to_string(),
                name: rust_i18n::t!("sidebar.standalone").to_string(),
                project_type: falcon_proto::ProjectType::Ssh,
                working_dir: None,
                shell: None,
                ssh: Some(falcon_proto::SshConfig {
                    host: host.host.clone(),
                    port: host.port,
                    username: host.username.clone(),
                    auth_method: host.auth_method,
                    key_path: host.key_path.clone(),
                    has_secret: host.has_secret,
                }),
                host_id: Some(host.id.clone()),
                worktree: None,
                default_worktree_branch: None,
                multi: None,
                created_at: host.created_at,
            });
        }
        self.project(id).cloned()
    }

    pub fn session(&self, id: &str) -> Option<&SessionWithProject> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// 当前活动视图对应的窗口 key；总览 / 项目空页没有窗口
    pub fn active_key(&self) -> Option<String> {
        falcon_core::workspace::active_key(&self.state.active)
    }

    /// ⌘T / ⌘D / ＋：侧栏选中的项目优先，否则当前活动窗口所属项目，再否则第一个项目
    pub fn current_project_id(&self) -> Option<String> {
        self.focus_project_id().or_else(|| self.projects.first().map(|p| p.id.clone()))
    }

    // ---------------- 终端视图 ----------------

    /// 不在 tabs 或实时列表里的视图 Detach（关 WS）；重新出现时按原排布补建。
    pub fn sync_terminals(&mut self) {
        let wanted: HashSet<&String> = self.state.tabs.iter().collect();
        let live: HashSet<&String> = self.sessions.iter().map(|s| &s.id).collect();
        let wanted_browsers: HashSet<_> = self.state.browsers.iter()
            .filter(|target| live.contains(&target.session_id))
            .map(BrowserTarget::key)
            .collect();
        self.terminals.retain(|id, _| wanted.contains(id) && live.contains(id));
        self.browsers.retain(|key, _| wanted_browsers.contains(key));
    }

    /// 在窗口上下文里补建缺的终端视图（Workspace 本身拿不到 Window）
    pub fn ensure_terminal_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let local = self.profile.is_loopback();
        let ids: Vec<String> = self
            .state
            .tabs
            .iter()
            .filter(|t| self.session(t).is_some() && !self.terminals.contains_key(*t))
            .cloned()
            .collect();
        for id in ids {
            let client = self.client.clone();
            let sid = id.clone();
            let durable = self.session(&id).map(|s| s.durable);
            let view = cx.new(|cx| TerminalView::new(sid, client, local, durable, window, cx));
            let sid = id.clone();
            cx.subscribe(&view, move |this, view, ev: &TerminalViewEvent, cx| match ev {
                TerminalViewEvent::OpenFile { project_id, path } => this.open_file(project_id, path, cx),
                TerminalViewEvent::TitleChanged => {
                    let title = view.read(cx).server_title.clone();
                    this.apply_session_title(&sid, title, cx);
                }
                TerminalViewEvent::StateChanged => cx.notify(),
                TerminalViewEvent::State { state, dead_reason } => this.apply_session_state(&sid, *state, *dead_reason, cx),
                TerminalViewEvent::Terminated => this.on_session_terminated(&sid, cx),
                TerminalViewEvent::Reconnected => this.refresh_inventory(cx),
                TerminalViewEvent::NewTerminal => {
                    if let Some(pid) = this.tab_project_id(&sid) {
                        this.new_terminal(&pid, None, None, cx);
                    }
                }
                TerminalViewEvent::ClearRecord => this.clear_dead(&sid, cx),
                TerminalViewEvent::Askpass { id, prompt } => this.push_askpass(id.clone(), prompt.clone(), cx),
                // 服务端不再因未登录关掉 socket。旧服务端若仍回 4401，终端自己停在未连接，不切登录页。
                TerminalViewEvent::Unauthorized => {}
                TerminalViewEvent::Toast { kind, title, body } => this.toast(*kind, title.clone(), body.clone(), cx),
                TerminalViewEvent::ProgramStatus(records) => this.apply_program_records(&sid, records, cx),
            })
            .detach();
            self.terminals.insert(id, view);
        }
        for (id, view) in &self.terminals {
            let project = self.tab_project_id(id).and_then(|pid| self.owner_project(&pid));
            view.update(cx, |v, _| v.set_file_context(project));
        }
        let visible_browsers = self.visible_browser_ids();
        let missing_browsers: Vec<BrowserTarget> = self
            .state
            .browsers
            .iter()
            .filter(|target| self.session(&target.session_id).is_some() && !self.browsers.contains_key(&target.key()))
            .cloned()
            .collect();
        for target in missing_browsers {
            let client = self.client.clone();
            let key = target.key();
            let initial_visible = visible_browsers.contains(&key);
            let view = cx.new(|cx| BrowserView::new(target, client, initial_visible, window, cx));
            cx.subscribe(&view, |_, _, _: &BrowserViewEvent, cx| cx.notify()).detach();
            self.browsers.insert(key, view);
        }
        // 新建的视图在 socket 上默认看得见；建在后台画布上的要在它报格子之前说清楚
        self.sync_terminal_visibility(cx);
    }

    pub fn persist(&self) {
        persist::save(&*self.platform, &self.profile, &self.state);
    }
}
