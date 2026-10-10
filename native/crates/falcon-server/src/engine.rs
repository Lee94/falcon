//! 会话引擎：跑在一条专用线程的单线程 LocalSet 上的 actor。
//!
//! Node 版的会话核心（SessionManager、SshLink、中转、px0）天然是单线程的：事件循环保证了
//! 「一段同步代码不会被别的回调打断」，manager.ts 里大量的状态机（单飞、`entry.attaching`、
//! 退避重连的登记在册检查）都默认了这一点。这里不把它们改写成 `Arc<Mutex<…>>` 的多线程
//! 版本——那等于逐行重新论证一遍并发正确性——而是保留同一个执行模型：
//!
//! - 引擎线程上一个 current_thread runtime + LocalSet，状态全是 `Rc` / `RefCell`，与 Node
//!   的事件循环同一个语义（同步段不会被打断；await 点才会让出）。
//! - HTTP / WS 处理器跑在多线程 runtime 上，经 [`EngineHandle`] 把一个闭包投进引擎的任务
//!   通道，需要结果的用 [`EngineHandle::call`] 拿 oneshot 等回来。投递保序：同一条 WS 连接
//!   发来的 input / resize 按到达顺序执行。
//! - 引擎里**不能有阻塞调用**（同 Node 里不能有同步 I/O）：阻塞的 PTY 读写在 local.rs 里各开
//!   线程，SQLite 的同步调用照 Node 版一样直接做（单条语句毫秒级）。

use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use crate::askpass::hub::AskpassHub;
use crate::crypto::SecretBox;
use crate::db::{Db, ProjectRow};
use crate::git::error::WorktreeError;
use crate::git::host::{GitHost, git_host_for};
use crate::git::remove::cleanup_worktree;
use crate::px0::manager::Px0Manager;
use crate::sessions::manager::SessionManager;

/// 投进引擎的一件事。在引擎线程上**同步**执行；要 await 的自己 spawn_local
pub type Job = Box<dyn FnOnce(&Rc<Engine>) + Send>;

/// 引擎线程已经退出（进程正在关闭，或引擎里 panic 了）
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("会话引擎已退出")]
pub struct EngineGone;

/// 引擎的投递口。克隆便宜，可以跨线程
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Job>,
}

impl EngineHandle {
    /// 投一件不需要结果的事（WS 的 input / resize 走这里，保序）
    pub fn send(&self, f: impl FnOnce(&Rc<Engine>) + Send + 'static) {
        let _ = self.tx.send(Box::new(f));
    }

    /// 在引擎上跑一段 async 代码并等结果。`f` 本身要 `Send`（从别的线程投过来），
    /// 它返回的 future 不必——那是在引擎线程上构造、在引擎线程上跑的
    pub async fn call<R, F, Fut>(&self, f: F) -> Result<R, EngineGone>
    where
        R: Send + 'static,
        F: FnOnce(Rc<Engine>) -> Fut + Send + 'static,
        Fut: Future<Output = R> + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.send(move |engine| {
            let engine = engine.clone();
            tokio::task::spawn_local(async move {
                let _ = tx.send(f(engine).await);
            });
        });
        // 引擎里 panic 的任务被 runtime 吞掉（sender 随之被丢），这里收到的是 Err
        rx.await.map_err(|_| EngineGone)
    }
}

/// 引擎线程上的全部状态
pub struct Engine {
    pub db: Arc<Db>,
    pub secrets: Arc<SecretBox>,
    pub data_dir: PathBuf,
    pub askpass: Arc<AskpassHub>,
    pub sessions: Rc<SessionManager>,
    /// px0 审阅实例（ADR 0017）：SSH 项目复用项目链路
    pub px0: Rc<Px0Manager>,
}

/// 起引擎时要的东西
pub struct EngineDeps {
    pub db: Arc<Db>,
    pub secrets: Arc<SecretBox>,
    pub data_dir: PathBuf,
    pub askpass: Arc<AskpassHub>,
}

/// 在专用线程上起引擎。`startup` 在引擎建好后、开始处理投递前跑一次（恢复中转、
/// 接回持久会话、预热本地环境）。返回的 JoinHandle 在所有 EngineHandle 都丢掉后结束
pub fn spawn(
    deps: EngineDeps,
    startup: impl FnOnce(&Rc<Engine>) + Send + 'static,
) -> std::io::Result<(EngineHandle, std::thread::JoinHandle<()>)> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
    let thread = std::thread::Builder::new().name("falcon-engine".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                log::error!("会话引擎起不来：{e}");
                return;
            }
        };
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async move {
            let engine = Engine::new(deps);
            startup(&engine);
            while let Some(job) = rx.recv().await {
                job(&engine);
            }
        });
    })?;
    Ok((EngineHandle { tx }, thread))
}

impl Engine {
    pub fn new(deps: EngineDeps) -> Rc<Self> {
        let sessions = SessionManager::new(deps.db.clone(), deps.secrets.clone(), deps.data_dir.clone(), deps.askpass.clone());
        let px0 = {
            let sessions = Rc::downgrade(&sessions);
            Px0Manager::new(
                deps.data_dir.clone(),
                Rc::new(move |row: &ProjectRow| sessions.upgrade().expect("SessionManager 还活着").get_link(row)),
            )
        };
        Rc::new(Engine { db: deps.db, secrets: deps.secrets, data_dir: deps.data_dir, askpass: deps.askpass, sessions, px0 })
    }

    /// 项目宿主机上的 git 执行环境。SSH 侧复用项目链路（按 projectId 缓存），不另开连接
    pub async fn git_host(&self, row: &ProjectRow) -> Result<GitHost, WorktreeError> {
        git_host_for(row, || self.sessions.get_link(row)).await
    }

    /// 清理附属项目的 worktree 目录（git/remove.rs 是唯一入口）。执行环境都拿不到时报错，
    /// 调用方把它写进 warning
    pub async fn cleanup_worktree(&self, row: &ProjectRow, other_dirs: &[String]) -> Result<Vec<String>, WorktreeError> {
        let host = self.git_host(row).await?;
        Ok(cleanup_worktree(row, &host, other_dirs).await)
    }

    /// 停掉项目的 px0 实例（它开的是旧目录 / 旧主机，或者目录要删了）
    pub async fn stop_px0(&self, project_id: &str) {
        self.px0.stop(project_id).await;
    }

    /// 进程退出前收尸：cloudflared 与本机 px0 是子进程，不杀的话 Quick Tunnel 还会在公网挂着、
    /// px0 还占着端口；远端的 px0 随 SSH 通道关闭（pty 挂断）自己退出
    pub async fn shutdown(&self) {
        futures::future::join(self.sessions.shares.shutdown(), self.px0.shutdown()).await;
    }
}
