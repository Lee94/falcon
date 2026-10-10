//! 端口转发运行时，按 SSH Host 挂（ADR 0016）。移植自 `packages/server/src/sessions/forward.ts`。
//! 规则在 DB，活着的隧道在内存。
//!
//! 隧道走主机自己的那条 SshLink（SessionManager.getHostLink），不是哪个项目的：
//! 同一台机器上开着几个项目，转发也只有一份。
//!
//! 链路断开只拆隧道、不删规则：重连成功后（link 的 "up"）按 enabled 再拉起来。
//! 本地监听器也跟着拆——断链期间占着端口会让人以为还通着。
//!
//! 同端口的规则可以有多条，同时只有一条 enabled：启用一条之前先把同槽位
//! （relay_spec::forward_slot）的其它规则落库成 disabled 并停掉，再起这一条——
//! 顺序反了新监听器会撞上旧的还没释放的端口。
//!
//! # 移植约定
//!
//! - 跑在会话核心的 LocalSet 上（单线程，同 Node 的事件循环）：状态是 `RefCell`，后台任务用
//!   `spawn_local`。**任何 `RefCell` 的借用都不跨 await**。
//! - TS 的 Promise 是立即开跑的，Rust 的 future 是惰性的。TS 里 `void this.start(id)` 这种
//!   「起个头就不管」在这里是 `kick`（同步登记在途尝试 + `spawn_local`）；`stop()` 的同步前缀
//!   （代数 +1、作废在 dial 的尝试）在 `stop_eager` 里调用时就做掉，返回的 future 只管等。
//!   这样「响应里是 starting」「先同步落库再异步停起」这些时序与 TS 一致。
//! - 建 / 改规则收的是**原样的请求体 JSON**（`serde_json::Value`），不是 falcon-proto 的
//!   `PortForwardInput` / `PortForwardPatch`：校验层（forward_spec）要保留 TS 的宽松语义
//!   （端口写成数字串也收、名称不是字符串回「名称无效」），见 forward_spec.rs 顶部。
//! - 链路抽成 [`RelayLink`]（生产上是 [`SshLink`]），单测换假链路。[`ForwardManager`] 就是
//!   `ForwardManagerOf<SshLink>`。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use falcon_proto::{ForwardKind, ForwardState, PortForward};
use futures::FutureExt as _;
use futures::future::{Shared, join_all};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::forward_spec::{field, validate_forward_input};
use super::relay::{LinkForOf, RelayLink, bind_local, or_empty, spawn_remote_acceptor, spawn_tunnel_listener};
use super::relay_spec::{ForwardSlotRow, displaced_by, forward_slot};
use super::ssh::SshLink;
use crate::db::{Db, HostForwardRow};
use crate::exec::LocalBoxFuture;

pub use super::relay::{LinkFor, RelayError, WantReconnect};

struct LiveForward {
    id: String,
    host_id: String,
    kind: ForwardKind,
    bind_host: String,
    bind_port: u16,
    dest_host: String,
    dest_port: u16,
    /// local 转发的本机监听循环。监听器归它：abort 并等它结束就放掉端口
    server: RefCell<Option<JoinHandle<()>>>,
    /// remote 转发的入站分发循环
    acceptor: RefCell<Option<JoinHandle<()>>>,
}

impl LiveForward {
    fn from_row(row: &HostForwardRow) -> Self {
        LiveForward {
            id: row.id.clone(),
            host_id: row.host_id.clone(),
            kind: kind_of(&row.kind),
            bind_host: row.bind_host.clone(),
            bind_port: row.bind_port as u16,
            dest_host: row.dest_host.clone(),
            dest_port: row.dest_port as u16,
            server: RefCell::new(None),
            acceptor: RefCell::new(None),
        }
    }
}

/// 库里是 TEXT；不是 local 一律按远端算（与 relay_spec 的槽位口径、TS 的 `kind === "local"` 分支一致）
fn kind_of(kind: &str) -> ForwardKind {
    ForwardKind::from_wire(kind).unwrap_or(ForwardKind::Remote)
}

type Done = Shared<LocalBoxFuture<'static, Result<(), RelayError>>>;

/// 一次启动尝试。epoch 是发起时该规则的代数，stop() 让代数 +1 就等于作废它；
/// dialing = 还在等主机链路连上（最长是 SSH 握手超时），这段不值得等。
struct Attempt {
    epoch: u64,
    dialing: Cell<bool>,
    done: RefCell<Option<Done>>,
}

/// 端口转发运行时。见模块注释
pub struct ForwardManagerOf<L: RelayLink> {
    db: Arc<Db>,
    link_for: LinkForOf<L>,
    /// 连不上主机时请 SessionManager 退避重连；连上后它的 "up" 会回调 on_link_up
    want_reconnect: WantReconnect,
    live: RefCell<HashMap<String, Rc<LiveForward>>>,
    attempts: RefCell<HashMap<String, Rc<Attempt>>>,
    epochs: RefCell<HashMap<String, u64>>,
    errors: RefCell<HashMap<String, String>>,
}

/// 生产上的端口转发管理器：链路是 [`SshLink`]
pub type ForwardManager = ForwardManagerOf<SshLink>;

impl<L: RelayLink> ForwardManagerOf<L> {
    pub fn new(db: Arc<Db>, link_for: LinkForOf<L>, want_reconnect: WantReconnect) -> Rc<Self> {
        Rc::new(ForwardManagerOf {
            db,
            link_for,
            want_reconnect,
            live: RefCell::default(),
            attempts: RefCell::default(),
            epochs: RefCell::default(),
            errors: RefCell::default(),
        })
    }

    pub fn list(&self) -> Vec<PortForward> {
        self.db.list_forwards().iter().map(|row| self.to_port_forward(row)).collect()
    }

    pub fn has_enabled(&self, host_id: &str) -> bool {
        self.db.list_forwards().iter().any(|r| r.host_id == host_id && r.enabled == 1)
    }

    /// 有启用中转发的主机（去重，按规则创建先后）
    pub fn enabled_host_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for r in self.db.list_forwards() {
            if r.enabled == 1 && !ids.contains(&r.host_id) {
                ids.push(r.host_id);
            }
        }
        ids
    }

    /// 建规则。`input` 是原样的请求体（`POST /api/forwards`）
    pub async fn create(self: &Rc<Self>, input: &Value) -> Result<PortForward, RelayError> {
        let input = or_empty(input);
        let host_id = match field(&input, "hostId") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        if host_id.is_empty() || self.db.get_host(&host_id).is_none() {
            return Err(RelayError::NotFound("主机不存在".into()));
        }
        let parsed = validate_forward_input(Some(&input)).map_err(RelayError::Invalid)?;

        let row = HostForwardRow {
            id: crate::askpass::hub::uuid_v4(),
            host_id,
            name: parsed.name,
            kind: parsed.kind.as_str().into(),
            bind_host: parsed.bind_host,
            bind_port: parsed.bind_port.into(),
            dest_host: parsed.dest_host,
            dest_port: parsed.dest_port.into(),
            enabled: parsed.enabled.into(),
            created_at: crate::auth::now_ms(),
        };
        let displaced = if row.enabled == 1 { self.displace(&row) } else { Vec::new() };
        self.db.insert_forward(&row);
        if row.enabled == 1 {
            self.start_after(displaced, &row.id);
        }
        self.db.get_forward(&row.id).map(|r| self.to_port_forward(&r)).ok_or_else(not_found)
    }

    /// 改规则。`patch` 是原样的请求体（`PATCH /api/forwards/:id`，TS 的 `Partial<PortForwardInput>`）
    pub async fn update(self: &Rc<Self>, id: &str, patch: &Value) -> Result<PortForward, RelayError> {
        let Some(existing) = self.db.get_forward(id) else { return Err(not_found()) };
        let patch = or_empty(patch);
        let mut merged = serde_json::Map::new();
        // name 用 `!== undefined`（显式传 null 就是清掉），其余字段用 `??`（null 也回退到库里的值）
        match field(&patch, "name") {
            Some(v) => {
                merged.insert("name".into(), v.clone());
            }
            None => {
                if let Some(n) = &existing.name {
                    merged.insert("name".into(), json!(n));
                }
            }
        }
        let pick = |key: &str, fallback: Value| match field(&patch, key) {
            Some(v) if !v.is_null() => v.clone(),
            _ => fallback,
        };
        merged.insert("kind".into(), pick("kind", json!(existing.kind)));
        merged.insert("bindHost".into(), pick("bindHost", json!(existing.bind_host)));
        merged.insert("bindPort".into(), pick("bindPort", json!(existing.bind_port)));
        merged.insert("destHost".into(), pick("destHost", json!(existing.dest_host)));
        merged.insert("destPort".into(), pick("destPort", json!(existing.dest_port)));
        merged.insert("enabled".into(), pick("enabled", json!(existing.enabled == 1)));
        let parsed = validate_forward_input(Some(&Value::Object(merged))).map_err(RelayError::Invalid)?;

        let next = HostForwardRow {
            name: parsed.name,
            kind: parsed.kind.as_str().into(),
            bind_host: parsed.bind_host,
            bind_port: parsed.bind_port.into(),
            dest_host: parsed.dest_host,
            dest_port: parsed.dest_port.into(),
            enabled: parsed.enabled.into(),
            ..existing
        };
        // 先同步落库（让位的那几条 + 自己），再做异步的停与起：两个并发请求各自启用
        // 同端口的两条时，库里任何时刻都只有一条 enabled，后到的赢
        let displaced = if next.enabled == 1 { self.displace(&next) } else { Vec::new() };
        self.db.update_forward(&next);

        self.stop(id).await;
        if next.enabled == 1 {
            self.start_after(displaced, id);
        }
        // stop 期间被并发删掉了：TS 这里是 `getForward(id)!` 解引用 undefined 抛 TypeError（400），这里回 404
        self.db.get_forward(id).map(|r| self.to_port_forward(&r)).ok_or_else(not_found)
    }

    pub async fn remove(self: &Rc<Self>, id: &str) -> Result<(), RelayError> {
        if self.db.get_forward(id).is_none() {
            return Err(not_found());
        }
        self.stop(id).await;
        self.db.delete_forward(id);
        self.errors.borrow_mut().remove(id);
        Ok(())
    }

    /// 删主机前调用：停掉该主机的全部隧道。规则由 Db::delete_relays_of_host 删
    pub async fn forget_host(self: &Rc<Self>, host_id: &str) {
        let ids: Vec<String> =
            self.db.list_forwards().into_iter().filter(|r| r.host_id == host_id).map(|r| r.id).collect();
        self.stop_many(&ids).await;
        let mut errors = self.errors.borrow_mut();
        for id in &ids {
            errors.remove(id);
        }
    }

    /// 起一条规则并等结果。已经活着就立刻返回；有在途尝试就并入它
    pub async fn start(self: &Rc<Self>, id: &str) -> Result<(), RelayError> {
        match self.kick(id) {
            Some(done) => done.await,
            None => Ok(()),
        }
    }

    /// TS `start()` 里同步的那一段：登记在途尝试、把 startNow 丢进 LocalSet。返回可等的结果；
    /// None = 已经活着。不 await 也照样跑完（TS 的 `void this.start(id)`）
    fn kick(self: &Rc<Self>, id: &str) -> Option<Done> {
        if self.live.borrow().contains_key(id) {
            return None;
        }
        if let Some(inflight) = self.attempts.borrow().get(id) {
            return inflight.done.borrow().clone();
        }
        let attempt = Rc::new(Attempt { epoch: self.epoch(id), dialing: Cell::new(false), done: RefCell::new(None) });
        let me = self.clone();
        let mine = attempt.clone();
        let owned = id.to_string();
        let task = tokio::task::spawn_local(async move {
            let r = me.start_now(&owned, &mine).await;
            // TS 的 finally：只摘自己——stop 期间可能已经换上了新的一次尝试
            let current = me.attempts.borrow().get(&owned).is_some_and(|a| Rc::ptr_eq(a, &mine));
            if current {
                me.attempts.borrow_mut().remove(&owned);
            }
            r
        });
        let done: LocalBoxFuture<'static, _> = Box::pin(async move {
            task.await.unwrap_or_else(|e| Err(RelayError::Failed(format!("启动任务异常退出：{e}"))))
        });
        let done = done.shared();
        *attempt.done.borrow_mut() = Some(done.clone());
        self.attempts.borrow_mut().insert(id.to_string(), attempt);
        Some(done)
    }

    /// 作废在途的尝试并拆掉活着的隧道。还在连主机的尝试不等：它连上后见代数变了
    /// 自己退出、不会再开监听——同端口从连不上的 A 机切到 B 机时，B 不必陪着等满
    /// A 的握手超时。已经连上、正在开监听的那段很短，等它开完再拆，端口才交接得干净。
    pub async fn stop(self: &Rc<Self>, id: &str) {
        self.stop_eager(id).await
    }

    /// [`Self::stop`] 的同步前缀在调用时就做掉（代数 +1、摘掉在 dial 的尝试），返回的 future 只管等
    fn stop_eager(self: &Rc<Self>, id: &str) -> LocalBoxFuture<'static, ()> {
        self.bump(id);
        let attempt = self.attempts.borrow().get(id).cloned();
        let wait = match attempt {
            Some(a) if a.dialing.get() => {
                self.attempts.borrow_mut().remove(id);
                None
            }
            Some(a) => a.done.borrow().clone(),
            None => None,
        };
        let me = self.clone();
        let id = id.to_string();
        Box::pin(async move {
            if let Some(done) = wait {
                let _ = done.await;
            }
            let live = me.live.borrow_mut().remove(&id);
            if let Some(live) = live {
                me.teardown(live).await;
            }
        })
    }

    /// 链路断了 / 要被换掉：同步拆掉该主机的全部隧道并作废在途尝试，不写错误
    pub fn stop_all(self: &Rc<Self>, host_id: &str) {
        let lives: Vec<Rc<LiveForward>> =
            self.live.borrow().values().filter(|l| l.host_id == host_id).cloned().collect();
        for live in lives {
            self.live.borrow_mut().remove(&live.id);
            // teardown 的同步前缀（abort 监听循环）现在就做，等收尾的那段丢到后台（TS 的 void）
            tokio::task::spawn_local(self.teardown(live));
        }
        let ids: Vec<String> = self.attempts.borrow().keys().cloned().collect();
        for id in ids {
            if self.db.get_forward(&id).is_none_or(|r| r.host_id != host_id) {
                continue;
            }
            self.bump(&id);
            self.attempts.borrow_mut().remove(&id);
        }
    }

    fn epoch(&self, id: &str) -> u64 {
        self.epochs.borrow().get(id).copied().unwrap_or(0)
    }

    fn bump(&self, id: &str) {
        *self.epochs.borrow_mut().entry(id.to_string()).or_insert(0) += 1;
    }

    pub fn on_link_down(self: &Rc<Self>, host_id: &str) {
        self.stop_all(host_id);
        let mut errors = self.errors.borrow_mut();
        for row in self.db.list_forwards() {
            if row.host_id == host_id && row.enabled == 1 {
                errors.insert(row.id, "SSH 链路断开".into());
            }
        }
    }

    pub fn on_link_up(self: &Rc<Self>, host_id: &str) {
        for row in self.db.list_forwards() {
            if row.host_id == host_id && row.enabled == 1 {
                let _ = self.kick(&row.id);
            }
        }
    }

    /// 主机连不上时（启动恢复 / 退避重连中）把原因挂到每条 enabled 规则上
    pub fn mark_unreachable(&self, host_id: &str, message: &str) {
        for row in self.db.list_forwards() {
            if row.host_id == host_id && row.enabled == 1 && !self.live.borrow().contains_key(&row.id) {
                self.errors.borrow_mut().insert(row.id, message.to_string());
            }
        }
    }

    /// 把同槽位的其它 enabled 规则落库成 disabled，返回它们的 id 供调用方停掉。
    /// 只写库、不 await：调用方紧接着写自己那一行，两步之间不让出事件循环。
    fn displace(&self, target: &HostForwardRow) -> Vec<String> {
        let rows: Vec<ForwardSlotRow> = self.db.list_forwards().iter().map(slot_row).collect();
        let ids = displaced_by(&rows, &slot_row(target), forward_slot);
        for id in &ids {
            self.db.set_forward_enabled(id, false);
            self.errors.borrow_mut().remove(id);
        }
        ids
    }

    async fn stop_many(self: &Rc<Self>, ids: &[String]) {
        join_all(ids.iter().map(|id| self.stop_eager(id))).await;
    }

    /// 停掉让位的规则之后再起这一条，丢到后台：起的时候要先把主机链路连上，主机不通
    /// 就要等满 SSH 握手超时（15s），await 的话添加按钮 / 勾选框会一直转。响应里是
    /// starting，轮询接到结果。停必须排在起之前，新监听器才不会撞上旧的端口。
    fn start_after(self: &Rc<Self>, displaced: Vec<String>, id: &str) {
        if displaced.is_empty() {
            let _ = self.kick(id);
            return;
        }
        // 各条 stop 的同步前缀现在就做（TS 的 stopMany 也是同步起头的）
        let stops: Vec<_> = displaced.iter().map(|d| self.stop_eager(d)).collect();
        let me = self.clone();
        let id = id.to_string();
        tokio::task::spawn_local(async move {
            join_all(stops).await;
            let _ = me.start(&id).await;
        });
    }

    async fn start_now(self: &Rc<Self>, id: &str, attempt: &Attempt) -> Result<(), RelayError> {
        let stale = || self.epoch(id) != attempt.epoch;
        let Some(row) = self.db.get_forward(id) else { return Ok(()) };
        if row.enabled != 1 || self.live.borrow().contains_key(id) {
            return Ok(());
        }
        let Some(link) = (self.link_for)(&row.host_id) else {
            self.errors.borrow_mut().insert(id.to_string(), "主机不存在".into());
            return Ok(());
        };

        self.errors.borrow_mut().remove(id);
        match self.open(id, attempt, &row, link).await {
            Ok(()) => Ok(()),
            Err(_) if stale() => Ok(()),
            Err(msg) => {
                self.errors.borrow_mut().insert(id.to_string(), msg.clone());
                Err(RelayError::Failed(msg))
            }
        }
    }

    /// startNow 的 try 块
    async fn open(self: &Rc<Self>, id: &str, attempt: &Attempt, row: &HostForwardRow, link: Rc<L>) -> Result<(), String> {
        let stale = || self.epoch(id) != attempt.epoch;
        // 先把链路连上再开监听：本地转发的监听器本身不碰 SSH，不先连的话主机明明
        // 连不上，规则却显示「运行中」，要等第一条 TCP 连进来才露馅
        attempt.dialing.set(true);
        let dialed = link.ensure_connected().await;
        attempt.dialing.set(false);
        if let Err(e) = dialed {
            if stale() {
                return Ok(());
            }
            (self.want_reconnect)(&row.host_id);
            return Err(e);
        }
        // 连接期间规则可能已被停用 / 让位 / 删掉 / 重启
        if stale() {
            return Ok(());
        }
        let Some(fresh) = self.db.get_forward(id) else { return Ok(()) };
        if fresh.enabled != 1 {
            return Ok(());
        }
        let live = Rc::new(LiveForward::from_row(&fresh));
        if live.kind == ForwardKind::Local {
            let server = self.listen_local(link, &live).await?;
            *live.server.borrow_mut() = Some(server);
        } else {
            self.listen_remote(&link, &live).await?;
        }
        // stop_all 不等在途尝试（它是同步的），开监听这段里被作废了就自己拆掉
        if stale() {
            self.teardown(live).await;
            return Ok(());
        }
        self.live.borrow_mut().insert(id.to_string(), live);
        Ok(())
    }

    async fn listen_local(self: &Rc<Self>, link: Rc<L>, live: &Rc<LiveForward>) -> Result<JoinHandle<()>, String> {
        let listener = bind_local(&live.bind_host, live.bind_port).await?;
        let me = Rc::downgrade(self);
        let weak = Rc::downgrade(live);
        let on_error = Box::new(move |msg: String| {
            let (Some(me), Some(live)) = (me.upgrade(), weak.upgrade()) else { return };
            me.errors.borrow_mut().insert(live.id.clone(), msg);
            // 只摘自己：TS 按 id 直接删，期间若已换上新的一条会误删
            let current = me.live.borrow().get(&live.id).is_some_and(|l| Rc::ptr_eq(l, &live));
            if current {
                me.live.borrow_mut().remove(&live.id);
            }
        });
        Ok(spawn_tunnel_listener(listener, link, live.dest_host.clone(), live.dest_port, on_error))
    }

    async fn listen_remote(&self, link: &Rc<L>, live: &Rc<LiveForward>) -> Result<(), String> {
        let incoming = link.listen_remote(&live.bind_host, live.bind_port).await?;
        let acceptor = spawn_remote_acceptor::<L>(incoming, live.dest_host.clone(), live.dest_port);
        *live.acceptor.borrow_mut() = Some(acceptor);
        Ok(())
    }

    /// 拆一条隧道。同步前缀（abort 监听 / 分发循环）在调用时就做；返回的 future 等监听器
    /// 真正 drop（端口放掉）、远端撤销 tcpip-forward。
    ///
    /// 不等已建立的连接：TS 的 `await server.close(cb)` 要等**所有连接都断开**才回调，
    /// 开着一条长连接（数据库客户端）时停用 / 改规则的请求会一直挂着（forward.ts 的 teardown）。
    /// 已有连接照 Node 的 close 语义保留，链路一断它们自然就断了。
    fn teardown(&self, live: Rc<LiveForward>) -> LocalBoxFuture<'static, ()> {
        let server = live.server.borrow_mut().take();
        let acceptor = live.acceptor.borrow_mut().take();
        for h in server.iter().chain(acceptor.iter()) {
            h.abort();
        }
        let link = if live.kind == ForwardKind::Remote { (self.link_for)(&live.host_id) } else { None };
        Box::pin(async move {
            // abort 之后 await JoinHandle：tokio 先 drop 任务里的 future（连同监听器）再交出结果
            if let Some(h) = server {
                let _ = h.await;
            }
            if let Some(h) = acceptor {
                let _ = h.await;
            }
            if let Some(link) = link {
                link.unlisten_remote(&live.bind_host, live.bind_port).await;
            }
        })
    }

    fn to_port_forward(&self, row: &HostForwardRow) -> PortForward {
        let enabled = row.enabled == 1;
        let state = if enabled && self.live.borrow().contains_key(&row.id) {
            ForwardState::Active
        } else if enabled && self.attempts.borrow().contains_key(&row.id) {
            ForwardState::Starting
        } else if enabled && self.errors.borrow().contains_key(&row.id) {
            ForwardState::Error
        } else {
            ForwardState::Stopped
        };
        PortForward {
            id: row.id.clone(),
            host_id: row.host_id.clone(),
            name: row.name.clone(),
            kind: kind_of(&row.kind),
            bind_host: row.bind_host.clone(),
            bind_port: row.bind_port as u16,
            dest_host: row.dest_host.clone(),
            dest_port: row.dest_port as u16,
            enabled,
            state,
            error: if enabled { self.errors.borrow().get(&row.id).cloned() } else { None },
            created_at: row.created_at,
        }
    }
}

fn not_found() -> RelayError {
    RelayError::NotFound("转发规则不存在".into())
}

fn slot_row(r: &HostForwardRow) -> ForwardSlotRow {
    ForwardSlotRow {
        id: r.id.clone(),
        host_id: r.host_id.clone(),
        kind: r.kind.clone(),
        bind_port: r.bind_port,
        enabled: r.enabled,
        created_at: r.created_at,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncWriteExt as _;
    use tokio::net::TcpStream;

    use super::super::relay::fake::{FakeLink, echo_server, roundtrip};
    use super::*;
    use crate::db::SshHostRow;

    struct Rig {
        _dir: tempfile::TempDir,
        db: Arc<Db>,
        link: Rc<FakeLink>,
        reconnects: Rc<RefCell<Vec<String>>>,
        mgr: Rc<ForwardManagerOf<FakeLink>>,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(dir.path()).unwrap());
        for id in ["h1", "h2"] {
            db.insert_host(&SshHostRow {
                id: id.into(),
                name: id.into(),
                host: "10.0.0.1".into(),
                port: 22,
                username: "fay".into(),
                auth_method: "agent".into(),
                created_at: 1,
                ..Default::default()
            });
        }
        let link = Rc::new(FakeLink::default());
        let reconnects = Rc::new(RefCell::new(Vec::new()));
        let l = link.clone();
        let link_for: LinkForOf<FakeLink> =
            Rc::new(move |host: &str| (host == "h1" || host == "h2").then(|| l.clone()));
        let r = reconnects.clone();
        let want: WantReconnect = Rc::new(move |host: &str| r.borrow_mut().push(host.to_string()));
        let mgr = ForwardManagerOf::new(db.clone(), link_for, want);
        Rig { _dir: dir, db, link, reconnects, mgr }
    }

    /// 本机一个此刻空着的端口（绑 0 拿到号再放掉）
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    /// 等后台的启动走完。启用了却还是 stopped 的那一下是让位规则还在停、这条还没登记尝试
    async fn settle(mgr: &Rc<ForwardManagerOf<FakeLink>>, id: &str) -> PortForward {
        for _ in 0..200 {
            let f = mgr.list().into_iter().find(|f| f.id == id).unwrap();
            let pending = f.state == ForwardState::Starting || (f.enabled && f.state == ForwardState::Stopped);
            if !pending {
                return f;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("一直是 starting");
    }

    fn local(f: impl std::future::Future<Output = ()>) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        tokio::task::LocalSet::new().block_on(&rt, f);
    }

    #[test]
    fn create_validates_host_then_input() {
        local(async {
            let r = rig();
            let err = r.mgr.create(&json!({ "kind": "local", "bindPort": 1, "destPort": 1 })).await.unwrap_err();
            assert_eq!(err, RelayError::NotFound("主机不存在".into()));
            let err = r.mgr.create(&json!({ "hostId": "nope", "kind": "local" })).await.unwrap_err();
            assert_eq!(err, RelayError::NotFound("主机不存在".into()));
            let err = r.mgr.create(&json!({ "hostId": "h1", "kind": "up" })).await.unwrap_err();
            assert_eq!(err, RelayError::Invalid("转发方向必须是 local 或 remote".into()));
            // 缺省的请求体按 {} 处理：先撞主机不存在
            assert_eq!(r.mgr.create(&Value::Null).await.unwrap_err().status(), http::StatusCode::NOT_FOUND);
            assert_eq!(
                r.mgr.update("nope", &json!({})).await.unwrap_err(),
                RelayError::NotFound("转发规则不存在".into())
            );
            assert_eq!(r.mgr.remove("nope").await.unwrap_err(), RelayError::NotFound("转发规则不存在".into()));
        });
    }

    #[test]
    fn local_forward_listens_tunnels_and_releases_the_port_on_stop() {
        local(async {
            let r = rig();
            let echo = echo_server().await;
            let port = free_port();
            // 远端的 5432 实际是本机的回显服务
            r.link.tunnel_to.borrow_mut().insert(5432, echo);
            let f = r
                .mgr
                .create(&json!({ "hostId": "h1", "kind": "local", "bindPort": port, "destPort": "5432", "name": " pg " }))
                .await
                .unwrap();
            assert_eq!(f.state, ForwardState::Starting, "响应里是 starting，后台去起");
            assert_eq!(f.name.as_deref(), Some("pg"));
            let f = settle(&r.mgr, &f.id).await;
            assert_eq!(f.state, ForwardState::Active);
            assert_eq!(r.link.connects.get(), 1, "开监听前先把链路连上");

            let mut c = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            assert_eq!(roundtrip(&mut c, b"hello").await, b"hello");
            assert_eq!(r.link.tunnels.borrow().as_slice(), [("127.0.0.1".to_string(), 5432)]);

            // 停用：端口立刻放掉（同步返回时监听器已 drop），已建立的连接照 Node 的 close 语义保留
            let f = r.mgr.update(&f.id, &json!({ "enabled": false })).await.unwrap();
            assert_eq!((f.enabled, f.state), (false, ForwardState::Stopped));
            assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(), "端口应已释放");
            assert_eq!(roundtrip(&mut c, b"still").await, b"still");
            c.shutdown().await.unwrap();
        });
    }

    #[test]
    fn enabling_a_same_port_rule_displaces_the_other_and_hands_over_the_port() {
        local(async {
            let r = rig();
            let echo = echo_server().await;
            r.link.tunnel_to.borrow_mut().insert(1, echo);
            r.link.tunnel_to.borrow_mut().insert(2, echo);
            let port = free_port();
            let a = r.mgr.create(&json!({ "hostId": "h1", "kind": "local", "bindPort": port, "destPort": 1 })).await.unwrap();
            assert_eq!(settle(&r.mgr, &a.id).await.state, ForwardState::Active);
            // 本地转发跨主机同槽位：挂 h2 的同端口规则启用后，h1 那条落库成 disabled
            let b = r.mgr.create(&json!({ "hostId": "h2", "kind": "local", "bindPort": port, "destPort": 2 })).await.unwrap();
            assert_eq!(r.db.get_forward(&a.id).unwrap().enabled, 0, "同步落库让位");
            let b = settle(&r.mgr, &b.id).await;
            assert_eq!(b.state, ForwardState::Active, "停在起之前，新监听器没撞上旧端口：{:?}", b.error);
            let a = r.mgr.list().into_iter().find(|f| f.id == a.id).unwrap();
            assert_eq!((a.enabled, a.state), (false, ForwardState::Stopped));
            let mut c = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            assert_eq!(roundtrip(&mut c, b"x").await, b"x");
            assert_eq!(r.link.tunnels.borrow().last().unwrap().1, 2, "连上的是 B 的目标");
        });
    }

    #[test]
    fn unreachable_host_marks_error_and_asks_for_reconnect() {
        local(async {
            let r = rig();
            *r.link.fail_connect.borrow_mut() = Some("Timed out while waiting for handshake".into());
            let port = free_port();
            let f = r.mgr.create(&json!({ "hostId": "h1", "kind": "local", "bindPort": port, "destPort": 1 })).await.unwrap();
            let f = settle(&r.mgr, &f.id).await;
            assert_eq!(f.state, ForwardState::Error);
            assert_eq!(f.error.as_deref(), Some("Timed out while waiting for handshake"));
            assert_eq!(r.reconnects.borrow().as_slice(), ["h1"]);
            // 没开监听：主机连不上就不该显示运行中
            assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
            // 链路回来了：on_link_up 按 enabled 拉起
            *r.link.fail_connect.borrow_mut() = None;
            r.mgr.on_link_up("h1");
            assert_eq!(settle(&r.mgr, &f.id).await.state, ForwardState::Active);
            assert!(r.mgr.has_enabled("h1") && !r.mgr.has_enabled("h2"));
            assert_eq!(r.mgr.enabled_host_ids(), ["h1"]);
        });
    }

    #[test]
    fn stop_does_not_wait_for_a_dialing_attempt_and_the_attempt_quits_quietly() {
        local(async {
            let r = rig();
            r.link.connect_delay_ms.set(300);
            *r.link.fail_connect.borrow_mut() = Some("连不上".into());
            let port = free_port();
            let f = r.mgr.create(&json!({ "hostId": "h1", "kind": "local", "bindPort": port, "destPort": 1 })).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let started = std::time::Instant::now();
            let f = r.mgr.update(&f.id, &json!({ "enabled": false })).await.unwrap();
            assert!(started.elapsed() < Duration::from_millis(200), "停用不陪着等握手");
            assert_eq!(f.state, ForwardState::Stopped);
            tokio::time::sleep(Duration::from_millis(400)).await;
            // 被作废的尝试失败了也不写错误、不请求重连
            assert!(r.mgr.errors.borrow().is_empty());
            assert!(r.reconnects.borrow().is_empty());
        });
    }

    #[test]
    fn port_in_use_is_reported_on_the_rule() {
        local(async {
            let r = rig();
            let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = taken.local_addr().unwrap().port();
            let f = r.mgr.create(&json!({ "hostId": "h1", "kind": "local", "bindPort": port, "destPort": 1 })).await.unwrap();
            assert_eq!(r.mgr.start(&f.id).await, Err(RelayError::Failed(format!("listen EADDRINUSE: address already in use 127.0.0.1:{port}"))));
            let f = settle(&r.mgr, &f.id).await;
            assert_eq!(f.state, ForwardState::Error);
            // 停用的规则不带错误
            let f = r.mgr.update(&f.id, &json!({ "enabled": false })).await.unwrap();
            assert_eq!((f.state, f.error), (ForwardState::Stopped, None));
        });
    }

    #[test]
    fn remote_forward_dials_the_local_destination_and_unlistens_on_stop() {
        local(async {
            let r = rig();
            let echo = echo_server().await;
            let f = r
                .mgr
                .create(&json!({ "hostId": "h1", "kind": "remote", "bindPort": 8080, "destPort": echo }))
                .await
                .unwrap();
            assert_eq!(settle(&r.mgr, &f.id).await.state, ForwardState::Active);
            // 远端有连接进来：分发到后端本机的目标
            let (mut ours, theirs) = tokio::io::duplex(1024);
            r.link.remote.borrow()[&8080].send(theirs).unwrap();
            assert_eq!(roundtrip(&mut ours, b"ping").await, b"ping");

            r.mgr.remove(&f.id).await.unwrap();
            assert_eq!(r.link.unlistened.borrow().as_slice(), [("127.0.0.1".to_string(), 8080)]);
            assert!(r.db.get_forward(&f.id).is_none());
            assert!(!r.link.remote.borrow().contains_key(&8080));
        });
    }

    #[test]
    fn remote_bind_failure_is_reported() {
        local(async {
            let r = rig();
            *r.link.fail_listen_remote.borrow_mut() = Some("Unable to bind to 127.0.0.1:80".into());
            let f = r.mgr.create(&json!({ "hostId": "h1", "kind": "remote", "bindPort": 80, "destPort": 80 })).await.unwrap();
            let f = settle(&r.mgr, &f.id).await;
            assert_eq!((f.state, f.error.as_deref()), (ForwardState::Error, Some("Unable to bind to 127.0.0.1:80")));
        });
    }

    #[test]
    fn link_down_tears_down_and_marks_rules_then_up_restores_them() {
        local(async {
            let r = rig();
            let port = free_port();
            let f = r.mgr.create(&json!({ "hostId": "h1", "kind": "local", "bindPort": port, "destPort": 1 })).await.unwrap();
            assert_eq!(settle(&r.mgr, &f.id).await.state, ForwardState::Active);
            r.mgr.on_link_down("h1");
            let down = r.mgr.list().pop().unwrap();
            assert_eq!((down.state, down.error.as_deref()), (ForwardState::Error, Some("SSH 链路断开")));
            // 拆监听是后台的那一段，让出一下事件循环
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(), "断链期间不占端口");
            r.mgr.mark_unreachable("h1", "No route to host");
            assert_eq!(r.mgr.list().pop().unwrap().error.as_deref(), Some("No route to host"));
            r.mgr.on_link_up("h1");
            assert_eq!(settle(&r.mgr, &f.id).await.state, ForwardState::Active);
            // 删主机：停掉隧道（规则由 Db::delete_relays_of_host 删）
            r.mgr.forget_host("h1").await;
            assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        });
    }

    #[test]
    fn update_merges_the_patch_over_the_stored_rule() {
        local(async {
            let r = rig();
            let f = r
                .mgr
                .create(&json!({ "hostId": "h1", "name": "pg", "kind": "remote", "bindPort": 1, "destPort": 2, "enabled": false }))
                .await
                .unwrap();
            // null 走 `??` 回退到库里的值；name 显式 null 是清掉
            let g = r.mgr.update(&f.id, &json!({ "bindPort": "3", "destHost": null, "name": null })).await.unwrap();
            assert_eq!((g.bind_port, g.dest_port, g.dest_host.as_str(), g.name), (3, 2, "127.0.0.1", None));
            assert_eq!((g.kind, g.enabled, g.state), (ForwardKind::Remote, false, ForwardState::Stopped));
            let err = r.mgr.update(&f.id, &json!({ "destPort": 0 })).await.unwrap_err();
            assert_eq!(err, RelayError::Invalid("目标端口无效".into()));
        });
    }
}
