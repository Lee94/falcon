//! Claude 对话通道。一条连接拥有一个 Claude Code 子进程，线程可接回历史。
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
    // 线程先回放历史，再发送 Ready。之后由宿主机回显已收录的提示词。
    ThreadHistory,
    User {
        text: String,
    },
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
