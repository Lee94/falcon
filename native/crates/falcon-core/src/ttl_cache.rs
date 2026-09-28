//! 带 TTL 与 in-flight 合并的内存缓存。对应 `packages/shared/src/ttlCache.ts`。
//!
//! 飞书项目面板两边共用：服务端挡住重复的 meegle CLI 进程（一次待办 2–4s），客户端挡住
//! 面板卸载后再挂上的重复 HTTP。同一 key 并发只跑一份 load；load 失败不入库，下次再试。
//! `fresh` 跳过已有条目，但仍并入正在飞的那一次——刷新时别再起一条请求。
//!
//! [`TtlCache::clear`] 会抬 generation：清之前已经在飞的 load 回来后不许写回，免得刚刷新
//! 又被旧结果盖住。
//!
//! 与 TS 的差别（Rust 的 future 是惰性的）：
//! - `load` 闭包在 [`TtlCache::get_or_load`] 里**同步**调用（与 TS 里 `load()` 在第一个
//!   `await` 之前就跑一样），但它返回的 future 要有人 poll 才会前进；共享的那份 future
//!   任何一个等待方 poll 都能推动，全都丢掉则 load 停在半路、in-flight 条目留着，直到
//!   下一次 `clear()`。TS 的 Promise 不管有没有人等都会跑完。
//! - TS 的缓存是异构的（`peek<T>`）；这里一个缓存一种值类型 `V`，要异构就用
//!   `Arc<dyn Any + Send + Sync>`（见 [`crate::meegle_cache`]）。
//! - load 的错误是 `anyhow::Error`，共享给所有等待方，所以包在 `Arc` 里。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures::FutureExt;
use futures::future::{BoxFuture, Shared};

/// 共享给所有等待方的 load 错误
pub type LoadError = Arc<anyhow::Error>;

/// 现在的 Unix 毫秒（TS 的 `Date.now()`）
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// 命中的条目
#[derive(Debug, Clone, PartialEq)]
pub struct TtlCacheEntry<V> {
    /// 写入时刻（Unix 毫秒）
    pub at: i64,
    pub value: V,
}

/// [`TtlCache::get_or_load`] 的选项
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TtlCacheLoadOpts {
    /// 忽略已有条目，强制跑 load（仍并入 in-flight）
    pub fresh: bool,
    /// 覆盖构造时的默认 TTL
    pub ttl_ms: Option<i64>,
    /// 测试用：冻结"现在"
    pub now: Option<i64>,
}

struct Slot<V> {
    at: i64,
    exp: i64,
    value: V,
}

type SharedLoad<V> = Shared<BoxFuture<'static, Result<V, LoadError>>>;

struct Inner<V> {
    values: HashMap<String, Slot<V>>,
    /// (这次 load 的编号, 共享 future)。编号给"只删自己那条"用（TS 比的是 Promise 引用）
    inflight: HashMap<String, (u64, SharedLoad<V>)>,
    generation: u64,
    next_id: u64,
}

pub struct TtlCache<V> {
    ttl_ms: i64,
    inner: Arc<Mutex<Inner<V>>>,
}

impl<V> Clone for TtlCache<V> {
    /// 共享同一份存储（像 JS 里传对象引用）
    fn clone(&self) -> Self {
        TtlCache { ttl_ms: self.ttl_ms, inner: self.inner.clone() }
    }
}

impl<V: Clone + Send + Sync + 'static> TtlCache<V> {
    pub fn new(ttl_ms: i64) -> Self {
        TtlCache {
            ttl_ms,
            inner: Arc::new(Mutex::new(Inner { values: HashMap::new(), inflight: HashMap::new(), generation: 0, next_id: 0 })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<V>> {
        // 锁里没有会 panic 的代码；真中毒了也照常用里面的数据
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 取未过期的条目；过期的顺手删掉。`now` 缺省是此刻
    pub fn peek(&self, key: &str, now: Option<i64>) -> Option<TtlCacheEntry<V>> {
        let now = now.unwrap_or_else(now_ms);
        let mut inner = self.lock();
        let hit = inner.values.get(key)?;
        if now >= hit.exp {
            inner.values.remove(key);
            return None;
        }
        Some(TtlCacheEntry { at: hit.at, value: hit.value.clone() })
    }

    /// 写入。`ttl_ms` 缺省用构造时的，`now` 缺省是此刻
    pub fn set(&self, key: &str, value: V, ttl_ms: Option<i64>, now: Option<i64>) {
        let now = now.unwrap_or_else(now_ms);
        let ttl = ttl_ms.unwrap_or(self.ttl_ms);
        self.lock().values.insert(key.to_string(), Slot { at: now, exp: now + ttl, value });
    }

    pub fn delete(&self, key: &str) {
        self.lock().values.remove(key);
    }

    pub fn clear(&self) {
        let mut inner = self.lock();
        inner.generation += 1;
        inner.values.clear();
        inner.inflight.clear();
    }

    /// 命中就给缓存值；否则并入同 key 正在飞的那次；再否则同步调用 `load` 起一次新的。
    /// 成功的结果（且期间没被 `clear()`）按 TTL 写回，失败不入库。
    pub fn get_or_load<F, Fut>(
        &self,
        key: &str,
        load: F,
        opts: TtlCacheLoadOpts,
    ) -> impl Future<Output = Result<V, LoadError>> + Send + 'static
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<V>> + Send + 'static,
    {
        enum Plan<V> {
            Hit(V),
            Wait(SharedLoad<V>),
        }

        let plan = 'plan: {
            let now = opts.now.unwrap_or_else(now_ms);
            let ttl = opts.ttl_ms.unwrap_or(self.ttl_ms);
            if !opts.fresh
                && let Some(hit) = self.peek(key, Some(now)) {
                    break 'plan Plan::Hit(hit.value);
                }
            let (generation, id) = {
                let mut inner = self.lock();
                if let Some((_, pending)) = inner.inflight.get(key) {
                    break 'plan Plan::Wait(pending.clone());
                }
                inner.next_id += 1;
                (inner.generation, inner.next_id)
            };

            let fut = load();
            let cache = self.clone();
            let key_owned = key.to_string();
            let frozen_now = opts.now;
            let shared: SharedLoad<V> = async move {
                let result = fut.await;
                let mut inner = cache.lock();
                let outcome = match result {
                    Ok(value) => {
                        if inner.generation == generation {
                            let at = frozen_now.unwrap_or_else(now_ms);
                            inner.values.insert(key_owned.clone(), Slot { at, exp: at + ttl, value: value.clone() });
                        }
                        Ok(value)
                    }
                    Err(err) => Err(Arc::new(err)),
                };
                // finally：只删自己那条（clear 之后同 key 可能已经起了新的一次）
                if inner.inflight.get(&key_owned).is_some_and(|(i, _)| *i == id) {
                    inner.inflight.remove(&key_owned);
                }
                outcome
            }
            .boxed()
            .shared();
            self.lock().inflight.insert(key.to_string(), (id, shared.clone()));
            Plan::Wait(shared)
        };

        async move {
            match plan {
                Plan::Hit(v) => Ok(v),
                Plan::Wait(s) => s.await,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::oneshot;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn at(now: i64) -> TtlCacheLoadOpts {
        TtlCacheLoadOpts { now: Some(now), ..Default::default() }
    }

    #[test]
    fn hits_within_ttl_and_misses_after_expiry() {
        let c = TtlCache::<u32>::new(1000);
        let n = Arc::new(AtomicU32::new(0));
        let load = || {
            let n = n.clone();
            async move { Ok(n.fetch_add(1, Ordering::SeqCst) + 1) }
        };
        assert_eq!(block_on(c.get_or_load("k", load, at(0))).unwrap(), 1);
        assert_eq!(block_on(c.get_or_load("k", load, at(999))).unwrap(), 1);
        assert_eq!(block_on(c.get_or_load("k", load, at(1000))).unwrap(), 2);
        assert_eq!(c.peek("k", Some(1000)).map(|e| e.value), Some(2));
        assert_eq!(c.peek("k", Some(2000)), None);
    }

    #[test]
    fn concurrent_callers_share_one_in_flight_load() {
        let c = TtlCache::<u32>::new(1000);
        let n = AtomicU32::new(0);
        let (tx, rx) = oneshot::channel::<u32>();
        let mut rx = Some(rx);
        let mut load = || {
            n.fetch_add(1, Ordering::SeqCst);
            let rx = rx.take().expect("load 只该跑一次");
            async move { Ok(rx.await?) }
        };
        let a = c.get_or_load("k", &mut load, TtlCacheLoadOpts::default());
        let b = c.get_or_load("k", &mut load, TtlCacheLoadOpts::default());
        assert_eq!(n.load(Ordering::SeqCst), 1);
        tx.send(7).unwrap();
        let (a, b) = block_on(futures::future::join(a, b));
        assert_eq!((a.unwrap(), b.unwrap()), (7, 7));
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn fresh_skips_the_entry_but_still_joins_the_in_flight_load() {
        let c = TtlCache::<u32>::new(1000);
        let n = Arc::new(AtomicU32::new(0));
        let bump = |n: &Arc<AtomicU32>| {
            let n = n.clone();
            move || async move { Ok(n.fetch_add(1, Ordering::SeqCst) + 1) }
        };
        assert_eq!(block_on(c.get_or_load("k", bump(&n), at(0))).unwrap(), 1);
        let fresh_at_10 = TtlCacheLoadOpts { fresh: true, now: Some(10), ..Default::default() };
        assert_eq!(block_on(c.get_or_load("k", bump(&n), fresh_at_10)).unwrap(), 2);

        let (tx, rx) = oneshot::channel::<u32>();
        let n2 = n.clone();
        let fresh = TtlCacheLoadOpts { fresh: true, ..Default::default() };
        let inflight = c.get_or_load(
            "k",
            move || {
                n2.fetch_add(1, Ordering::SeqCst);
                async move { Ok(rx.await?) }
            },
            fresh,
        );
        let joined = c.get_or_load("k", bump(&n), fresh);
        tx.send(9).unwrap();
        let (a, b) = block_on(futures::future::join(inflight, joined));
        assert_eq!((a.unwrap(), b.unwrap()), (9, 9));
        assert_eq!(n.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_failed_load_is_not_stored_and_is_retried_next_time() {
        let c = TtlCache::<u32>::new(1000);
        let n = Arc::new(AtomicU32::new(0));
        let n1 = n.clone();
        let failed = block_on(c.get_or_load(
            "k",
            move || async move {
                n1.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("boom"))
            },
            TtlCacheLoadOpts::default(),
        ));
        assert_eq!(failed.unwrap_err().to_string(), "boom");
        assert_eq!(c.peek("k", None), None);
        let n2 = n.clone();
        let ok = block_on(c.get_or_load(
            "k",
            move || async move { Ok(n2.fetch_add(1, Ordering::SeqCst) + 1) },
            TtlCacheLoadOpts::default(),
        ));
        assert_eq!(ok.unwrap(), 2);
    }

    #[test]
    fn results_in_flight_across_a_clear_are_not_written_back() {
        let c = TtlCache::<u32>::new(1000);
        let (tx, rx) = oneshot::channel::<u32>();
        let pending = c.get_or_load("k", move || async move { Ok(rx.await?) }, TtlCacheLoadOpts::default());
        c.clear();
        tx.send(1).unwrap();
        assert_eq!(block_on(pending).unwrap(), 1);
        assert_eq!(c.peek("k", None), None);
        assert_eq!(block_on(c.get_or_load("k", || async { Ok(2) }, TtlCacheLoadOpts::default())).unwrap(), 2);
    }

    #[test]
    fn a_single_entry_can_override_the_ttl() {
        let c = TtlCache::<&'static str>::new(1000);
        let opts = TtlCacheLoadOpts { ttl_ms: Some(5000), now: Some(0), ..Default::default() };
        block_on(c.get_or_load("k", || async { Ok("v") }, opts)).unwrap();
        assert_eq!(c.peek("k", Some(4999)).map(|e| e.value), Some("v"));
        assert_eq!(c.peek("k", Some(5000)), None);
    }
}
