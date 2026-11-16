//! 持久会话的授权 / 部署流程（ADR 0020；对照 GPUI 基线 `falcon-ui/src/dialogs/install.rs`）。
//!
//! SSH 项目第一次在某台远端主机上建会话前：先问一次授权（会往用户的服务器写可执行文件，
//! 授权按 host + port + username 记），同意后经 `/ws/install/:projectId` 看后端探测宿主机、
//! 推二进制、在宿主机上校验的进度；瞬时故障后端自动重试，稳定的环境事实（noexec、目录不可写）
//! 直接给重试按钮。拒绝 / 跳过则直接建非持久会话。`then_create` 带着开场请求：装完（或拒绝 /
//! 跳过）后接着把会话建出来；为 `None` 时（命令面板「为某主机启用持久会话」）只装不建。
//!
//! 授权是「允许 falcon 往这台服务器写可执行文件」的决定，该问；部署是随后的执行过程，只报进度。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[cfg(not(target_arch = "wasm32"))]
use falcon_client::{HostAuthorizationPatch, InstallEvent, InstallSocket};
use falcon_proto::{
    CreateSessionRequest, HostInstallStatus, InstallStage, NonDurableReason,
    can_retry_install,
};
#[cfg(not(target_arch = "wasm32"))]
use falcon_proto::{InstallServerMessage, SessionState};
use nsh_core::prelude::{Owner, Signal, signal};
#[cfg(not(target_arch = "wasm32"))]
use nsh_core::prelude::spawn;

#[cfg(not(target_arch = "wasm32"))]
use crate::i18n::t;
use crate::store::{ActiveDialog, SessionPlacement, WorkspaceStore};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InstallPhase {
    Ask,
    Installing,
    Failed,
}

/// 对话框的一份快照（放在 Signal 里，界面订阅；内部过程量 settled 也在里面）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct InstallState {
    pub project_id: String,
    pub host: String,
    /// `HostInstallStatus.required_version`：这次要推的守护进程版本（写进授权询问正文）
    pub required_version: String,
    pub phase: InstallPhase,
    pub stage: InstallStage,
    /// 部署过程中远端实际跑过的命令；最新一条是正在执行或刚失败的那条
    pub commands: Vec<String>,
    pub failure: Option<NonDurableReason>,
    pub detail: Option<String>,
    /// 后端本轮自动重试到第几次；>1 时说明"还在转"是因为在重试，而不是卡住了
    pub attempt: u32,
    /// 这一轮后端已经报过结果：之后通道的断开只是收尾，不能拿它盖掉真正的结果
    pub settled: bool,
    pub show_detail: bool,
    /// 授权请求失败的本地错误（就地显示，不弹走对话框）
    pub error: Option<String>,
}

/// 装完 / 放弃之后回到用户本来想干的事（GPUI 的 `then_create`）：新建终端的请求与摆放位置。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingCreate {
    pub project_id: String,
    pub request: CreateSessionRequest,
    pub placement: SessionPlacement,
}

/// 授权询问 → 部署进度 → 续建会话 的整条流程。状态只服务一个对话框（模态层一次只开一个）。
#[derive(Clone)]
pub(crate) struct InstallOperations {
    owner: Owner,
    state: Signal<Option<InstallState>>,
    pending_create: Rc<RefCell<Option<PendingCreate>>>,
    /// 取消通道：丢掉发送端，读部署通道的任务就给服务端发 cancel 再收摊
    cancel: Rc<RefCell<Option<futures::channel::mpsc::UnboundedSender<()>>>>,
    /// 每次 `start_install` +1：重试 / 取消后迟到的旧事件凭它丢弃
    generation: Rc<Cell<u64>>,
}

impl InstallOperations {
    pub fn new(owner: Owner) -> Self {
        Self {
            owner,
            state: signal(None),
            pending_create: Rc::new(RefCell::new(None)),
            cancel: Rc::new(RefCell::new(None)),
            generation: Rc::new(Cell::new(0)),
        }
    }

    pub fn state(&self) -> Option<InstallState> {
        self.state.get()
    }

    fn update(&self, f: impl FnOnce(&mut InstallState)) {
        self.state.update(|state| {
            if let Some(state) = state.as_mut() {
                f(state);
            }
        });
    }

    /// `create_terminal_with_request` 的前置门槛：SSH 项目先查宿主机守护进程状态，
    /// 需要授权 / 部署就弹对话框接管建会话。返回 `true` 表示已接管（调用方别再直接建）。
    pub fn gate(
        &self,
        store: &WorkspaceStore,
        project_id: &str,
        request: CreateSessionRequest,
        placement: SessionPlacement,
    ) -> bool {
        let Some(project) = store
            .projects
            .get_untracked()
            .into_iter()
            .find(|project| project.id == project_id)
        else {
            return false;
        };
        let is_ssh = project.source.as_ref().map_or_else(
            || project.host_id.is_some(),
            |source| source.project_type == falcon_proto::ProjectType::Ssh,
        );
        if !is_ssh {
            return false;
        }
        // 对话框已开着：吞掉这次按下——既不能覆盖进行中的部署，也不能静默建出非持久会话
        if self.state.get_untracked().is_some() {
            return true;
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (request, placement);
            false
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(client) = store.client() else {
                return false;
            };
            let connection_generation = store.connection_generation();
            let host = project
                .host_name
                .clone()
                .unwrap_or_else(|| project.name.clone());
            let operations = self.clone();
            let store = store.clone();
            let pid = project_id.to_string();
            self.owner.run(move || {
                spawn(async move {
                    let status = client.host_status(&pid).await;
                    if store.connection_generation() != connection_generation {
                        return;
                    }
                    match status {
                        // 没问过授权，或宿主机上没有这一版守护进程：先走对话框看着进度部署
                        Ok(status)
                            if falcon_core::host_install::needs_host_setup(&status) =>
                        {
                            operations.open(
                                &store,
                                status,
                                pid.clone(),
                                host,
                                Some(PendingCreate {
                                    project_id: pid,
                                    request,
                                    placement,
                                }),
                            );
                        }
                        // 查不到主机状态就照常建会话，由后端判定持久性
                        _ => {
                            store.create_terminal_now(pid, request, placement);
                        }
                    }
                });
            });
            true
        }
    }

    /// 打开对话框。已授权（宿主机上没有这一版守护进程）就直接部署，不再打扰用户。
    fn open(
        &self,
        store: &WorkspaceStore,
        status: HostInstallStatus,
        project_id: String,
        host: String,
        pending: Option<PendingCreate>,
    ) {
        *self.pending_create.borrow_mut() = pending;
        let authorized = status.authorized == Some(true);
        self.state.set(Some(InstallState {
            project_id,
            host,
            required_version: status.required_version,
            phase: InstallPhase::Ask,
            stage: InstallStage::Probing,
            commands: Vec::new(),
            failure: None,
            detail: None,
            attempt: 1,
            settled: false,
            show_detail: false,
            error: None,
        }));
        store.active_dialog.set(ActiveDialog::Install);
        if authorized {
            self.start_install(store);
        }
    }

    /// 装完 / 跳过 / 取消 / 关掉：停掉还在跑的部署，关对话框；有续建就接着把会话建出来
    /// （跳过 / 拒绝 → 后端建出来的就是非持久会话）。
    pub fn finish(&self, store: &WorkspaceStore) {
        self.abort();
        let pending = self.pending_create.borrow_mut().take();
        self.state.set(None);
        if store.active_dialog.get_untracked() == ActiveDialog::Install {
            store.active_dialog.set(ActiveDialog::None);
        }
        if let Some(pending) = pending {
            store.create_terminal_now(pending.project_id, pending.request, pending.placement);
        }
    }

    /// 切换服务端 / 断线时丢弃整条流程：取消部署、清状态，不续建。
    pub fn reset(&self) {
        self.abort();
        self.pending_create.borrow_mut().take();
        self.state.set(None);
    }

    /// 回车：当前阶段的主按钮（Ask → allow；Failed 且可重试 → retry）。
    pub fn primary(&self, store: &WorkspaceStore) {
        let Some(state) = self.state.get_untracked() else {
            return;
        };
        match state.phase {
            InstallPhase::Ask => self.allow(store),
            InstallPhase::Failed if can_retry_install(state.failure) => self.retry(store),
            _ => {}
        }
    }

    pub fn toggle_detail(&self) {
        self.update(|s| s.show_detail = !s.show_detail);
    }

    /// 丢掉发送端 = 读部署通道的任务给服务端发 cancel 再收摊（服务端据此 abort）。
    fn abort(&self) {
        self.cancel.borrow_mut().take();
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn start_install(&self, store: &WorkspaceStore) {
        self.abort();
        let generation = self.generation.get();
        let Some(state) = self.state.get_untracked() else {
            return;
        };
        let Some(client) = store.client() else {
            self.update(|s| {
                s.phase = InstallPhase::Failed;
                s.failure = Some(NonDurableReason::ProbeFailed);
                s.detail = Some(t!("native.nsh.serverNotConnected"));
                s.settled = true;
            });
            return;
        };
        self.update(|s| {
            s.phase = InstallPhase::Installing;
            s.stage = InstallStage::Probing;
            s.attempt = 1;
            s.failure = None;
            s.detail = None;
            s.error = None;
            s.settled = false;
            s.commands.clear();
        });
        let mut socket = InstallSocket::open(&client, &state.project_id);
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        *self.cancel.borrow_mut() = Some(tx);
        let operations = self.clone();
        let store = store.clone();
        self.owner.run(move || {
            spawn(async move {
                use futures::{FutureExt, StreamExt};
                loop {
                    futures::select! {
                        ev = socket.next().fuse() => match ev {
                            Some(ev) => operations.on_event(&store, generation, ev),
                            // 服务端报完结果自己关通道，流到这里结束
                            None => break,
                        },
                        _ = rx.next().fuse() => {
                            // 显式取消与 InstallSocket 被丢掉走同一条路：告诉服务端取消再关
                            socket.cancel();
                            break;
                        }
                    }
                    if operations.generation.get() != generation {
                        socket.cancel();
                        break;
                    }
                }
            });
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn on_event(&self, store: &WorkspaceStore, generation: u64, ev: InstallEvent) {
        if self.generation.get() != generation {
            return;
        }
        match ev {
            InstallEvent::Message(InstallServerMessage::Stage {
                stage,
                attempt,
                command,
            }) => self.update(|s| {
                s.stage = stage;
                s.attempt = attempt;
                if let Some(cmd) = command
                    && s.commands.last() != Some(&cmd)
                {
                    s.commands.push(cmd);
                }
            }),
            InstallEvent::Message(InstallServerMessage::Done) => {
                self.update(|s| s.settled = true);
                self.on_installed(store);
            }
            InstallEvent::Message(InstallServerMessage::Failed {
                reason,
                detail,
                attempts,
            }) => self.update(|s| {
                s.settled = true;
                s.failure = Some(reason);
                s.detail = detail;
                s.attempt = attempts.unwrap_or(1);
                s.phase = InstallPhase::Failed;
            }),
            InstallEvent::Message(InstallServerMessage::Unknown) | InstallEvent::Closed => {}
            // 连不上后端：这条通道自己就断了，跟远端装没装成无关，同样给重试按钮
            InstallEvent::ChannelError(message) => {
                if self.state.get_untracked().is_none_or(|s| s.settled) {
                    return;
                }
                self.update(|s| {
                    s.settled = true;
                    s.failure = Some(NonDurableReason::ProbeFailed);
                    s.detail = Some(format!("{}\n{message}", t!("durable.channelError")));
                    s.phase = InstallPhase::Failed;
                });
            }
            InstallEvent::Unauthorized => {
                if self.state.get_untracked().is_none_or(|s| s.settled) {
                    return;
                }
                self.update(|s| {
                    s.settled = true;
                    s.failure = Some(NonDurableReason::ProbeFailed);
                    s.detail = Some(t!("durable.channelError"));
                    s.phase = InstallPhase::Failed;
                });
            }
        }
    }

    /// 装好之后已经起来的会话**不会**变持久——它们是普通 PTY 上的裸 shell，不归守护进程管。
    /// 不说清楚的话用户会以为问题解决了，下次断线才发现工作没保住。
    #[cfg(not(target_arch = "wasm32"))]
    fn on_installed(&self, store: &WorkspaceStore) {
        let Some(state) = self.state.get_untracked() else {
            return;
        };
        let live = store
            .sessions
            .get_untracked()
            .iter()
            .filter(|s| s.project_id == state.project_id && s.state != SessionState::Dead)
            .count();
        let host = state.host.clone();
        self.finish(store);
        if live > 0 {
            store.show_toast_sticky(format!(
                "{}\n{}",
                t!("durable.readyTitle", host = host),
                t!("durable.readyBody", n = live)
            ));
        } else {
            store.show_toast(t!("durable.readyTitle", host = host));
        }
    }
}

/// 部署只走原生 `FalconClient`；wasm 端这些方法永不可达（对话框开不出来），给空调用体。
#[cfg(not(target_arch = "wasm32"))]
impl InstallOperations {
    /// 命令面板「为某主机启用持久会话」：只装不建（`then_create = None`）。
    /// 查不到主机状态时只报 toast，不静默（GPUI 的 `handle_error` 路径）。
    pub fn open_for_project(&self, store: &WorkspaceStore, project_id: &str) {
        let Some(client) = store.client() else {
            return;
        };
        let Some(project) = store
            .projects
            .get_untracked()
            .into_iter()
            .find(|project| project.id == project_id)
        else {
            return;
        };
        let host = project
            .host_name
            .clone()
            .unwrap_or_else(|| project.name.clone());
        let connection_generation = store.connection_generation();
        let operations = self.clone();
        let store = store.clone();
        let pid = project_id.to_string();
        self.owner.run(move || {
            spawn(async move {
                let status = client.host_status(&pid).await;
                if store.connection_generation() != connection_generation {
                    return;
                }
                match status {
                    Ok(status) => operations.open(&store, status, pid, host, None),
                    Err(err) => store.show_toast(err.message),
                }
            });
        });
    }

    /// 「启用并上传」：授权写库成功后直接开始部署；写库失败就地显示错误留在询问页。
    pub fn allow(&self, store: &WorkspaceStore) {
        let Some(state) = self.state.get_untracked() else {
            return;
        };
        let Some(client) = store.client() else {
            self.update(|s| s.error = Some(t!("native.nsh.serverNotConnected")));
            return;
        };
        let patch = HostAuthorizationPatch {
            authorized: Some(true),
        };
        let operations = self.clone();
        let store = store.clone();
        self.owner.run(move || {
            spawn(async move {
                let result = client
                    .set_host_authorization(&state.project_id, &patch)
                    .await;
                // 对话框已被换掉 / 关闭：结果就丢
                if operations
                    .state
                    .get_untracked()
                    .map(|s| s.project_id)
                    .as_deref()
                    != Some(state.project_id.as_str())
                {
                    return;
                }
                match result {
                    Ok(_) => operations.start_install(&store),
                    Err(err) => {
                        store.show_toast(err.message.clone());
                        operations.update(|s| s.error = Some(err.message));
                    }
                }
            });
        });
    }

    /// 「不启用」：显式拒绝会写库；措辞里把可逆性说出来，反悔的入口在命令面板。
    /// 写完（或写失败）都收掉对话框并续建——拒绝后建的本来就是非持久会话。
    pub fn deny(&self, store: &WorkspaceStore) {
        let Some(state) = self.state.get_untracked() else {
            self.finish(store);
            return;
        };
        let Some(client) = store.client() else {
            self.finish(store);
            return;
        };
        let patch = HostAuthorizationPatch {
            authorized: Some(false),
        };
        let operations = self.clone();
        let store = store.clone();
        self.owner.run(move || {
            spawn(async move {
                let result = client
                    .set_host_authorization(&state.project_id, &patch)
                    .await;
                if operations
                    .state
                    .get_untracked()
                    .map(|s| s.project_id)
                    .as_deref()
                    != Some(state.project_id.as_str())
                {
                    return;
                }
                match result {
                    Ok(_) => store.show_toast_sticky(format!(
                        "{}\n{}",
                        t!("durable.skippedTitle"),
                        t!("durable.skippedBody")
                    )),
                    Err(err) => store.show_toast(err.message),
                }
                operations.finish(&store);
            });
        });
    }

    pub fn retry(&self, store: &WorkspaceStore) {
        self.start_install(store);
    }
}

#[cfg(target_arch = "wasm32")]
impl InstallOperations {
    pub fn open_for_project(&self, _store: &WorkspaceStore, _project_id: &str) {}
    pub fn allow(&self, _store: &WorkspaceStore) {}
    pub fn deny(&self, store: &WorkspaceStore) {
        self.finish(store);
    }
    pub fn retry(&self, _store: &WorkspaceStore) {}
    fn start_install(&self, _store: &WorkspaceStore) {}
}
