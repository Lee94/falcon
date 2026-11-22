//! Agent 浏览器（ADR 0029）：环境变量注入、被动发现、上游 stream 连接、按 ack 扇出、
//! action policy 接管锁、输入换算与操作记录。

pub mod discovery;
pub mod env;
pub mod log;
pub mod manager;
pub mod policy;
pub mod upstream;

use std::path::{Path, PathBuf};

/// `<dataDir>/agent-browser` 根目录。
pub fn root_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("agent-browser")
}

/// agent-browser 守护进程的 socket / stream / pid / version 目录。
pub fn run_dir(data_dir: &Path) -> PathBuf {
    root_dir(data_dir).join("run")
}

/// 每个会话的 action policy 目录。
pub fn policy_dir(data_dir: &Path) -> PathBuf {
    root_dir(data_dir).join("policy")
}

/// 每个会话的操作记录 jsonl 目录。
pub fn log_dir(data_dir: &Path) -> PathBuf {
    root_dir(data_dir).join("log")
}

/// 每个会话的缩略图目录。
pub fn thumbs_dir(data_dir: &Path) -> PathBuf {
    root_dir(data_dir).join("thumbs")
}

/// 终端会话对应的 agent-browser 会话名：`falcon-<sessionId>`。
pub fn browser_session_name(session_id: &str) -> String {
    format!("falcon-{session_id}")
}

/// 检查 session_id 是否只含字母、数字和 `-`（符合 agent-browser 会话名限制，且防路径穿越）。
pub fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty() && session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
