//! 独立 Claude 对话通道，一条连接拥有一个 Claude Code 子进程。
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClaudeInput {
    Prompt { text: String },
    Permission { id: String, allow: bool },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClaudeEvent {
    Ready,
    Delta {
        text: String,
    },
    // 完整消息替换当前草稿，兼容没有增量的提供方。
    Assistant {
        text: String,
        tools: Vec<String>,
    },
    Permission {
        id: String,
        tool: String,
        input: Value,
    },
    CancelPermission {
        id: String,
    },
    Done {
        error: Option<String>,
    },
    Closed {
        reason: String,
    },
}
