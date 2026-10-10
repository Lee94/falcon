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
    DeadReason, NonDurableReason, OscColorHint, ServerMessage, Session, SessionAgent, SessionForeground, SessionState,
    TERM_FRAME_OUTPUT, TERM_FRAME_REPLAY, TermAppearance,
};
use futures::FutureExt as _;
use futures::future::Shared;
use tokio::task::AbortHandle;
use tokio_util::sync::CancellationToken;

use super::backend::{Backend, BackendCallbacks, SessionGoneError};
use super::local::{
    LocalHost, attach_local, default_local_shell, local_foreground, local_has_session, local_kill, local_scroll_pipe,
    local_terminal_pane,
};
use super::ssh::SshLink;
use super::ssh_zellij::{AttachOptions, HostFacts};
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
use crate::virtualdir::{
    container_manifest_files, posix_write_manifest_command, remove_local_virtual_dir, remove_virtual_dir_command,
    virtual_project_dir, windows_write_manifest_commands, write_local_manifest,
};
use crate::zellij::command::ScrollPosition;
use crate::zellij::host::{HostKind, HostLayout};
use crate::zellij::install::{InstallError, StageFn};

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

/// 自动标题的探测节流窗口。每次探测是宿主机上一条实打实的命令（Zellij list-clients，
/// SSH 上还要开一条 channel），不能跟着输出走。
///
/// 2.5s 是"命令跑起来到标题出现"的可感延迟上限，也压住了 `yes` 这种满屏输出的会话——
/// 窗口内无论来多少输出都只探一次。
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
    pub layout: Option<HostLayout>,
    /// 滚动位置插件已就位（ADR 0019）；新会话据此用带插件的配置
    pub scroll: bool,
}

/// 会话操作的失败。Clone 是因为接回是单飞的（多个等待者共享同一个结果）
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// 会话 / 项目不存在（TS 的 NotFoundError）
    #[error("{0}")]
    NotFound(String),
    /// 接回时宿主机上的 Zellij 会话已经不在了（TS 的 SessionGoneError）
    #[error("Zellij 会话不存在")]
    Gone,
    #[error("{0}")]
    Failed(String),
}

impl From<anyhow::Error> for SessionError {
    fn from(e: anyhow::Error) -> Self {
        if e.downcast_ref::<SessionGoneError>().is_some() {
            SessionError::Gone
        } else {
            SessionError::Failed(e.to_string())
        }
    }
}

/// 中转（端口转发 / 公网发布）在主机链路上的钩子。ForwardManager 与 ShareManager 实现它，
/// manager 只管把主机链路的 up / down / 连不上转告给它们（ADR 0016）
pub trait RelayHost {
    fn has_enabled(&self, host_id: &str) -> bool;
    fn enabled_host_ids(&self) -> Vec<String>;
    fn on_link_down(&self, host_id: &str);
    fn on_link_up(&self, host_id: &str);
    fn mark_unreachable(&self, host_id: &str, message: &str);
    /// 主机链路要丢了（改凭据 / 删主机）：先把挂在它上面的中转停掉
    fn forget_host<'a>(&'a self, host_id: &'a str) -> LocalBoxFuture<'a, ()>;
    /// 后端启动时把本机上的中转拉起来（只有公网发布有）
    fn restore_local(&self) {}
}

struct EntryState {
    /// 持久会话所用的 Zellij 布局，接回时要用
    layout: Option<HostLayout>,
    backend: Option<Rc<dyn Backend>>,
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
    /// 持有深浅的那个 Viewer 报的终端深浅；接回后的内层 env 冻住了，只影响 OSC 答复和新会话
    appearance: Option<TermAppearance>,
    background: Option<String>,
    foreground: Option<String>,
    osc: OscColorGate,
    /// 输出流里的 VT 模式跟踪，replay 时重建（模式序列早被 RingBuffer 挤掉了）
    modes: TermModeTracker,
    /// 最近一次探到的前台命令（自动标题）；None = 空闲在 shell 里或探不到
    title: Option<String>,
    /// 探测在飞：Viewer 进出与输出可能同时点火，只许有一条在跑
    title_probing: bool,
    /// 会话用带滚动位置插件的配置建的（sessions.scroll_plugin），见 scroll()
    scroll: bool,
    /// 插件按它找 pane；首次查询时问一次 list-panes 缓存下来
    scroll_pane: Option<u32>,
    /// 查询在飞：同一时刻只跑一条
    scroll_busy: bool,
    /// 排着的请求：只剩最后一个。内层 None = 只问位置
    scroll_want: Option<Option<f64>>,
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
    layout: Option<HostLayout>,
    scroll: bool,
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
                layout: init.layout,
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
                title_probing: false,
                scroll: init.scroll,
                scroll_pane: None,
                scroll_busy: false,
                scroll_want: None,
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

    fn layout(&self) -> Option<HostLayout> {
        self.s.borrow().layout.clone()
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
    /// 本机这台宿主（Zellij 准备结果、PTY 基底环境）
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
    relays: RefCell<Vec<Rc<dyn RelayHost>>>,
    me: Weak<SessionManager>,
}

impl SessionManager {
    pub fn new(db: Arc<Db>, secrets: Arc<SecretBox>, data_dir: PathBuf, askpass: Arc<AskpassHub>) -> Rc<Self> {
        db.recover_sessions_on_startup();
        Rc::new_cyclic(|me| SessionManager {
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
            relays: RefCell::default(),
            me: me.clone(),
        })
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

    /// 挂上中转（引擎建好 ForwardManager / ShareManager 之后调）
    pub fn add_relay_host(&self, relay: Rc<dyn RelayHost>) {
        self.relays.borrow_mut().push(relay);
    }

    fn relays(&self) -> Vec<Rc<dyn RelayHost>> {
        self.relays.borrow().clone()
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

    /// 判定某项目的宿主机能否提供持久会话，必要时触发 Zellij 安装。
    ///
    /// SSH 项目要求用户先授权——这会往用户的服务器上写入可执行文件，属于该问的那一类。
    /// 授权按主机记（host+port+username），不按项目：同一台机器上的第二个项目
    /// 不该再问一遍，二进制本来就已经装好了。
    ///
    /// fresh：丢掉上一次的判定重新来过。用户显式发起安装/重试时必须带上——
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
            let local = self.local.prepare(on_stage, cancel).await;
            return DurableState {
                durable: local.durable,
                reason: local.reason,
                detail: local.detail,
                layout: local.layout,
                scroll: local.scroll,
            };
        }

        let saved = self.db.get_zellij_host(
            project.ssh_host.as_deref().unwrap_or_default(),
            project.ssh_port.unwrap_or(22) as u16,
            project.ssh_username.as_deref().unwrap_or_default(),
        );
        if saved.and_then(|s| s.authorized) != Some(1) {
            return DurableState { durable: false, reason: Some(NonDurableReason::NotAuthorized), ..Default::default() };
        }

        let link = self.get_link(project);
        if fresh {
            link.reset_zellij();
        }
        let remote = link.prepare_zellij(on_stage, cancel).await;
        DurableState {
            durable: remote.durable,
            reason: remote.reason,
            detail: remote.detail,
            layout: remote.layout,
            scroll: remote.scroll,
        }
    }

    /// 用户改了授权或下载源之后，清掉缓存的判定以便重试
    pub fn reset_prepare(&self, project_id: &str) {
        let link = self.links.borrow().get(project_id).cloned();
        if let Some(link) = link {
            link.reset_zellij();
        }
    }

    /// 本地宿主的持久能力；None = 尚未探测（不主动触发安装）
    pub fn local_durable_state(&self) -> Option<DurableState> {
        let local = self.local.peek()?;
        Some(DurableState { durable: local.durable, reason: local.reason, layout: local.layout, ..Default::default() })
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
                for r in me.relays() {
                    r.on_link_down(&host_id);
                }
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
                for r in me.relays() {
                    r.on_link_up(&host_id);
                }
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
        let relays = self.relays();
        futures::future::join_all(relays.iter().map(|r| r.forget_host(host_id))).await;
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
        let relays = self.relays();
        for r in &relays {
            r.restore_local();
        }
        let mut host_ids: Vec<String> = Vec::new();
        for r in &relays {
            for id in r.enabled_host_ids() {
                if !host_ids.contains(&id) {
                    host_ids.push(id);
                }
            }
        }
        for host_id in host_ids {
            let me = self.me();
            tokio::task::spawn_local(async move { me.connect_host(&host_id).await });
        }
    }

    fn wants_host_link(&self, host_id: &str) -> bool {
        self.relays().iter().any(|r| r.has_enabled(host_id))
    }

    fn mark_host_unreachable(&self, host_id: &str, message: &str) {
        for r in self.relays() {
            r.mark_unreachable(host_id, message);
        }
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

        let row = SessionRow {
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
            // 建会话这一刻定终身：接回时必须照这套配置来，见 SessionRow.scroll_plugin
            scroll_plugin: Some(i64::from(prep.durable && prep.scroll)),
        };

        let entry = LiveEntry::new(EntryInit {
            session_id: id.clone(),
            project_id: project.id.clone(),
            durable: prep.durable,
            layout: prep.layout.clone(),
            scroll: row.scroll_plugin == Some(1),
            size: None,
            hint,
        });
        self.entries.borrow_mut().insert(id.clone(), entry.clone());

        if let Err(e) = self.attach_backend(&entry, project, &row, false).await {
            self.entries.borrow_mut().remove(&id);
            return Err(e);
        }

        self.db.insert_session(&row);
        Ok(Db::to_session(&row))
    }

    /// 这个会话该用什么 shell 开场。
    ///
    /// 普通会话就是项目配的 shell（或让下游按平台挑默认）；agent 会话则换成写在宿主机上
    /// 的启动脚本（sessions/agent.rs）。脚本没写成时**退回普通 shell**：把一个不存在的
    /// 路径当 shell 交给 Zellij，pane 根本起不来，用户只会看到一片空白。
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
        // 附着返回之前就可能有 OSC 查询要答：先攒着，backend 到手再写
        let slot: Rc<RefCell<(Option<Rc<dyn Backend>>, Vec<String>)>> =
            Rc::new(RefCell::new((entry.backend(), Vec::new())));
        let cb = {
            let me = self.me.clone();
            let weak = Rc::downgrade(entry);
            let slot_data = slot.clone();
            let slot_exit = slot.clone();
            let me_exit = self.me.clone();
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
            }
        };

        // 接回时 entry.layout 可能是空的（后端重启后重建的 entry），重新准备一次
        if entry.durable && entry.layout().is_none() {
            let prep = self.prepare(project, None, None, false).await;
            match (prep.durable, prep.layout) {
                (true, Some(layout)) => entry.s.borrow_mut().layout = Some(layout),
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
        let opts = AttachOptions {
            session_id: entry.session_id.clone(),
            cwd,
            shell: self.session_shell(project, row).await,
            durable: entry.durable,
            layout: entry.layout(),
            reattach,
            cols: size.cols,
            rows: size.rows,
            appearance: entry.s.borrow().appearance,
            askpass_bin,
            scroll: row.scroll_plugin == Some(1),
        };

        let result = if project.project_type == "local" {
            attach_local(&self.local, &opts, cb).await?
        } else {
            self.get_link(project).attach_session(&opts, cb).await?
        };

        let backend = result.backend;
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
        if reattach && let Some(history) = result.captured_history {
            entry.s.borrow_mut().buffer.reset(Some(&history));
        }
        Ok(())
    }

    fn on_backend_data(&self, entry: &Rc<LiveEntry>, slot: &RefCell<(Option<Rc<dyn Backend>>, Vec<String>)>, data: &str) {
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

    /// 确保会话有活的 backend。
    /// unverified 的持久会话在此被验证并恢复为 active；Zellij 不在了则标记 dead。
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
                                if err == SessionError::Gone {
                                    me.mark_dead(&e, DeadReason::SessionGone);
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
    /// 持久会话问 Zellij（list-clients 的 RUNNING_COMMAND 就是聚焦 pane 的前台
    /// 命令）；非持久本地会话问 PTY 自己。侦测不到的场景一律按空闲放行：
    /// 非持久 SSH（channel 里问不到远端 shell 的进程树）、Windows 远端
    /// （Zellij 没有 /proc 可读，恒报 N/A）、unverified/断链（拿到的答案没有意义）。
    /// 这是道保险，探测失败不能把关 tab 拦下来，所以也不报错。
    pub async fn foreground(&self, session_id: &str) -> SessionForeground {
        let idle = SessionForeground { busy: false, command: None };
        let Some(row) = self.db.get_session(session_id).filter(|r| r.state == "active") else { return idle };
        let Some(project) = self.db.get_project(&row.project_id) else { return idle };
        let entry = self.entry(session_id);

        let command = if row.durable != 1 {
            entry.as_ref().and_then(|e| e.backend()).and_then(|b| b.process_name())
        } else if let Some(layout) = entry.as_ref().and_then(|e| e.layout()) {
            // 没有活的附着就没有 Zellij 客户端，list-clients 必为空，不必跑
            if project.project_type == "local" {
                local_foreground(&layout, session_id).await
            } else {
                // 不为一次探测去重建链路
                let Some(link) = self.existing_link(&project.id).filter(|l| l.is_connected()) else { return idle };
                link.foreground(&layout, session_id).await.ok().flatten()
            }
        } else {
            None
        };
        match command {
            Some(c) if !is_shell_command(&c, project.shell.as_deref()) => SessionForeground { busy: true, command: Some(c) },
            _ => idle,
        }
    }

    // ---------- 自动标题 ----------

    /// 会话此刻的自动标题（前台命令）。没有 live entry 的会话没有标题，不为它现探。
    pub fn title_of(&self, session_id: &str) -> Option<String> {
        self.entry(session_id).and_then(|e| e.s.borrow().title.clone())
    }

    /// 安排一次前台命令探测。
    ///
    /// 只在有 Viewer 时探：每次探测是宿主机上一条实打实的命令（SSH 上还要开一条
    /// channel），没人看着的会话探出来也没人用。代价是**后台会话的标题会陈旧**——
    /// 停在最后一个 Viewer 离开时探到的那次，直到下次有人打开它。宁可陈旧也不轮询：
    /// 会话可以有十几个，定时全量探测会把 SSH 链路占满。
    ///
    /// 节流而非 debounce：窗口内重复点火直接忽略，所以满屏输出的会话也按固定间隔
    /// 出标题，不必等它安静下来。
    fn schedule_title_probe(&self, entry: &Rc<LiveEntry>) {
        if entry.title_timer.is_set() || entry.viewer_count() == 0 {
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

    /// 探一次并广播变化。foreground() 自己吞掉所有错误，这里不会失败。
    async fn probe_title(&self, entry: &Rc<LiveEntry>) {
        if entry.s.borrow().title_probing {
            return;
        }
        entry.s.borrow_mut().title_probing = true;
        let fg = self.foreground(&entry.session_id).await;
        entry.s.borrow_mut().title_probing = false;
        // 探测期间会话可能已被终止，entry 也可能被重建过
        if !self.is_current(entry) {
            return;
        }
        let next = fg.command;
        if next == entry.s.borrow().title {
            return;
        }
        entry.s.borrow_mut().title = next.clone();
        self.broadcast(entry, &ServerMessage::Title { title: next });
    }

    // ---------- 滚动条（ADR 0019） ----------

    /// Viewer 要滚动位置（seek 缺省），或要滚到「视口下方还剩 seek 行」处。结果广播给
    /// 所有 Viewer——大家看的是同一个 zellij 视口。
    ///
    /// 不支持的会话（非持久、升级前用老配置建的）直接不理：前端收不到回话就不画滚动条。
    /// 单飞：一条在飞时后来的请求只记最后一个，seek 压过只问位置（seek 本身也回位置），
    /// 拖滑块时几十个 seek 只会落地头尾几个。
    pub fn scroll(&self, session_id: &str, seek: Option<f64>) {
        let Some(entry) = self.entry(session_id) else { return };
        let busy = {
            let mut s = entry.s.borrow_mut();
            if !s.scroll || s.backend.is_none() || s.layout.is_none() {
                return;
            }
            s.scroll_want = match seek {
                Some(v) => Some(Some(v)),
                None => Some(s.scroll_want.unwrap_or(None)),
            };
            s.scroll_busy
        };
        if !busy {
            let me = self.me();
            tokio::task::spawn_local(async move { me.run_scroll(&entry).await });
        }
    }

    async fn run_scroll(&self, entry: &Rc<LiveEntry>) {
        entry.s.borrow_mut().scroll_busy = true;
        loop {
            let Some(seek) = entry.s.borrow_mut().scroll_want.take() else { break };
            let pos = self.scroll_pipe(entry, seek).await;
            // 查询期间会话可能已被终止，entry 也可能被重建过
            if !self.is_current(entry) {
                break;
            }
            if let Some(pos) = pos {
                let rows = entry.size().map(|s| u32::from(s.rows)).unwrap_or(0);
                self.broadcast(entry, &ServerMessage::Scroll { position: pos.position, length: pos.length, rows });
            }
        }
        entry.s.borrow_mut().scroll_busy = false;
    }

    async fn scroll_pipe(&self, entry: &Rc<LiveEntry>, seek: Option<f64>) -> Option<ScrollPosition> {
        let layout = entry.layout()?;
        let project = self.db.get_project(&entry.project_id)?;
        let cached = entry.s.borrow().scroll_pane;
        if project.project_type == "local" {
            let pane = match cached {
                Some(p) => p,
                None => {
                    let p = local_terminal_pane(&layout, &entry.session_id).await?;
                    entry.s.borrow_mut().scroll_pane = Some(p);
                    p
                }
            };
            return local_scroll_pipe(&layout, &entry.session_id, pane, seek).await;
        }
        // 不为一次查询去重建链路
        let link = self.existing_link(&project.id).filter(|l| l.is_connected())?;
        let pane = match cached {
            Some(p) => p,
            None => {
                let p = link.terminal_pane(&layout, &entry.session_id).await.ok().flatten()?;
                entry.s.borrow_mut().scroll_pane = Some(p);
                p
            }
        };
        link.scroll_pipe(&layout, &entry.session_id, pane, seek).await.ok().flatten()
    }

    fn clear_title(&self, entry: &LiveEntry) {
        entry.title_timer.clear();
        entry.s.borrow_mut().title = None;
    }

    /// 终止一个会话。
    ///
    /// wait_gone：终止后等到宿主机上的 Zellij session 真的消失。删除 worktree 目录之前
    /// 必须等——Zellij pane 的 cwd 就在那个目录里，Windows 上会被句柄占住直接删不掉；
    /// POSIX 上删得掉，但没死透的会话 cwd 变成已删 inode，pwd 报错、任何碰 `.` 的命令
    /// 行为诡异，比 Windows 隐蔽得多。
    ///
    /// 用 Zellij 侧的会话存在性当信号，而不是等 backend 退出：Backend::destroy() 不返回
    /// 结果（本地是发 SIGHUP、SSH 是关 channel，都是异步的），接口层面根本拿不到
    /// "已经退出"。非持久会话没有可观测的信号，等不了——由调用方的退避重试兜底。
    pub async fn terminate(&self, session_id: &str, wait_gone: bool) {
        let Some(row) = self.db.get_session(session_id) else { return };
        let entry = self.entry(session_id);
        if let Some(e) = &entry {
            e.s.borrow_mut().terminating = true;
        }

        if row.durable == 1 {
            let project = self.db.get_project(&row.project_id);
            let layout = match (entry.as_ref().and_then(|e| e.layout()), &project) {
                (Some(l), _) => Some(l),
                (None, Some(p)) => self.prepare(p, None, None, false).await.layout,
                (None, None) => None,
            };
            if let (Some(project), Some(layout)) = (project, layout) {
                if project.project_type == "local" {
                    local_kill(&layout, session_id).await;
                    if wait_gone {
                        wait_until_gone(|| local_has_session(&layout, session_id)).await;
                    }
                } else {
                    let link = self.get_link(&project);
                    link.kill_session(&layout, session_id).await;
                    if wait_gone {
                        wait_until_gone(|| async { link.has_session(&layout, session_id).await.unwrap_or(false) }).await;
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
        // 最后一个人走之前再探一次：Detach 掉的会话在侧栏上要还看得出在跑什么，
        // 而这是它能更新标题的最后时机（此后不再探，见 schedule_title_probe）
        if entry.viewer_count() == 0 {
            self.spawn_probe_title(&entry);
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

    /// 仲裁出的尺寸落到 entry 上；变了才落库、才 resize PTY（每次 resize Zellij 都整屏重画）
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

    /// 换上一份终端深浅。接回已有 Zellij 会话改不了内层 env，
    /// 但 OSC 10/11/12 答复跟这份走，主题切换后新启动的查询能拿到新底色。
    ///
    /// 深浅真的翻转且流里见过 DECSET 2031 订阅时，再注入 CSI ?997;1/2 n：
    /// Claude Code 这类 auto 主题程序运行中只认这个通知。Zellij client 自己
    /// 就会订并把通知转发给订阅的 pane（0.44 实测）；没人订阅时绝不能注入，
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
            layout: None,
            scroll: row.scroll_plugin == Some(1),
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
        // 前端对 replay 帧先 reset 再写入，reset 会清掉全部 VT 模式；
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
        entry.s.borrow_mut().buffer.reset(None);
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

        // 持久会话：Zellij session 还在 → unverified（可接回）；不在 → 真退出
        let layout = entry.layout();
        if project.as_ref().is_some_and(|p| p.project_type == "local") {
            match layout {
                Some(l) if local_has_session(&l, session_id).await => self.mark_unverified(&entry),
                _ => self.mark_dead(&entry, DeadReason::Exited),
            }
            return;
        }

        let link = self.existing_link(&entry.project_id).filter(|l| l.is_connected());
        let (Some(link), Some(layout)) = (link, layout) else {
            // 链路断开，交给 handle_link_down / 重连流程
            self.mark_unverified(&entry);
            return;
        };
        match link.has_session(&layout, session_id).await {
            Ok(false) => self.mark_dead(&entry, DeadReason::Exited),
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

    /// 本地持久会话的自动接回。PTY 掉了但 Zellij 还在时，不必等用户点。
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

/// 轮询到 still() 返回 false 为止，或用完次数。
/// 超时不算失败：调用方还有退避重试，实在删不掉会如实报给用户。
async fn wait_until_gone<F, Fut>(still: F)
where
    F: Fn() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..10 {
        if !still().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[cfg(test)]
mod tests;
