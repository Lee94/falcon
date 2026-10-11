//! 飞书项目面板的客户端内存缓存。对应旧 React 版的 `lib/meegleCache.ts`（ADR 0010）。
//!
//! 面板卸载（切到别的面板、收起右侧栏）后数据还在，再打开立刻画出上次的列表；
//! [`MEEGLE_REVALIDATE_MS`] 内不打网络。刷新按钮 / 换人登录会清空。
//!
//! 不落盘：工作项是内部数据，不该跟着偏好文件走。后端另有一份同样 TTL 的 CLI 缓存，
//! 重开 App 后 HTTP 也是毫秒级。
//!
//! React 版是模块级单例；这里是可实例化的 [`MeegleCache`]，另给一个进程级的
//! [`meegle_cache()`]。值是异构的（待办列表、视图条目、详情各是各的类型），存成
//! `Arc<dyn Any>`，取的时候按类型认——类型对不上当没命中。

use std::any::Any;
use std::future::Future;
use std::sync::Arc;
#[cfg(not(target_family = "wasm"))]
use std::sync::OnceLock;

use crate::maybe_send::MaybeSend;
use crate::ttl_cache::{LoadError, TtlCache, TtlCacheLoadOpts, now_ms};

pub const MEEGLE_CACHE_MS: i64 = 5 * 60_000;
/// 超过这个时间仍画出缓存，但后台静默再拉一次
pub const MEEGLE_REVALIDATE_MS: i64 = 30_000;

type AnyValue = Arc<dyn Any + Send + Sync>;

#[derive(Clone)]
pub struct MeegleCache {
    cache: TtlCache<AnyValue>,
}

impl Default for MeegleCache {
    fn default() -> Self {
        MeegleCache { cache: TtlCache::new(MEEGLE_CACHE_MS) }
    }
}

impl MeegleCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取缓存值（未过期、且类型对得上）
    pub fn peek<T: Clone + 'static>(&self, key: &str) -> Option<T> {
        self.cache.peek(key, None)?.value.downcast_ref::<T>().cloned()
    }

    /// 没有、或写入已超过 [`MEEGLE_REVALIDATE_MS`]：该在后台再拉一次了
    pub fn stale(&self, key: &str, now: Option<i64>) -> bool {
        let now = now.unwrap_or_else(now_ms);
        match self.cache.peek(key, Some(now)) {
            None => true,
            Some(hit) => now - hit.at >= MEEGLE_REVALIDATE_MS,
        }
    }

    pub fn write<T: Send + Sync + 'static>(&self, key: &str, value: T) {
        self.cache.set(key, Arc::new(value), None, None);
    }

    /// 命中给缓存值，否则并入 / 发起一次 load（`fresh` 跳过已有条目）。
    /// 缓存里同 key 存的是别的类型时当没命中，重新 load。
    pub fn load<T, F, Fut>(&self, key: &str, load: F, fresh: bool) -> impl Future<Output = Result<T, LoadError>> + MaybeSend + 'static
    where
        T: Clone + Send + Sync + 'static,
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<T>> + MaybeSend + 'static,
    {
        let fresh = fresh || (self.cache.peek(key, None).is_some_and(|hit| !hit.value.is::<T>()));
        let fut = self.cache.get_or_load(
            key,
            move || {
                let inner = load();
                async move { inner.await.map(|v| Arc::new(v) as AnyValue) }
            },
            TtlCacheLoadOpts { fresh, ..Default::default() },
        );
        async move {
            let value = fut.await?;
            value
                .downcast_ref::<T>()
                .cloned()
                .ok_or_else(|| Arc::new(anyhow::anyhow!("飞书项目缓存里这个 key 的值类型对不上")))
        }
    }

    pub fn clear(&self) {
        self.cache.clear();
    }
}

/// 进程级的那一份（React 版的模块单例）
#[cfg(not(target_family = "wasm"))]
pub fn meegle_cache() -> &'static MeegleCache {
    static CACHE: OnceLock<MeegleCache> = OnceLock::new();
    CACHE.get_or_init(MeegleCache::new)
}

/// 浏览器里只有一条线程；在飞的 load 不是 `Send`，缓存也就不是 `Sync`，放不进 static。
/// 线程局部里存一份泄漏出来的 `'static` 引用，对外签名不变
#[cfg(target_family = "wasm")]
pub fn meegle_cache() -> &'static MeegleCache {
    thread_local! {
        static CACHE: &'static MeegleCache = Box::leak(Box::new(MeegleCache::new()));
    }
    CACHE.with(|c| *c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn write_then_peek_and_clear_removes_it() {
        let cache = MeegleCache::new();
        cache.write("todo:todo", vec![1]);
        assert_eq!(cache.peek::<Vec<i32>>("todo:todo"), Some(vec![1]));
        cache.clear();
        assert_eq!(cache.peek::<Vec<i32>>("todo:todo"), None);
    }

    #[test]
    fn a_fresh_write_is_not_stale_and_get_or_load_skips_the_load_on_hit() {
        let cache = MeegleCache::new();
        cache.write("k", 1u32);
        assert!(!cache.stale("k", None));
        let n = Arc::new(AtomicU32::new(0));
        let bump = |n: &Arc<AtomicU32>| {
            let n = n.clone();
            move || async move { Ok(n.fetch_add(1, Ordering::SeqCst) + 1) }
        };
        assert_eq!(block_on(cache.load::<u32, _, _>("k", bump(&n), false)).unwrap(), 1);
        assert_eq!(n.load(Ordering::SeqCst), 0);
        assert_eq!(block_on(cache.load::<u32, _, _>("k", bump(&n), true)).unwrap(), 1);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn staleness_and_type_mismatch() {
        // 以下是 Rust 侧补的：30s 之后算陈旧；类型对不上当没命中
        let cache = MeegleCache::new();
        assert!(cache.stale("none", None));
        cache.write("k", 1u32);
        assert!(cache.stale("k", Some(now_ms() + MEEGLE_REVALIDATE_MS)));
        assert_eq!(cache.peek::<String>("k"), None);
        let v = block_on(cache.load::<String, _, _>("k", || async { Ok("s".to_string()) }, false)).unwrap();
        assert_eq!(v, "s");
        assert!(std::ptr::eq(meegle_cache(), meegle_cache()));
    }
}
