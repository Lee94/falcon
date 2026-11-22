//! Agent 浏览器（ADR 0029）：环境变量注入、被动发现、上游 stream 连接、按 ack 扇出、
//! action policy 接管锁、输入换算与操作记录。

pub mod discovery;
pub mod env;
pub mod log;
pub mod manager;
pub mod policy;
pub mod upstream;

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

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

/// agent-browser 允许 Unicode 字母 / 数字、`-` 与 `_`。限制长度既给扩展名留出文件名字节，
/// 也避免把异常文件名变成很大的查询参数与持久化键。
pub fn is_valid_browser_session_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 180
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
}

/// 一个远端浏览器实例。`session_id` 决定所属终端与节点路由，`browser_session` 决定
/// 这台宿主机上的具体 agent-browser 守护进程；同一终端可以有任意多个实例。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserKey {
    pub session_id: String,
    pub browser_session: String,
}

impl BrowserKey {
    pub fn new(session_id: &str, browser_session: &str) -> Option<Self> {
        (is_valid_session_id(session_id) && is_valid_browser_session_name(browser_session)).then(|| Self {
            session_id: session_id.to_string(),
            browser_session: browser_session.to_string(),
        })
    }

    pub fn for_request(session_id: &str, browser_session: Option<&str>) -> Option<Self> {
        match browser_session {
            Some(name) => Self::new(session_id, name),
            None => Self::new(session_id, &browser_session_name(session_id)),
        }
    }

    pub fn is_default(&self) -> bool {
        self.browser_session == browser_session_name(&self.session_id)
    }

    /// 日志与缩略图使用的安全文件名。默认实例沿用旧路径；自定义实例加内容哈希，
    /// 既支持 Unicode 名称，也不会让不同实例共用事件序号与缩略图。
    pub fn storage_id(&self) -> String {
        if self.is_default() {
            return self.session_id.clone();
        }
        let digest = Sha256::digest(self.browser_session.as_bytes());
        format!("{}.{}", self.session_id, hex::encode(digest))
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
