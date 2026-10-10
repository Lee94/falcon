//! 会话：纯函数部分（S1）。SessionManager、PTY / SSH backend、转发与发布在 S3 / S4 / S6。
pub mod agent;
pub mod forward_spec;
pub mod login_env;
pub mod relay_spec;
pub mod scroll_plugin;
pub mod share_spec;
pub mod term_size;
pub mod viewer_arbiter;
