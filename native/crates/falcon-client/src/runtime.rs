//! 全局网络运行时，以及"把工作丢过去、外面只等结果"的那层胶水。
//!
//! 两个 target 的口径不同，对 crate 其余部分只露同一组函数：
//!
//! - **原生**：crate 自带一个 tokio 多线程运行时（[`runtime()`]），[`run`] 把工作 spawn 过去
//!   再等 JoinHandle，[`spawn`] 把会话 socket 的驱动循环挂上去；
//! - **浏览器（wasm32）**：只有一条线程、没有 tokio 运行时。[`run`] 就地 await（fetch 本来就
//!   不占线程），[`spawn`] 是 `spawn_local`，[`sleep`] 是 setTimeout。返回的 future 不是
//!   `Send`（里面攥着 JsValue），所以公开方法的返回类型用 [`MaybeSend`] 而不是 `Send`。

use std::future::Future;
use std::time::Duration;

/// 原生上等于 `Send`；wasm 上不要求（浏览器里只有一条线程，fetch / WebSocket 的 future
/// 本来就不是 `Send`）。公开方法的返回类型写 `impl Future<..> + MaybeSend + 'static`。
#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send + ?Sized> MaybeSend for T {}

#[cfg(target_family = "wasm")]
pub trait MaybeSend {}
#[cfg(target_family = "wasm")]
impl<T: ?Sized> MaybeSend for T {}

/// 在会话回调里做一段重活（解析 4MB 回放）。原生上回调跑在网络运行时的 worker 上，用
/// `block_in_place` 让 tokio 把这个 worker 上的其他任务挪走（回调约定：别长时间占着网络
/// worker）；浏览器里只有主线程，就地跑。都是在当前线程上同步执行——回调的顺序不变
#[cfg(not(target_family = "wasm"))]
pub fn block_in_place<R>(f: impl FnOnce() -> R) -> R {
    tokio::task::block_in_place(f)
}

#[cfg(target_family = "wasm")]
pub fn block_in_place<R>(f: impl FnOnce() -> R) -> R {
    f()
}

/// 后台跑一个驱动循环（会话 / 安装 socket），不等结果。
#[cfg(not(target_family = "wasm"))]
pub(crate) fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
    runtime().spawn(fut);
}

#[cfg(target_family = "wasm")]
pub(crate) fn spawn(fut: impl Future<Output = ()> + 'static) {
    wasm_bindgen_futures::spawn_local(fut);
}

/// 与执行器无关地睡一会儿：原生走网络运行时的 tokio 定时器（调用方本来就跑在它上面），
/// 浏览器走 setTimeout。
#[cfg(not(target_family = "wasm"))]
pub(crate) async fn sleep(d: Duration) {
    tokio::time::sleep(d).await
}

#[cfg(target_family = "wasm")]
pub(crate) async fn sleep(d: Duration) {
    // setTimeout 的延迟是 i32 毫秒，再长就溢出成立即触发；停车用的一小时远在上限之内
    let ms = d.as_millis().min(i32::MAX as u128) as u32;
    gloo_timers::future::TimeoutFuture::new(ms).await
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

/// 浏览器上没有别的线程可以丢：就地 await。签名与原生那份一致。
#[cfg(target_family = "wasm")]
pub(crate) async fn run<T: 'static>(fut: impl Future<Output = T> + 'static) -> Result<T, Cancelled> {
    Ok(fut.await)
}

/// 网络任务在出结果之前被取消（运行时关停）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cancelled;

#[cfg(not(target_family = "wasm"))]
pub use native::runtime;
#[cfg(not(target_family = "wasm"))]
pub(crate) use native::run;

#[cfg(not(target_family = "wasm"))]
mod native;
