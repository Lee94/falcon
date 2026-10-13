//! 会话守护进程 falcon-sessiond 在服务端这一侧（ADR 0020，设计文档 `docs/design/session-daemon.md`
//! 第 10 节）。分层照 `git/` 的样子：
//! - `command.rs`：纯函数，产 argv、文件名、命令行，零 I/O；
//! - `bin.rs`：各平台的二进制从哪来；
//! - `deploy.rs`：推到宿主机、握手；
//! - `backend.rs`：守护进程会话的 Backend（帧协议、快照、程序状态）。
//!
//! 守护进程本身（holder / VT 进程）在 `native/sessiond/`，是独立的工作区。
pub mod backend;
pub mod bin;
pub mod command;
pub mod deploy;
