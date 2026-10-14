//! 会话核心。移植自 `packages/server/src/sessions/manager.ts`（含 ViewerArbiter 那一版）。
//!
//! 跑在会话引擎的 LocalSet 上（engine.rs）：状态是 `Rc` / `RefCell`，执行语义与 Node 的事件
//! 循环相同——同步段不会被打断，只在 await 处让出。TS 里的 `setTimeout` 对应 [`Timer`]
//! （spawn_local 一个 sleep），`clearTimeout` 对应 abort。
//!
//! RefCell 的纪律：借用只在一个同步小段里持有，绝不跨 await、绝不在借着的时候调别的方法
//! （别的方法可能再借同一个 RefCell）。Viewer / Backend 的调用都是往 channel 里塞东西，不会
//! 回调进来。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use falcon_proto::term_modes::TermModeTracker;
use falcon_proto::{
    DeadReason, NonDurableReason, OscColorHint, ProgramStatusRecord, ProgramStatusSummary, ServerMessage, Session, SessionAgent,
    SessionForeground, SessionState, TERM_FRAME_OUTPUT, TERM_FRAME_REPLAY, TermAppearance, summarize,
};
use futures::FutureExt as _;
use futures::future::Shared;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use super::auto_title::{AutoTitle, Probe};
use super::backend::{Backend, BackendCallbacks, BackendEvent};
use super::forward::ForwardManager;
use super::local::{
    LocalHost, LocalSessionState, attach_local, default_local_shell, local_kill, local_list_sessions, local_session_state,
    open_local_sessiond, spawn_failure_reason,
};
use super::relay::{LinkFor, WantReconnect};
use super::share::ShareManager;
use super::ssh::SshLink;
use super::ssh_host::{AttachOptions, HostFacts};
use super::term_size::{TermSize, ViewerAttach, ViewerAttachInput, decide_viewer_attach, fallback_term_size, parse_stored_term_size};
use super::viewer::Viewer;
use super::viewer_arbiter::ViewerArbiter;
use crate::askpass::hub::{AskpassHub, AskpassPrompt, uuid_v4};
use crate::askpass::install::write_local_askpass;
use crate::auth::now_ms;
use crate::crypto::SecretBox;
use crate::db::{Db, ProjectRow, SessionRow, SshHostRow};
use crate::exec::{Exec as _, LocalBoxFuture};
use crate::ringbuffer::RingBuffer;
use crate::shells::is_shell_command;
use crate::term_env::{OscColorGate, parse_hex_rgb};
use crate::host::HostKind;
use crate::sessiond::backend::ConnectError;
use crate::sessiond::deploy::{Deployed, InstallError, StageFn};
use crate::virtualdir::{
    container_manifest_files, posix_write_manifest_command, remove_local_virtual_dir, remove_virtual_dir_command,
    virtual_project_dir, windows_write_manifest_commands, write_local_manifest,
};

/// 终端数据帧：1 字节类型 + UTF-8 载荷，广播前只序列化一次
fn encode_term_frame(kind: u8, data: &str) -> Bytes {
    let mut frame = Vec::with_capacity(1 + data.len());
    frame.push(kind);
    frame.extend_from_slice(data.as_bytes());
    Bytes::from(frame)
}

/// 输出合并窗口。PTY / ssh channel 在高吞吐下（`yes`、构建日志）每秒触发上千次 onData，
/// 逐 chunk 一帧就是每秒上千次序列化 + syscall。窗口内攒起来一次发，把帧率封在 ~60/s，
/// 肉眼无感知延迟。
const OUTPUT_FLUSH_MS: u64 = 16;

/// 自动标题的探测节流窗口。持久会话是问一次宿主机上的守护进程（一来一回），非持久的本地
/// PTY 是一次系统调用；都不该跟着输出走。
///
/// 2.5s 是"命令跑起来到标题出现"的可感延迟上限，也压住了 `yes` 这种满屏输出的会话——
/// 窗口内无论来多少输出都只探一次。报了 OSC 7501 的程序不受这个延迟：它的状态一到，标题就
/// 能出来（auto_title.rs）。
const TITLE_PROBE_MS: u64 = 2_500;

const RECONNECT_MAX_DELAY_MS: u64 = 30_000;

/// 指数退避：1s 起翻倍，封顶 30s
fn backoff_ms(attempt: u32) -> u64 {
    RECONNECT_MAX_DELAY_MS.min(1000 * 2u64.pow(attempt.saturating_sub(1).min(10)))
}

/// `setTimeout` 的等价物。到点先把自己清掉（同 TS 里定时回调开头的 `timer = null`），再跑
/// 回调；清掉之后 [`Timer::clear`] 就 abort 不到在途的回调了——与 clearTimeout 只拦得住
/// 还没触发的定时器同一个语义。
#[derive(Default)]
pub(crate) struct Timer {
    handle: RefCell<Option<AbortHandle>>,
    generation: Cell<u64>,
}

impl Timer {
    fn set(self: &Rc<Self>, ms: u64, fut: impl Future<Output = ()> + 'static) {
        self.clear();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let me = Rc::downgrade(self);
        let task = tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            if let Some(t) = me.upgrade()
                && t.generation.get() == generation
            {
                t.handle.borrow_mut().take();
            }
            fut.await;
        });
        *self.handle.borrow_mut() = Some(task.abort_handle());
    }

    fn clear(&self) {
        if let Some(h) = self.handle.borrow_mut().take() {
            h.abort();
        }
    }

    fn is_set(&self) -> bool {
        self.handle.borrow().is_some()
    }
}

/// 某个项目的宿主机能否提供持久会话
#[derive(Debug, Clone, Default)]
pub struct DurableState {
    pub durable: bool,
    pub reason: Option<NonDurableReason>,
    /// 失败详情，供 UI 显示"到底卡在哪"，用户据此决定是修环境还是直接重试
    pub detail: Option<String>,
    /// 宿主机上已就位的会话守护进程（durable 时必有）
    pub deployed: Option<Deployed>,
}

/// 孤儿清理的宽限：建会话时 holder 先起来、会话行后落库，刚起来的 holder 在库里还查不到
const ORPHAN_GRACE_MS: i64 = 3600 * 1000;

/// 会话操作的失败。Clone 是因为接回是单飞的（多个等待者共享同一个结果）
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// 会话 / 项目不存在（TS 的 NotFoundError）
    #[error("{0}")]
    NotFound(String),
    /// 接回时宿主机上的会话已经不在了（TS 的 SessionGoneError）
    #[error("会话在宿主机上已经不存在")]
    Gone,
    /// 接回时发现会话在断线期间正常结束了
    #[error("会话已经结束")]
    Exited,
    #[error("{0}")]
    Failed(String),
}

impl From<anyhow::Error> for SessionError {
    fn from(e: anyhow::Error) -> Self {
        SessionError::Failed(e.to_string())
    }
}

impl From<ConnectError> for SessionError {
    fn from(e: ConnectError) -> Self {
        match e {
            ConnectError::Gone => SessionError::Gone,
            ConnectError::Exited => SessionError::Exited,
            other => SessionError::Failed(other.to_string()),
        }
    }
}

struct EntryState {
    /// 持久会话所用的守护进程，接回时要用
    deployed: Option<Deployed>,
    backend: Option<Rc<dyn Backend>>,
    /// 非持久会话的回放来源；持久会话的回放是守护进程给的快照，不用它
    buffer: RingBuffer,
    viewers: Vec<Rc<Viewer>>,
    /// 多个 Viewer 共用这一个 PTY：尺寸取最小、深浅跟最近操作的那端，见 viewer_arbiter.rs
    arbiter: ViewerArbiter<u64>,
    /// 合并窗口内攒下的输出（已过 OSC 网关、已进 RingBuffer），flush 时一次广播
    pending_out: String,
    /// 给 PTY 的尺寸（各 Viewer 的最小格子）。None = 从未被 Viewer 量过；接回时不能用 80×24 顶替
    size: Option<TermSize>,
    attaching: Option<Shared<LocalBoxFuture<'static, Result<(), SessionError>>>>,
    terminating: bool,
    /// 持有深浅的那个 Viewer 报的终端深浅；会话的 env 在创建时冻住了，只影响 OSC 答复和新会话
    appearance: Option<TermAppearance>,
    background: Option<String>,
    foreground: Option<String>,
    osc: OscColorGate,
    /// 输出流里的 VT 模式跟踪，replay 时重建（模式序列早被 RingBuffer 挤掉了）
    modes: TermModeTracker,
    /// 最近一次推出去的自动标题；None = 空闲在 shell 里或探不到
    title: Option<String>,
    /// 自动标题的素材（前台命令、程序状态里的身份、程序设的标题）与它们的新旧，见 auto_title.rs
    auto_title: AutoTitle,
    /// 探测在飞：Viewer 进出与输出可能同时点火，只许有一条在跑
    title_probing: bool,
    /// 守护进程会话：发了 Sync、快照还没回来。等回放的 Viewer 都搭这一班（设计文档 5.5）
    sync_in_flight: bool,
    /// 程序状态（OSC 7501）全集，守护进程推来的
    program_status: Vec<ProgramStatusRecord>,
    /// 守护进程报过 shell 退出（随后连接关闭）
    exit_seen: bool,
    /// SSH 上 bridge 的退出码（连接结束时给）
    bridge_exit: Option<Option<u32>>,
}

struct LiveEntry {
    session_id: String,
    project_id: String,
    durable: bool,
    s: RefCell<EntryState>,
    flush_timer: Rc<Timer>,
    /// 自动标题的探测节流窗口，见 schedule_title_probe
    title_timer: Rc<Timer>,
}

struct EntryInit {
    session_id: String,
    project_id: String,
    durable: bool,
    deployed: Option<Deployed>,
    size: Option<TermSize>,
    hint: OscColorHint,
}

impl LiveEntry {
    fn new(init: EntryInit) -> Rc<Self> {
        Rc::new(LiveEntry {
            session_id: init.session_id,
            project_id: init.project_id,
            durable: init.durable,
            s: RefCell::new(EntryState {
                deployed: init.deployed,
                backend: None,
                buffer: RingBuffer::new(),
                viewers: Vec::new(),
                arbiter: ViewerArbiter::new(),
                pending_out: String::new(),
                size: init.size,
                attaching: None,
                terminating: false,
                appearance: init.hint.appearance,
                background: init.hint.background,
                foreground: init.hint.foreground,
                osc: OscColorGate::new(),
                modes: TermModeTracker::new(),
                title: None,
                auto_title: AutoTitle::new(),
                title_probing: false,
                sync_in_flight: false,
                program_status: Vec::new(),
                exit_seen: false,
                bridge_exit: None,
            }),
            flush_timer: Rc::default(),
            title_timer: Rc::default(),
        })
    }

    fn color_hint(&self) -> OscColorHint {
        let s = self.s.borrow();
        OscColorHint { appearance: s.appearance, background: s.background.clone(), foreground: s.foreground.clone() }
    }

    fn viewers(&self) -> Vec<Rc<Viewer>> {
        self.s.borrow().viewers.clone()
    }

    fn viewer_count(&self) -> usize {
        self.s.borrow().viewers.len()
    }

    fn backend(&self) -> Option<Rc<dyn Backend>> {
        self.s.borrow().backend.clone()
    }

    fn deployed(&self) -> Option<Deployed> {
        self.s.borrow().deployed.clone()
    }

    fn size(&self) -> Option<TermSize> {
        self.s.borrow().size
    }

    fn terminating(&self) -> bool {
        self.s.borrow().terminating
    }

    /// 有 Viewer 还没报格子
    fn waiting_for_size(&self) -> bool {
        let s = self.s.borrow();
        !s.viewers.is_empty() && s.size.is_none()
    }
}

fn same_backend(a: &Rc<dyn Backend>, b: &Rc<dyn Backend>) -> bool {
    std::ptr::addr_eq(Rc::as_ptr(a), Rc::as_ptr(b))
}

#[derive(Default)]
struct ReconnectState {
    attempt: Cell<u32>,
    timer: Rc<Timer>,
}

/// 浏览远端目录时把已保存主机构成一条假 ProjectRow，好复用 SshLink。
pub fn host_as_project(host: &SshHostRow) -> ProjectRow {
    ProjectRow {
        id: format!("host:{}", host.id),
        name: host.name.clone(),
        project_type: "ssh".into(),
        working_dir: None,
        shell: None,
        ssh_host: Some(host.host.clone()),
        ssh_port: Some(host.port),
        ssh_username: Some(host.username.clone()),
        ssh_auth_method: Some(host.auth_method.clone()),
        ssh_key_path: host.key_path.clone(),
        ssh_secret_enc: host.secret_enc.clone(),
        host_id: Some(host.id.clone()),
        created_at: host.created_at,
        ..Default::default()
    }
}

pub struct SessionManager {
    db: Arc<Db>,
    secrets: Arc<SecretBox>,
    data_dir: PathBuf,
    pub askpass: Arc<AskpassHub>,
    /// 本机这台宿主（守护进程的准备结果、PTY 基底环境）
    pub local: LocalHost,
    links: RefCell<HashMap<String, Rc<SshLink>>>,
    /// 按 hostId 缓存的主机链路：「浏览远端目录」与中转（端口转发 / 远端公网发布）共用。
    /// 与项目链路（links）互不牵连——中转挂机器不挂项目（ADR 0016）。
    host_links: RefCell<HashMap<String, Rc<SshLink>>>,
    /// 主机链路的退避重连，只为还有启用中的中转而坚持；键是 hostId
    host_reconnects: RefCell<HashMap<String, Rc<ReconnectState>>>,
    entries: RefCell<HashMap<String, Rc<LiveEntry>>>,
    reconnects: RefCell<HashMap<String, Rc<ReconnectState>>>,
    /// 本地持久会话的自动接回退避，键是 sessionId
    local_reattach: RefCell<HashMap<String, Rc<ReconnectState>>>,
    /// 本进程内已确认写过的虚拟项目目录，供文件面板等只读用途复用；attach 路径不走缓存
    virtual_dirs: RefCell<HashMap<String, String>>,
    last_touch: RefCell<HashMap<String, i64>>,
    /// resize 落库的合并窗口：拖窗时前端逐帧发 resize，不能每次都同步 fsync
    size_flush: RefCell<HashSet<String>>,
    /// 已经扫过孤儿会话的宿主机（"local" 或 user@host:port）
    swept: RefCell<HashSet<String>>,
    /// 端口转发与公网发布（ADR 0016）：挂主机链路，不挂项目链路
    pub forwards: Rc<ForwardManager>,
    pub shares: Rc<ShareManager>,
    me: Weak<SessionManager>,
}

impl SessionManager {
    pub fn new(db: Arc<Db>, secrets: Arc<SecretBox>, data_dir: PathBuf, askpass: Arc<AskpassHub>) -> Rc<Self> {
        db.recover_sessions_on_startup();
        let mgr = Rc::new_cyclic(|me: &Weak<SessionManager>| {
            let link_for: LinkFor = {
                let me = me.clone();
                Rc::new(move |host_id: &str| {
                    let me = me.upgrade()?;
                    let host = me.db.get_host(host_id)?;
                    Some(me.get_host_link(&host))
                })
            };
            let want_reconnect: WantReconnect = {
                let me = me.clone();
                Rc::new(move |host_id: &str| {
                    if let Some(me) = me.upgrade() {
                        me.schedule_host_reconnect(host_id);
                    }
                })
            };
            SessionManager {
                forwards: ForwardManager::new(db.clone(), link_for.clone(), want_reconnect.clone()),
                shares: ShareManager::new(db.clone(), data_dir.clone(), link_for, want_reconnect),
                local: LocalHost::new(data_dir.clone()),
                db,
                secrets,
                data_dir,
                askpass,
                links: RefCell::default(),
                host_links: RefCell::default(),
                host_reconnects: RefCell::default(),
                entries: RefCell::default(),
                reconnects: RefCell::default(),
                local_reattach: RefCell::default(),
                virtual_dirs: RefCell::default(),
                last_touch: RefCell::default(),
                size_flush: RefCell::default(),
                swept: RefCell::default(),
                me: me.clone(),
            }
        });
        // cloudflared 拿的是 login 解析过的基底环境（与本地 PTY 同一份），不是服务进程自己的
        let weak = Rc::downgrade(&mgr);
        mgr.shares.set_base_env(Rc::new(move || {
            let weak = weak.clone();
            Box::pin(async move {
                match weak.upgrade() {
                    Some(me) => me.local.base_env().await,
                    None => Rc::new(Vec::new()),
                }
            })
        }));
        mgr
    }

    fn me(&self) -> Rc<Self> {
        self.me.upgrade().expect("SessionManager 还活着")
    }

    pub fn db(&self) -> &Arc<Db> {
        &self.db
    }

    pub fn secrets(&self) -> &Arc<SecretBox> {
        &self.secrets
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    fn entry(&self, session_id: &str) -> Option<Rc<LiveEntry>> {
        self.entries.borrow().get(session_id).cloned()
    }

    fn is_current(&self, entry: &Rc<LiveEntry>) -> bool {
        self.entry(&entry.session_id).is_some_and(|e| Rc::ptr_eq(&e, entry))
    }

    /// 后端刚起来时，上次 active 的持久会话都在 DB 里停成 unverified。
    /// 以前要等用户打开 tab 或点「接回」才验证—— falcon 一重启侧栏就一片黄。
    /// 这里按库存尺寸自动接回（跟总览里点接回同一条路，见 term_size.rs）。
    pub fn resume_unverified(&self) {
        for row in self.db.list_sessions() {
            if row.state != "unverified" || row.durable != 1 {
                continue;
            }
            let entry = self.hydrate_entry(&row);
            self.kick_auto_reattach(&entry);
        }
    }

    /// 判定某项目的宿主机能否提供持久会话，必要时把会话守护进程推上去（ADR 0020）。
    ///
    /// SSH 项目要求用户先授权——这会往用户的服务器上写入可执行文件，属于该问的那一类。
    /// 授权按主机记（host+port+username），不按项目：同一台机器上的第二个项目
    /// 不该再问一遍，二进制本来就已经在那里了。
    ///
    /// fresh：丢掉上一次的判定重新来过。用户显式发起部署/重试时必须带上——
    /// 失败判定是缓存的，不清掉的话点一百次重试都是同一个秒回的失败。
    pub async fn prepare(
        &self,
        project: &ProjectRow,
        on_stage: Option<StageFn<'_>>,
        cancel: Option<&CancellationToken>,
        fresh: bool,
    ) -> DurableState {
        if project.project_type == "local" {
            if fresh {
                self.local.reset();
            }
            let local = self.local.prepare().await;
            if local.durable {
                self.sweep_local_orphans_once();
            }
            return DurableState { durable: local.durable, reason: local.reason, detail: local.detail, deployed: local.deployed };
        }

        let saved = self.db.get_session_host(
            project.ssh_host.as_deref().unwrap_or_default(),
            project.ssh_port.unwrap_or(22) as u16,
            project.ssh_username.as_deref().unwrap_or_default(),
        );
        if saved.and_then(|s| s.authorized) != Some(1) {
            return DurableState { durable: false, reason: Some(NonDurableReason::NotAuthorized), ..Default::default() };
        }

        let link = self.get_link(project);
        if fresh {
            link.reset_sessiond();
        }
        let remote = link.prepare_sessiond(on_stage, cancel).await;
        if let Some(d) = remote.deployed.clone() {
            self.sweep_remote_orphans_once(&link, d);
        }
        DurableState { durable: remote.durable, reason: remote.reason, detail: remote.detail, deployed: remote.deployed }
    }

    /// 用户改了授权之后，清掉缓存的判定以便重试
    pub fn reset_prepare(&self, project_id: &str) {
        let link = self.links.borrow().get(project_id).cloned();
        if let Some(link) = link {
            link.reset_sessiond();
        }
    }

    /// 本地宿主的持久能力；None = 尚未探测（不主动触发部署）
    pub fn local_durable_state(&self) -> Option<DurableState> {
        let local = self.local.peek()?;
        Some(DurableState { durable: local.durable, reason: local.reason, detail: local.detail, deployed: local.deployed })
    }

    // ---------- 孤儿清理（设计文档 10.7） ----------
    //
    // 宿主机上守护进程起着、库里却没有这一行的会话：建会话的半路后端挂了、库被换掉了……
    // 只清 owner 是本实例的（同一台远端可能同时被两台机器上的 falcon 用），而且只清起来超过
    // 一小时的（建会话时 holder 先起来、行后落库，刚起来的在库里还查不到）。每台机器每次
    // 后端启动只扫一次。

    fn orphans(&self, entries: Vec<falcon_proto::sessiond::ListEntry>) -> Vec<String> {
        let me = self.db.instance_id();
        let now = now_ms();
        entries
            .into_iter()
            .filter(|e| e.alive && e.owner == me && now - e.created_at > ORPHAN_GRACE_MS)
            .filter(|e| self.db.get_session(&e.session_id).is_none())
            .map(|e| e.sid)
            .collect()
    }

    fn sweep_local_orphans_once(&self) {
        if self.swept.borrow_mut().insert("local".into()) {
            let me = self.me();
            tokio::task::spawn_local(async move {
                let Some(d) = me.local.peek().and_then(|l| l.deployed) else { return };
                let Some(list) = local_list_sessions(&d).await else { return };
                let orphans = me.orphans(list);
                if !orphans.is_empty() {
                    log::info!("清理本机的孤儿会话：{orphans:?}");
                    local_kill(&d, &orphans).await;
                }
            });
        }
    }

    fn sweep_remote_orphans_once(&self, link: &Rc<SshLink>, deployed: Deployed) {
        let p = link.project();
        let key = format!("{}@{}:{}", p.ssh_username.unwrap_or_default(), p.ssh_host.unwrap_or_default(), p.ssh_port.unwrap_or(22));
        if self.swept.borrow_mut().insert(key.clone()) {
            let me = self.me();
            let link = link.clone();
            tokio::task::spawn_local(async move {
                let Some(list) = link.list_sessions(&deployed).await else { return };
                let orphans = me.orphans(list);
                if !orphans.is_empty() {
                    log::info!("清理 {key} 上的孤儿会话：{orphans:?}");
                    link.kill_sessions(&deployed, &orphans).await;
                }
            });
        }
    }

    // ---------- 项目链路 ----------

    pub fn get_link(&self, project: &ProjectRow) -> Rc<SshLink> {
        let existing = self.links.borrow().get(&project.id).cloned();
        if let Some(link) = existing {
            link.update_project(project.clone());
            return link;
        }
        let link = SshLink::new(project.clone(), self.db.clone(), self.secrets.clone());
        let me = self.me.clone();
        let project_id = project.id.clone();
        link.on_down(move || {
            if let Some(me) = me.upgrade() {
                me.handle_link_down(&project_id);
            }
        });
        self.links.borrow_mut().insert(project.id.clone(), link.clone());
        link
    }

    /// 已经建过的项目链路（不新建）
    pub fn existing_link(&self, project_id: &str) -> Option<Rc<SshLink>> {
        self.links.borrow().get(project_id).cloned()
    }

    pub fn dispose_link(&self, project_id: &str) {
        let link = self.links.borrow_mut().remove(project_id);
        if let Some(link) = link {
            link.dispose();
        }
        let rec = self.reconnects.borrow_mut().remove(project_id);
        if let Some(rec) = rec {
            rec.timer.clear();
        }
    }

    /// 已保存主机的链路：浏览远端目录与中转共用一条。改凭据或删主机时必须 dispose，
    /// 否则会拿着旧密钥连。
    ///
    /// 断线只为中转重连（schedule_host_reconnect）：浏览不是会话，断了下次点再连；
    /// 连上（"up"）时把该主机启用中的转发 / 发布拉起来。
    pub fn get_host_link(&self, host: &SshHostRow) -> Rc<SshLink> {
        let existing = self.host_links.borrow().get(&host.id).cloned();
        if let Some(link) = existing {
            link.update_project(host_as_project(host));
            return link;
        }
        let created = SshLink::new(host_as_project(host), self.db.clone(), self.secrets.clone());
        // 只认还登记在册的那条：dispose 不会打断在途的 connect，被换下的旧链路
        // 之后照样可能 "up" / "down"，不拦的话会拆掉新链路上的隧道
        let current = {
            let me = self.me.clone();
            let weak = Rc::downgrade(&created);
            let host_id = host.id.clone();
            move || -> Option<Rc<SessionManager>> {
                let me = me.upgrade()?;
                let mine = weak.upgrade()?;
                let registered = me.host_links.borrow().get(&host_id).is_some_and(|l| Rc::ptr_eq(l, &mine));
                registered.then_some(me)
            }
        };
        {
            let current = current.clone();
            let host_id = host.id.clone();
            created.on_down(move || {
                let Some(me) = current() else { return };
                me.forwards.on_link_down(&host_id);
                me.shares.on_link_down(&host_id);
                me.schedule_host_reconnect(&host_id);
            });
        }
        {
            let weak = Rc::downgrade(&created);
            let host_id = host.id.clone();
            created.on_up(move || {
                let Some(me) = current() else {
                    if let Some(link) = weak.upgrade() {
                        link.dispose();
                    }
                    return;
                };
                me.forwards.on_link_up(&host_id);
                me.shares.on_link_up(&host_id);
            });
        }
        self.host_links.borrow_mut().insert(host.id.clone(), created.clone());
        created
    }

    /// 丢掉主机链路（凭据改了 / 主机删了）。先停中转再 dispose——dispose 不发 "down"，
    /// 不停的话本机监听器还开着、指向一条已经没了的链路。
    /// 删主机时 keep_relays=false，规则由调用方随后删库；改凭据时拿新链路把启用中的
    /// 中转重新拉起来。
    pub async fn dispose_host_link(&self, host_id: &str, keep_relays: bool) {
        let rec = self.host_reconnects.borrow_mut().remove(host_id);
        if let Some(rec) = rec {
            rec.timer.clear();
        }
        futures::future::join(self.forwards.forget_host(host_id), self.shares.forget_host(host_id)).await;
        let link = self.host_links.borrow_mut().remove(host_id);
        if let Some(link) = link {
            link.dispose();
        }
        if keep_relays {
            let me = self.me();
            let host_id = host_id.to_string();
            tokio::task::spawn_local(async move { me.connect_host(&host_id).await });
        }
    }

    /// 后端启动时把启用中的中转拉起来：本机的公网发布直接起，挂主机的等链路连上
    pub fn restore_relays(&self) {
        self.shares.restore_local();
        let mut host_ids: Vec<String> = Vec::new();
        for id in self.forwards.enabled_host_ids().into_iter().chain(self.shares.enabled_host_ids()) {
            if !host_ids.contains(&id) {
                host_ids.push(id);
            }
        }
        for host_id in host_ids {
            let me = self.me();
            tokio::task::spawn_local(async move { me.connect_host(&host_id).await });
        }
    }

    fn wants_host_link(&self, host_id: &str) -> bool {
        self.forwards.has_enabled(host_id) || self.shares.has_enabled(host_id)
    }

    fn mark_host_unreachable(&self, host_id: &str, message: &str) {
        self.forwards.mark_unreachable(host_id, message);
        self.shares.mark_unreachable(host_id, message);
    }

    /// 连一次主机链路。连上由 "up" 拉起中转；连不上把原因挂到规则上并退避重连
    async fn connect_host(&self, host_id: &str) {
        if !self.wants_host_link(host_id) {
            return;
        }
        let Some(host) = self.db.get_host(host_id) else { return };
        if let Err(e) = self.get_host_link(&host).get_client().await {
            self.mark_host_unreachable(host_id, &e.message);
            self.schedule_host_reconnect(host_id);
        }
    }

    /// 主机链路的指数退避重连，与项目的 schedule_reconnect 同一套节奏。
    /// 只要还有启用中的中转就坚持；全停了、主机删了就收手。
    pub fn schedule_host_reconnect(&self, host_id: &str) {
        // 登记在册就说明有一轮活着（定时器在等，或 tick 正在连）：每条出口要么删登记、
        // 要么重排定时器。不能像项目那样只看 timer——tick 在途时再排一个会跑出两轮
        if self.host_reconnects.borrow().contains_key(host_id) {
            return;
        }
        let state = Rc::new(ReconnectState::default());
        self.host_reconnects.borrow_mut().insert(host_id.to_string(), state.clone());
        self.host_reconnect_after(host_id.to_string(), state, 1000);
    }

    fn host_reconnect_after(&self, host_id: String, state: Rc<ReconnectState>, ms: u64) {
        let me = self.me();
        let st = state.clone();
        state.timer.set(ms, async move { me.host_reconnect_tick(host_id, st).await });
    }

    async fn host_reconnect_tick(&self, host_id: String, state: Rc<ReconnectState>) {
        // 每一步都先确认自己还是登记在册的那一轮：dispose_host_link 会清掉旧的一轮、
        // 随后可能另起一轮，旧一轮在途的 tick 不能把新一轮的登记删掉
        let mine = || self.host_reconnects.borrow().get(&host_id).is_some_and(|s| Rc::ptr_eq(s, &state));
        if !mine() {
            return;
        }
        let host = self.db.get_host(&host_id);
        let Some(host) = host.filter(|_| self.wants_host_link(&host_id)) else {
            self.host_reconnects.borrow_mut().remove(&host_id);
            return;
        };
        state.attempt.set(state.attempt.get() + 1);
        match self.get_host_link(&host).get_client().await {
            Ok(_) => {
                if mine() {
                    self.host_reconnects.borrow_mut().remove(&host_id);
                }
                return;
            }
            Err(e) => self.mark_host_unreachable(&host_id, &e.message),
        }
        if !mine() {
            return;
        }
        let delay = backoff_ms(state.attempt.get());
        self.host_reconnect_after(host_id, state, delay);
    }

    /// 试连一组 SSH 凭据。用一次性链路，测完就拆——
    /// 表单里可能是还没保存的草稿，不能写进 host_links 污染浏览缓存。
    pub async fn probe_ssh(&self, project: &ProjectRow) -> Result<HostFacts, InstallError> {
        let link = SshLink::new(project.clone(), self.db.clone(), self.secrets.clone());
        let res = link.host_facts().await;
        link.dispose();
        res
    }

    // ---------- 虚拟项目目录 ----------
    // 多仓库容器没有自己的聚合目录，falcon 在数据根下按 projectId 生成一个
    // （清单内容与取舍见 virtualdir.rs），working_dir 留空时它就是会话 cwd。

    fn local_virtual_dir(&self, project_id: &str) -> PathBuf {
        let root = std::path::absolute(&self.data_dir).unwrap_or_else(|_| self.data_dir.clone());
        root.join("projects").join(project_id)
    }

    /// 生成/刷新容器的虚拟项目目录，返回其绝对路径。
    /// 内容是确定性模板的覆盖写，天然幂等、可重入。refresh 跳过缓存强制重写——
    /// attach 路径必须带上：成员或项目名编辑后新会话要拿到新清单，
    /// 用户误删目录后接回也靠它自愈（否则 --default-cwd 会指向不存在的目录）。
    pub async fn ensure_virtual_dir(&self, project: &ProjectRow, refresh: bool) -> anyhow::Result<String> {
        if !refresh && let Some(hit) = self.virtual_dirs.borrow().get(&project.id).cloned() {
            return Ok(hit);
        }
        let members: Vec<String> =
            Db::parse_multi_repos(project.multi_repos.as_deref()).unwrap_or_default().into_iter().map(|m| m.dir).collect();
        let files = container_manifest_files(&project.name, &members);
        let dir = if project.project_type == "local" {
            let dir = self.local_virtual_dir(&project.id);
            write_local_manifest(&dir, &files)?;
            dir.to_string_lossy().into_owned()
        } else {
            let link = self.get_link(project);
            let facts = link.host_facts().await?;
            let dir = virtual_project_dir(facts.kind, &facts.root, &project.id);
            if facts.kind == HostKind::Windows {
                for w in windows_write_manifest_commands(&dir, &files) {
                    let res = link.exec_with_input(&w.cmd, w.stdin_base64.as_bytes()).await?;
                    if !res.ok() {
                        anyhow::bail!("{}", fail_text(&res.stderr, "虚拟项目目录写入失败", res.code));
                    }
                }
            } else {
                let res = link.exec(&posix_write_manifest_command(&dir, &files), None).await?;
                if !res.ok() {
                    anyhow::bail!("{}", fail_text(&res.stderr, "虚拟项目目录写入失败", res.code));
                }
            }
            dir
        };
        self.virtual_dirs.borrow_mut().insert(project.id.clone(), dir.clone());
        Ok(dir)
    }

    pub fn invalidate_virtual_dir(&self, project_id: &str) {
        self.virtual_dirs.borrow_mut().remove(project_id);
    }

    /// 删容器时清理虚拟目录。克制：只删 falcon 写的固定文件 + 非递归 rmdir——
    /// 它是会话 cwd，agent/用户可能落了别的文件，非空整目录保留。
    /// 返回 warning 文本或 None；链路故障往上抛，调用方决定要不要打扰用户。
    pub async fn remove_virtual_dir(&self, project: &ProjectRow) -> anyhow::Result<Option<String>> {
        self.virtual_dirs.borrow_mut().remove(&project.id);
        let (dir, left) = if project.project_type == "local" {
            let dir = self.local_virtual_dir(&project.id);
            let left = !remove_local_virtual_dir(&dir);
            (dir.to_string_lossy().into_owned(), left)
        } else {
            let link = self.get_link(project);
            let facts = link.host_facts().await?;
            let dir = virtual_project_dir(facts.kind, &facts.root, &project.id);
            let res = link.exec(&remove_virtual_dir_command(facts.kind, &dir), None).await?;
            if !res.ok() {
                anyhow::bail!("{}", fail_text(&res.stderr, "虚拟项目目录清理失败", res.code));
            }
            let left = crate::term_env::js::trim(&res.stdout) == "left";
            (dir, left)
        };
        Ok(left.then(|| format!("虚拟项目目录未删除（内有其他文件）：{dir}")))
    }

    // ---------- 会话生命周期 ----------

    pub async fn create_session(
        &self,
        project: &ProjectRow,
        name: String,
        hint: OscColorHint,
        agent: Option<SessionAgent>,
    ) -> Result<Session, SessionError> {
        let id = uuid_v4();
        let now = now_ms();

        let prep = self.prepare(project, None, None, false).await;

        let mut row = SessionRow {
            id: id.clone(),
            project_id: project.id.clone(),
            name,
            state: "active".into(),
            durable: i64::from(prep.durable),
            dead_reason: None,
            non_durable_reason: if prep.durable { None } else { prep.reason.map(|r| r.as_str().to_string()) },
            created_at: now,
            last_active_at: now,
            cols: None,
            rows: None,
            agent: agent.map(|a| a.as_str().to_string()),
        };

        let entry = LiveEntry::new(EntryInit {
            session_id: id.clone(),
            project_id: project.id.clone(),
            durable: prep.durable,
            deployed: prep.deployed.clone(),
            size: None,
            hint: hint.clone(),
        });
        self.entries.borrow_mut().insert(id.clone(), entry.clone());

        match self.attach_backend(&entry, project, &row, false).await {
            Ok(()) => {}
            Err(e) if prep.durable => {
                // 守护进程拉不起来（Windows 的 Job 不让脱离、WMI 也被禁了……）：按 ADR 0001 退成
                // 非持久并写明原因，会话照样开得出来，而不是让用户看到一个建不了的会话
                let reason =
                    if project.project_type == "local" { spawn_failure_reason().await } else { NonDurableReason::VerifyFailed };
                log::warn!("会话 {id} 起不来守护进程（{e}），退成非持久：{reason}");
                row.durable = 0;
                row.non_durable_reason = Some(reason.as_str().to_string());
                let entry = LiveEntry::new(EntryInit {
                    session_id: id.clone(),
                    project_id: project.id.clone(),
                    durable: false,
                    deployed: None,
                    size: None,
                    hint,
                });
                self.entries.borrow_mut().insert(id.clone(), entry.clone());
                if let Err(e) = self.attach_backend(&entry, project, &row, false).await {
                    self.entries.borrow_mut().remove(&id);
                    return Err(e);
                }
            }
            Err(e) => {
                self.entries.borrow_mut().remove(&id);
                return Err(e);
            }
        }

        self.db.insert_session(&row);
        Ok(Db::to_session(&row))
    }

    /// 这个会话该用什么 shell 开场。
    ///
    /// 普通会话就是项目配的 shell（或让下游按平台挑默认）；agent 会话则换成写在宿主机上
    /// 的启动脚本（sessions/agent.rs）。脚本没写成时**退回普通 shell**：把一个不存在的
    /// 路径当 shell 交给守护进程，会话根本起不来，用户只会看到一片空白。
    ///
    /// 每次附着都重写一遍脚本（本地一次 fs 写、远端一次往返）：用户删了它、换了登录
    /// shell、falcon 升级换了脚本内容，都能自愈。
    async fn session_shell(&self, project: &ProjectRow, row: &SessionRow) -> Option<String> {
        let shell = project.shell.clone();
        let Some(agent) = row.agent.as_deref().and_then(SessionAgent::from_wire).filter(|a| *a != SessionAgent::Unknown)
        else {
            return shell;
        };
        if project.project_type == "local" {
            let login = shell.clone().unwrap_or_else(default_local_shell);
            return match super::agent::write_local_launcher(&self.data_dir, agent, &login) {
                Ok(p) => Some(p.to_string_lossy().into_owned()),
                Err(_) => shell,
            };
        }
        self.get_link(project).ensure_agent_launcher(agent, shell.as_deref()).await.or(shell)
    }

    async fn attach_backend(
        &self,
        entry: &Rc<LiveEntry>,
        project: &ProjectRow,
        row: &SessionRow,
        reattach: bool,
    ) -> Result<(), SessionError> {
        // 非持久会话：附着返回之前就可能有 OSC 查询要答（OscColorGate），先攒着，backend 到手再写
        let slot: Rc<RefCell<(Option<Rc<dyn Backend>>, Vec<String>)>> =
            Rc::new(RefCell::new((entry.backend(), Vec::new())));
        let cb = {
            let me = self.me.clone();
            let weak = Rc::downgrade(entry);
            let slot_data = slot.clone();
            let slot_exit = slot.clone();
            let me_exit = self.me.clone();
            let me_event = self.me.clone();
            let weak_event = Rc::downgrade(entry);
            let session_id = entry.session_id.clone();
            BackendCallbacks {
                on_data: Rc::new(move |data: String| {
                    let (Some(me), Some(entry)) = (me.upgrade(), weak.upgrade()) else { return };
                    me.on_backend_data(&entry, &slot_data, &data);
                }),
                on_exit: Rc::new(move || {
                    let Some(me) = me_exit.upgrade() else { return };
                    let mine = slot_exit.borrow().0.clone();
                    let session_id = session_id.clone();
                    tokio::task::spawn_local(async move { me.handle_backend_exit(&session_id, mine).await });
                }),
                on_event: Rc::new(move |ev: BackendEvent| {
                    let (Some(me), Some(entry)) = (me_event.upgrade(), weak_event.upgrade()) else { return };
                    me.on_backend_event(&entry, ev);
                }),
            }
        };

        // 接回时 entry 可能还没有守护进程（后端重启后重建的 entry），重新准备一次
        if entry.durable && entry.deployed().is_none() {
            let prep = self.prepare(project, None, None, false).await;
            match (prep.durable, prep.deployed, prep.reason) {
                (true, Some(d), _) => entry.s.borrow_mut().deployed = Some(d),
                // 链路一类的故障：会话多半还在，下次再试
                (_, _, None | Some(NonDurableReason::ProbeFailed | NonDurableReason::UploadFailed | NonDurableReason::Cancelled))
                | (_, _, Some(NonDurableReason::Unknown)) => {
                    return Err(SessionError::Failed(prep.detail.unwrap_or_else(|| "宿主机上的守护进程没就位".into())));
                }
                // 稳定的环境事实（没有这个架构的构建、没授权……）：这样的宿主机上不可能有守护进程会话
                _ => return Err(SessionError::Gone),
            }
        }

        // 容器（multi 且非派生）working_dir 留空时把会话开进虚拟项目目录。
        // 先写后指：materialize 成功才拿到路径，失败回退 None（现状行为）——
        // 创建与接回（ensure_attached 同走本函数）都不能被它挡住。
        let mut cwd = project.working_dir.clone();
        if cwd.is_none() && project.multi_repos.is_some() && project.source_project_id.is_none() {
            cwd = self.ensure_virtual_dir(project, true).await.ok();
        }

        // 包装没写上不挡开会话；agent 的 sudo 仍会报没 tty
        let askpass_bin = if project.project_type == "local" {
            write_local_askpass(&self.data_dir, &self.askpass).ok().map(|p| p.to_string_lossy().into_owned())
        } else {
            self.get_link(project).ensure_askpass(&self.askpass).await
        };

        let size = fallback_term_size(entry.size());
        let hint = entry.color_hint();
        let opts = AttachOptions {
            session_id: entry.session_id.clone(),
            cwd,
            // 接回时用不上（会话早就起着了），省一次远端往返
            shell: if reattach { None } else { self.session_shell(project, row).await },
            durable: entry.durable,
            deployed: entry.deployed(),
            reattach,
            cols: size.cols,
            rows: size.rows,
            appearance: hint.appearance,
            foreground: hint.foreground.clone(),
            background: hint.background.clone(),
            askpass_bin,
            owner: self.db.instance_id(),
        };

        let backend: Rc<dyn Backend> = if entry.durable {
            let (backend, welcome) = if project.project_type == "local" {
                open_local_sessiond(&self.local, &opts, cb).await?
            } else {
                self.get_link(project).open_sessiond(&opts, cb).await?
            };
            {
                let mut s = entry.s.borrow_mut();
                s.exit_seen = false;
                s.bridge_exit = None;
                s.sync_in_flight = false;
            }
            // 宿主机上的 VT 进程不是这份二进制里的 ghostty：热升级（设计文档 2.3），升级 ghostty
            // 不必等存量会话自然消亡。holder 会用二进制快照交接，解不了就用原始尾巴重建
            if let Some(d) = entry.deployed()
                && !d.ghostty.is_empty()
                && !welcome.vt_version.is_empty()
                && welcome.vt_version != d.ghostty
            {
                log::info!("会话 {} 的 VT 进程是 ghostty {}，换成 {}", entry.session_id, welcome.vt_version, d.ghostty);
                backend.upgrade_vt(&d.bin);
            }
            backend.set_appearance(&hint);
            backend
        } else {
            let result = if project.project_type == "local" {
                attach_local(&self.local, &opts, cb).await?
            } else {
                self.get_link(project).attach_session(&opts, cb).await?
            };
            result.backend
        };

        let queued = {
            let mut slot = slot.borrow_mut();
            slot.0 = Some(backend.clone());
            std::mem::take(&mut slot.1)
        };
        entry.s.borrow_mut().backend = Some(backend.clone());
        for reply in queued {
            backend.write(&reply);
        }
        // attach 期间 Viewer 可能已经报了真实格子，按最新的再 resize 一次
        if let Some(size) = entry.size() {
            backend.resize(size.cols, size.rows);
        }
        Ok(())
    }

    fn on_backend_data(&self, entry: &Rc<LiveEntry>, slot: &RefCell<(Option<Rc<dyn Backend>>, Vec<String>)>, data: &str) {
        if entry.durable {
            // 守护进程会话：终端查询由宿主机的 VT 进程回答，回放用它的快照、2031 由 holder 发，
            // 这里只管转发
            self.queue_output(entry, data.to_string());
            return;
        }
        let hint = entry.color_hint();
        let out = entry.s.borrow_mut().osc.push(&hint, data);
        for reply in out.replies {
            let backend = slot.borrow().0.clone();
            match backend {
                Some(b) => b.write(&reply),
                None => slot.borrow_mut().1.push(reply),
            }
        }
        if out.visible.is_empty() {
            return;
        }
        {
            let mut s = entry.s.borrow_mut();
            s.modes.track(out.visible.as_bytes());
            s.buffer.append(out.visible.clone());
        }
        self.queue_output(entry, out.visible);
    }

    /// 守护进程会话的事件。按连接上到达的顺序在 LocalSet 上同步调用
    fn on_backend_event(&self, entry: &Rc<LiveEntry>, ev: BackendEvent) {
        match ev {
            BackendEvent::Synced(text) => {
                // 快照之前到的输出都已含在快照里：先发给已经在看的 Viewer（等回放的那些
                // flush 时会跳过），再把快照给等回放的，此后它们照常收输出（设计文档 5.5）
                self.flush_output(entry);
                entry.s.borrow_mut().sync_in_flight = false;
                let frame = encode_term_frame(TERM_FRAME_REPLAY, &text);
                for v in entry.viewers() {
                    if v.syncing.get() {
                        v.syncing.set(false);
                        let ok = v.send_bytes(frame.clone());
                        v.lagged.set(!ok);
                    }
                }
            }
            BackendEvent::ProgramStatus(records) => {
                {
                    let mut s = entry.s.borrow_mut();
                    let old = std::mem::replace(&mut s.program_status, records.clone());
                    s.auto_title.on_status(&old, &records);
                }
                self.broadcast(entry, &ServerMessage::ProgramStatus { records });
                // 程序报了状态多半是前台换了人（或刚起来）：标题先按它出，再排一次探测认前台
                self.publish_title(entry);
                self.schedule_title_probe(entry);
            }
            BackendEvent::Title(title) => {
                entry.s.borrow_mut().auto_title.on_program_title(&title);
                self.publish_title(entry);
                self.schedule_title_probe(entry);
            }
            BackendEvent::Exit(_) => entry.s.borrow_mut().exit_seen = true,
            BackendEvent::BridgeExit(code) => entry.s.borrow_mut().bridge_exit = Some(code),
            BackendEvent::VtRestarted => log::warn!("会话 {} 的 VT 进程挂过，已用原始尾巴重建", entry.session_id),
            BackendEvent::VtDegraded => {
                log::warn!("会话 {} 的 VT 进程反复崩溃，宿主机上只剩纯转发", entry.session_id)
            }
        }
    }

    /// 守护进程会话：要一份快照。已经有一班在路上就搭它（回来时所有等着的 Viewer 一起拿）
    fn request_sync(&self, entry: &LiveEntry) {
        if entry.s.borrow().sync_in_flight {
            return;
        }
        if let Some(b) = entry.backend()
            && b.request_sync()
        {
            entry.s.borrow_mut().sync_in_flight = true;
        }
    }

    /// 确保会话有活的 backend。
    /// unverified 的持久会话在此被验证并恢复为 active；宿主机上的会话不在了 / 已经结束则标记 dead。
    ///
    /// force：用户点了「接回」，或后台自动接回时没有 Viewer 在量格子。
    /// 必须马上 attach 并把 DB 写成 active，不能卡在 unverified。
    pub async fn ensure_attached(&self, session_id: &str, force: bool) -> Result<SessionRow, SessionError> {
        let row = self.db.get_session(session_id).ok_or_else(|| SessionError::NotFound("会话不存在".into()))?;
        if row.state == "dead" {
            return Ok(row);
        }

        let entry = self.hydrate_entry(&row);
        if entry.backend().is_some() {
            // PTY 已挂上但 DB 还停在 unverified：点接回必须把状态扳回来
            if row.state != "active" {
                self.mark_active(&entry, false);
                return self.db.get_session(session_id).ok_or_else(|| SessionError::NotFound("会话不存在".into()));
            }
            return Ok(row);
        }

        if !entry.durable {
            // 非持久会话丢了 backend 即死亡（理论上已在别处标记）
            self.mark_dead(&entry, DeadReason::BackendRestart);
            return self.db.get_session(session_id).ok_or_else(|| SessionError::NotFound("会话不存在".into()));
        }

        // 有 Viewer 在量格子时等它报尺寸，避免用 80×24 抢跑把 TUI 挤扁。
        // 后台自动接回 / 用户点接回走 force，不能卡在 unverified。
        if !force && entry.waiting_for_size() {
            return Ok(row);
        }

        let pending = entry.s.borrow().attaching.clone();
        let attaching = match pending {
            Some(f) => f,
            None => {
                // 单独起一个任务跑：等待者都走了它也得跑完，不然 attaching 永远挂着
                let (tx, rx) = tokio::sync::oneshot::channel();
                let me = self.me();
                let e = entry.clone();
                tokio::task::spawn_local(async move {
                    let res = async {
                        let project = me
                            .db
                            .get_project(&row.project_id)
                            .ok_or_else(|| SessionError::NotFound("项目不存在".into()))?;
                        match me.attach_backend(&e, &project, &row, true).await {
                            Ok(()) => {
                                me.mark_active(&e, true);
                                Ok(())
                            }
                            Err(err) => {
                                match err {
                                    SessionError::Gone => me.mark_dead(&e, DeadReason::SessionGone),
                                    SessionError::Exited => me.mark_dead(&e, DeadReason::Exited),
                                    _ => {}
                                }
                                Err(err)
                            }
                        }
                    }
                    .await;
                    e.s.borrow_mut().attaching = None;
                    let _ = tx.send(res);
                });
                let fut: LocalBoxFuture<'static, Result<(), SessionError>> =
                    Box::pin(async move { rx.await.unwrap_or_else(|_| Err(SessionError::Failed("接回被中断".into()))) });
                let shared = fut.shared();
                // 任务可能已经同步跑完了？不会：spawn_local 要等下一次让出才开始跑
                entry.s.borrow_mut().attaching = Some(shared.clone());
                shared
            }
        };
        attaching.await?;
        self.db.get_session(session_id).ok_or_else(|| SessionError::NotFound("会话不存在".into()))
    }

    /// 会话前台是否有程序在跑（关 tab 前的确认依据）。
    ///
    /// 持久会话问宿主机上的守护进程（它直接 tcgetpgrp，Windows 上沿进程树找）；非持久本地
    /// 会话问 PTY 自己。侦测不到的场景一律按空闲放行：非持久 SSH（channel 里问不到远端 shell
    /// 的进程树）、unverified/断链（拿到的答案没有意义）。这是道保险，探测失败不能把关 tab
    /// 拦下来，所以也不报错。
    pub async fn foreground(&self, session_id: &str) -> SessionForeground {
        match self.probe_foreground(session_id).await {
            Probe::Busy(c) => SessionForeground { busy: true, command: Some(c) },
            Probe::Shell | Probe::Unknown => SessionForeground { busy: false, command: None },
        }
    }

    /// [`SessionManager::foreground`] 的细分版：自动标题要分清"在 shell 里"和"问不到"——前者让
    /// 旧的标题素材作废，后者什么都不该改（auto_title.rs）
    async fn probe_foreground(&self, session_id: &str) -> Probe {
        let Some(row) = self.db.get_session(session_id).filter(|r| r.state == "active") else { return Probe::Unknown };
        let Some(project) = self.db.get_project(&row.project_id) else { return Probe::Unknown };
        let backend = self.entry(session_id).and_then(|e| e.backend());
        let command = match backend {
            Some(b) if row.durable == 1 => match b.foreground() {
                Some(f) => f.await,
                None => None,
            },
            Some(b) => b.process_name(),
            None => None,
        };
        match command {
            Some(c) if is_shell_command(&c, project.shell.as_deref()) => Probe::Shell,
            Some(c) => Probe::Busy(c),
            None => Probe::Unknown,
        }
    }

    // ---------- 自动标题 ----------

    /// 会话此刻的自动标题（程序设的标题 > 程序身份 > 前台命令，auto_title.rs）。没有 live entry
    /// 的会话没有标题，不为它现探。
    pub fn title_of(&self, session_id: &str) -> Option<String> {
        self.entry(session_id).and_then(|e| e.s.borrow().title.clone())
    }

    /// 安排一次前台命令探测。
    ///
    /// 没人看着的会话也探：侧栏上的后台会话要看得出此刻在跑什么。早先只在有 Viewer 时探，因为
    /// 那时每次探测是宿主机上一条实打实的命令（SSH 上还要开一条 channel）；现在持久会话是守护
    /// 进程连接上一来一回的小帧，非持久的本地 PTY 是一次系统调用，非持久 SSH 会话根本不探
    /// （`process_name` 恒为 None）。仍然不定时轮询：点火的只有输出、程序状态与程序标题这些
    /// "前台可能换了人"的信号，安静的会话不花一分钱。
    ///
    /// 节流而非 debounce：窗口内重复点火直接忽略，所以满屏输出的会话也按固定间隔
    /// 出标题，不必等它安静下来。
    fn schedule_title_probe(&self, entry: &Rc<LiveEntry>) {
        if entry.title_timer.is_set() {
            return;
        }
        let me = self.me();
        let e = entry.clone();
        entry.title_timer.set(TITLE_PROBE_MS, async move { me.probe_title(&e).await });
    }

    fn spawn_probe_title(&self, entry: &Rc<LiveEntry>) {
        let me = self.me();
        let e = entry.clone();
        tokio::task::spawn_local(async move { me.probe_title(&e).await });
    }

    /// 探一次并广播变化。probe_foreground() 自己吞掉所有错误，这里不会失败。
    async fn probe_title(&self, entry: &Rc<LiveEntry>) {
        let started = {
            let mut s = entry.s.borrow_mut();
            if s.title_probing {
                return;
            }
            s.title_probing = true;
            s.auto_title.probe_started()
        };
        let probe = self.probe_foreground(&entry.session_id).await;
        entry.s.borrow_mut().title_probing = false;
        // 探测期间会话可能已被终止，entry 也可能被重建过
        if !self.is_current(entry) {
            return;
        }
        entry.s.borrow_mut().auto_title.on_probe(started, probe);
        self.publish_title(entry);
    }

    /// 自动标题变了就广播
    fn publish_title(&self, entry: &LiveEntry) {
        let next = entry.s.borrow().auto_title.title();
        if next == entry.s.borrow().title {
            return;
        }
        entry.s.borrow_mut().title = next.clone();
        self.broadcast(entry, &ServerMessage::Title { title: next });
    }

    // ---------- 程序状态（OSC 7501，ADR 0020 决定六） ----------

    /// 用户看过了这个会话（窗口获得焦点）：清掉 done / error
    pub fn status_seen(&self, session_id: &str, ids: Vec<String>) {
        let Some(entry) = self.entry(session_id) else { return };
        if let Some(b) = entry.backend() {
            b.status_seen(&ids);
        }
    }

    /// 会话列表上显示的那一条；没有 live entry 的会话没有
    pub fn program_status_of(&self, session_id: &str) -> Option<ProgramStatusSummary> {
        self.entry(session_id).and_then(|e| summarize(&e.s.borrow().program_status))
    }

    fn clear_title(&self, entry: &LiveEntry) {
        entry.title_timer.clear();
        let mut s = entry.s.borrow_mut();
        s.title = None;
        s.auto_title = AutoTitle::new();
    }

    /// 终止一个会话。
    ///
    /// wait_gone：终止后等到宿主机上的会话真的结束。删除 worktree 目录之前必须等——shell 的
    /// cwd 就在那个目录里，Windows 上会被句柄占住直接删不掉；POSIX 上删得掉，但没死透的会话
    /// cwd 变成已删 inode，pwd 报错、任何碰 `.` 的命令行为诡异，比 Windows 隐蔽得多。
    ///
    /// 持久会话交给守护进程结束（给 shell 的进程组 SIGHUP，2 秒后补 SIGKILL），它报了 Exit 就是
    /// 结束了；连不上它时按 pid 动手（`falcon-sessiond kill`）。非持久会话没有可观测的信号，
    /// 等不了——由调用方的退避重试兜底。
    pub async fn terminate(&self, session_id: &str, wait_gone: bool) {
        let Some(row) = self.db.get_session(session_id) else { return };
        let entry = self.entry(session_id);
        if let Some(e) = &entry {
            e.s.borrow_mut().terminating = true;
        }

        if row.durable == 1 {
            let backend = entry.as_ref().and_then(|e| e.backend());
            match backend {
                Some(b) if b.terminate() => {
                    if wait_gone && let Some(e) = &entry {
                        for _ in 0..30 {
                            if e.s.borrow().exit_seen {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                _ => {
                    let project = self.db.get_project(&row.project_id);
                    let deployed = match (entry.as_ref().and_then(|e| e.deployed()), &project) {
                        (Some(d), _) => Some(d),
                        (None, Some(p)) => self.prepare(p, None, None, false).await.deployed,
                        (None, None) => None,
                    };
                    if let (Some(project), Some(d)) = (project, deployed) {
                        let sid = vec![falcon_proto::sessiond::sid_of(session_id)];
                        if project.project_type == "local" {
                            local_kill(&d, &sid).await;
                        } else {
                            self.get_link(&project).kill_sessions(&d, &sid).await;
                        }
                    }
                }
            }
        }
        if let Some(b) = entry.as_ref().and_then(|e| e.backend()) {
            b.destroy();
        }

        if let Some(e) = &entry {
            self.broadcast(e, &ServerMessage::State { state: SessionState::Dead, dead_reason: Some(DeadReason::Exited) });
            self.drop_output(e);
            self.entries.borrow_mut().remove(session_id);
        }
        self.db.delete_session(session_id);
    }

    /// 清除 dead 会话记录
    pub fn delete_dead(&self, session_id: &str) -> bool {
        let Some(row) = self.db.get_session(session_id) else { return false };
        if row.state != "dead" {
            return false;
        }
        let entry = self.entries.borrow_mut().remove(session_id);
        if let Some(e) = entry {
            self.drop_output(&e);
        }
        self.db.delete_session(session_id);
        true
    }

    // ---------- Viewer ----------

    pub fn add_viewer(&self, session_id: &str, viewer: Rc<Viewer>) {
        let Some(row) = self.db.get_session(session_id) else {
            viewer.send(&ServerMessage::Error { message: "会话不存在".into() });
            return;
        };
        if row.state == "dead" {
            viewer.send(&ServerMessage::State { state: SessionState::Dead, dead_reason: dead_reason_of(&row) });
            return;
        }

        let entry = self.hydrate_entry(&row);

        let already_live = entry.backend().is_some();
        // 先冲掉旧 Viewer 的合并窗口再加入新人：pending 里的数据已经进了
        // RingBuffer，不冲的话新 Viewer 会在 replay 之后再收到重复段
        self.flush_output(&entry);
        entry.s.borrow_mut().viewers.push(viewer.clone());

        let gate = decide_viewer_attach(ViewerAttachInput { durable: entry.durable, has_backend: already_live });

        if gate == ViewerAttach::Dead {
            self.mark_dead(&entry, DeadReason::BackendRestart);
            viewer.send(&ServerMessage::State { state: SessionState::Dead, dead_reason: Some(DeadReason::BackendRestart) });
            return;
        }

        if gate == ViewerAttach::WaitSize {
            // 等这条连接自己的 resize 再 attach，见 decide_viewer_attach。
            // 先把当前状态告诉 Viewer，不然 tab 会假装还在 active，标黄只出现在侧栏。
            let state = match SessionState::from_wire(&row.state).unwrap_or(SessionState::Unknown) {
                SessionState::Active => SessionState::Unverified,
                s => s,
            };
            viewer.send(&ServerMessage::State { state, dead_reason: dead_reason_of(&row) });
            self.flush_askpass(&viewer, session_id);
            return;
        }

        self.send_replay(&entry, &viewer);
        viewer.send(&ServerMessage::State { state: SessionState::Active, dead_reason: None });
        self.flush_askpass(&viewer, session_id);
        self.touch(session_id, true);
        // 刚打开的窗口不等节流窗口，立刻给一次标题
        self.spawn_probe_title(&entry);
    }

    pub fn remove_viewer(&self, session_id: &str, viewer_id: u64) {
        let Some(entry) = self.entry(session_id) else { return };
        let outcome = {
            let mut s = entry.s.borrow_mut();
            s.viewers.retain(|v| v.id != viewer_id);
            // 走的若是格子最小的那个，尺寸还给留下的人（只剩没量过的人或没人了就维持原样）；
            // 深浅的持有者走了，交给剩下的人里最近操作过的那个
            s.arbiter.leave(&viewer_id)
        };
        if let Some(size) = outcome.size {
            self.apply_size(&entry, size);
        }
        if let Some(hint) = outcome.hint {
            self.apply_colors(&entry, &hint);
        }
    }

    pub fn input(&self, session_id: &str, viewer_id: u64, data: &str) {
        if let Some(entry) = self.entry(session_id) {
            // 谁在操作深浅就跟谁（见 viewer_arbiter.rs）。先翻色再送这次按键：
            // 997 通知排在前面，前台程序对这一键的响应已经按新配色画
            let hint = entry.s.borrow_mut().arbiter.input(&viewer_id, data);
            if let Some(hint) = hint {
                self.apply_colors(&entry, &hint);
            }
            if let Some(b) = entry.backend() {
                b.write(data);
            }
        }
        self.touch(session_id, false);
    }

    pub fn resize(&self, session_id: &str, viewer_id: u64, cols: u16, rows: u16) {
        let Some(entry) = self.entry(session_id) else { return };
        // 给 PTY 的是各 Viewer 的最小格子：大屏那端拖窗不一定改得动它。
        // 尺寸没变时只跳过落库与 backend.resize，attach 分支必须照走：
        // decide_viewer_attach 的 wait-size 门控靠这条 resize 触发接回，
        // 而重连的 Viewer 报上来的尺寸很可能与存量一致。
        let size = entry.s.borrow_mut().arbiter.resize(&viewer_id, TermSize { cols, rows });
        self.apply_size(&entry, size);
        if entry.backend().is_some() {
            return;
        }
        if entry.durable && entry.viewer_count() > 0 && !entry.terminating() {
            let me = self.me();
            let session_id = session_id.to_string();
            tokio::task::spawn_local(async move {
                let Err(err) = me.ensure_attached(&session_id, false).await else { return };
                if me.db.get_session(&session_id).is_some_and(|r| r.state == "dead") {
                    return;
                }
                me.broadcast(&entry, &ServerMessage::Error { message: format!("接回失败：{err}") });
                me.broadcast(&entry, &ServerMessage::State { state: SessionState::Unverified, dead_reason: None });
                me.schedule_reconnect(&entry.project_id);
            });
        }
    }

    /// Viewer 报上来的终端深浅。多个 Viewer 时只有持有者（最近操作的那端）的这份生效，
    /// 其余的先存着，等它操作时再换上（见 viewer_arbiter.rs）。
    pub fn set_appearance(&self, session_id: &str, viewer_id: u64, hint: OscColorHint) {
        if hint.appearance.is_none() {
            return;
        }
        let Some(entry) = self.entry(session_id) else { return };
        let owned = entry.s.borrow_mut().arbiter.hint(&viewer_id, hint);
        if let Some(owned) = owned {
            self.apply_colors(&entry, &owned);
        }
    }

    // ---------- 内部 ----------

    /// 仲裁出的尺寸落到 entry 上；变了才落库、才 resize（每次 resize 前台程序都要整屏重画）
    fn apply_size(&self, entry: &Rc<LiveEntry>, size: TermSize) {
        if entry.size() == Some(size) {
            return;
        }
        entry.s.borrow_mut().size = Some(size);
        self.schedule_size_persist(&entry.session_id);
        if let Some(b) = entry.backend() {
            b.resize(size.cols, size.rows);
        }
    }

    /// 换上一份终端深浅。会话的 env 在创建时冻住了，改不了；但 OSC 10/11/12 的答复跟这份走，
    /// 主题切换后新启动的查询能拿到新底色。
    ///
    /// 持久会话交给守护进程：它的 VT 进程按这份回答查询，深浅翻转且程序订了 2031 时由 holder
    /// 注入通知。非持久会话由服务端自己来：深浅真的翻转且流里见过 DECSET 2031 订阅时，注入
    /// CSI ?997;1/2 n——Claude Code 这类 auto 主题程序运行中只认这个通知；没人订阅时绝不能注入，
    /// 这串字节会被前台程序当键盘输入吃掉。
    fn apply_colors(&self, entry: &LiveEntry, hint: &OscColorHint) {
        let Some(appearance) = hint.appearance else { return };
        let notify = {
            let mut s = entry.s.borrow_mut();
            let flipped = s.appearance.is_some_and(|a| a != appearance);
            s.appearance = Some(appearance);
            if let Some(bg) = hint.background.as_deref().filter(|c| parse_hex_rgb(c).is_some()) {
                s.background = Some(bg.to_string());
            }
            if let Some(fg) = hint.foreground.as_deref().filter(|c| parse_hex_rgb(c).is_some()) {
                s.foreground = Some(fg.to_string());
            }
            flipped && s.modes.theme_notify()
        };
        if entry.durable {
            if let Some(b) = entry.backend() {
                b.set_appearance(&entry.color_hint());
            }
            return;
        }
        if notify && let Some(b) = entry.backend() {
            b.write(if appearance == TermAppearance::Light { "\x1b[?997;2n" } else { "\x1b[?997;1n" });
        }
    }

    /// 从 DB 行拿到（或新建）LiveEntry。库存尺寸只在还没被 Viewer 量过时用。
    fn hydrate_entry(&self, row: &SessionRow) -> Rc<LiveEntry> {
        if let Some(existing) = self.entry(&row.id) {
            return existing;
        }
        let entry = LiveEntry::new(EntryInit {
            session_id: row.id.clone(),
            project_id: row.project_id.clone(),
            durable: row.durable == 1,
            deployed: None,
            size: parse_stored_term_size(row.cols.map(|c| c as f64), row.rows.map(|r| r as f64)),
            hint: OscColorHint::default(),
        });
        self.entries.borrow_mut().insert(row.id.clone(), entry.clone());
        entry
    }

    fn queue_output(&self, entry: &Rc<LiveEntry>, data: String) {
        // 有输出多半意味着前台换了程序（或刚跑完），顺手给自动标题点一次火
        self.schedule_title_probe(entry);
        entry.s.borrow_mut().pending_out.push_str(&data);
        if entry.flush_timer.is_set() {
            return;
        }
        let me = self.me();
        let e = entry.clone();
        entry.flush_timer.set(OUTPUT_FLUSH_MS, async move { me.flush_output(&e) });
    }

    fn flush_output(&self, entry: &LiveEntry) {
        entry.flush_timer.clear();
        let (data, viewers) = {
            let mut s = entry.s.borrow_mut();
            if s.pending_out.is_empty() {
                return;
            }
            (std::mem::take(&mut s.pending_out), s.viewers.clone())
        };
        // 序列化一次，N 个 Viewer 共享同一个帧
        let frame = encode_term_frame(TERM_FRAME_OUTPUT, &data);
        for v in viewers {
            // 在等快照的 Viewer：这些输出都会含在快照里
            if v.syncing.get() {
                continue;
            }
            if v.lagged.get() {
                // 落后的 Viewer 不追增量（那正是它堆积的原因），排空后整体重放对齐
                if v.drained() {
                    self.send_replay(entry, &v);
                }
                continue;
            }
            if !v.send_bytes(frame.clone()) {
                v.lagged.set(true);
            }
        }
    }

    fn drop_output(&self, entry: &LiveEntry) {
        entry.flush_timer.clear();
        entry.s.borrow_mut().pending_out.clear();
    }

    fn flush_askpass(&self, viewer: &Viewer, session_id: &str) {
        for p in self.askpass.pending_prompts() {
            if p.session_id.as_deref().is_some_and(|s| s != session_id) {
                continue;
            }
            viewer.send(&ServerMessage::Askpass { id: p.id, prompt: p.prompt });
        }
    }

    /// sudo askpass 弹窗：优先推给该会话的 Viewer；sessionId 对不上或没人看
    /// 时发给所有还连着的 Viewer——agent 的 sudo 往往发生在当前 tab。
    pub fn broadcast_askpass(&self, prompt: &AskpassPrompt) {
        let msg = ServerMessage::Askpass { id: prompt.id.clone(), prompt: prompt.prompt.clone() };
        let target = prompt.session_id.as_deref().and_then(|id| self.entry(id));
        if let Some(target) = target.filter(|t| t.viewer_count() > 0) {
            self.broadcast(&target, &msg);
            return;
        }
        let entries: Vec<_> = self.entries.borrow().values().cloned().collect();
        for entry in entries {
            if entry.viewer_count() > 0 {
                self.broadcast(&entry, &msg);
            }
        }
    }

    fn broadcast(&self, entry: &LiveEntry, msg: &ServerMessage) {
        // 控制消息与输出保持时序：先把合并窗口里的输出冲出去
        self.flush_output(entry);
        for v in entry.viewers() {
            v.send(msg);
        }
    }

    fn send_replay(&self, entry: &LiveEntry, viewer: &Viewer) {
        if entry.durable {
            // 守护进程会话：要一份快照，回来之前这个 Viewer 不收输出（on_backend_event）。
            // 还没连上（等接回）时先挂着，接回成功的 mark_active 会再要
            viewer.syncing.set(true);
            viewer.lagged.set(false);
            self.request_sync(entry);
            return;
        }
        // 前端拿 replay 帧整体换掉终端状态（等同先 reset 再写入），会清掉全部 VT 模式；
        // 快照里往往已没有当初的模式序列（4MB 环挤掉了），这里用跟踪到的
        // 当前模式作前缀重建，否则重连后的 Viewer 永久丢失 mouse tracking
        // （滚轮失效）和 bracketed paste（多行粘贴被逐行执行）。
        let payload = {
            let s = entry.s.borrow();
            let mut p = s.modes.prefix();
            p.push_str(&s.buffer.snapshot());
            p
        };
        let ok = viewer.send_bytes(encode_term_frame(TERM_FRAME_REPLAY, &payload));
        viewer.lagged.set(!ok);
    }

    fn mark_active(&self, entry: &LiveEntry, replay: bool) {
        self.db.update_session_state(&entry.session_id, SessionState::Active, None);
        if replay {
            self.flush_output(entry);
            for v in entry.viewers() {
                self.send_replay(entry, &v);
            }
        }
        self.broadcast(entry, &ServerMessage::State { state: SessionState::Active, dead_reason: None });
    }

    fn mark_dead(&self, entry: &LiveEntry, reason: DeadReason) {
        entry.s.borrow_mut().backend = None;
        // 先把还没广播的尾巴发出去（shell 的告别输出），再释放
        self.flush_output(entry);
        // dead 会话不可能再回放（add_viewer 对 dead 行早退），立刻释放 Scrollback
        {
            let mut s = entry.s.borrow_mut();
            s.buffer.reset(None);
            // 进程都没了，working / blocked 不再成立；done / error 也没有可回去看的会话了
            s.program_status.clear();
        }
        self.last_touch.borrow_mut().remove(&entry.session_id);
        // 死了就没有前台命令可言，停掉待发的探测并把标题收回去
        self.clear_title(entry);
        self.db.update_session_state(&entry.session_id, SessionState::Dead, Some(reason));
        self.broadcast(entry, &ServerMessage::State { state: SessionState::Dead, dead_reason: Some(reason) });
        self.broadcast(entry, &ServerMessage::Title { title: None });
    }

    fn mark_unverified(&self, entry: &Rc<LiveEntry>) {
        entry.s.borrow_mut().backend = None;
        self.db.update_session_state(&entry.session_id, SessionState::Unverified, None);
        self.broadcast(entry, &ServerMessage::State { state: SessionState::Unverified, dead_reason: None });
        if !entry.terminating() {
            self.kick_auto_reattach(entry);
        }
    }

    /// 标黄之后自己去接，不再等用户点。SSH 走链路重连，本地直接再 attach。
    fn kick_auto_reattach(&self, entry: &LiveEntry) {
        if !entry.durable {
            return;
        }
        let Some(project) = self.db.get_project(&entry.project_id) else { return };
        if project.project_type == "ssh" {
            self.schedule_reconnect(&project.id);
        } else {
            self.schedule_local_reattach(&entry.session_id);
        }
    }

    /// backend 附着结束：区分真退出 / 手动 detach / 链路断开。
    ///
    /// `ended` 是退出的那个 backend。与 TS 的出入：entry 上已经换成了别的 backend（旧链路
    /// 的 channel 在新附着之后才报关闭）时不理——TS 会把新 backend 一并置空，这是个竞态。
    async fn handle_backend_exit(&self, session_id: &str, ended: Option<Rc<dyn Backend>>) {
        let Some(entry) = self.entry(session_id) else { return };
        if entry.terminating() {
            return;
        }
        if let (Some(current), Some(ended)) = (entry.backend(), &ended)
            && !same_backend(&current, ended)
        {
            return;
        }
        entry.s.borrow_mut().backend = None;

        if self.db.get_session(session_id).is_none() {
            return;
        }
        let project = self.db.get_project(&entry.project_id);

        if !entry.durable {
            let link_lost = project.as_ref().is_some_and(|p| p.project_type == "ssh")
                && !self.existing_link(&entry.project_id).is_some_and(|l| l.is_connected());
            self.mark_dead(&entry, if link_lost { DeadReason::LinkLost } else { DeadReason::Exited });
            return;
        }

        // 持久会话：守护进程报过 shell 退出 → 真退出；否则看它留下的痕迹
        let (exit_seen, bridge_exit) = {
            let s = entry.s.borrow();
            (s.exit_seen, s.bridge_exit)
        };
        if exit_seen {
            self.mark_dead(&entry, DeadReason::Exited);
            return;
        }
        if project.as_ref().is_some_and(|p| p.project_type == "local") {
            match entry.deployed().map(|d| local_session_state(&d, session_id)) {
                Some(LocalSessionState::Exited) => self.mark_dead(&entry, DeadReason::Exited),
                Some(LocalSessionState::Alive) => self.mark_unverified(&entry),
                _ => self.mark_dead(&entry, DeadReason::SessionGone),
            }
            return;
        }
        // SSH：bridge 的退出码说了算；没有退出码（链路断了、bridge 被杀）就等重连再看
        match bridge_exit.flatten().map(|c| c as i32) {
            Some(falcon_proto::sessiond::BRIDGE_EXIT_GONE) => self.mark_dead(&entry, DeadReason::SessionGone),
            Some(falcon_proto::sessiond::BRIDGE_EXIT_EXITED) => self.mark_dead(&entry, DeadReason::Exited),
            _ => self.mark_unverified(&entry),
        }
    }

    fn handle_link_down(&self, project_id: &str) {
        let entries: Vec<_> = self.entries.borrow().values().cloned().collect();
        for entry in entries {
            if entry.project_id != project_id || entry.terminating() {
                continue;
            }
            match self.db.get_session(&entry.session_id) {
                Some(row) if row.state != "dead" => {}
                _ => continue,
            }
            if entry.durable {
                self.mark_unverified(&entry);
            } else {
                self.mark_dead(&entry, DeadReason::LinkLost);
            }
        }
        // 待接回的持久会话由 mark_unverified → kick_auto_reattach 排重连；
        // 中转不走项目链路（ADR 0016），这里不用再为它们坚持
    }

    /// 等着被接回的会话（项目重连的目标）
    fn reconnect_targets(&self, project_id: &str) -> Vec<Rc<LiveEntry>> {
        let entries: Vec<_> = self.entries.borrow().values().cloned().collect();
        entries
            .into_iter()
            .filter(|e| {
                e.project_id == project_id
                    && e.durable
                    && e.backend().is_none()
                    && !e.terminating()
                    && self.db.get_session(&e.session_id).is_some_and(|r| r.state == "unverified")
            })
            .collect()
    }

    /// SSH 断线自动重连：指数退避。
    /// 有待接回的持久会话就坚持——不再要求有人正在看。中转挂在主机链路上，
    /// 重连见 schedule_host_reconnect。
    fn schedule_reconnect(&self, project_id: &str) {
        let existing = self.reconnects.borrow().get(project_id).cloned();
        if existing.as_ref().is_some_and(|s| s.timer.is_set()) {
            return;
        }
        let state = existing.unwrap_or_default();
        self.reconnects.borrow_mut().insert(project_id.to_string(), state.clone());
        self.reconnect_after(project_id.to_string(), state, 1000);
    }

    fn reconnect_after(&self, project_id: String, state: Rc<ReconnectState>, ms: u64) {
        let me = self.me();
        let st = state.clone();
        state.timer.set(ms, async move { me.reconnect_tick(project_id, st).await });
    }

    async fn reconnect_tick(&self, project_id: String, state: Rc<ReconnectState>) {
        let waiting = self.reconnect_targets(&project_id);
        if waiting.is_empty() {
            self.reconnects.borrow_mut().remove(&project_id);
            return;
        }
        state.attempt.set(state.attempt.get() + 1);
        for e in &waiting {
            self.broadcast(e, &ServerMessage::Reconnecting { attempt: state.attempt.get() });
        }
        let Some(project) = self.db.get_project(&project_id) else {
            self.reconnects.borrow_mut().remove(&project_id);
            return;
        };
        // 链路还没通就继续退避
        if self.get_link(&project).get_client().await.is_ok() {
            for e in self.reconnect_targets(&project_id) {
                // 有 Viewer 还没报格子：等它自己的 resize，别用库存尺寸抢跑
                if e.waiting_for_size() {
                    continue;
                }
                // 单个会话接回失败（如 session-gone），已在 ensure_attached 中标记
                let _ = self.ensure_attached(&e.session_id, e.viewer_count() == 0).await;
            }
            let still = self.reconnect_targets(&project_id).into_iter().filter(|e| !e.waiting_for_size()).count();
            if still == 0 {
                self.reconnects.borrow_mut().remove(&project_id);
                return;
            }
        }
        let delay = backoff_ms(state.attempt.get());
        self.reconnect_after(project_id, state, delay);
    }

    /// 本地持久会话的自动接回。连接掉了但守护进程还在时，不必等用户点。
    fn schedule_local_reattach(&self, session_id: &str) {
        let existing = self.local_reattach.borrow().get(session_id).cloned();
        if existing.as_ref().is_some_and(|s| s.timer.is_set()) {
            return;
        }
        let state = existing.unwrap_or_default();
        self.local_reattach.borrow_mut().insert(session_id.to_string(), state.clone());
        self.local_reattach_after(session_id.to_string(), state, 250);
    }

    fn local_reattach_after(&self, session_id: String, state: Rc<ReconnectState>, ms: u64) {
        let me = self.me();
        let st = state.clone();
        state.timer.set(ms, async move { me.local_reattach_tick(session_id, st).await });
    }

    async fn local_reattach_tick(&self, session_id: String, state: Rc<ReconnectState>) {
        let entry = self.entry(&session_id);
        let row = self.db.get_session(&session_id);
        let Some(entry) = entry.filter(|e| {
            row.as_ref().is_some_and(|r| r.state == "unverified") && e.backend().is_none() && !e.terminating()
        }) else {
            self.local_reattach.borrow_mut().remove(&session_id);
            return;
        };
        // 有 Viewer 还没报格子：resize() 会触发 ensure_attached，这里空转没有意义
        if entry.waiting_for_size() {
            self.local_reattach.borrow_mut().remove(&session_id);
            return;
        }
        state.attempt.set(state.attempt.get() + 1);
        self.broadcast(&entry, &ServerMessage::Reconnecting { attempt: state.attempt.get() });
        match self.ensure_attached(&session_id, entry.viewer_count() == 0).await {
            Ok(_) => {
                self.local_reattach.borrow_mut().remove(&session_id);
            }
            Err(e) => {
                log::warn!("本地会话 {session_id} 自动接回失败（第 {} 次）：{e}", state.attempt.get());
                if self.db.get_session(&session_id).is_some_and(|r| r.state == "dead") {
                    self.local_reattach.borrow_mut().remove(&session_id);
                    return;
                }
                let delay = backoff_ms(state.attempt.get());
                self.local_reattach_after(session_id, state, delay);
            }
        }
    }

    /// 500ms 合并窗口内最多落库一次，到点时取 entry 上的最新值，所以窗口内的后续变化
    /// 不丢——只是推迟。进程退出最多丢 500ms 内的最后一次尺寸，对 cols/rows 这种数据
    /// 完全可接受。
    fn schedule_size_persist(&self, session_id: &str) {
        if !self.size_flush.borrow_mut().insert(session_id.to_string()) {
            return;
        }
        let me = self.me();
        let session_id = session_id.to_string();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            me.size_flush.borrow_mut().remove(&session_id);
            if let Some(size) = me.entry(&session_id).and_then(|e| e.size()) {
                me.db.update_session_size(&session_id, size.cols, size.rows);
            }
        });
    }

    fn touch(&self, session_id: &str, force: bool) {
        let now = now_ms();
        let last = self.last_touch.borrow().get(session_id).copied().unwrap_or(0);
        if force || now - last > 30_000 {
            self.last_touch.borrow_mut().insert(session_id.to_string(), now);
            self.db.touch_session(session_id, now);
        }
    }
}

fn dead_reason_of(row: &SessionRow) -> Option<DeadReason> {
    row.dead_reason.as_deref().map(|r| DeadReason::from_wire(r).unwrap_or(DeadReason::Unknown))
}

/// 远端命令失败的文案：stderr 优先，没有就带上退出码
fn fail_text(stderr: &str, what: &str, code: Option<i32>) -> String {
    let trimmed = crate::term_env::js::trim(stderr);
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    match code {
        Some(c) => format!("{what}（exit {c}）"),
        None => format!("{what}（exit null）"),
    }
}

#[cfg(test)]
mod tests;
