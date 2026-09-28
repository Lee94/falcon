//! 全局网络运行时，以及"把工作丢过去、外面只等结果"的那层胶水。

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use tokio::runtime::Runtime;
use tokio::task::{JoinError, JoinHandle};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// crate 内部的 tokio 多线程运行时，第一次用到时创建，进程内只有一个。
///
/// worker 数取 2–4：网络层是 I/O 密集的，线程多了只是多几个空转的 epoll；但至少
/// 要 2 个——会话 socket 在 worker 上同步回调 sink（app 在里面解析终端字节），
/// 一个 worker 被某个会话的大段回放占住时，别的会话和 REST 还得有人跑。
///
/// app 一般不需要碰它；暴露出来是给测试与少数确实要在网络线程上跑点东西的场合。
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4))
            .unwrap_or(2);
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .thread_name("falcon-net")
            .enable_all()
            .build()
            .expect("创建 falcon-net 运行时失败")
    })
}

/// 把 `fut` spawn 到网络运行时上，返回一个等它结果的 future。
///
/// 返回的 future 只 poll 一个 `JoinHandle`，不需要 tokio 上下文——GPUI 的执行器、
/// `futures::executor::block_on` 都能等。它被丢掉时顺手 abort 那个任务：调用方已经
/// 不要结果了，请求没必要继续跑（尤其是上传 / 下载）。
///
/// 任务 panic 原样在等待方重新抛出：那是 bug，别吞成一个看似正常的网络错误。
pub(crate) async fn run<T: Send + 'static>(
    fut: impl Future<Output = T> + Send + 'static,
) -> Result<T, Cancelled> {
    match (AbortOnDrop(runtime().spawn(fut))).await {
        Ok(v) => Ok(v),
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        // 只有运行时关停（进程退出）才会走到这里；我们自己 abort 时没人在等
        Err(_) => Err(Cancelled),
    }
}

/// 网络任务在出结果之前被取消（运行时关停）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cancelled;

/// 被丢掉时 abort 任务的 JoinHandle。任务已经结束时 abort 是空操作。
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn runs_outside_tokio() {
        // 不在任何 tokio 上下文里：block_on 只认 Waker，照样拿得到结果
        let v = futures::executor::block_on(run(async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            std::thread::current().name().map(str::to_owned)
        }))
        .unwrap();
        assert_eq!(v.as_deref(), Some("falcon-net"));
    }

    #[test]
    fn dropping_the_future_aborts_the_task() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let fut = Box::pin(run(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            flag.store(true, Ordering::SeqCst);
        }));
        // poll 一次让它真的 spawn 出去，再丢掉
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = fut;
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        drop(fut);
        std::thread::sleep(Duration::from_millis(400));
        assert!(!finished.load(Ordering::SeqCst), "被丢掉的请求不该跑完");
    }
}
