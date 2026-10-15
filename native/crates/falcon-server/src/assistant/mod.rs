//! AI 助手（docs/design/ai-assistant.md）：对话与 agent，服务端这一侧。
//!
//! - [`hub`]：对话、观看者、持久化、审批，内置 agent 的循环；跑在会话引擎线程上；
//! - [`claude`]：Claude Messages API 的流式客户端（纯函数为主）；
//! - [`tools`]：内置 agent 的四个工具与系统提示词；
//! - [`settings`]：模型、API key（加密落库、不下发）、审批策略。
//!
//! 对话终端的那一半（按机器挂的会话、shell integration、命令块、往 shell 里跑命令）在 `sessions/`：
//! machine.rs、shell_integration.rs、shell_marks.rs、manager/machine_run.rs。

pub mod acp;
pub mod claude;
pub mod hub;
pub mod settings;
pub mod tools;
