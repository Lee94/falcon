//! Agent 浏览器管理器（跑在 Engine 的单线程 LocalSet 上）。
//!
//! - 每个浏览器实例只维持一条上游 stream 连接，分发给查看它的所有窗口；
//! - 每个窗口只保留最新一帧（旧帧覆盖），只要有一个看得见的窗口回 ack 就向上游 ack；
//! - 没人看时不向上游 ack，仅在每次 `result` 之后放行一帧用来存缩略图；
//! - 接管锁按会话存在服务端，通过原子写 action policy 文件拦住 agent，全部窗口断线满 30 秒自动交还。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use falcon_proto::{
    BrowserBrief, BrowserClientMessage, BrowserControl, BrowserControlHolder, BrowserEventKind, BrowserLogPage,
    BrowserServerMessage, BrowserStatus, BrowserTab, encode_browser_frame,
};
use futures::{SinkExt as _, StreamExt as _};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::Message as TungsteniteMsg;

use super::discovery::{DiscoveredBrowser, scan_run_dir};
use super::log::{SessionLog, read_thumb, remove_session_logs};
use super::policy::{
    is_allowed_during_takeover, remove_policy, reset_all_to_allow, settings_overrides_browser_policy, write_allow,
    write_takeover,
};
use super::upstream::{
    FrameMeta, UpstreamMessage, encode_ack, encode_key, encode_pointer, encode_text, encode_wheel,
    parse_upstream_message, translate_normalized_coords, upstream_url,
};
use super::{BrowserKey, browser_session_name, now_ms, run_dir};
use crate::db::Db;

const SCAN_INTERVAL: Duration = Duration::from_secs(2);
const RELEASE_GRACE: Duration = Duration::from_secs(30);
const FRAME_META_HISTORY: usize = 32;

static VIEWER_ID: AtomicU64 = AtomicU64::new(1);

struct TerminalTakeover {
    since: i64,
    policy_written: bool,
    release_timer: Option<u64>,
}

/// 一个连到 `/ws/browser/:sessionId` 的下游窗口句柄。
pub struct BrowserViewer {
    pub id: u64,
    ctrl_tx: mpsc::UnboundedSender<String>,
    frame_slot: Arc<LatestFrameSlot>,
    visible: Cell<bool>,
}

/// 单槽最新帧缓冲：新帧到来直接覆盖旧帧，慢链路不会积压。
pub struct LatestFrameSlot {
    frame: Mutex<Option<Bytes>>,
    notify: Notify,
}

impl LatestFrameSlot {
    fn new() -> Self {
        Self { frame: Mutex::new(None), notify: Notify::new() }
    }

    fn push(&self, frame: Bytes) {
        if let Ok(mut guard) = self.frame.lock() {
            *guard = Some(frame);
        }
        self.notify.notify_one();
    }

    pub async fn next(&self) -> Bytes {
        loop {
            let notified = self.notify.notified();
            if let Ok(mut guard) = self.frame.lock()
                && let Some(frame) = guard.take()
            {
                return frame;
            }
            notified.await;
        }
    }
}

/// 交给 WS 写端任务的接收端。
pub struct BrowserViewerRx {
    pub ctrl_rx: mpsc::UnboundedReceiver<String>,
    pub frame_slot: Arc<LatestFrameSlot>,
}

impl BrowserViewer {
    pub fn new() -> (Self, BrowserViewerRx) {
        let (ctrl_tx, ctrl_rx) = mpsc::unbounded_channel();
        let frame_slot = Arc::new(LatestFrameSlot::new());
        let viewer = Self {
            id: VIEWER_ID.fetch_add(1, Ordering::Relaxed),
            ctrl_tx,
            frame_slot: frame_slot.clone(),
            visible: Cell::new(true),
        };
        (viewer, BrowserViewerRx { ctrl_rx, frame_slot })
    }

    pub fn send_json(&self, msg: &BrowserServerMessage) {
        if let Ok(text) = serde_json::to_string(msg) {
            let _ = self.ctrl_tx.send(text);
        }
    }

    pub fn send_frame(&self, frame: Bytes) {
        self.frame_slot.push(frame);
    }

    pub fn is_visible(&self) -> bool {
        self.visible.get()
    }

    pub fn set_visible(&self, visible: bool) {
        self.visible.set(visible);
    }
}

struct SessionBrowser {
    session_id: String,
    browser_session: String,
    discovered: Option<DiscoveredBrowser>,
    alive: bool,
    status: BrowserStatus,
    url: String,
    title: String,
    tabs: Vec<BrowserTab>,
    control: BrowserControl,
    /// 上游实际执行了接管期间应拒绝的操作。扫描不能撤销这个判断。
    policy_bypassed: bool,
    /// 当前活跃的上游连接发送端
    upstream_tx: Option<mpsc::UnboundedSender<String>>,
    /// 当前上游连接的代次（区分重连后迟到的退出通知）
    upstream_gen: u64,
    /// 正在运行中的上游任务句柄
    upstream_task: Option<tokio::task::JoinHandle<()>>,
    /// 最近一帧二进制消息（供新连上来的窗口和浏览器关闭后保留最后一帧）
    last_frame: Option<Bytes>,
    /// 最近几帧的尺寸信息（seq -> FrameMeta），用于把归一化坐标换算成页面坐标
    frame_metas: VecDeque<FrameMeta>,
    /// 单调兜底 seq（若上游某帧未带 seq）
    seq_counter: u64,
    /// 上游正在等待 ack 的帧 seq
    awaiting_ack_seq: Option<u64>,
    /// 收到 result 后待采集缩略图的事件 id
    pending_thumb_event_id: Option<u64>,
    /// 接管期间正在执行中的命令（command_id -> action），用于在 result ok 时判断锁是否被命令行参数绕过
    inflight_commands: HashMap<String, String>,
    /// 下游连接
    viewers: HashMap<u64, Rc<BrowserViewer>>,
    /// 操作记录存储
    log: SessionLog,
}

impl SessionBrowser {
    fn new(data_dir: &Path, key: &BrowserKey) -> Self {
        let storage_id = key.storage_id();
        Self {
            log: SessionLog::load(data_dir, &storage_id),
            session_id: key.session_id.clone(),
            browser_session: key.browser_session.clone(),
            discovered: None,
            alive: false,
            status: BrowserStatus::default(),
            url: String::new(),
            title: String::new(),
            tabs: Vec::new(),
            control: BrowserControl::default(),
            policy_bypassed: false,
            upstream_tx: None,
            upstream_gen: 0,
            upstream_task: None,
            last_frame: None,
            frame_metas: VecDeque::with_capacity(FRAME_META_HISTORY),
            seq_counter: 0,
            awaiting_ack_seq: None,
            pending_thumb_event_id: None,
            inflight_commands: HashMap::new(),
            viewers: HashMap::new(),
        }
    }

    fn has_visible_viewer(&self) -> bool {
        self.viewers.values().any(|v| v.is_visible())
    }

    fn broadcast(&self, msg: &BrowserServerMessage) {
        if self.viewers.is_empty() {
            return;
        }
        let Ok(text) = serde_json::to_string(msg) else { return };
        for v in self.viewers.values() {
            let _ = v.ctrl_tx.send(text.clone());
        }
    }

    fn send_upstream(&self, text: String) {
        if let Some(tx) = &self.upstream_tx {
            let _ = tx.send(text);
        }
    }

    fn ack_upstream(&mut self, seq: u64) {
        if self.awaiting_ack_seq == Some(seq) {
            self.send_upstream(encode_ack(seq));
            self.awaiting_ack_seq = None;
        }
    }

    fn lookup_meta(&self, seq: u64) -> Option<FrameMeta> {
        self.frame_metas
            .iter()
            .rev()
            .find(|m| m.seq == seq)
            .or_else(|| self.frame_metas.back())
            .copied()
    }

    fn brief(&self) -> Option<BrowserBrief> {
        self.alive.then(|| BrowserBrief {
            // 网关会给终端 ID 加节点前缀。默认浏览器用缺省名称，客户端无需解码原始 ID。
            browser_session: if self.browser_session == browser_session_name(&self.session_id) {
                String::new()
            } else {
                self.browser_session.clone()
            },
            url: self.url.clone(),
            title: self.title.clone(),
            control: self.control.holder,
        })
    }
}

pub struct BrowserManager {
    data_dir: PathBuf,
    db: Arc<Db>,
    entries: RefCell<HashMap<BrowserKey, SessionBrowser>>,
    /// 启动会话时记录的 policy 生效状态（还没发现浏览器之前先记下）
    policy_enforced_flags: RefCell<HashMap<String, bool>>,
    /// 终端级接管状态，新出现的浏览器也使用同一把锁。
    takeovers: RefCell<HashMap<String, TerminalTakeover>>,
    next_generation: Cell<u64>,
    /// 已记录过"版本过低"日志的实例与 pid，避免 2 秒扫描刷屏
    logged_rejected: RefCell<HashSet<(BrowserKey, u32)>>,
    watcher: RefCell<Option<RecommendedWatcher>>,
    scan_task: RefCell<Option<tokio::task::JoinHandle<()>>>,
}

impl BrowserManager {
    pub fn new(data_dir: PathBuf, db: Arc<Db>) -> Rc<Self> {
        Rc::new(Self {
            data_dir,
            db,
            entries: RefCell::new(HashMap::new()),
            policy_enforced_flags: RefCell::new(HashMap::new()),
            takeovers: RefCell::new(HashMap::new()),
            next_generation: Cell::new(0),
            logged_rejected: RefCell::new(HashSet::new()),
            watcher: RefCell::new(None),
            scan_task: RefCell::new(None),
        })
    }

    /// 服务端启动时调用：重置所有 policy 为 allow，启动目录监听与 2 秒兜底扫描。
    pub fn startup(self: &Rc<Self>) {
        reset_all_to_allow(&self.data_dir);
        let run = run_dir(&self.data_dir);
        if let Err(e) = std::fs::create_dir_all(&run) {
            log::warn!("创建 agent-browser run 目录失败：{e}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700));
        }

        let (notify_tx, mut notify_rx) = mpsc::unbounded_channel::<()>();
        let watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            if res.is_ok() {
                let _ = notify_tx.send(());
            }
        });
        match watcher {
            Ok(mut w) => {
                if let Err(e) = w.watch(&run, RecursiveMode::NonRecursive) {
                    log::warn!("监听 agent-browser run 目录失败（将依赖 2 秒兜底扫描）：{e}");
                } else {
                    *self.watcher.borrow_mut() = Some(w);
                }
            }
            Err(e) => {
                log::warn!("初始化 agent-browser 目录监听器失败（将依赖 2 秒兜底扫描）：{e}");
            }
        }

        self.scan_now();
        let weak = Rc::downgrade(self);
        let task = tokio::task::spawn_local(async move {
            let mut interval = tokio::time::interval(SCAN_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = interval.tick() => {}
                    ev = notify_rx.recv(), if !notify_rx.is_closed() => {
                        if ev.is_none() {
                            // 监听器初始化失败时仍保留定时扫描。
                            continue;
                        }
                        // 合并同一波文件系统事件
                        while notify_rx.try_recv().is_ok() {}
                    }
                }
                let Some(mgr) = weak.upgrade() else { break };
                mgr.scan_now();
            }
        });
        *self.scan_task.borrow_mut() = Some(task);
    }

    /// 服务端退出前清理：停掉监听与上游连接，并把所有 policy 重置成 allow。
    pub fn shutdown(&self) {
        if let Some(t) = self.scan_task.borrow_mut().take() {
            t.abort();
        }
        self.watcher.borrow_mut().take();
        for entry in self.entries.borrow_mut().values_mut() {
            if let Some(t) = entry.upstream_task.take() {
                t.abort();
            }
            entry.upstream_tx = None;
        }
        reset_all_to_allow(&self.data_dir);
    }

    /// 终端会话启动时调用，记下该会话的 policy 环境变量是否受 falcon 掌控。
    pub fn record_session_policy_enforced(&self, session_id: &str, enforced: bool) {
        self.policy_enforced_flags.borrow_mut().insert(session_id.to_string(), enforced);
        if enforced && let Some(takeover) = self.takeovers.borrow_mut().get_mut(session_id) {
            // 原地重开终端时，启动流程会先写默认 policy。已有浏览器仍处于接管，恢复它们的锁。
            takeover.policy_written = match write_takeover(&self.data_dir, session_id) {
                Ok(_) => true,
                Err(error) => {
                    log::warn!("恢复浏览器接管规则失败（{session_id}）：{error}");
                    false
                }
            };
        }
        for entry in self.entries.borrow_mut().values_mut().filter(|e| e.session_id == session_id) {
            self.refresh_control(entry);
        }
    }

    fn refresh_control(&self, entry: &mut SessionBrowser) {
        let takeovers = self.takeovers.borrow();
        let takeover = takeovers.get(&entry.session_id);
        let control = BrowserControl {
            holder: if takeover.is_some() { BrowserControlHolder::Human } else { BrowserControlHolder::Agent },
            since: takeover.map(|state| state.since),
            policy_enforced: takeover.is_none_or(|state| state.policy_written)
                && !entry.policy_bypassed
                && self.policy_enforced_flags.borrow().get(&entry.session_id).copied().unwrap_or(true)
                && !settings_overrides_browser_policy(&self.data_dir, &entry.session_id, &entry.browser_session),
        };
        if entry.control != control {
            entry.broadcast(&BrowserServerMessage::Control {
                holder: control.holder,
                since: control.since,
                policy_enforced: control.policy_enforced,
            });
            entry.control = control;
        }
    }

    /// 默认实例的名称为空，按名称排序自然会排在最前面。
    pub fn briefs_of(&self, session_id: &str) -> Vec<BrowserBrief> {
        let mut briefs: Vec<_> = self
            .entries
            .borrow()
            .values()
            .filter(|entry| entry.session_id == session_id)
            .filter_map(SessionBrowser::brief)
            .collect();
        briefs.sort_by(|a, b| a.browser_session.cmp(&b.browser_session));
        briefs
    }

    fn next_generation(&self) -> u64 {
        let generation = self.next_generation.get().wrapping_add(1);
        self.next_generation.set(generation);
        generation
    }

    /// 扫描 `run/` 目录，把发现的浏览器实例连上、消失的标记为关闭。
    pub fn scan_now(self: &Rc<Self>) {
        let mut report = scan_run_dir(&run_dir(&self.data_dir));
        // 已删除终端的浏览器进程可能继续运行，不能用遗留的运行文件复活 Falcon 记录。
        let mut owners = HashMap::new();
        report.active.retain(|key, _| {
            *owners.entry(key.session_id.clone()).or_insert_with(|| self.db.get_session(&key.session_id).is_some())
        });
        for rej in report.rejected {
            let key = (rej.key.clone(), rej.pid);
            if self.logged_rejected.borrow_mut().insert(key) {
                log::warn!(
                    "忽略版本过低的 agent-browser 实例 {} / {}（pid {}，版本 {:?}，最低要求 {}.{}.{}）",
                    rej.key.session_id,
                    rej.key.browser_session,
                    rej.pid,
                    rej.raw_version,
                    falcon_proto::MIN_AGENT_BROWSER_VERSION.0,
                    falcon_proto::MIN_AGENT_BROWSER_VERSION.1,
                    falcon_proto::MIN_AGENT_BROWSER_VERSION.2
                );
            }
        }

        let mut to_connect: Vec<DiscoveredBrowser> = Vec::new();
        let mut gone_sessions: Vec<BrowserKey> = Vec::new();

        {
            let mut entries = self.entries.borrow_mut();
            for (key, found) in &report.active {
                let entry = entries
                    .entry(key.clone())
                    .or_insert_with(|| SessionBrowser::new(&self.data_dir, key));
                let replaced = entry.discovered.as_ref() != Some(found);
                if replaced {
                    entry.policy_bypassed = false;
                    entry.pending_thumb_event_id = None;
                    entry.last_frame = None;
                }
                self.refresh_control(entry);
                entry.discovered = Some(found.clone());
                entry.alive = true;
                if !entry.viewers.is_empty()
                    && let Some(takeover) = self.takeovers.borrow_mut().get_mut(&key.session_id)
                {
                    takeover.release_timer = None;
                }
                if replaced || entry.upstream_task.is_none() {
                    to_connect.push(found.clone());
                }
            }

            for (key, entry) in entries.iter_mut() {
                if entry.alive && !report.active.contains_key(key) {
                    gone_sessions.push(key.clone());
                }
            }
        }

        for found in to_connect {
            self.connect_upstream(found);
        }
        for key in gone_sessions {
            self.mark_gone(&key);
        }
    }

    fn connect_upstream(self: &Rc<Self>, found: DiscoveredBrowser) {
        let key = found.key.clone();
        let generation = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.get_mut(&key) else { return };
            if let Some(old) = entry.upstream_task.take() {
                old.abort();
            }
            entry.upstream_gen = self.next_generation();
            entry.upstream_tx = None;
            entry.status = BrowserStatus::default();
            entry.awaiting_ack_seq = None;
            entry.frame_metas.clear();
            entry.inflight_commands.clear();
            entry.upstream_gen
        };

        let weak = Rc::downgrade(self);
        let task_key = key.clone();
        let port = found.port;
        let task = tokio::task::spawn_local(async move {
            let url = upstream_url(port);
            let tcp = match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(s) => s,
                Err(e) => {
                    log::debug!("连接 agent-browser 上游失败（{} 127.0.0.1:{port}）：{e}", task_key.browser_session);
                    if let Some(mgr) = weak.upgrade() {
                        mgr.on_upstream_closed(&task_key, generation);
                    }
                    return;
                }
            };
            let _ = tcp.set_nodelay(true);
            let (ws, _) = match tokio_tungstenite::client_async(&url, tcp).await {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("agent-browser 上游握手失败（{} {url}）：{e}", task_key.browser_session);
                    if let Some(mgr) = weak.upgrade() {
                        mgr.on_upstream_closed(&task_key, generation);
                    }
                    return;
                }
            };

            let (mut up_tx, mut up_rx) = ws.split();
            let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();

            if let Some(mgr) = weak.upgrade() {
                let mut entries = mgr.entries.borrow_mut();
                if let Some(entry) = entries.get_mut(&task_key)
                    && entry.upstream_gen == generation
                {
                    entry.upstream_tx = Some(out_tx);
                    entry.status.connected = true;
                    entry.broadcast(&BrowserServerMessage::Status {
                        status: entry.status.clone(),
                        url: Some(entry.url.clone()),
                        title: Some(entry.title.clone()),
                        tabs: Some(entry.tabs.clone()),
                    });
                } else {
                    return;
                }
            } else {
                return;
            }

            loop {
                tokio::select! {
                    outgoing = out_rx.recv() => {
                        let Some(text) = outgoing else { break };
                        if up_tx.send(TungsteniteMsg::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    incoming = up_rx.next() => match incoming {
                        Some(Ok(TungsteniteMsg::Text(text))) => {
                            let Some(mgr) = weak.upgrade() else { break };
                            mgr.on_upstream_text(&task_key, generation, text.as_str());
                        }
                        Some(Ok(TungsteniteMsg::Ping(_))) => {
                            if up_tx.flush().await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(TungsteniteMsg::Close(_)) | Err(_)) | None => break,
                        _ => {}
                    }
                }
            }
            if let Some(mgr) = weak.upgrade() {
                mgr.on_upstream_closed(&task_key, generation);
            }
        });

        if let Some(entry) = self.entries.borrow_mut().get_mut(&key)
            && entry.upstream_gen == generation
        {
            entry.upstream_task = Some(task);
        }
    }

    fn on_upstream_text(&self, key: &BrowserKey, generation: u64, text: &str) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(key) else { return };
        if entry.upstream_gen != generation {
            return;
        }
        entry.seq_counter = entry.seq_counter.wrapping_add(1);
        let fallback_seq = entry.seq_counter;
        let Some(parsed) = parse_upstream_message(text, fallback_seq) else { return };
        match parsed {
            UpstreamMessage::Frame {
                meta,
                image_width,
                image_height,
                jpeg,
            } => {
                if let Some(eid) = entry.pending_thumb_event_id.take()
                    && let Some(thumb_id) = entry.log.save_thumb(eid, &jpeg)
                {
                    entry.broadcast(&BrowserServerMessage::Thumb { event_id: eid, thumb_id });
                }
                entry.frame_metas.push_back(meta);
                if entry.frame_metas.len() > FRAME_META_HISTORY {
                    entry.frame_metas.pop_front();
                }
                let header = meta.to_header(image_width, image_height);
                let frame = Bytes::from(encode_browser_frame(&header, &jpeg));
                entry.last_frame = Some(frame.clone());
                entry.awaiting_ack_seq = Some(meta.seq);
                for viewer in entry.viewers.values() {
                    if viewer.is_visible() {
                        viewer.send_frame(frame.clone());
                    }
                }
            }
            UpstreamMessage::Status(status) => {
                entry.status = status.clone();
                entry.broadcast(&BrowserServerMessage::Status {
                    status,
                    url: None,
                    title: None,
                    tabs: None,
                });
            }
            UpstreamMessage::Url { url, title } => {
                let changed = entry.url != url || title.as_deref().is_some_and(|t| t != entry.title);
                entry.url = url.clone();
                if let Some(t) = &title {
                    entry.title = t.clone();
                }
                entry.broadcast(&BrowserServerMessage::Status {
                    status: entry.status.clone(),
                    url: Some(entry.url.clone()),
                    title: Some(entry.title.clone()),
                    tabs: None,
                });
                if changed && !url.is_empty() {
                    let ev = entry.log.append(now_ms(), BrowserEventKind::Url { url, title });
                    entry.broadcast(&BrowserServerMessage::Event { event: ev });
                }
            }
            UpstreamMessage::Tabs(tabs) => {
                if let Some(active) = tabs.iter().find(|t| t.active) {
                    if !active.url.is_empty() {
                        entry.url = active.url.clone();
                    }
                    if !active.title.is_empty() {
                        entry.title = active.title.clone();
                    }
                }
                entry.tabs = tabs.clone();
                entry.broadcast(&BrowserServerMessage::Status {
                    status: entry.status.clone(),
                    url: Some(entry.url.clone()),
                    title: Some(entry.title.clone()),
                    tabs: Some(tabs),
                });
            }
            UpstreamMessage::Event(kind) => {
                // 接管期间追踪非只读命令：如果它执行成功，说明 agent 用 `--action-policy` 绕过了锁
                if let BrowserEventKind::Command { ref action, ref command_id, .. } = kind
                    && entry.control.holder == BrowserControlHolder::Human
                    && let Some(cid) = command_id
                {
                    entry.inflight_commands.insert(cid.clone(), action.clone());
                }
                if let BrowserEventKind::Result { ref action, ok, ref command_id, .. } = kind {
                    let tracked_action = command_id.as_ref().and_then(|cid| entry.inflight_commands.remove(cid));
                    let resolved_action = action.as_deref().or(tracked_action.as_deref());
                    if ok
                        && entry.control.holder == BrowserControlHolder::Human
                        && entry.control.policy_enforced
                        && resolved_action.is_some_and(|a| !is_allowed_during_takeover(a))
                    {
                        entry.policy_bypassed = true;
                        self.refresh_control(entry);
                    }
                }

                let is_result = matches!(kind, BrowserEventKind::Result { .. });
                let ev = entry.log.append(now_ms(), kind);
                let ev_id = ev.id;
                entry.broadcast(&BrowserServerMessage::Event { event: ev });

                if is_result {
                    entry.pending_thumb_event_id = Some(ev_id);
                    // 没人看时放行一帧以捕获本次 result 之后的缩略图（ADR 0029 决定四）
                    if !entry.has_visible_viewer()
                        && let Some(seq) = entry.awaiting_ack_seq
                    {
                        entry.ack_upstream(seq);
                    }
                }
            }
        }
    }

    fn on_upstream_closed(&self, key: &BrowserKey, generation: u64) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(key) else { return };
        if entry.upstream_gen != generation {
            return;
        }
        entry.upstream_tx = None;
        entry.upstream_task = None;
        entry.status.connected = false;
        // 下一次目录扫描决定是重连还是发送 Gone，避免失败时用旧端口立即循环重连。
    }

    fn mark_gone(self: &Rc<Self>, key: &BrowserKey) {
        let release_terminal = {
            let mut entries = self.entries.borrow_mut();
            let Some(entry) = entries.get_mut(key) else { return };
            if !entry.alive {
                return;
            }
            entry.alive = false;
            entry.discovered = None;
            entry.status.connected = false;
            entry.status.screencasting = false;
            if let Some(t) = entry.upstream_task.take() {
                t.abort();
            }
            entry.upstream_tx = None;
            entry.broadcast(&BrowserServerMessage::Gone);
            !entries.values().any(|candidate| candidate.session_id == key.session_id && candidate.alive)
        };
        // 锁属于整个终端：同终端还有其他存活浏览器时，关掉其中一个不能提前交还。
        if release_terminal {
            self.release(key);
        } else {
            self.schedule_release_if_unwatched(key);
        }
        self.prune_closed_entry(key);
    }

    /// 新浏览器 WS 连接接入。
    pub fn add_viewer(&self, key: &BrowserKey, viewer: Rc<BrowserViewer>) {
        if self.db.get_session(&key.session_id).is_none() {
            viewer.send_json(&BrowserServerMessage::Gone);
            return;
        }
        let mut entries = self.entries.borrow_mut();
        let entry = entries.entry(key.clone()).or_insert_with(|| SessionBrowser::new(&self.data_dir, key));
        self.refresh_control(entry);
        if entry.alive && let Some(takeover) = self.takeovers.borrow_mut().get_mut(&key.session_id) {
            takeover.release_timer = None;
        }
        viewer.send_json(&BrowserServerMessage::Hello {
            control: entry.control.clone(),
            status: entry.status.clone(),
            url: entry.url.clone(),
            title: entry.title.clone(),
            tabs: entry.tabs.clone(),
            gone: !entry.alive,
        });
        if let Some(frame) = &entry.last_frame {
            viewer.send_frame(frame.clone());
        }
        entry.viewers.insert(viewer.id, viewer);
    }

    /// 只统计连着存活浏览器的窗口；显示「浏览器已关闭」的旧窗口不能一直持有接管锁。
    fn is_watched(&self, session_id: &str) -> bool {
        self.entries.borrow().values().any(|entry| {
            entry.session_id == session_id && entry.alive && !entry.viewers.is_empty()
        })
    }

    pub fn remove_viewer(self: &Rc<Self>, key: &BrowserKey, viewer_id: u64) {
        if let Some(entry) = self.entries.borrow_mut().get_mut(key) {
            entry.viewers.remove(&viewer_id);
        }
        self.schedule_release_if_unwatched(key);
        self.prune_closed_entry(key);
    }

    fn prune_closed_entry(&self, key: &BrowserKey) {
        let mut entries = self.entries.borrow_mut();
        if entries.get(key).is_some_and(|entry| !entry.alive && entry.viewers.is_empty()) {
            entries.remove(key);
        }
    }

    fn schedule_release_if_unwatched(self: &Rc<Self>, key: &BrowserKey) {
        if self.is_watched(&key.session_id) {
            return;
        }
        let generation = {
            let mut takeovers = self.takeovers.borrow_mut();
            let Some(state) = takeovers.get_mut(&key.session_id) else { return };
            if state.release_timer.is_some() {
                return;
            }
            let generation = self.next_generation();
            state.release_timer = Some(generation);
            generation
        };
        let weak = Rc::downgrade(self);
        let key = key.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(RELEASE_GRACE).await;
            let Some(mgr) = weak.upgrade() else { return };
            mgr.release_if_timer_matches(&key, generation);
        });
    }

    fn release_if_timer_matches(&self, key: &BrowserKey, generation: u64) {
        let current = self.takeovers.borrow().get(&key.session_id).and_then(|state| state.release_timer);
        if current == Some(generation) && !self.is_watched(&key.session_id) {
            self.release(key);
        }
    }

    /// 处理下游窗口发来的控制 / 输入消息。
    pub fn handle_client_message(&self, key: &BrowserKey, viewer_id: u64, msg: BrowserClientMessage) {
        let mut entries = self.entries.borrow_mut();
        let Some(entry) = entries.get_mut(key) else { return };
        let Some(viewer) = entry.viewers.get(&viewer_id) else { return };
        match msg {
            BrowserClientMessage::Takeover { device } if entry.alive => {
                drop(entries);
                self.takeover(key, device);
            }
            BrowserClientMessage::Release if entry.alive => {
                drop(entries);
                self.release(key);
            }
            BrowserClientMessage::Ack { seq } => {
                if viewer.is_visible() {
                    entry.ack_upstream(seq);
                }
            }
            BrowserClientMessage::Visibility { visible } => {
                let was_visible = viewer.is_visible();
                viewer.set_visible(visible);
                if visible && !was_visible && let Some(frame) = &entry.last_frame {
                    viewer.send_frame(frame.clone());
                }
            }
            BrowserClientMessage::Takeover { .. } | BrowserClientMessage::Release => {}
            BrowserClientMessage::Pointer {
                kind,
                x,
                y,
                button,
                clicks,
                modifiers,
                seq,
            } => {
                if entry.control.holder != BrowserControlHolder::Human || !entry.alive {
                    return;
                }
                let Some(meta) = entry.lookup_meta(seq) else { return };
                let (px, py) = translate_normalized_coords(x, y, meta);
                if let Some(payload) = encode_pointer(kind, px, py, button, clicks, modifiers) {
                    entry.send_upstream(payload);
                }
            }
            BrowserClientMessage::Wheel { x, y, dx, dy, seq } => {
                if entry.control.holder != BrowserControlHolder::Human || !entry.alive {
                    return;
                }
                let Some(meta) = entry.lookup_meta(seq) else { return };
                let (px, py) = translate_normalized_coords(x, y, meta);
                entry.send_upstream(encode_wheel(px, py, dx, dy));
            }
            BrowserClientMessage::Key {
                kind,
                key,
                code,
                modifiers,
            } => {
                if entry.control.holder != BrowserControlHolder::Human || !entry.alive {
                    return;
                }
                if let Some(payload) = encode_key(kind, &key, &code, modifiers) {
                    entry.send_upstream(payload);
                }
            }
            BrowserClientMessage::Text { text } => {
                if entry.control.holder != BrowserControlHolder::Human || !entry.alive || text.is_empty() {
                    return;
                }
                // 逐字发 char，兼顾粘贴长文本与输入法上屏（ADR 0029 决定六）
                for ch in text.chars() {
                    let mut buf = [0u8; 4];
                    entry.send_upstream(encode_text(ch.encode_utf8(&mut buf)));
                }
            }
            BrowserClientMessage::Unknown => {}
        }
    }

    fn takeover(&self, key: &BrowserKey, device: Option<String>) {
        if self.takeovers.borrow().contains_key(&key.session_id) {
            return;
        }
        let policy_written = match write_takeover(&self.data_dir, &key.session_id) {
            Ok(_) => true,
            Err(error) => {
                log::warn!("写入浏览器接管规则失败（{}）：{error}", key.session_id);
                false
            }
        };
        self.takeovers.borrow_mut().insert(key.session_id.clone(), TerminalTakeover {
            since: now_ms(),
            policy_written,
            release_timer: None,
        });
        self.publish_control(key, BrowserControlHolder::Human, device);
    }

    fn release(&self, key: &BrowserKey) {
        if !self.takeovers.borrow().contains_key(&key.session_id) {
            return;
        }
        if let Err(error) = write_allow(&self.data_dir, &key.session_id) {
            log::warn!("解除浏览器接管规则失败（{}）：{error}", key.session_id);
            if let Some(state) = self.takeovers.borrow_mut().get_mut(&key.session_id) {
                state.release_timer = None;
            }
            return;
        }
        self.takeovers.borrow_mut().remove(&key.session_id);
        self.publish_control(key, BrowserControlHolder::Agent, None);
    }

    /// 输入仍只发往当前窗口对应的实例，接管状态则同步到同终端的全部实例。
    fn publish_control(&self, key: &BrowserKey, holder: BrowserControlHolder, device: Option<String>) {
        let mut entries = self.entries.borrow_mut();
        for entry in entries.values_mut().filter(|entry| entry.session_id == key.session_id) {
            entry.inflight_commands.clear();
            entry.policy_bypassed = false;
            self.refresh_control(entry);
        }
        // 宽限期结束时，触发计时的窗口可能已经被回收。它的记录仍在远端磁盘上。
        let event = BrowserEventKind::Control { holder, device };
        if let Some(entry) = entries.get_mut(key) {
            let event = entry.log.append(now_ms(), event);
            entry.broadcast(&BrowserServerMessage::Event { event });
        } else {
            SessionLog::load(&self.data_dir, &key.storage_id()).append(now_ms(), event);
        }
    }

    /// `GET /api/sessions/:id/browser/log?browserSession=...`
    pub fn log_page(&self, key: &BrowserKey, before: Option<u64>, limit: usize) -> BrowserLogPage {
        if let Some(entry) = self.entries.borrow().get(key) {
            return entry.log.query(before, limit);
        }
        SessionLog::load(&self.data_dir, &key.storage_id()).query(before, limit)
    }

    /// `GET /api/sessions/:id/browser/thumbs/:thumbId?browserSession=...`
    pub fn read_thumb(&self, key: &BrowserKey, thumb_id: &str) -> Option<Vec<u8>> {
        read_thumb(&self.data_dir, &key.storage_id(), thumb_id)
    }

    /// 终端会话删除 / 终止时清理 policy、全部实例的记录、缩略图与内存状态。
    pub fn cleanup_session(&self, session_id: &str) {
        let removed: Vec<_> = {
            let mut entries = self.entries.borrow_mut();
            let keys: Vec<_> = entries.keys().filter(|key| key.session_id == session_id).cloned().collect();
            keys.into_iter().filter_map(|key| entries.remove(&key)).collect()
        };
        for mut entry in removed {
            if let Some(t) = entry.upstream_task.take() {
                t.abort();
            }
            entry.broadcast(&BrowserServerMessage::Gone);
        }
        self.policy_enforced_flags.borrow_mut().remove(session_id);
        self.takeovers.borrow_mut().remove(session_id);
        remove_policy(&self.data_dir, session_id);
        remove_session_logs(&self.data_dir, session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use falcon_proto::decode_browser_frame;
    use serde_json::json;
    use tokio::net::TcpListener;

    #[tokio::test(start_paused = true)]
    async fn takeover_includes_new_instances_and_releases_after_last_live_viewer() {
        tokio::task::LocalSet::new().run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let db = Arc::new(Db::open(dir.path()).unwrap());
            let session_id = "terminal";
            db.insert_session(&crate::db::SessionRow { id: session_id.into(), state: "active".into(), ..Default::default() });
            let mgr = BrowserManager::new(dir.path().to_path_buf(), db);
            let open = |name| {
                let key = BrowserKey::new(session_id, name).unwrap();
                let mut entry = SessionBrowser::new(dir.path(), &key);
                entry.alive = true;
                mgr.entries.borrow_mut().insert(key.clone(), entry);
                let (viewer, rx) = BrowserViewer::new();
                let id = viewer.id;
                mgr.add_viewer(&key, Rc::new(viewer));
                (key, id, rx)
            };

            let (first, first_id, _first_rx) = open("first");
            mgr.handle_client_message(&first, first_id, BrowserClientMessage::Takeover { device: None });
            let policy = super::super::policy::policy_path(dir.path(), session_id);
            let (second, second_id, mut second_rx) = open("second");
            let hello = second_rx.ctrl_rx.recv().await.unwrap();
            assert!(matches!(serde_json::from_str(&hello), Ok(BrowserServerMessage::Hello {
                control: BrowserControl { holder: BrowserControlHolder::Human, .. }, ..
            })));

            // 第一个窗口还连着，但它显示的是已经关闭的浏览器。
            mgr.mark_gone(&first);
            assert!(mgr.takeovers.borrow()[session_id].release_timer.is_none());
            mgr.remove_viewer(&second, second_id);
            tokio::task::yield_now().await;
            tokio::time::advance(RELEASE_GRACE - Duration::from_secs(1)).await;
            assert_eq!(std::fs::read_to_string(&policy).unwrap(), super::super::policy::TAKEOVER_POLICY_JSON);
            tokio::time::advance(Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
            assert_eq!(std::fs::read_to_string(&policy).unwrap(), super::super::policy::ALLOW_POLICY_JSON);
            assert!(mgr.takeovers.borrow().is_empty());
        }).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn end_to_end_discovery_fanout_ack_and_takeover_policy() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let port = listener.local_addr().unwrap().port();

                let (up_in_tx, mut up_in_rx) = mpsc::unbounded_channel::<String>();
                let (up_out_tx, mut up_out_rx) = mpsc::unbounded_channel::<String>();
                let server_task = tokio::task::spawn_local(async move {
                    let (tcp, _) = listener.accept().await.unwrap();
                    let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
                    let (mut tx, mut rx) = ws.split();
                    loop {
                        tokio::select! {
                            Some(out) = up_out_rx.recv() => {
                                if tx.send(TungsteniteMsg::Text(out.into())).await.is_err() {
                                    break;
                                }
                            }
                            msg = rx.next() => match msg {
                                Some(Ok(TungsteniteMsg::Text(t))) => {
                                    let _ = up_in_tx.send(t.to_string());
                                }
                                _ => break,
                            }
                        }
                    }
                });

                // 准备 discovery 文件
                let run = run_dir(dir.path());
                std::fs::create_dir_all(&run).unwrap();
                std::fs::write(run.join("falcon-s1.stream"), format!("{port}\n")).unwrap();
                std::fs::write(run.join("falcon-s1.pid"), format!("{}\n", std::process::id())).unwrap();
                std::fs::write(run.join("falcon-s1.version"), "0.43.0\n").unwrap();
                write_allow(dir.path(), "s1").unwrap();
                let key = BrowserKey::for_request("s1", None).unwrap();

                let db = Arc::new(Db::open(dir.path()).unwrap());
                db.insert_session(&crate::db::SessionRow { id: "s1".into(), state: "active".into(), ..Default::default() });
                let mgr = BrowserManager::new(dir.path().to_path_buf(), db);
                mgr.scan_now();

                // 等上游连上
                for _ in 0..50 {
                    if !mgr.briefs_of("s1").is_empty()
                        && mgr.entries.borrow().get(&key).is_some_and(|e| e.upstream_tx.is_some())
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert!(!mgr.briefs_of("s1").is_empty());

                // 挂一个下游 viewer
                let (viewer, mut rx) = BrowserViewer::new();
                let vid = viewer.id;
                mgr.add_viewer(&key, Rc::new(viewer));
                let hello_raw = rx.ctrl_rx.recv().await.unwrap();
                let hello: BrowserServerMessage = serde_json::from_str(&hello_raw).unwrap();
                assert!(matches!(hello, BrowserServerMessage::Hello { gone: false, .. }));

                // 上游发一帧 JPEG
                let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
                let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
                up_out_tx
                    .send(
                        json!({
                            "type": "frame",
                            "seq": 7,
                            "data": b64,
                            "metadata": { "deviceWidth": 1000, "deviceHeight": 500, "pageScaleFactor": 1.0, "offsetTop": 0.0 }
                        })
                        .to_string(),
                    )
                    .unwrap();

                let bin = tokio::time::timeout(Duration::from_secs(2), rx.frame_slot.next()).await.unwrap();
                let (hdr, body) = decode_browser_frame(&bin).unwrap();
                assert_eq!(hdr.seq, 7);
                assert_eq!(body, jpeg.as_slice());

                // 没收到 viewer ack 之前，上游不能收到 ack
                assert!(up_in_rx.try_recv().is_err());

                // viewer 回 ack -> 上游收到 ack
                mgr.handle_client_message(&key, vid, BrowserClientMessage::Ack { seq: 7 });
                let ack = tokio::time::timeout(Duration::from_secs(2), up_in_rx.recv()).await.unwrap().unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&ack).unwrap()["seq"], 7);

                // 未接管时输入被丢弃
                mgr.handle_client_message(
                    &key,
                    vid,
                    BrowserClientMessage::Pointer {
                        kind: falcon_proto::PointerKind::Down,
                        x: 0.5,
                        y: 0.5,
                        button: falcon_proto::PointerButton::Left,
                        clicks: 1,
                        modifiers: 0,
                        seq: 7,
                    },
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
                assert!(up_in_rx.try_recv().is_err());

                // 接管 -> policy 文件切换为 deny + allow，输入正常转发并换算坐标
                mgr.handle_client_message(&key, vid, BrowserClientMessage::Takeover { device: Some("Mac".into()) });
                assert_eq!(
                    std::fs::read_to_string(super::super::policy::policy_path(dir.path(), "s1")).unwrap(),
                    super::super::policy::TAKEOVER_POLICY_JSON
                );
                mgr.handle_client_message(
                    &key,
                    vid,
                    BrowserClientMessage::Pointer {
                        kind: falcon_proto::PointerKind::Down,
                        x: 0.5,
                        y: 0.5,
                        button: falcon_proto::PointerButton::Left,
                        clicks: 1,
                        modifiers: 0,
                        seq: 7,
                    },
                );
                let mouse_raw = tokio::time::timeout(Duration::from_secs(2), up_in_rx.recv()).await.unwrap().unwrap();
                let mouse: serde_json::Value = serde_json::from_str(&mouse_raw).unwrap();
                assert_eq!(mouse["type"], "input_mouse");
                assert_eq!(mouse["x"], 500.0);
                assert_eq!(mouse["y"], 250.0);

                // 交还 -> policy 文件恢复 allow
                mgr.handle_client_message(&key, vid, BrowserClientMessage::Release);
                assert_eq!(
                    std::fs::read_to_string(super::super::policy::policy_path(dir.path(), "s1")).unwrap(),
                    super::super::policy::ALLOW_POLICY_JSON
                );

                mgr.shutdown();
                server_task.abort();
            })
            .await;
    }
}
