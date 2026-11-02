//! 全局网络运行时，以及"把工作丢过去、外面只等结果"的那层胶水。
//!
//! crate 自带一个 tokio 多线程运行时（[`runtime()`]），[`run`] 把工作 spawn 过去
//! 再等 JoinHandle，[`spawn`] 把会话 socket 的驱动循环挂上去。

use std::future::Future;
use std::time::Duration;

pub trait MaybeSend: Send {}
impl<T: Send + ?Sized> MaybeSend for T {}

/// 在会话回调里做一段重活（解析 4MB 回放）。回调跑在网络运行时的 worker 上，用
/// `block_in_place` 让 tokio 把这个 worker 上的其他任务挪走（回调约定：别长时间占着网络
/// worker）。在当前线程上同步执行——回调的顺序不变
pub fn block_in_place<R>(f: impl FnOnce() -> R) -> R {
    tokio::task::block_in_place(f)
}

/// 后台跑一个驱动循环（会话 / 安装 socket），不等结果。
pub(crate) fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
    runtime().spawn(fut);
}

/// 走网络运行时的 tokio 定时器（调用方本来就跑在它上面）。
pub(crate) async fn sleep(d: Duration) {
    tokio::time::sleep(d).await
}

/// `fut` 在 `d` 之内完成就是 `Some`，否则 `None`（`fut` 被丢掉）。
pub(crate) async fn timeout<T>(d: Duration, fut: impl Future<Output = T>) -> Option<T> {
    use futures::future::{Either, select};
    let fut = std::pin::pin!(fut);
    let timer = std::pin::pin!(sleep(d));
    match select(fut, timer).await {
        Either::Left((v, _)) => Some(v),
        Either::Right(_) => None,
    }
}

/// 网络任务在出结果之前被取消（运行时关停）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cancelled;

pub use native::runtime;
pub(crate) use native::run;

mod native;
