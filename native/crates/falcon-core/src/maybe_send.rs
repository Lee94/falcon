//! 原生与浏览器两种执行模型的那一点差别：原生的 future 会被丢到线程池上，必须 `Send`；
//! 浏览器（wasm32）只有一条线程，fetch / WebSocket 的 future 里攥着 JsValue，天生不是 `Send`。
//!
//! 下层 crate 的泛型约束写 [`MaybeSend`] 而不是 `Send`，装箱写 [`MaybeBoxFuture`]：
//! 原生上它们就是 `Send` / `BoxFuture`，一个字节不变；wasm 上放宽成不要求 `Send`。
//! falcon-client 有一份同样的定义（它不依赖本 crate），两者都是全类型的 blanket impl，可以互相满足。

use std::future::Future;

#[cfg(not(target_family = "wasm"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_family = "wasm"))]
impl<T: Send + ?Sized> MaybeSend for T {}

#[cfg(target_family = "wasm")]
pub trait MaybeSend {}
#[cfg(target_family = "wasm")]
impl<T: ?Sized> MaybeSend for T {}

#[cfg(not(target_family = "wasm"))]
pub type MaybeBoxFuture<'a, T> = futures::future::BoxFuture<'a, T>;
#[cfg(target_family = "wasm")]
pub type MaybeBoxFuture<'a, T> = futures::future::LocalBoxFuture<'a, T>;

/// 装箱成 [`MaybeBoxFuture`]
pub fn boxed<'a, T>(fut: impl Future<Output = T> + MaybeSend + 'a) -> MaybeBoxFuture<'a, T> {
    Box::pin(fut)
}
