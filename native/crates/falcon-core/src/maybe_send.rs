//! 下层 crate 的泛型约束别名（`Send` / `BoxFuture`）。

use std::future::Future;

pub trait MaybeSend: Send {}
impl<T: Send + ?Sized> MaybeSend for T {}

pub type MaybeBoxFuture<'a, T> = futures::future::BoxFuture<'a, T>;

/// 装箱成 [`MaybeBoxFuture`]
pub fn boxed<'a, T>(fut: impl Future<Output = T> + MaybeSend + 'a) -> MaybeBoxFuture<'a, T> {
    Box::pin(fut)
}
