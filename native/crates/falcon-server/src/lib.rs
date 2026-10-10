//! falcon 服务端的 Rust 实现。方案与分期见 `docs/design/rust-unification.md`（S 线）。
//!
//! 模块与 `packages/server/src` 一一对应（文件名换成 snake_case），移植时照 TS 原文，
//! 包括顶部和关键处解释"为什么"的注释——那些多半是实测出来的坑，附录 A 有清单。
//! 协议类型一律用 falcon-proto（与原生客户端同一份），不在这里另起一套。

pub mod api;
pub mod app_icon;
pub mod askpass;
pub mod auth;
pub mod cloudflared;
pub mod config;
pub mod crypto;
pub mod db;
pub mod engine;
pub mod exec;
pub mod files;
pub mod fs;
pub mod git;
pub mod meegle;
pub mod paste;
pub mod px0;
pub mod ringbuffer;
pub mod service;
pub mod service_cli;
pub mod sessions;
pub mod shells;
pub mod term_env;
pub mod transfer;
pub mod virtualdir;
pub mod zellij;
