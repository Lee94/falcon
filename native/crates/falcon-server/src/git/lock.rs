//! 按仓库串行化 worktree 操作。移植自 `packages/server/src/git/lock.ts`。
//!
//! 并发 `worktree add` 会争 .git 下的锁，报出来的
//! `fatal: Unable to create ... File exists` 对用户毫无意义；两个请求算出同一个目标
//! 目录时还会撞 TOCTOU。服务端是单进程，一个进程内的锁就够了——TS 版用 Promise 链
//! （形状照抄 SessionManager 里 `entry.attaching: Promise | null` 的做法），这里用
//! tokio 的公平互斥锁：排队顺序就是到达顺序，与 Promise 链一致。
//!
//! 键必须是「宿主机 + 仓库根」，**不是 projectId**：两个 Project 完全可能指向同一个
//! 仓库，按 projectId 加锁等于没加。
//!
//! 锁表是进程级的 `static`（不是 thread_local）：会话核心跑在单线程的 LocalSet 上，
//! 但锁本身不依赖这一点——哪天有调用点跑到别的线程上，同一仓库照样互斥。

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

use super::host::GitHost;
use super::path::canon_key;
use crate::zellij::host::HostKind;

/// 仓库锁的唯一键构造。此前三处调用点手拼：派生用 `::`、commit 与 pull/push 用
/// `:`——两个键空间不相交，同一仓库上的派生与 pull/push 根本不互斥（潜伏 bug，ADR 0003
/// 「顺手修掉的锁键分歧」）。统一从这里出；分隔符取 `::`，因为 ssh 的 host.key 本身就含 `:`。
pub fn repo_lock_key(host: &GitHost, repo_root: &str) -> String {
    lock_key(&host.key, host.kind, repo_root)
}

/// [`repo_lock_key`] 的拆开版（TS 吃的是 `Pick<GitHost, "key" | "kind">`），测试与没有
/// 完整 GitHost 的调用点用
pub fn lock_key(host_key: &str, kind: HostKind, repo_root: &str) -> String {
    format!("{host_key}::{}", canon_key(kind, repo_root))
}

type Slot = Arc<tokio::sync::Mutex<()>>;

static CHAINS: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(Mutex::default);

/// 持有一个键的锁槽。drop 时若自己是最后一个用它的，就把它从表里摘掉——
/// 对应 TS 的"自己是队尾才清理，否则会把后来者的链头擦掉"。
///
/// 放在 Drop 里而不是 `with_repo_lock` 的末尾：Rust 的 future 可以被中途丢弃
/// （客户端断开、外层 select），那时末尾的清理代码不会跑，表里就漏一条。
struct Holder {
    key: String,
    slot: Slot,
}

impl Holder {
    fn acquire(key: String) -> Self {
        let mut map = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
        let slot = map.entry(key.clone()).or_default().clone();
        Holder { key, slot }
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let mut map = CHAINS.lock().unwrap_or_else(|e| e.into_inner());
        // 2 = 表里一份 + 自己一份：没有别人在排队或持有。新来者要先拿表锁才能 clone，
        // 所以这里看到的计数在表锁之下是准的
        if Arc::strong_count(&self.slot) == 2 && map.get(&self.key).is_some_and(|s| Arc::ptr_eq(s, &self.slot)) {
            map.remove(&self.key);
        }
    }
}

/// 在键 `key` 的锁内跑 `fut`。同键串行、先到先跑；前一个失败（返回 Err）也照样让
/// 后一个跑起来——锁只管顺序，不看结果（TS 里 `prev.then(fn, fn)` 两个分支都接 fn）。
///
/// `fut` 在拿到锁之前不会被 poll：Rust 的 future 是惰性的，等价于 TS 传 thunk。
pub async fn with_repo_lock<F: Future>(key: String, fut: F) -> F::Output {
    let holder = Holder::acquire(key);
    let _guard = holder.slot.lock().await;
    fut.await
    // _guard 先于 holder 释放（声明顺序的逆序），holder 的 Drop 看到的计数已不含锁本身
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn key_is_host_plus_canonical_root() {
        assert_eq!(lock_key("ssh:u@h:22", HostKind::Posix, "/a/repo/"), "ssh:u@h:22::/a/repo");
        // Windows：分隔符与大小写归一，同一仓库的不同写法落到同一把锁
        assert_eq!(
            lock_key("local", HostKind::Windows, "D:/Code/Repo"),
            lock_key("local", HostKind::Windows, "d:\\code\\repo\\")
        );
    }

    #[tokio::test]
    async fn same_key_runs_in_arrival_order_even_after_a_failure() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let step = |name: &'static str, ms: u64, fail: bool| {
            let log = log.clone();
            async move {
                log.borrow_mut().push(format!("{name}+"));
                tokio::time::sleep(Duration::from_millis(ms)).await;
                log.borrow_mut().push(format!("{name}-"));
                if fail { Err(name) } else { Ok(name) }
            }
        };
        let key = "test::same-key-order".to_string();
        let (a, b, c) = tokio::join!(
            with_repo_lock(key.clone(), step("a", 30, true)),
            with_repo_lock(key.clone(), step("b", 10, false)),
            with_repo_lock(key.clone(), step("c", 1, false)),
        );
        assert_eq!((a, b, c), (Err("a"), Ok("b"), Ok("c")));
        // 不交错：每个都先进后出完才轮到下一个
        assert_eq!(*log.borrow(), ["a+", "a-", "b+", "b-", "c+", "c-"]);
    }

    #[tokio::test]
    async fn different_keys_do_not_block_each_other() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let l1 = log.clone();
        let l2 = log.clone();
        tokio::join!(
            with_repo_lock("test::k1".into(), async move {
                l1.borrow_mut().push("k1+");
                tokio::time::sleep(Duration::from_millis(30)).await;
                l1.borrow_mut().push("k1-");
            }),
            with_repo_lock("test::k2".into(), async move {
                l2.borrow_mut().push("k2+");
                l2.borrow_mut().push("k2-");
            }),
        );
        assert_eq!(*log.borrow(), ["k1+", "k2+", "k2-", "k1-"]);
    }

    #[tokio::test]
    async fn table_entry_is_dropped_when_the_queue_drains_or_a_waiter_is_cancelled() {
        let key = "test::cleanup".to_string();
        with_repo_lock(key.clone(), async {}).await;
        assert!(!CHAINS.lock().unwrap().contains_key(&key));

        // 排队中被丢弃（外层超时）的那个也不能把表项漏下
        let holder = with_repo_lock(key.clone(), tokio::time::sleep(Duration::from_millis(50)));
        let waiter = tokio::time::timeout(Duration::from_millis(5), with_repo_lock(key.clone(), async {}));
        let (_, w) = tokio::join!(holder, waiter);
        assert!(w.is_err());
        assert!(!CHAINS.lock().unwrap().contains_key(&key));
    }
}
