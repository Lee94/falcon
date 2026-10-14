//! libghostty-vt 的原始绑定。`bindings.rs` 由 `cargo xtask ghostty-bindgen` 从钉死 commit 的
//! 头文件生成后**入库**：日常构建不需要 libclang，升级 ghostty 时这个文件的 diff 就是那次的
//! ABI 变化。布局断言（`const _: () = …`）在每个目标上编译期就会核对，交叉编译时也一样。
//!
//! 只给 `ghostty-vt` 这一个 crate 用；别处要终端能力去那里加安全封装。
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case, dead_code, clippy::all)]
#![allow(unnecessary_transmutes)]

include!("bindings.rs");

/// 静态库对应的 ghostty（来自 ghostty.pin）：commit，打了 falcon 的补丁时是
/// `<commit>+falcon.<补丁哈希前 12 位>`。VT 进程把它报给服务端，不同就热升级
pub const GHOSTTY_COMMIT: &str = env!("GHOSTTY_COMMIT");
