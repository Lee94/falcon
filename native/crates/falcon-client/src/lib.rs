//! falcon-client：原生客户端的网络层（设计文档 `docs/design/gpui-client.md` 决定四、§3.2、§4.7）。
//!
//! 对 falcon 服务端来说，原生客户端只是又一个会用 cookie 的 Viewer：同一套 REST
//! （`/api/*`）、同一条 `/ws/sessions/:id`、同一个登录 cookie（`falcon_token`）。
//! 这个 crate 不认识任何 GPUI 类型，可以脱离窗口单测，也能对真服务端跑 e2e
//! （`tests/e2e.rs`）。
//!
//! # 异步模型：与执行器无关
//!
//! app 跑在 GPUI 自己的执行器上，不是 tokio。reqwest / tungstenite 却离不开 tokio 的
//! reactor。所以 crate 内部懒创建一个全局 tokio 多线程运行时（[`runtime()`]，线程名
//! `falcon-net`），**所有公开的 async 方法返回的 future 都只是"把真正的工作 spawn
//! 到那个运行时上、再等它的 JoinHandle"**：
//!
//! - 在 GPUI 的 `cx.spawn` 里直接 `.await` 就能用，不需要 tokio 上下文；
//! - future 是 `Send + 'static` 的，参数在调用时就拷成了自有值，可以随便挪；
//! - future 是**惰性**的：不 poll 就不发请求；poll 之后中途丢掉 = 取消（spawn 出去的
//!   任务会被 abort，下载的临时文件会被删掉）。
//!
//! 会话 WebSocket 反过来：[`SessionSocket`] 在网络线程上**顺序**回调 [`SessionSink`]，
//! app 在回调里直接把终端字节喂给 VT 解析器——这是设计上的零跳转路径（§3.1）。
//!
//! 浏览器版（wasm32）没有这个运行时：future 就地 await，会话 socket 的驱动循环挂在
//! `spawn_local` 上，回调在主线程上。返回类型里的 [`MaybeSend`] 在 wasm 上不要求 `Send`。
//!
//! # 认证
//!
//! 服务端的登录 token 只存在内存里（`auth.ts`），**后端一重启就全部失效**——远处的
//! 服务端升级、重启都会碰到。所以客户端可以记住访问密码
//! （[`FalconClient::set_relogin_password`]，app 从钥匙串里取出来设进来）：REST 收到
//! 401、WS 收到关闭码 4401 时自动重新登录一次再重放 / 重连，失败才通知 app 弹登录框
//! （[`AuthEvent::LoginRequired`]）。这样不用改服务端去加一种长效令牌（§4.7）。
//!
//! # 模块
//!
//! - [`FalconClient`]：REST 客户端本体，方法按功能域分在 `api/` 下的各文件里，
//!   与 web 的 `packages/web/src/api.ts` 逐条对应（名字是它的 snake_case）。
//! - [`SessionSocket`] / [`InstallSocket`]：`/ws/sessions/:id` 与 `/ws/install/:projectId`。
//! - [`ApiError`]：对齐 web 的 `ApiRequestError`。

mod api;
mod client;
mod error;
mod external;
mod runtime;
#[cfg(not(target_family = "wasm"))]
mod tls;
mod ws;

pub use api::files::{RawBytes, raw_url};
pub use api::git::{GitFileRef, GitLogQuery, GitSyncAction};
pub use api::projects::{DeriveInput, HostAuthorizationPatch};
pub use api::system::ProbeTarget;
pub use client::{AuthEvent, AuthEvents, AuthState, FalconClient};
pub use error::{ApiError, ApiErrorKind, ApiResult};
pub use external::{EXTERNAL_MAX_BYTES, ExternalResponse, fetch_external};
#[cfg(not(target_family = "wasm"))]
pub use runtime::runtime;
pub use runtime::{MaybeSend, block_in_place};
pub use ws::install::{InstallEvent, InstallSocket};
pub use ws::session::{SessionEvent, SessionSink, SessionSocket, SocketOptions};

/// 传输进度回调：`(已完成字节数, 总字节数)`。总数未知时为 `None`。
///
/// 在网络线程上调用（浏览器里是主线程），按时间节流（约 100ms 一次，开头与结尾各保证一次）；
/// 回调里别做重活，要更新界面就把数字丢给 UI 线程。
pub type ProgressFn = dyn Fn(u64, Option<u64>) + Send + Sync;
