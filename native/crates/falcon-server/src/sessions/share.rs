//! 公网发布运行时，挂在本机或 SSH Host 上（ADR 0016）。移植自 `packages/server/src/sessions/share.ts`。
//! 规则在 DB，活着的 cloudflared 进程在内存。
//!
//! cloudflared 只在 falcon 后端本机跑。远端目标先在本机听一个临时端口、经
//! SSH forwardOut 打到远端，再把 Quick Tunnel 指到这个临时端口——远端宿主机
//! 上既没有 cloudflared，也不需要出网到 Cloudflare。桥走主机自己的那条
//! SshLink（SessionManager.getHostLink），与端口转发共用。
//!
//! URL 是 Quick Tunnel 的随机子域，不入库：进程一重启就会变，存下来只会骗人。
//!
//! 同一台机器上发同一个端口的规则可以有多条，同时只有一条 enabled，
//! 做法与 ForwardManager 相同（见 relay_spec::share_slot）。
//!
//! # 移植约定
//!
//! 执行模型、「同步前缀 + 惰性 future」、原样 JSON 入参、链路 trait 同 `forward.rs` 顶部。另外：
//!
//! - 子进程是 `tokio::process`，`kill_on_drop`：后端退出（运行时关掉、收尸任务被 drop）时
//!   cloudflared 跟着被杀。`wait` 要 `&mut Child`，所以 Child 归一个收尸任务独占，别处只拿 pid、
//!   退出状态（watch）与强杀通道。
//! - 停进程照 TS：先 SIGTERM，2s 不退再 SIGKILL（Windows 没有 SIGTERM，Node 的 kill 在那边就是
//!   TerminateProcess，这里直接强杀）。
//! - 日志按管道流式解码（TS 是逐块 `toString("utf8")`，会把跨块的中文切成 U+FFFD）。
//! - `/quicktunnel` 用一条手写的 HTTP/1.0 请求问（只打本机回环，cloudflared 的 Go 服务对 1.0
//!   请求回的是不分块的整段响应），不为它拖进 HTTP 客户端。
//! - 本地 PTY 的基底环境（TS 的 `resolveLocalBaseEnv()`）由 SessionManager 经
//!   [`ShareManagerOf::set_base_env`] 接进来（它在 `LocalHost` 上）；没接时用进程环境。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use falcon_proto::{ForwardState, PublicShare};
use futures::FutureExt as _;
use futures::future::{Shared, join_all};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use super::forward_spec::field;
use super::relay::{LinkForOf, RelayLink, bind_local, or_empty, spawn_tunnel_listener};
use super::relay_spec::{ShareSlotRow, displaced_by, share_slot};
use super::share_spec::validate_share_input;
use super::ssh::SshLink;
use crate::cloudflared::bin::{CloudflaredBinError, ensure_cloudflared, hide_window};
use crate::cloudflared::command::{
    TunnelArgs, extract_metrics_addr, extract_quick_tunnel_url, http_host_header, last_log_lines, origin_url,
    parse_quick_tunnel_metrics, tunnel_args,
};
use crate::db::{Db, HostShareRow};
use crate::exec::{LocalBoxFuture, Utf8Decoder};

pub use super::relay::{LinkFor, RelayError, WantReconnect};

const URL_WAIT: Duration = Duration::from_millis(45_000);
const METRICS_POLL: Duration = Duration::from_millis(300);
const STOP_GRACE: Duration = Duration::from_millis(2_000);
/// TS `fetch` 的 `AbortSignal.timeout(800)`
const METRICS_TIMEOUT: Duration = Duration::from_millis(800);
/// 进程退出后等管道读完的上限：exit 常常先于最后几行日志到，不等的话错误详情是空的
const PIPE_DRAIN: Duration = Duration::from_millis(500);

/// 本地 PTY 的基底环境（`LocalHost::base_env`）
pub type BaseEnvFn = Rc<dyn Fn() -> LocalBoxFuture<'static, Rc<Vec<(String, String)>>>>;
/// cloudflared 定位（缺省是 [`ensure_cloudflared`]；单测换成假脚本）
type BinFn = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<String, CloudflaredBinError>>>;

struct LiveShare {
    id: String,
    /// None = 本机
    host_id: Option<String>,
    dest_host: String,
    dest_port: u16,
    child: RefCell<Option<Rc<Proc>>>,
    /// 远端目标的本机桥：listen(0) → SSH forwardOut。监听器归这个任务
    bridge: RefCell<Option<JoinHandle<()>>>,
    /// 我们主动杀的，exit 时不要写成 error
    stopping: Cell<bool>,
}

impl LiveShare {
    fn from_row(row: &HostShareRow) -> Self {
        LiveShare {
            id: row.id.clone(),
            host_id: row.host_id.clone(),
            dest_host: row.dest_host.clone(),
            dest_port: row.dest_port as u16,
            child: RefCell::new(None),
            bridge: RefCell::new(None),
            stopping: Cell::new(false),
        }
    }
}

type Done = Shared<LocalBoxFuture<'static, Result<(), RelayError>>>;

/// 一次在途的启动。seq 认身份：结束时只摘自己那一条
struct Run {
    seq: u64,
    done: Done,
}

/// stop() 进行中的规则（TS 的 `cancelled: Set`）。计数而不是集合：同一条并发 stop 两次时，
/// 先结束的那个不能把后一个的标记摘掉。标记随 [`CancelMark`] drop 撤销（TS 的 finally）
type Cancelled = Rc<RefCell<HashMap<String, usize>>>;

struct CancelMark {
    set: Cancelled,
    id: String,
}

impl CancelMark {
    fn new(set: &Cancelled, id: &str) -> Self {
        *set.borrow_mut().entry(id.to_string()).or_insert(0) += 1;
        CancelMark { set: set.clone(), id: id.to_string() }
    }
}

impl Drop for CancelMark {
    fn drop(&mut self) {
        let mut set = self.set.borrow_mut();
        if let Some(n) = set.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                set.remove(&self.id);
            }
        }
    }
}

/// 公网发布运行时。见模块注释
pub struct ShareManagerOf<L: RelayLink> {
    db: Arc<Db>,
    link_for: LinkForOf<L>,
    /// 远端目标连不上主机时请 SessionManager 退避重连
    want_reconnect: WantReconnect,
    live: RefCell<HashMap<String, Rc<LiveShare>>>,
    starting: RefCell<HashMap<String, Run>>,
    errors: RefCell<HashMap<String, String>>,
    urls: RefCell<HashMap<String, String>>,
    /// stop() 进行中的规则：start_now 在 spawn 前看一眼，别再起一个马上要杀的进程
    cancelled: Cancelled,
    /// stop_all 作废在途启动用的代数（TS 没有，见 stop_all）
    epochs: RefCell<HashMap<String, u64>>,
    seq: Cell<u64>,
    /// shutdown 之后不再起新进程
    shut: Cell<bool>,
    base_env: RefCell<Option<BaseEnvFn>>,
    bin: RefCell<BinFn>,
}

/// 生产上的公网发布管理器：链路是 [`SshLink`]
pub type ShareManager = ShareManagerOf<SshLink>;

impl<L: RelayLink> ShareManagerOf<L> {
    pub fn new(db: Arc<Db>, data_dir: PathBuf, link_for: LinkForOf<L>, want_reconnect: WantReconnect) -> Rc<Self> {
        let dir = data_dir;
        let bin: BinFn = Rc::new(move || {
            let dir = dir.clone();
            Box::pin(async move {
                let env_bin = std::env::var("FALCON_CLOUDFLARED_BIN").ok();
                ensure_cloudflared(&dir, env_bin.as_deref()).await
            })
        });
        Rc::new(ShareManagerOf {
            db,
            link_for,
            want_reconnect,
            live: RefCell::default(),
            starting: RefCell::default(),
            errors: RefCell::default(),
            urls: RefCell::default(),
            cancelled: Rc::default(),
            epochs: RefCell::default(),
            seq: Cell::new(0),
            shut: Cell::new(false),
            base_env: RefCell::new(None),
            bin: RefCell::new(bin),
        })
    }

    /// 接上本地 PTY 的基底环境（`LocalHost::base_env`）：cloudflared 照 TS 用 login 解析过的环境起，
    /// 后端由 launchd 拉起时进程环境里的 PATH / 代理变量往往是残的
    pub fn set_base_env(&self, f: BaseEnvFn) {
        *self.base_env.borrow_mut() = Some(f);
    }

    pub fn list(&self) -> Vec<PublicShare> {
        self.db.list_shares().iter().map(|row| self.to_share(row)).collect()
    }

    /// 该主机上有没有要靠 SSH 链路维持的发布（本机的不算）
    pub fn has_enabled(&self, host_id: &str) -> bool {
        self.db.list_shares().iter().any(|r| r.host_id.as_deref() == Some(host_id) && r.enabled == 1)
    }

    /// 有启用中发布的 SSH Host（去重，按规则创建先后；本机的不算）
    pub fn enabled_host_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for r in self.db.list_shares() {
            if let (1, Some(h)) = (r.enabled, r.host_id)
                && !ids.contains(&h)
            {
                ids.push(h);
            }
        }
        ids
    }

    /// 建规则。`input` 是原样的请求体（`POST /api/shares`）
    pub async fn create(self: &Rc<Self>, input: &Value) -> Result<PublicShare, RelayError> {
        let input = or_empty(input);
        // hostId 缺省 / null / 空串 = 本机；不是字符串或主机不在 = 404
        let host_id = match field(&input, "hostId") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) if self.db.get_host(s).is_some() => Some(s.clone()),
            Some(_) => return Err(RelayError::NotFound("主机不存在".into())),
        };
        let parsed = validate_share_input(Some(&input)).map_err(RelayError::Invalid)?;

        let row = HostShareRow {
            id: crate::askpass::hub::uuid_v4(),
            host_id,
            name: parsed.name,
            dest_host: parsed.dest_host,
            dest_port: parsed.dest_port.into(),
            enabled: parsed.enabled.into(),
            created_at: crate::auth::now_ms(),
        };
        let displaced = if row.enabled == 1 { self.displace(&row) } else { Vec::new() };
        self.db.insert_share(&row);
        // 第一次会下载 ~20MB 的 cloudflared，await 会让 POST 卡一两分钟，
        // 面板以为表单死了。丢到后台，列表立刻是 starting，轮询接到 URL。
        if row.enabled == 1 {
            self.start_after(displaced, &row.id);
        }
        self.db.get_share(&row.id).map(|r| self.to_share(&r)).ok_or_else(not_found)
    }

    /// 改规则。`patch` 是原样的请求体（`PATCH /api/shares/:id`，TS 的 `Partial<PublicShareInput>`）
    pub async fn update(self: &Rc<Self>, id: &str, patch: &Value) -> Result<PublicShare, RelayError> {
        let Some(existing) = self.db.get_share(id) else { return Err(not_found()) };
        let patch = or_empty(patch);
        let mut merged = serde_json::Map::new();
        // name 用 `!== undefined`（显式传 null 就是清掉），其余字段用 `??`
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
        merged.insert("destHost".into(), pick("destHost", json!(existing.dest_host)));
        merged.insert("destPort".into(), pick("destPort", json!(existing.dest_port)));
        merged.insert("enabled".into(), pick("enabled", json!(existing.enabled == 1)));
        let parsed = validate_share_input(Some(&Value::Object(merged))).map_err(RelayError::Invalid)?;

        let next = HostShareRow {
            name: parsed.name,
            dest_host: parsed.dest_host,
            dest_port: parsed.dest_port.into(),
            enabled: parsed.enabled.into(),
            ..existing
        };
        let displaced = if next.enabled == 1 { self.displace(&next) } else { Vec::new() };
        self.db.update_share(&next);

        self.stop(id).await;
        // 与新建同理丢到后台：起的时候可能要下载 cloudflared、连主机链路
        if next.enabled == 1 {
            self.start_after(displaced, id);
        }
        self.db.get_share(id).map(|r| self.to_share(&r)).ok_or_else(not_found)
    }

    pub async fn remove(self: &Rc<Self>, id: &str) -> Result<(), RelayError> {
        if self.db.get_share(id).is_none() {
            return Err(not_found());
        }
        self.stop(id).await;
        self.db.delete_share(id);
        self.errors.borrow_mut().remove(id);
        self.urls.borrow_mut().remove(id);
        Ok(())
    }

    /// 删主机前调用：停掉该主机的全部发布。规则由 Db::delete_relays_of_host 删
    pub async fn forget_host(self: &Rc<Self>, host_id: &str) {
        let ids: Vec<String> = self
            .db
            .list_shares()
            .into_iter()
            .filter(|r| r.host_id.as_deref() == Some(host_id))
            .map(|r| r.id)
            .collect();
        self.stop_many(&ids).await;
        let mut errors = self.errors.borrow_mut();
        for id in &ids {
            errors.remove(id);
        }
    }

    /// 起一条规则并等结果（拿到公网 URL 或失败）。已经活着就立刻返回；有在途的就并入它
    pub async fn start(self: &Rc<Self>, id: &str) -> Result<(), RelayError> {
        match self.kick(id) {
            Some(done) => done.await,
            None => Ok(()),
        }
    }

    /// TS `start()` 里同步的那一段：登记在途、把 startNow 丢进 LocalSet。None = 已经活着
    fn kick(self: &Rc<Self>, id: &str) -> Option<Done> {
        if self.live.borrow().contains_key(id) {
            return None;
        }
        if let Some(run) = self.starting.borrow().get(id) {
            return Some(run.done.clone());
        }
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let epoch = self.epoch(id);
        let me = self.clone();
        let owned = id.to_string();
        let task = tokio::task::spawn_local(async move {
            let r = me.start_now(&owned, epoch).await;
            // TS 的 finally 无条件 delete：stop_all 摘掉旧的一次、on_link_up 又起了新的一次时，
            // 旧的结束会把新的登记删掉。这里只摘自己
            let current = me.starting.borrow().get(&owned).is_some_and(|r| r.seq == seq);
            if current {
                me.starting.borrow_mut().remove(&owned);
            }
            r
        });
        let done: LocalBoxFuture<'static, _> = Box::pin(async move {
            task.await.unwrap_or_else(|e| Err(RelayError::Failed(format!("启动任务异常退出：{e}"))))
        });
        let done = done.shared();
        self.starting.borrow_mut().insert(id.to_string(), Run { seq, done: done.clone() });
        Some(done)
    }

    pub async fn stop(self: &Rc<Self>, id: &str) {
        self.stop_eager(id).await
    }

    /// [`Self::stop`] 的同步前缀（打取消标记、摘下活着的那条并开始杀）在调用时就做掉
    fn stop_eager(self: &Rc<Self>, id: &str) -> LocalBoxFuture<'static, ()> {
        let mark = CancelMark::new(&self.cancelled, id);
        // 已经 spawn 了就先杀：wait_for_url 见进程退出立刻放弃，不必干等最长 45s 的 URL
        let removed = self.live.borrow_mut().remove(id);
        let first = removed.map(teardown);
        let me = self.clone();
        let id = id.to_string();
        Box::pin(async move {
            let _mark = mark;
            if let Some(td) = first {
                td.await;
            }
            // 下载 cloudflared / 连 SSH 的那段打断不了，等它走到 spawn 前的检查点自己退出
            let inflight = me.starting.borrow().get(&id).map(|r| r.done.clone());
            if let Some(run) = inflight {
                let _ = run.await;
            }
            let late = me.live.borrow_mut().remove(&id);
            if let Some(late) = late {
                teardown(late).await;
            }
            me.urls.borrow_mut().remove(&id);
        })
    }

    /// 链路要被换掉（改了凭据 / 删主机）或断了：同步拆掉该主机的全部发布，不写错误。
    ///
    /// 在途的启动：TS 只把它从 `starting` 里摘掉、任它跑完——它若已过了连链路那步，会照样
    /// spawn 出 cloudflared 并登记成活的；此时 on_link_up 再起一次，就是同一条规则两个进程，
    /// 先登记的那个从表里被覆盖、再也没人杀（share.ts 的 stopAll）。这里另让代数 +1，
    /// 旧的那次走到 spawn 前的检查点就自己退出。
    pub fn stop_all(self: &Rc<Self>, host_id: &str) {
        let lives: Vec<Rc<LiveShare>> = self
            .live
            .borrow()
            .values()
            .filter(|l| l.host_id.as_deref() == Some(host_id))
            .cloned()
            .collect();
        for live in lives {
            self.live.borrow_mut().remove(&live.id);
            self.urls.borrow_mut().remove(&live.id);
            tokio::task::spawn_local(teardown(live));
        }
        let ids: Vec<String> = self.starting.borrow().keys().cloned().collect();
        for id in ids {
            if self.db.get_share(&id).is_some_and(|r| r.host_id.as_deref() == Some(host_id)) {
                self.starting.borrow_mut().remove(&id);
                self.bump(&id);
            }
        }
    }

    pub fn on_link_down(self: &Rc<Self>, host_id: &str) {
        self.stop_all(host_id);
        let mut errors = self.errors.borrow_mut();
        for row in self.db.list_shares() {
            if row.host_id.as_deref() == Some(host_id) && row.enabled == 1 {
                errors.insert(row.id, "SSH 链路断开".into());
            }
        }
    }

    pub fn on_link_up(self: &Rc<Self>, host_id: &str) {
        for row in self.db.list_shares() {
            if row.host_id.as_deref() == Some(host_id) && row.enabled == 1 {
                let _ = self.kick(&row.id);
            }
        }
    }

    pub fn mark_unreachable(&self, host_id: &str, message: &str) {
        for row in self.db.list_shares() {
            if row.host_id.as_deref() == Some(host_id) && row.enabled == 1 && !self.live.borrow().contains_key(&row.id) {
                self.errors.borrow_mut().insert(row.id, message.to_string());
            }
        }
    }

    /// 后端退出前调用：杀掉全部 cloudflared。之后在途的启动走到 spawn 前的检查点就退出
    pub async fn shutdown(&self) {
        self.shut.set(true);
        let lives: Vec<Rc<LiveShare>> = self.live.borrow_mut().drain().map(|(_, l)| l).collect();
        self.urls.borrow_mut().clear();
        self.starting.borrow_mut().clear();
        join_all(lives.into_iter().map(teardown)).await;
    }

    /// 启动时拉起本机的发布。远端的等主机链路连上（"up"）时由 on_link_up 拉
    pub fn restore_local(self: &Rc<Self>) {
        for row in self.db.list_shares() {
            if row.host_id.is_none() && row.enabled == 1 {
                let _ = self.kick(&row.id);
            }
        }
    }

    fn displace(&self, target: &HostShareRow) -> Vec<String> {
        let rows: Vec<ShareSlotRow> = self.db.list_shares().iter().map(slot_row).collect();
        let ids = displaced_by(&rows, &slot_row(target), share_slot);
        for id in &ids {
            self.db.set_share_enabled(id, false);
            self.errors.borrow_mut().remove(id);
        }
        ids
    }

    async fn stop_many(self: &Rc<Self>, ids: &[String]) {
        join_all(ids.iter().map(|id| self.stop_eager(id))).await;
    }

    /// 停掉让位的之后再起这一条，丢到后台。没有要让位的就同步登记，响应里即是 starting
    /// （TS 一律走 `stopMany([]).then(start)`，登记落在微任务里，POST 的响应还是 stopped；
    /// 前端写完会重拉列表，看不出差别）
    fn start_after(self: &Rc<Self>, displaced: Vec<String>, id: &str) {
        if displaced.is_empty() {
            let _ = self.kick(id);
            return;
        }
        let stops: Vec<_> = displaced.iter().map(|d| self.stop_eager(d)).collect();
        let me = self.clone();
        let id = id.to_string();
        tokio::task::spawn_local(async move {
            join_all(stops).await;
            let _ = me.start(&id).await;
        });
    }

    fn epoch(&self, id: &str) -> u64 {
        self.epochs.borrow().get(id).copied().unwrap_or(0)
    }

    fn bump(&self, id: &str) {
        *self.epochs.borrow_mut().entry(id.to_string()).or_insert(0) += 1;
    }

    async fn start_now(self: &Rc<Self>, id: &str, epoch: u64) -> Result<(), RelayError> {
        let Some(row) = self.db.get_share(id) else { return Ok(()) };
        if row.enabled != 1 || self.live.borrow().contains_key(id) {
            return Ok(());
        }

        self.errors.borrow_mut().remove(id);
        self.urls.borrow_mut().remove(id);

        let live = Rc::new(LiveShare::from_row(&row));
        match self.launch(id, epoch, &row, &live).await {
            Ok(()) => Ok(()),
            Err(msg) => {
                // 我们自己杀的（停用 / 让位 / 重启）、被 stop_all 作废的，都不是故障
                let cancelled = live.stopping.get() || self.epoch(id) != epoch;
                teardown(live.clone()).await;
                let current = self.live.borrow().get(id).is_some_and(|l| Rc::ptr_eq(l, &live));
                if current {
                    self.live.borrow_mut().remove(id);
                }
                self.urls.borrow_mut().remove(id);
                if cancelled {
                    return Ok(());
                }
                self.errors.borrow_mut().insert(id.to_string(), msg.clone());
                Err(RelayError::Failed(msg))
            }
        }
    }

    /// startNow 的 try 块
    async fn launch(self: &Rc<Self>, id: &str, epoch: u64, row: &HostShareRow, live: &Rc<LiveShare>) -> Result<(), String> {
        let resolve = self.bin.borrow().clone();
        let bin = resolve().await.map_err(|e| e.message)?;
        let mut tunnel_host = row.dest_host.clone();
        let mut tunnel_port = row.dest_port as u16;
        if let Some(host_id) = &row.host_id {
            let link = (self.link_for)(host_id).ok_or_else(|| "主机不存在".to_string())?;
            // 先连上再开桥：桥的监听器本身不碰 SSH，不先连的话主机连不上也能拿到
            // 一条公网 URL，点开才是 502
            if let Err(e) = link.ensure_connected().await {
                (self.want_reconnect)(host_id);
                return Err(e);
            }
            let (bridge, port) = self.listen_bridge(link, live).await?;
            *live.bridge.borrow_mut() = Some(bridge);
            tunnel_host = "127.0.0.1".into();
            tunnel_port = port;
        }

        let metrics_port = free_port()?;
        let base_env = self.base_env.borrow().clone();
        let mut env = match base_env {
            Some(f) => f().await.as_ref().clone(),
            None => std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?))).collect(),
        };
        set_env(&mut env, "NO_AUTOUPDATE", "true");
        let cancelled = self.cancelled.borrow().contains_key(id) || self.epoch(id) != epoch || self.shut.get();
        if cancelled {
            live.stopping.set(true);
            teardown(live.clone()).await;
            return Ok(());
        }
        let args = tunnel_args(&TunnelArgs {
            origin_url: origin_url(&tunnel_host, tunnel_port),
            // Host 按真正的 origin 写，不是本机桥的临时端口——vite 认的是它自己的端口
            http_host_header: http_host_header(&row.dest_host, row.dest_port as u16),
            metrics: format!("127.0.0.1:{metrics_port}"),
        });
        let child = Proc::spawn(&bin, &args, &env)?;
        *live.child.borrow_mut() = Some(child.clone());
        self.live.borrow_mut().insert(id.to_string(), live.clone());

        let url = wait_for_url(&child, metrics_port, || live.stopping.get()).await?;
        if live.stopping.get() {
            return Ok(());
        }
        self.urls.borrow_mut().insert(id.to_string(), url);

        let me = Rc::downgrade(self);
        let watched = live.clone();
        let id = id.to_string();
        tokio::task::spawn_local(async move {
            let exit = child.wait_exit().await;
            let Some(me) = me.upgrade() else { return };
            if watched.stopping.get() {
                return;
            }
            let current = me.live.borrow().get(&id).is_some_and(|l| Rc::ptr_eq(l, &watched));
            if !current {
                return;
            }
            me.live.borrow_mut().remove(&id);
            me.urls.borrow_mut().remove(&id);
            me.errors.borrow_mut().insert(id, format!("cloudflared 已退出{}", exit.suffix()));
            let bridge = watched.bridge.borrow_mut().take();
            if let Some(bridge) = bridge {
                bridge.abort();
            }
        });
        Ok(())
    }

    async fn listen_bridge(self: &Rc<Self>, link: Rc<L>, live: &Rc<LiveShare>) -> Result<(JoinHandle<()>, u16), String> {
        let listener = bind_local("127.0.0.1", 0).await?;
        let port = listener.local_addr().map_err(|_| "本机桥没有绑到端口".to_string())?.port();
        let me = Rc::downgrade(self);
        let weak = Rc::downgrade(live);
        let on_error = Box::new(move |msg: String| {
            let (Some(me), Some(live)) = (me.upgrade(), weak.upgrade()) else { return };
            me.errors.borrow_mut().insert(live.id.clone(), msg);
            let current = me.live.borrow().get(&live.id).is_some_and(|l| Rc::ptr_eq(l, &live));
            if current {
                me.live.borrow_mut().remove(&live.id);
                me.urls.borrow_mut().remove(&live.id);
                // TS 只关桥、不杀 cloudflared：进程从表里摘掉了却还活着，再也没人杀（share.ts 的
                // listenBridge）。这里连进程一起拆
                tokio::task::spawn_local(teardown(live));
            }
        });
        Ok((spawn_tunnel_listener(listener, link, live.dest_host.clone(), live.dest_port, on_error), port))
    }

    fn to_share(&self, row: &HostShareRow) -> PublicShare {
        let enabled = row.enabled == 1;
        let live = self.live.borrow().contains_key(&row.id);
        let state = if enabled && live && self.urls.borrow().contains_key(&row.id) {
            ForwardState::Active
        } else if enabled && (self.starting.borrow().contains_key(&row.id) || live) {
            ForwardState::Starting
        } else if enabled && self.errors.borrow().contains_key(&row.id) {
            ForwardState::Error
        } else {
            ForwardState::Stopped
        };
        PublicShare {
            id: row.id.clone(),
            host_id: row.host_id.clone(),
            name: row.name.clone(),
            dest_host: row.dest_host.clone(),
            dest_port: row.dest_port as u16,
            enabled,
            state,
            public_url: if enabled { self.urls.borrow().get(&row.id).cloned() } else { None },
            error: if enabled { self.errors.borrow().get(&row.id).cloned() } else { None },
            created_at: row.created_at,
        }
    }
}

fn not_found() -> RelayError {
    RelayError::NotFound("发布规则不存在".into())
}

fn slot_row(r: &HostShareRow) -> ShareSlotRow {
    ShareSlotRow {
        id: r.id.clone(),
        host_id: r.host_id.clone(),
        dest_port: r.dest_port,
        enabled: r.enabled,
        created_at: r.created_at,
    }
}

/// TS 的 `{ ...env, KEY: value }`：已有同名键就地改值，没有就追加
fn set_env(env: &mut Vec<(String, String)>, key: &str, value: &str) {
    match env.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value.to_string(),
        None => env.push((key.to_string(), value.to_string())),
    }
}

/// 拆一条发布。同步前缀（标 stopping、发 SIGTERM、abort 桥）在调用时就做；返回的 future
/// 等进程退出（2s 不退就 SIGKILL）、桥的监听器 drop。
fn teardown(live: Rc<LiveShare>) -> LocalBoxFuture<'static, ()> {
    live.stopping.set(true);
    let kill = live.child.borrow_mut().take().map(|child| child.kill());
    let bridge = live.bridge.borrow_mut().take();
    if let Some(h) = &bridge {
        h.abort();
    }
    Box::pin(async move {
        if let Some(kill) = kill {
            kill.await;
        }
        if let Some(h) = bridge {
            let _ = h.await;
        }
    })
}

/// 本机一个此刻空着的端口（给 cloudflared 的 metrics 用）。绑 0 拿到号就放掉，
/// 与 cloudflared 真正去绑之间有个小窗口，同 TS
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// 进程退出的样子（Node exit 事件的 code / signal）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Exit {
    code: Option<i32>,
    signal: Option<i32>,
}

impl Exit {
    fn from_status(status: std::io::Result<std::process::ExitStatus>) -> Exit {
        let Ok(status) = status else { return Exit::default() };
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Exit { code: status.code(), signal }
    }

    /// `cloudflared 已退出（1）` / `（SIGTERM）` 的括号部分
    fn suffix(&self) -> String {
        match (self.code, self.signal) {
            (Some(code), _) => format!("（{code}）"),
            (None, Some(sig)) => format!("（{}）", signal_name(sig)),
            (None, None) => String::new(),
        }
    }
}

/// Node 报的是信号名。这几个号在 Linux 与 macOS 上相同
fn signal_name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".into(),
        2 => "SIGINT".into(),
        3 => "SIGQUIT".into(),
        6 => "SIGABRT".into(),
        9 => "SIGKILL".into(),
        11 => "SIGSEGV".into(),
        13 => "SIGPIPE".into(),
        15 => "SIGTERM".into(),
        n => format!("signal {n}"),
    }
}

/// 一个 cloudflared 进程。Child 归收尸任务独占，这里只有 pid、退出状态与强杀通道，
/// 外加两根管道攒下的日志
struct Proc {
    pid: Option<u32>,
    exit: watch::Receiver<Option<Exit>>,
    force_kill: mpsc::UnboundedSender<()>,
    log: RefCell<String>,
    /// 拿到 URL 之后不再攒日志（管道照读，不读满了 cloudflared 会卡在写 stderr 上）
    capturing: Cell<bool>,
    /// 有新日志 / 管道关了
    changed: Notify,
    pipes_open: Cell<u8>,
}

impl Proc {
    fn spawn(bin: &str, args: &[String], env: &[(String, String)]) -> Result<Rc<Proc>, String> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| spawn_error(bin, &e))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (exit_tx, exit_rx) = watch::channel(None);
        let (force_tx, mut force_rx) = mpsc::unbounded_channel::<()>();
        let proc = Rc::new(Proc {
            pid: child.id(),
            exit: exit_rx,
            force_kill: force_tx,
            log: RefCell::new(String::new()),
            capturing: Cell::new(true),
            changed: Notify::new(),
            pipes_open: Cell::new(0),
        });
        // 收尸任务在 LocalSet 上：收尸与写退出状态在同一次 poll 里，别的任务不会看到「已收尸、
        // 状态还没写」的中间态，于是「没退出才发 SIGTERM」不会打到被复用的 pid 上
        tokio::task::spawn_local(async move {
            let mut force_open = true;
            loop {
                tokio::select! {
                    status = child.wait() => {
                        let _ = exit_tx.send(Some(Exit::from_status(status)));
                        break;
                    }
                    msg = force_rx.recv(), if force_open => match msg {
                        Some(()) => {
                            let _ = child.start_kill();
                        }
                        None => force_open = false,
                    },
                }
            }
        });
        if let Some(out) = stdout {
            Self::read_pipe(&proc, out);
        }
        if let Some(err) = stderr {
            Self::read_pipe(&proc, err);
        }
        Ok(proc)
    }

    fn read_pipe<R: AsyncRead + Unpin + 'static>(proc: &Rc<Proc>, mut pipe: R) {
        proc.pipes_open.set(proc.pipes_open.get() + 1);
        let proc = proc.clone();
        tokio::task::spawn_local(async move {
            let mut decoder = Utf8Decoder::default();
            let mut buf = vec![0u8; 8192];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = decoder.write(&buf[..n]);
                        proc.push_log(&text);
                    }
                }
            }
            let tail = decoder.end();
            proc.push_log(&tail);
            proc.pipes_open.set(proc.pipes_open.get() - 1);
            proc.changed.notify_one();
        });
    }

    fn push_log(&self, text: &str) {
        if text.is_empty() || !self.capturing.get() {
            return;
        }
        self.log.borrow_mut().push_str(text);
        self.changed.notify_one();
    }

    /// 已退出就是 Some。收尸任务没了（运行时在关）也当它退出了
    fn exit_now(&self) -> Option<Exit> {
        if let Some(exit) = *self.exit.borrow() {
            return Some(exit);
        }
        self.exit.has_changed().is_err().then(Exit::default)
    }

    async fn wait_exit(&self) -> Exit {
        let mut rx = self.exit.clone();
        match rx.wait_for(|v| v.is_some()).await {
            Ok(v) => v.unwrap_or_default(),
            Err(_) => Exit::default(),
        }
    }

    async fn pipes_closed(&self) {
        while self.pipes_open.get() > 0 {
            self.changed.notified().await;
        }
    }

    /// 先 SIGTERM，2s 不退再强杀（TS 的 killChild）。同步前缀（发 SIGTERM）在调用时就做
    fn kill(self: &Rc<Self>) -> LocalBoxFuture<'static, ()> {
        if self.exit_now().is_some() {
            return Box::pin(async {});
        }
        #[cfg(unix)]
        match self.pid {
            // SAFETY: 只是发信号。进程还没被收尸（见 spawn 里收尸任务的注释），pid 不会被复用
            Some(pid) => unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            },
            None => {
                let _ = self.force_kill.send(());
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.force_kill.send(());
        }
        let me = self.clone();
        Box::pin(async move {
            if tokio::time::timeout(STOP_GRACE, me.wait_exit()).await.is_err() {
                let _ = me.force_kill.send(());
                me.wait_exit().await;
            }
        })
    }
}

/// Node `spawn` 的 'error' 长这样：`spawn /path/to/cloudflared ENOENT`
fn spawn_error(bin: &str, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("spawn {bin} ENOENT"),
        std::io::ErrorKind::PermissionDenied => format!("spawn {bin} EACCES"),
        _ => format!("spawn {bin}：{e}"),
    }
}

/// 等公网 URL。日志解析与 /quicktunnel 轮询并行：有的版本框打得晚、metrics 先好，
/// 反过来也有。进程先退出就立刻失败，别空等到超时。
async fn wait_for_url(proc: &Rc<Proc>, metrics_port: u16, is_stopping: impl Fn() -> bool) -> Result<String, String> {
    let deadline = tokio::time::sleep(URL_WAIT);
    tokio::pin!(deadline);
    let mut poll = poll_metrics(proc.clone(), metrics_port);
    let mut drained = false;
    loop {
        let found = extract_quick_tunnel_url(&proc.log.borrow());
        if let Some(url) = found {
            proc.capturing.set(false);
            return Ok(url);
        }
        if let Some(exit) = proc.exit_now() {
            if !drained && proc.pipes_open.get() > 0 {
                drained = true;
                let _ = tokio::time::timeout(PIPE_DRAIN, proc.pipes_closed()).await;
                continue;
            }
            if is_stopping() {
                return Err("已取消".into());
            }
            let detail = last_log_lines(&proc.log.borrow(), None, None);
            return Err(if detail.is_empty() {
                format!("cloudflared 退出（{}）", exit.code.map_or("?".to_string(), |c| c.to_string()))
            } else {
                format!("cloudflared 退出：{detail}")
            });
        }
        tokio::select! {
            _ = proc.changed.notified() => {}
            _ = proc.wait_exit() => {}
            _ = &mut deadline => {
                let detail = last_log_lines(&proc.log.borrow(), None, None);
                tokio::task::spawn_local(proc.kill());
                return Err(if detail.is_empty() {
                    "等不到公网地址（Quick Tunnel 超时）".into()
                } else {
                    format!("等不到公网地址：{detail}")
                });
            }
            url = &mut poll => {
                if let Some(url) = url {
                    proc.capturing.set(false);
                    return Ok(url);
                }
                poll = poll_metrics(proc.clone(), metrics_port);
            }
        }
    }
}

/// 隔 300ms 问一次 metrics。地址优先认日志里打出来的那个
fn poll_metrics(proc: Rc<Proc>, metrics_port: u16) -> LocalBoxFuture<'static, Option<String>> {
    Box::pin(async move {
        tokio::time::sleep(METRICS_POLL).await;
        let from_log = extract_metrics_addr(&proc.log.borrow());
        let addr = from_log.unwrap_or_else(|| format!("127.0.0.1:{metrics_port}"));
        fetch_quick_tunnel(&addr).await
    })
}

/// `GET http://<addr>/quicktunnel`，2xx 才解析。连不上 / 超时（800ms）/ 不是隧道地址都是 None
async fn fetch_quick_tunnel(addr: &str) -> Option<String> {
    let fetch = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        let req = format!("GET /quicktunnel HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await.ok()?;
        let mut raw = Vec::new();
        (&mut stream).take(64 * 1024).read_to_end(&mut raw).await.ok()?;
        let text = String::from_utf8_lossy(&raw);
        let (head, body) = text.split_once("\r\n\r\n")?;
        let status: u16 = head.split_whitespace().nth(1)?.parse().ok()?;
        if !(200..300).contains(&status) {
            return None;
        }
        parse_quick_tunnel_metrics(body)
    };
    tokio::time::timeout(METRICS_TIMEOUT, fetch).await.ok().flatten()
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use tokio::net::TcpListener;

    use super::super::relay::fake::{FakeLink, echo_server, roundtrip};
    use super::*;
    use crate::db::SshHostRow;

    const URL: &str = "https://quiet-lake-1234.trycloudflare.com";

    struct Rig {
        dir: tempfile::TempDir,
        db: Arc<Db>,
        link: Rc<FakeLink>,
        reconnects: Rc<RefCell<Vec<String>>>,
        mgr: Rc<ShareManagerOf<FakeLink>>,
    }

    /// 假 cloudflared：把 argv / pid / NO_AUTOUPDATE 记到脚本旁边，再跑 `body`
    fn fake_cloudflared(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\necho $$ > \"$0.pid\"\necho \"$NO_AUTOUPDATE\" > \"$0.env\"\n{body}\n"
        );
        std::fs::write(&path, script).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// 打出公网地址后一直挂着（exec：SIGTERM 直接落在 sleep 上，不留孤儿）
    fn serving() -> String {
        format!("echo '2026-10-10T00:00:00Z INF |  {URL}  |' >&2\nexec sleep 30")
    }

    fn rig(body: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(dir.path()).unwrap());
        db.insert_host(&SshHostRow {
            id: "h1".into(),
            name: "h1".into(),
            host: "10.0.0.1".into(),
            port: 22,
            username: "fay".into(),
            auth_method: "agent".into(),
            created_at: 1,
            ..Default::default()
        });
        let link = Rc::new(FakeLink::default());
        let reconnects = Rc::new(RefCell::new(Vec::new()));
        let l = link.clone();
        let link_for: LinkForOf<FakeLink> = Rc::new(move |host: &str| (host == "h1").then(|| l.clone()));
        let r = reconnects.clone();
        let want: WantReconnect = Rc::new(move |host: &str| r.borrow_mut().push(host.to_string()));
        let mgr = ShareManagerOf::new(db.clone(), dir.path().to_path_buf(), link_for, want);
        let bin = fake_cloudflared(dir.path(), "cloudflared", body);
        *mgr.bin.borrow_mut() = Rc::new(move || {
            let bin = bin.clone();
            Box::pin(async move { Ok(bin) })
        });
        mgr.set_base_env(Rc::new(|| Box::pin(async { Rc::new(vec![("PATH".to_string(), "/usr/bin:/bin".to_string())]) })));
        Rig { dir, db, link, reconnects, mgr }
    }

    fn sidecar(r: &Rig, ext: &str) -> String {
        std::fs::read_to_string(r.dir.path().join(format!("cloudflared.{ext}"))).unwrap_or_default()
    }

    fn pid_alive(pid: &str) -> bool {
        let pid: i32 = pid.trim().parse().unwrap();
        // SAFETY: 信号 0 只探测存在与否
        unsafe { libc::kill(pid, 0) == 0 }
    }

    async fn settle(mgr: &Rc<ShareManagerOf<FakeLink>>, id: &str) -> PublicShare {
        for _ in 0..500 {
            let s = mgr.list().into_iter().find(|s| s.id == id).unwrap();
            let pending = s.state == ForwardState::Starting || (s.enabled && s.state == ForwardState::Stopped);
            if !pending {
                return s;
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
    fn local_share_gets_a_url_and_stopping_kills_cloudflared() {
        local(async {
            let r = rig(&serving());
            let s = r.mgr.create(&json!({ "destPort": 5173, "name": "vite" })).await.unwrap();
            assert_eq!(s.state, ForwardState::Starting);
            assert_eq!(s.host_id, None);
            let s = settle(&r.mgr, &s.id).await;
            assert_eq!((s.state, s.public_url.as_deref()), (ForwardState::Active, Some(URL)));

            let args: Vec<String> = sidecar(&r, "args").lines().map(Into::into).collect();
            assert_eq!(&args[..7], [
                "tunnel",
                "--no-autoupdate",
                "--url",
                "http://127.0.0.1:5173",
                "--http-host-header",
                "localhost:5173",
                "--metrics"
            ]);
            assert!(args[7].starts_with("127.0.0.1:"));
            assert_eq!(sidecar(&r, "env").trim(), "true", "NO_AUTOUPDATE 打进子进程环境");

            let pid = sidecar(&r, "pid");
            assert!(pid_alive(&pid));
            let s = r.mgr.update(&s.id, &json!({ "enabled": false })).await.unwrap();
            assert_eq!((s.state, s.public_url, s.error), (ForwardState::Stopped, None, None));
            assert!(!pid_alive(&pid), "停用要杀掉 cloudflared");
        });
    }

    #[test]
    fn exit_before_the_url_reports_the_last_log_lines() {
        local(async {
            let r = rig("echo 'INF Requesting new quick Tunnel' >&2\necho 'ERR failed to request quick Tunnel: 429 Too Many Requests' >&2\nexit 1");
            let s = r.mgr.create(&json!({ "destPort": 3000 })).await.unwrap();
            assert_eq!(
                r.mgr.start(&s.id).await,
                Err(RelayError::Failed(
                    "cloudflared 退出：INF Requesting new quick Tunnel · ERR failed to request quick Tunnel: 429 Too Many Requests"
                        .into()
                ))
            );
            let s = settle(&r.mgr, &s.id).await;
            assert_eq!(s.state, ForwardState::Error);
            assert!(s.error.unwrap().contains("429"));
        });
    }

    #[test]
    fn exit_after_the_url_turns_the_rule_into_an_error() {
        local(async {
            let r = rig(&format!("echo '{URL}' >&2\nsleep 0.3\nexit 3"));
            let s = r.mgr.create(&json!({ "destPort": 3000 })).await.unwrap();
            assert_eq!(settle(&r.mgr, &s.id).await.state, ForwardState::Active);
            for _ in 0..300 {
                if r.mgr.list()[0].state == ForwardState::Error {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let s = r.mgr.list().remove(0);
            assert_eq!((s.state, s.error.as_deref(), s.public_url), (ForwardState::Error, Some("cloudflared 已退出（3）"), None));
        });
    }

    #[test]
    fn stopping_while_waiting_for_the_url_is_not_an_error() {
        local(async {
            let r = rig("exec sleep 30");
            let s = r.mgr.create(&json!({ "destPort": 3000 })).await.unwrap();
            for _ in 0..200 {
                if !sidecar(&r, "pid").is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let pid = sidecar(&r, "pid");
            let started = std::time::Instant::now();
            let s = r.mgr.update(&s.id, &json!({ "enabled": false })).await.unwrap();
            assert!(started.elapsed() < Duration::from_secs(2), "SIGTERM 就够，不该等满宽限");
            assert_eq!((s.state, s.error), (ForwardState::Stopped, None));
            assert!(!pid_alive(&pid));
            assert!(r.mgr.errors.borrow().is_empty());
        });
    }

    #[test]
    fn a_second_share_of_the_same_port_displaces_the_first() {
        local(async {
            let r = rig(&serving());
            let a = r.mgr.create(&json!({ "destPort": 5173 })).await.unwrap();
            assert_eq!(settle(&r.mgr, &a.id).await.state, ForwardState::Active);
            let pid_a = sidecar(&r, "pid");
            // 同一台机器上发另一个端口不让位；同端口让位
            let other = r.mgr.create(&json!({ "destPort": 5174, "enabled": false })).await.unwrap();
            let b = r.mgr.create(&json!({ "destPort": "5173", "destHost": "localhost" })).await.unwrap();
            assert_eq!(r.db.get_share(&a.id).unwrap().enabled, 0);
            assert_eq!(settle(&r.mgr, &b.id).await.state, ForwardState::Active);
            assert!(!pid_alive(&pid_a), "让位的那条的进程被杀掉");
            let a = r.mgr.list().into_iter().find(|s| s.id == a.id).unwrap();
            assert_eq!((a.enabled, a.state, a.public_url), (false, ForwardState::Stopped, None));
            assert_eq!(r.mgr.list().into_iter().find(|s| s.id == other.id).unwrap().state, ForwardState::Stopped);
            r.mgr.shutdown().await;
            assert!(!pid_alive(&sidecar(&r, "pid")), "shutdown 杀掉全部 cloudflared");
        });
    }

    #[test]
    fn remote_share_bridges_through_the_host_link() {
        local(async {
            let r = rig(&serving());
            let echo = echo_server().await;
            r.link.tunnel_to.borrow_mut().insert(8080, echo);
            let s = r.mgr.create(&json!({ "hostId": "h1", "destPort": 8080 })).await.unwrap();
            assert_eq!(s.host_id.as_deref(), Some("h1"));
            assert_eq!(settle(&r.mgr, &s.id).await.state, ForwardState::Active);
            assert!(r.mgr.has_enabled("h1"));
            assert_eq!(r.mgr.enabled_host_ids(), ["h1"]);

            let args: Vec<String> = sidecar(&r, "args").lines().map(Into::into).collect();
            // Quick Tunnel 指向本机桥，Host 仍是远端 origin 自己的
            let bridge: u16 = args[3].strip_prefix("http://127.0.0.1:").unwrap().parse().unwrap();
            assert_ne!(bridge, 8080);
            assert_eq!(args[5], "localhost:8080");
            let mut c = TcpStream::connect(("127.0.0.1", bridge)).await.unwrap();
            assert_eq!(roundtrip(&mut c, b"GET /").await, b"GET /");
            assert_eq!(r.link.tunnels.borrow().as_slice(), [("127.0.0.1".to_string(), 8080)]);

            // 断链：拆桥杀进程、挂上原因；回来后重新拉起
            let pid = sidecar(&r, "pid");
            r.mgr.on_link_down("h1");
            let s = r.mgr.list().remove(0);
            assert_eq!((s.state, s.error.as_deref()), (ForwardState::Error, Some("SSH 链路断开")));
            for _ in 0..300 {
                if !pid_alive(&pid) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(!pid_alive(&pid));
            r.mgr.on_link_up("h1");
            assert_eq!(settle(&r.mgr, &s.id).await.state, ForwardState::Active);
            r.mgr.forget_host("h1").await;
            assert!(!pid_alive(&sidecar(&r, "pid")));
        });
    }

    #[test]
    fn unreachable_host_is_an_error_and_asks_for_reconnect() {
        local(async {
            let r = rig(&serving());
            *r.link.fail_connect.borrow_mut() = Some("All configured authentication methods failed".into());
            let s = r.mgr.create(&json!({ "hostId": "h1", "destPort": 8080 })).await.unwrap();
            let s = settle(&r.mgr, &s.id).await;
            assert_eq!(
                (s.state, s.error.as_deref()),
                (ForwardState::Error, Some("All configured authentication methods failed"))
            );
            assert_eq!(r.reconnects.borrow().as_slice(), ["h1"]);
            assert!(sidecar(&r, "pid").is_empty(), "连不上就不起 cloudflared");
            r.mgr.mark_unreachable("h1", "Connection refused");
            assert_eq!(r.mgr.list()[0].error.as_deref(), Some("Connection refused"));
        });
    }

    #[test]
    fn create_and_update_validate_like_ts() {
        local(async {
            let r = rig(&serving());
            assert_eq!(r.mgr.create(&json!({ "hostId": 5, "destPort": 1 })).await.unwrap_err(), RelayError::NotFound("主机不存在".into()));
            assert_eq!(r.mgr.create(&json!({ "hostId": "gone", "destPort": 1 })).await.unwrap_err(), RelayError::NotFound("主机不存在".into()));
            // 缺省的请求体按 {} 处理：挂本机，然后在端口上失败
            assert_eq!(r.mgr.create(&Value::Null).await.unwrap_err(), RelayError::Invalid("目标端口无效".into()));
            let s = r.mgr.create(&json!({ "hostId": "", "destPort": 1, "enabled": false, "name": "x" })).await.unwrap();
            assert_eq!((s.host_id, s.state), (None, ForwardState::Stopped));
            let s = r.mgr.update(&s.id, &json!({ "destPort": "2", "name": null })).await.unwrap();
            assert_eq!((s.dest_port, s.name, s.enabled), (2, None, false));
            assert_eq!(r.mgr.update(&s.id, &json!({ "destHost": "a b" })).await.unwrap_err(), RelayError::Invalid("目标地址无效".into()));
            assert_eq!(r.mgr.update("nope", &json!({})).await.unwrap_err(), RelayError::NotFound("发布规则不存在".into()));
            assert_eq!(r.mgr.remove("nope").await.unwrap_err(), RelayError::NotFound("发布规则不存在".into()));
            r.mgr.remove(&s.id).await.unwrap();
            assert!(r.mgr.list().is_empty());
        });
    }

    #[test]
    fn restore_local_only_starts_local_enabled_shares() {
        local(async {
            let r = rig(&serving());
            let row = |id: &str, host: Option<&str>, enabled: i64| HostShareRow {
                id: id.into(),
                host_id: host.map(Into::into),
                dest_host: "127.0.0.1".into(),
                dest_port: 3000,
                enabled,
                created_at: 1,
                ..Default::default()
            };
            r.db.insert_share(&row("local-on", None, 1));
            r.db.insert_share(&row("local-off", None, 0));
            r.db.insert_share(&row("remote-on", Some("h1"), 1));
            r.mgr.restore_local();
            assert_eq!(settle(&r.mgr, "local-on").await.state, ForwardState::Active);
            let states: Vec<_> = r.mgr.list().into_iter().map(|s| (s.id, s.state)).collect();
            assert!(states.contains(&("local-off".into(), ForwardState::Stopped)));
            assert!(states.contains(&("remote-on".into(), ForwardState::Stopped)), "远端的等链路 up");
            assert_eq!(r.link.connects.get(), 0);
            r.mgr.shutdown().await;
        });
    }

    #[test]
    fn url_can_come_from_the_metrics_endpoint() {
        local(async {
            // 假 metrics 服务：/quicktunnel 回裸域名
            let metrics = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = metrics.local_addr().unwrap().port();
            tokio::spawn(async move {
                while let Ok((mut s, _)) = metrics.accept().await {
                    let mut buf = [0u8; 1024];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /quicktunnel HTTP/1.0\r\n"));
                    let _ = s
                        .write_all(b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"hostname\":\"metrics-only.trycloudflare.com\"}")
                        .await;
                }
            });
            // 日志里只有 metrics 地址、没有 URL 框
            let r = rig(&format!("echo 'INF Starting metrics server on 127.0.0.1:{port}/metrics' >&2\nexec sleep 30"));
            let s = r.mgr.create(&json!({ "destPort": 3000 })).await.unwrap();
            let s = settle(&r.mgr, &s.id).await;
            assert_eq!(s.public_url.as_deref(), Some("https://metrics-only.trycloudflare.com"));
            r.mgr.shutdown().await;
        });
    }

    #[test]
    fn fetch_quick_tunnel_only_accepts_2xx() {
        local(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move {
                let mut n = 0;
                while let Ok((mut s, _)) = listener.accept().await {
                    let mut buf = [0u8; 512];
                    let _ = s.read(&mut buf).await;
                    let resp: &[u8] = if n == 0 {
                        b"HTTP/1.0 404 Not Found\r\n\r\n{\"hostname\":\"a.trycloudflare.com\"}"
                    } else {
                        b"HTTP/1.0 200 OK\r\n\r\n{\"hostname\":\"https://a.trycloudflare.com/\"}"
                    };
                    n += 1;
                    let _ = s.write_all(resp).await;
                }
            });
            assert_eq!(fetch_quick_tunnel(&addr).await, None);
            assert_eq!(fetch_quick_tunnel(&addr).await.as_deref(), Some("https://a.trycloudflare.com"));
            assert_eq!(fetch_quick_tunnel("127.0.0.1:1").await, None);
        });
    }

    #[test]
    fn exit_suffix_matches_node() {
        assert_eq!(Exit { code: Some(1), signal: None }.suffix(), "（1）");
        assert_eq!(Exit { code: None, signal: Some(15) }.suffix(), "（SIGTERM）");
        assert_eq!(Exit::default().suffix(), "");
        let mut env = vec![("A".to_string(), "1".to_string()), ("NO_AUTOUPDATE".into(), "false".into())];
        set_env(&mut env, "NO_AUTOUPDATE", "true");
        set_env(&mut env, "B", "2");
        assert_eq!(env, [("A".into(), "1".into()), ("NO_AUTOUPDATE".into(), "true".into()), ("B".into(), "2".into())]);
    }
}
