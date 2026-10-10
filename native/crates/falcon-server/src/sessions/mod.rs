//! 会话：纯函数（S1）、backend 接口、本地 PTY 与 SSH 链路（S3）、SessionManager 与 Viewer（S4）。转发与发布在 S6。
pub mod agent;
pub mod backend;
pub mod forward_spec;
pub mod local;
pub mod login_env;
pub mod manager;
pub mod relay_spec;
pub mod scroll_plugin;
pub mod share_spec;
pub mod ssh;
pub mod ssh_zellij;
pub mod term_size;
pub mod viewer;
pub mod viewer_arbiter;
