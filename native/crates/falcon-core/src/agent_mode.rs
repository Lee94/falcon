//! Agent 会话模式转换状态机与交互门控（ADR 0027）。
//!
//! 统一原桌面端与移动端 `AgentModeTransition` 的双向协议转换规则：
//! - **真实运行协议切换**（`TUI ↔ GUI`，`POST /api/sessions/:id/agent-mode` 的 `"gui-control"` / `"terminal-control"`）：
//!   HTTP 200 仅代表服务端接受或完成了切换，客户端必须等到当前活跃 WebSocket 收到与目标模式匹配且状态就绪的
//!   `AgentSnapshot`（或重连后的权威快照）才解除输入锁定，严禁在过渡窗口向旧 PTY 或未就绪 GUI 漏发字节。
//! - **本地排版切换**（`AgentDisplayStyle::Rich` / `Compact`）：只在可交互 GUI 模式内部改变视觉密度，不触碰底层 CLI 进程。

use falcon_proto::{AgentTurnStatus, AgentViewState, SessionAgent, SessionState};
use serde::{Deserialize, Serialize};

/// 可交互 GUI 模式内部的本地排版样式。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentDisplayStyle {
    /// 富图形气泡与卡片排版（进入可交互 GUI 时的默认样式）。
    #[default]
    Rich,
    /// 紧凑终端风格排版（等宽字体、`›` 提示符与 1px 细边框，仍走结构化双向流）。
    Compact,
}

impl AgentDisplayStyle {
    #[must_use]
    pub const fn toggle(self) -> Self {
        match self {
            Self::Rich => Self::Compact,
            Self::Compact => Self::Rich,
        }
    }
}

pub use falcon_proto::AgentModeCapabilities;

/// `TUI ↔ GUI` 双向模式切换与 WebSocket 快照同步的纯值状态机。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentModeTransition {
    pub synchronized: bool,
    pub request_in_flight: bool,
    pub target_graphical: Option<bool>,
    pub target_observed: bool,
    pub accept_current_mode: bool,
}

impl AgentModeTransition {
    pub const fn new() -> Self {
        Self {
            synchronized: false,
            request_in_flight: false,
            target_graphical: None,
            target_observed: false,
            accept_current_mode: false,
        }
    }

    /// HTTP 请求在途，或仍在等待 WebSocket 权威快照确认目标模式。
    pub const fn switching(&self) -> bool {
        self.request_in_flight || self.target_graphical.is_some()
    }

    /// HTTP 已返回成功，正在等待当前连接上的快照落地。
    pub const fn awaiting_snapshot(&self) -> bool {
        !self.request_in_flight && self.target_graphical.is_some()
    }

    /// WebSocket 建立连接。`requires_snapshot` 为真时须等首帧 `AgentSnapshot` 到达才开放会话输入。
    #[must_use]
    pub const fn connected(self, requires_snapshot: bool) -> Self {
        Self {
            synchronized: !requires_snapshot,
            accept_current_mode: self.awaiting_snapshot(),
            ..self
        }
    }

    /// WebSocket 断开。调用方若处于可交互 GUI 模式，应同时把本地 `AgentViewState.status` 降级为 `Unknown`。
    #[must_use]
    pub const fn disconnected(self) -> Self {
        Self {
            synchronized: false,
            accept_current_mode: false,
            ..self
        }
    }

    /// 发起 `POST /api/sessions/:id/agent-mode`（`graphical = true` -> `"gui-control"`，`false` -> `"terminal-control"`）。
    #[must_use]
    pub const fn start(self, graphical: bool) -> Self {
        Self {
            request_in_flight: true,
            target_graphical: Some(graphical),
            target_observed: false,
            accept_current_mode: false,
            ..self
        }
    }

    /// 收到 `AgentSnapshot`（或已同步连接上的 `AgentMeta`）。
    #[must_use]
    pub fn snapshot(self, graphical: bool, ready: bool) -> Self {
        let observed = self.target_observed
            || (ready
                && self.target_graphical.is_some()
                && (Some(graphical) == self.target_graphical || self.accept_current_mode));
        Self {
            synchronized: true,
            target_observed: observed,
            target_graphical: if observed && !self.request_in_flight {
                None
            } else {
                self.target_graphical
            },
            ..self
        }
    }

    /// HTTP 切换请求结束。
    ///
    /// 若 `succeeded == false`，网络响应可能在服务端实际完成切换后丢失，因此强制将
    /// `synchronized` 置回 `false`，必须经重连拿到新鲜快照后才重新放开输入。
    #[must_use]
    pub const fn finish(self, succeeded: bool) -> Self {
        Self {
            request_in_flight: false,
            synchronized: self.synchronized && succeeded,
            target_graphical: if !succeeded || self.target_observed {
                None
            } else {
                self.target_graphical
            },
            ..self
        }
    }
}

/// 当前快照是否处于由客户端 GUI 接管输入的可交互模式（`active && !terminal_backed`）。
pub fn is_interactive_gui(state: Option<&AgentViewState>) -> bool {
    state.is_some_and(|s| s.active && !s.terminal_backed)
}

/// 快照是否已具备明确的运行轮次状态（非可交互模式恒就绪；可交互 GUI 须非 `Unknown`）。
pub fn is_snapshot_ready(state: &AgentViewState) -> bool {
    !state.active || state.terminal_backed || state.status != AgentTurnStatus::Unknown
}

/// 会话通道整体是否已同步且无 SSH 重连挂起。
pub fn is_session_available(
    ws_connected: bool,
    transition: &AgentModeTransition,
    session_state: SessionState,
    ssh_reconnect_attempt: u32,
) -> bool {
    ws_connected
        && transition.synchronized
        && session_state == SessionState::Active
        && ssh_reconnect_attempt == 0
}

/// 是否允许向底层 PTY 发送键盘/粘贴输入帧。
pub fn can_send_pty_input(
    ws_connected: bool,
    transition: &AgentModeTransition,
    session_state: SessionState,
    ssh_reconnect_attempt: u32,
    agent_state: Option<&AgentViewState>,
) -> bool {
    is_session_available(ws_connected, transition, session_state, ssh_reconnect_attempt)
        && !transition.switching()
        && !is_interactive_gui(agent_state)
}

/// 是否允许在可交互 GUI 中发送新的用户提示词或切换模型/思考程度。
pub fn can_send_agent_prompt(
    ws_connected: bool,
    transition: &AgentModeTransition,
    session_state: SessionState,
    ssh_reconnect_attempt: u32,
    agent_state: Option<&AgentViewState>,
) -> bool {
    is_session_available(ws_connected, transition, session_state, ssh_reconnect_attempt)
        && !transition.switching()
        && agent_state.is_some_and(|s| {
            s.active
                && !s.terminal_backed
                && matches!(s.status, AgentTurnStatus::Idle | AgentTurnStatus::Error)
        })
}

/// 是否允许在可交互 GUI 中提交权限审批决定。
pub fn can_send_agent_permission(
    ws_connected: bool,
    transition: &AgentModeTransition,
    session_state: SessionState,
    ssh_reconnect_attempt: u32,
    agent_state: Option<&AgentViewState>,
) -> bool {
    is_session_available(ws_connected, transition, session_state, ssh_reconnect_attempt)
        && !transition.switching()
        && agent_state.is_some_and(|s| {
            s.active && !s.terminal_backed && s.status == AgentTurnStatus::AwaitingPermission
        })
}

/// 是否允许在可交互 GUI 中中断当前轮次。
pub fn can_interrupt_agent(
    ws_connected: bool,
    transition: &AgentModeTransition,
    session_state: SessionState,
    ssh_reconnect_attempt: u32,
    agent_state: Option<&AgentViewState>,
) -> bool {
    is_session_available(ws_connected, transition, session_state, ssh_reconnect_attempt)
        && !transition.switching()
        && agent_state.is_some_and(|s| {
            s.active
                && !s.terminal_backed
                && matches!(
                    s.status,
                    AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission
                )
        })
}

/// 是否允许点击顶栏的 `TUI ↔ GUI` 协议切换按钮（当前仅 Claude 支持运行中双向切换）。
pub fn can_switch_mode(
    agent: Option<SessionAgent>,
    ws_connected: bool,
    transition: &AgentModeTransition,
    session_state: SessionState,
    ssh_reconnect_attempt: u32,
    agent_state: Option<&AgentViewState>,
) -> bool {
    agent == Some(SessionAgent::Claude)
        && is_session_available(ws_connected, transition, session_state, ssh_reconnect_attempt)
        && !transition.switching()
        && (!is_interactive_gui(agent_state)
            || agent_state.is_some_and(|s| {
                matches!(s.status, AgentTurnStatus::Idle | AgentTurnStatus::Error)
            }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_to_gui_waits_for_both_http_and_ready_snapshot() {
        let mut t = AgentModeTransition::new().connected(false);
        assert!(t.synchronized);
        assert!(can_send_pty_input(true, &t, SessionState::Active, 0, None));

        // 发起切换到 GUI：立即封锁 PTY 与 GUI 输入
        t = t.start(true);
        assert!(t.switching());
        assert!(!can_send_pty_input(true, &t, SessionState::Active, 0, None));

        // HTTP 先返回成功，但 WS 快照尚未到达：仍处于 switching
        t = t.finish(true);
        assert!(t.switching());
        assert!(t.awaiting_snapshot());

        // 收到旧模式的快照（graphical = false）：不能解除切换锁
        t = t.snapshot(false, true);
        assert!(t.switching());

        // 收到目标 GUI 模式但 status 仍是 Unknown（ready = false）：仍不解锁
        t = t.snapshot(true, false);
        assert!(t.switching());

        // 收到就绪的 GUI 快照：解锁并允许发送 Agent 提示词，同时继续禁止向 PTY 写字节
        t = t.snapshot(true, true);
        assert!(!t.switching());
        let gui = AgentViewState::new(SessionAgent::Claude, true);
        assert!(!can_send_pty_input(true, &t, SessionState::Active, 0, Some(&gui)));
        assert!(can_send_agent_prompt(true, &t, SessionState::Active, 0, Some(&gui)));
    }

    #[test]
    fn snapshot_arriving_before_http_response_unlocks_on_finish() {
        let mut t = AgentModeTransition::new().connected(true).snapshot(true, true);
        t = t.start(false); // GUI -> TUI
        t = t.snapshot(false, true); // WS 先推来 TUI 快照
        assert!(t.switching(), "HTTP 尚未结束时继续锁住");
        t = t.finish(true);
        assert!(!t.switching(), "HTTP 结束后立即解锁");
    }

    #[test]
    fn http_failure_drops_synchronization_until_fresh_snapshot() {
        let mut t = AgentModeTransition::new().connected(false).start(true);
        t = t.finish(false);
        assert!(!t.switching());
        assert!(
            !t.synchronized,
            "HTTP 失败后服务端可能其实已切换，必须等重连快照重新同步"
        );
        assert!(!can_send_pty_input(true, &t, SessionState::Active, 0, None));

        t = t.disconnected().connected(true);
        assert!(!t.synchronized);
        t = t.snapshot(true, true);
        assert!(t.synchronized);
    }

    #[test]
    fn reconnect_while_awaiting_snapshot_accepts_authoritative_server_mode() {
        let mut t = AgentModeTransition::new().connected(false).start(true).finish(true);
        assert!(t.awaiting_snapshot());
        // 重连后别的一端可能又切回了 TUI，接受重连首帧权威模式避免永久卡死
        t = t.disconnected().connected(true);
        assert!(t.accept_current_mode);
        t = t.snapshot(false, true);
        assert!(!t.switching());
        assert!(t.synchronized);
    }
}
