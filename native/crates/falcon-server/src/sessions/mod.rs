//! 会话：纯函数（S1）、backend 接口与 SSH 链路（S3）、中转运行时（S6：端口转发 + 公网发布）。
pub mod agent;
pub mod backend;
pub mod forward;
pub mod forward_spec;
pub mod local;
pub mod login_env;
pub mod relay;
pub mod relay_spec;
pub mod scroll_plugin;
pub mod share;
pub mod share_spec;
pub mod ssh;
pub mod ssh_zellij;
pub mod term_size;
pub mod viewer_arbiter;
