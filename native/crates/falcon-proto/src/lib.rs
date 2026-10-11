//! falcon-proto：falcon 的线上协议类型，服务端（falcon-server）与客户端（falcon-client /
//! falcon-ui）共用这一份。原是 `packages/shared/src/index.ts` 的手写镜像（设计文档
//! `docs/design/gpui-client.md` 决定五）；TS 一侧删除后（原文在提交 `9c9d045`）这里就是
//! **协议的唯一真相来源**。`tests/fixtures/` 是 `cargo xtask fixtures` 从真服务端落盘的
//! 响应：改了线上形状就重生 fixture，契约测试保证服务端写出的 JSON 这边读得回来。
//! 下面的形状约定是从 TS 时代延续下来的线上格式，已经有的客户端与数据都按它来，别改。
//!
//! # 线上形状的约定
//!
//! - 字段一律 `rename_all = "camelCase"`，与服务端 `JSON.stringify` 的输出逐字段一致。
//! - TS 的可选字段 `x?: T` → `Option<T>` + `default` + `skip_serializing_if`：缺省就是
//!   缺省，写回时不会凭空多出一个 `null`（对服务端来说 `null` 与缺省并不总是一回事）。
//! - TS 的必填可空字段 `x: T | null` → `Option<T>`，**不跳过**，`None` 写成 `null`。
//! - TS 的 `x?: T | null`（只有 `GitSnapshot.ahead / behind`）→ `Option<Option<T>>`，
//!   缺省 / null / 有值三态原样来回。
//! - 字符串字面量联合 → 枚举，每个变体显式写出字面量（见 `wire::wire_enum!`）。
//! - 可辨识联合 → `#[serde(tag = "…")]` 的枚举；唯一的例外是按布尔 `ok` 判别的
//!   [`SshProbeResult`]，经平铺形状转一道。
//! - `extends` → `#[serde(flatten)]` + `Deref`（[`SessionWithProject`]、
//!   [`GitWorkingFile`]、[`MeegleTodoItem`]、[`MeegleWorkItemDetail`]）。
//! - TS 里内联的匿名对象类型在 Rust 侧起了名字（如 [`MultiRepoProbeMember`]、
//!   [`FileRemoveError`]、[`MeegleAttachment`]），注释里写明出处。
//!
//! # 数字
//!
//! JS 只有一种数字，Rust 侧按语义挑：毫秒时间戳 `i64`；端口 `u16`；终端行列 `u16`；
//! 字节数 `u64`；条目数 / 轮次 / 页码 `u32`；git 的增删行数 `u64`；文件 mtime 是
//! Unix **秒**（`i64`）；飞书 CLI 原样透传的 `expiresInMinutes` 不保证是整数，用 `f64`。
//!
//! # `Unknown` 兜底
//!
//! 服务端将来可能扩展的状态 / 原因类枚举多一个 `Unknown`（`#[serde(other)]`），
//! 新服务端多一个值时，老客户端不至于让整张列表反序列化失败：
//! [`SessionState`]、[`DeadReason`]、[`SessionAgent`]、[`NonDurableReason`]、
//! [`ZellijInstallFailure`]、[`ZellijInstallStage`]、[`WorktreeFailure`]、
//! [`GitUnavailableReason`]、[`GitConflictKind`]、[`ForwardState`]、
//! [`MeegleUnavailableReason`]、[`MeeglePinKind`]，以及可辨识联合
//! [`FilePreview`]、[`ServerMessage`]、[`InstallServerMessage`]。各自的理由写在类型注释里。
//!
//! 刻意**不加**的：客户端写出去的值（请求体、[`ClientMessage`]、[`GitOpInput`]、
//! [`MeegleTodoAction`]）；TS 注释明说不会再加值的结构性判别（[`ProjectType`]
//! 等）；单个响应里认不出就该报错的判别（[`MeegleUrlTarget`]）。
//!
//! # 没有镜像的
//!
//! index.ts 从 termEnv / termModes / ttlCache 转出的运行时逻辑（`OscColorGate`、
//! `termPtyEnv` 等 PTY 环境函数、`TermModeTracker`、`TtlCache` 及其
//! `TtlCacheEntry` / `TtlCacheLoadOpts`）不是线上形状，不当协议类型镜像：前者只在服务端跑
//! （`falcon-server` 的 `term_env.rs`），`TtlCache` 在 falcon-core 的 `ttl_cache.rs`。
//! 例外是 `TermModeTracker`：服务端回放前缀与客户端鼠标上报都要它，就放在本 crate 的
//! [`term_modes`] 里两边共用。
//!
//! # shared 里没有、但线上确实存在的
//!
//! 照服务端源码补了几个：[`OkResponse`]、[`AskpassPrompt`]、[`FsValidateResult`]、
//! [`PortForwardPatch`] / [`PublicSharePatch`]（TS 的 `Partial<…Input>`）、
//! [`WS_CLOSE_UNAUTHORIZED`]、[`WS_MAX_PAYLOAD_BYTES`]。各自注释里写了出处。

mod wire;

pub mod files;
pub mod forward;
pub mod git;
pub mod meegle;
pub mod project;
pub mod session;
pub mod system;
pub mod term_env;
/// VT 模式跟踪：服务端拼回放前缀、客户端合成鼠标报文共用一个跟踪器（shared/termModes.ts）
pub mod term_modes;
pub mod worktree;
pub mod ws;
pub mod zellij;

pub use files::*;
pub use forward::*;
pub use git::*;
pub use meegle::*;
pub use project::*;
pub use session::*;
pub use system::*;
pub use term_env::*;
pub use worktree::*;
pub use ws::*;
pub use zellij::*;

#[cfg(test)]
mod test_util;
