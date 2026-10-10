//! 一个已附着的底层终端（本地 PTY 或 SSH channel）。移植自 `packages/server/src/sessions/backend.ts`。
//!
//! 会话核心跑在单线程的 LocalSet 上，backend 与回调都是 `!Send` 的 `Rc`。

use std::rc::Rc;

pub trait Backend {
    fn write(&self, data: &str);
    fn resize(&self, cols: u16, rows: u16);
    /// 仅断开本端附着，不终止底层（用于持久会话链路重建前的清理）
    fn destroy(&self);
    /// tty 前台进程名。只有本地 POSIX PTY 提供；持久会话不走这里（外层 PTY 的前台永远是
    /// Zellij 客户端），问 Zellij
    fn process_name(&self) -> Option<String> {
        None
    }
}

/// backend 推给会话核心的两件事。在 LocalSet 上顺序调用
#[derive(Clone)]
pub struct BackendCallbacks {
    pub on_data: Rc<dyn Fn(String)>,
    /// 底层附着结束（shell 退出 / 链路关闭）。持久会话由上层判断是链路问题还是真退出
    pub on_exit: Rc<dyn Fn()>,
}

pub struct AttachResult {
    pub backend: Rc<dyn Backend>,
    pub durable: bool,
    /// 接回持久会话时用 dump-screen 抓回的历史，用于重建 Scrollback
    pub captured_history: Option<String>,
}

/// dump-screen 输出为 \n 结尾的行，回放进终端需要 \r\n
pub fn normalize_captured(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 32);
    let mut prev_cr = false;
    for c in text.chars() {
        if c == '\n' && !prev_cr {
            out.push('\r');
        }
        out.push(c);
        prev_cr = c == '\r';
    }
    out
}

/// 接回持久会话时，宿主机上的 Zellij session 已经不在了
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("Zellij 会话不存在")]
pub struct SessionGoneError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_line_endings() {
        assert_eq!(normalize_captured("a\nb\r\nc\n"), "a\r\nb\r\nc\r\n");
    }
}
