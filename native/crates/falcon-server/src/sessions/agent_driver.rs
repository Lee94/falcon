//! 原生 Coding Agent 协议驱动器：将 `Claude Code` (`stream-json`) 与 `Codex` (`app-server`) 的
//! JSON 行流与 [`AgentViewState`] / [`ServerMessage`] 双向桥接。

use std::collections::{HashMap, HashSet};

use falcon_proto::{
    AgentDeltaKind, AgentEntry, AgentModelOption, AgentNoticeLevel, AgentPermissionDecision, AgentPlanItem,
    AgentPlanStatus, AgentSlashCommand, AgentToolCall, AgentToolContent, AgentToolKind,
    AgentToolStatus, AgentTurnStatus, AgentViewState, ProgramBlockKind, ProgramState,
    ProgramStatusRecord, ServerMessage, SessionAgent,
};
use serde_json::{Value, json};

use super::agent::{AGENT_CONNECT_SENTINEL, AGENT_EXIT_ACK, AGENT_EXIT_SENTINEL, AGENT_PROCESS_SENTINEL_PREFIX, AGENT_READY_SENTINEL, AGENT_START_SENTINEL_PREFIX};
use crate::auth::now_ms;

/// 单次处理输出或用户操作后的副产物，由 `SessionManager` 负责写回 PTY 与广播给 Viewer。
#[derive(Default, Debug)]
pub struct AgentDriverOutput {
    /// 需要写入 PTY 的原始文本（通常是以 `\n` 结尾的 JSON 行或流控哨兵）。
    pub to_pty: Vec<String>,
    /// 需要向当前会话所有 Viewer 广播的 WebSocket 控制消息。
    pub messages: Vec<ServerMessage>,
    /// CLI 退出结构化模式后交还给常规终端视图的透传字节。
    pub terminal_passthrough: String,
    /// 程序状态（OSC 7501 合成记录）是否发生变化。
    pub status_changed: bool,
    /// 会话自动标题更新（首次用户提问摘要）。
    pub title: Option<String>,
    /// 本次输出是否刚处理完 `__FALCON_AGENT_EXIT__` 并正式切换回终端视图。
    pub exited_native: bool,
}

#[derive(Debug, Clone)]
struct ClaudePermission {
    request_id: String,
    input: Value,
}

#[derive(Debug)]
enum PendingSetting {
    Model(String),
    ReasoningEffort(Option<String>),
}

#[derive(Default)]
struct ClaudeMessage {
    blocks: std::collections::BTreeMap<u64, ClaudeBlock>,
    join_text_runs: bool,
}

struct ClaudeBlock {
    kind: AgentDeltaKind,
    id: String,
    finalized: bool,
}

pub struct AgentDriver {
    pub state: AgentViewState,
    line_buf: String,
    started: bool,
    guarded_exit: bool,
    native_cli_pid: Option<u32>,
    stopping_for_terminal: bool,
    terminal_exit_received: bool,
    initialized: bool,
    next_seq: u64,
    pending_prompts: Vec<String>,
    pending_settings: HashMap<String, PendingSetting>,
    /// interrupt 的乐观 Idle 不能证明轮次结束；只由真实 result 核销已发出的轮次。
    pending_turns: usize,
    pending_controls: HashSet<String>,
    transport_uncertain: bool,
    expected_session_id: Option<String>,
    session_mismatch: bool,
    claude_messages: HashMap<String, ClaudeMessage>,
    claude_stream_ids: HashMap<Option<String>, String>,
    claude_seen_uuids: HashSet<String>,
    claude_catalog_recovery_requested: bool,
    claude_permissions: HashMap<String, ClaudePermission>,
    codex_completed_items: HashSet<String>,
    codex_reasoning_summaries: HashSet<String>,
    codex_model_cursors: HashSet<String>,
    codex_thread_id: Option<String>,
    codex_turn_id: Option<String>,
    codex_permissions: HashMap<String, Value>,
    has_title: bool,
    last_finished_state: Option<(ProgramState, Option<String>)>,
}

impl AgentDriver {
    pub fn new(agent: SessionAgent, cwd: Option<String>) -> Self {
        let mut state = AgentViewState::new(agent, true);
        state.cwd = cwd;
        Self {
            state,
            line_buf: String::new(),
            started: false,
            guarded_exit: false,
            native_cli_pid: None,
            stopping_for_terminal: false,
            terminal_exit_received: false,
            initialized: false,
            next_seq: 10,
            pending_prompts: Vec::new(),
            pending_settings: HashMap::new(),
            pending_turns: 0,
            pending_controls: HashSet::new(),
            transport_uncertain: false,
            expected_session_id: None,
            session_mismatch: false,
            claude_messages: HashMap::new(),
            claude_stream_ids: HashMap::new(),
            claude_seen_uuids: HashSet::new(),
            claude_catalog_recovery_requested: false,
            claude_permissions: HashMap::new(),
            codex_completed_items: HashSet::new(),
            codex_reasoning_summaries: HashSet::new(),
            codex_model_cursors: HashSet::new(),
            codex_thread_id: None,
            codex_turn_id: None,
            codex_permissions: HashMap::new(),
            has_title: false,
            last_finished_state: None,
        }
    }

    pub fn pin_session_id(&mut self, id: &str) {
        self.expected_session_id = Some(id.to_string());
    }

    pub fn ready_to_switch(&self) -> bool {
        self.state.active && self.initialized && !self.transport_uncertain && !self.session_mismatch
            && matches!(self.state.status, AgentTurnStatus::Idle | AgentTurnStatus::Error)
            && self.pending_turns == 0 && self.pending_prompts.is_empty() && self.pending_settings.is_empty()
            && self.pending_controls.is_empty() && self.claude_permissions.is_empty()
            && !self.state.entries.iter().any(|e| matches!(e, AgentEntry::ToolCall(tc) if tc.permission_pending))
    }

    /// 只有当前脚本的实时 pid 与 guarded EXIT/ACK 同时存在时，才具备退出 CLI 的生命周期证据。
    pub fn terminal_cli_pid(&self, session_id: &str) -> Option<u32> {
        (self.ready_to_switch() && self.guarded_exit && self.expected_session_id.as_deref() == Some(session_id))
            .then_some(self.native_cli_pid).flatten()
    }

    /// 结束 CLI 之后禁止迟到控制响应触发后续 JSON；EXIT/ACK 仍必须继续解析。
    pub fn begin_terminal_conversion(&mut self) -> AgentDriverOutput {
        self.stopping_for_terminal = true;
        self.transport_uncertain = true;
        self.state.status = AgentTurnStatus::Unknown;
        AgentDriverOutput { messages: vec![ServerMessage::AgentSnapshot { state: self.state.clone() }], ..Default::default() }
    }

    pub fn is_initialized(&self) -> bool { self.initialized }

    pub fn control_ready(&self) -> bool {
        self.initialized && !self.transport_uncertain && !self.session_mismatch
    }

    /// 启动超时后保留可见状态，但不把提示词或控制请求交给身份未确认的程序。
    pub fn initialization_failed(&mut self, reason: &str) -> AgentDriverOutput {
        self.transport_uncertain = true;
        self.state.status = AgentTurnStatus::Unknown;
        let mut out = AgentDriverOutput::default();
        self.notice(reason, &mut out);
        out.messages.push(ServerMessage::AgentSnapshot { state: self.state.clone() });
        out
    }

    /// 断线期间 CLI 可能已退出到 shell，不能向未知 PTY 发 initialize。保留模式并显示明确的
    /// 恢复限制，既不重放 VT 截图里的 JSON，也不允许在未知轮次上提交新输入。
    pub fn reconnect_stream(&mut self) -> AgentDriverOutput {
        self.started = true;
        self.initialized = false;
        self.line_buf.clear();
        self.transport_uncertain = true;
        self.state.status = AgentTurnStatus::Unknown;
        self.pending_controls.clear();
        let mut out = AgentDriverOutput::default();
        self.notice("结构化宿主机连接已断开，无法安全确认原 CLI 与轮次；暂不支持自动重附着。请终止该会话后在终端中使用固定启动 UUID 续接对话。", &mut out);
        out.messages.push(ServerMessage::AgentSnapshot { state: self.state.clone() });
        out
    }

    pub fn is_active(&self) -> bool {
        self.state.active && !self.terminal_exit_received
    }

    pub fn is_started(&self) -> bool {
        self.started
    }

    /// EXIT 后恢复原始字节；显式返回 TUI 期间，Viewer 仍保持 unknown，直到服务端确认新 CLI 可用。
    pub fn should_intercept_pty(&self) -> bool {
        self.is_active()
    }

    fn alloc_id(&mut self, prefix: &str) -> String {
        self.next_seq += 1;
        format!("{prefix}-{}", self.next_seq)
    }

    /// 合成供侧栏徽标、通知横幅与提示音使用的 [`ProgramStatusRecord`]。
    pub fn program_status_records(&self) -> Vec<ProgramStatusRecord> {
        if !self.is_active() {
            return Vec::new();
        }
        let app = match self.state.agent {
            SessionAgent::Claude => "Claude Code",
            SessionAgent::Codex => "Codex",
            other => other.as_str(),
        };
        let now = now_ms();
        match self.state.status {
            AgentTurnStatus::Running => vec![ProgramStatusRecord {
                id: "agent".into(),
                state: ProgramState::Working,
                kind: None,
                app: Some(app.into()),
                title: Some("正在思考与执行…".into()),
                message: None,
                progress: None,
                updated_at: now,
            }],
            AgentTurnStatus::AwaitingPermission => {
                let pending = self.state.entries.iter().rev().find_map(|e| match e {
                    AgentEntry::ToolCall(tc) if tc.permission_pending => Some(tc),
                    _ => None,
                });
                vec![ProgramStatusRecord {
                    id: "agent".into(),
                    state: ProgramState::Blocked,
                    kind: Some(ProgramBlockKind::Permission),
                    app: Some(app.into()),
                    title: pending.map(|tc| tc.title.clone()).or_else(|| Some("等待权限审批".into())),
                    message: pending.and_then(|tc| tc.permission_reason.clone()),
                    progress: None,
                    updated_at: now,
                }]
            }
            AgentTurnStatus::Idle | AgentTurnStatus::Error | AgentTurnStatus::Unknown => {
                if let Some((st, msg)) = &self.last_finished_state {
                    vec![ProgramStatusRecord {
                        id: "agent".into(),
                        state: *st,
                        kind: None,
                        app: Some(app.into()),
                        title: msg.clone(),
                        message: None,
                        progress: None,
                        updated_at: now,
                    }]
                } else {
                    Vec::new()
                }
            }
        }
    }

    /// 用户已查看会话（窗口聚焦）：清除完成或报错提示状态。
    pub fn clear_seen_status(&mut self) -> bool {
        if self.last_finished_state.take().is_some() {
            return true;
        }
        false
    }

    /// 处理用户提交的提示词。
    pub fn send_prompt(&mut self, text: &str) -> AgentDriverOutput {
        let mut out = AgentDriverOutput::default();
        let trimmed = text.trim();
        if trimmed.is_empty() || !self.state.active {
            return out;
        }

        if self.transport_uncertain || self.session_mismatch {
            self.notice("无法确认当前轮次或对话 UUID，暂时不能提交新轮次。", &mut out);
            return out;
        }

        // 支持 `/clear` 清空当前时间线视图
        if trimmed == "/clear" {
            self.state.entries.clear();
            out.messages.push(ServerMessage::AgentSnapshot {
                state: self.state.clone(),
            });
            return out;
        }

        let user_id = self.alloc_id("user");
        let user_entry = AgentEntry::User {
            id: user_id,
            text: trimmed.to_string(),
        };
        self.state.entries.push(user_entry.clone());
        out.messages.push(ServerMessage::AgentEntry { entry: user_entry });

        if !self.has_title {
            self.has_title = true;
            let first_line = trimmed.lines().next().unwrap_or(trimmed);
            let summary: String = first_line.chars().take(48).collect();
            if !summary.is_empty() {
                out.title = Some(summary);
            }
        }

        self.last_finished_state = None;
        self.state.status = AgentTurnStatus::Running;
        out.status_changed = true;
        out.messages.push(self.meta_message(false));

        // 参数确认与下一条输入可以相邻到达；保留输入，等确认后按流顺序发送。
        if !self.initialized || !self.pending_settings.is_empty() {
            self.pending_prompts.push(trimmed.to_string());
            return out;
        }

        self.encode_prompt_to_pty(trimmed, &mut out);
        out
    }

    fn encode_prompt_to_pty(&mut self, text: &str, out: &mut AgentDriverOutput) {
        self.claude_stream_ids.clear();
        match self.state.agent {
            SessionAgent::Claude => {
                self.pending_turns += 1;
                let msg = json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": text,
                    },
                    "parent_tool_use_id": Value::Null,
                });
                out.to_pty.push(format!("{msg}\n"));
            }
            SessionAgent::Codex => {
                if let Some(thread_id) = self.codex_thread_id.clone() {
                    let req_id = self.alloc_id("turn");
                    let msg = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "method": "turn/start",
                        "params": {
                            "threadId": thread_id,
                            "model": self.state.model,
                            "effort": self.state.reasoning_effort.as_ref().or_else(|| {
                                self.state.selected_model_option().and_then(|m| m.default_reasoning_effort.as_ref())
                            }),
                            "input": [{
                                "type": "text",
                                "text": text,
                                "text_elements": [],
                            }],
                        }
                    });
                    out.to_pty.push(format!("{msg}\n"));
                } else {
                    self.pending_prompts.push(text.to_string());
                }
            }
            _ => {}
        }
    }

    /// 中断当前正在执行的轮次。
    pub fn interrupt(&mut self) -> AgentDriverOutput {
        let mut out = AgentDriverOutput::default();
        if !self.state.active {
            return out;
        }
        if self.transport_uncertain || self.session_mismatch {
            self.notice("无法确认当前 Agent 控制通道，未发送中断请求。", &mut out);
            return out;
        }
        self.pending_prompts.clear();
        match self.state.agent {
            SessionAgent::Claude => {
                let req_id = self.alloc_id("int");
                let msg = json!({
                    "type": "control_request",
                    "request_id": req_id,
                    "request": { "subtype": "interrupt" }
                });
                out.to_pty.push(format!("{msg}\n"));
            }
            SessionAgent::Codex => {
                if let (Some(thread_id), Some(turn_id)) =
                    (self.codex_thread_id.clone(), self.codex_turn_id.clone())
                {
                    let req_id = self.alloc_id("int");
                    let msg = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "method": "turn/interrupt",
                        "params": {
                            "threadId": thread_id,
                            "turnId": turn_id,
                        }
                    });
                    out.to_pty.push(format!("{msg}\n"));
                }
            }
            _ => {}
        }
        self.state.status = AgentTurnStatus::Idle;
        out.status_changed = true;
        out.messages.push(self.meta_message(false));
        out
    }

    /// 答复工具调用的权限审批。
    pub fn answer_permission(
        &mut self,
        call_id: &str,
        decision: AgentPermissionDecision,
        updated_command: Option<String>,
    ) -> AgentDriverOutput {
        let mut out = AgentDriverOutput::default();
        if !self.state.active {
            return out;
        }

        if self.transport_uncertain || self.session_mismatch {
            self.notice("无法确认当前 Agent 控制通道，未发送审批答复。", &mut out);
            return out;
        }
        match self.state.agent {
            SessionAgent::Claude => {
                if let Some(perm) = self.claude_permissions.remove(call_id) {
                    let mut input = perm.input;
                    if let Some(cmd) = &updated_command {
                        if let Some(obj) = input.as_object_mut() {
                            obj.insert("command".into(), Value::String(cmd.clone()));
                        }
                    }
                    let response_payload = match decision {
                        AgentPermissionDecision::Allow | AgentPermissionDecision::AllowSession => {
                            json!({
                                "behavior": "allow",
                                "updatedInput": input,
                            })
                        }
                        AgentPermissionDecision::Deny => {
                            json!({
                                "behavior": "deny",
                                "message": "用户拒绝了该工具调用",
                            })
                        }
                    };
                    let msg = json!({
                        "type": "control_response",
                        "response": {
                            "subtype": "success",
                            "request_id": perm.request_id,
                            "response": response_payload,
                        }
                    });
                    out.to_pty.push(format!("{msg}\n"));
                    if decision == AgentPermissionDecision::AllowSession {
                        let req_id = self.alloc_id("perm-mode");
                        let mode_msg = json!({
                            "type": "control_request",
                            "request_id": req_id,
                            "request": {
                                "subtype": "set_permission_mode",
                                "mode": "acceptEdits",
                            }
                        });
                        out.to_pty.push(format!("{mode_msg}\n"));
                        self.state.permission_mode = Some("acceptEdits".into());
                    }
                }
            }
            SessionAgent::Codex => {
                if let Some(rpc_id) = self.codex_permissions.remove(call_id) {
                    let dec_str = match decision {
                        AgentPermissionDecision::Allow => "accept",
                        AgentPermissionDecision::AllowSession => "acceptForSession",
                        AgentPermissionDecision::Deny => "decline",
                    };
                    let msg = json!({
                        "jsonrpc": "2.0",
                        "id": rpc_id,
                        "result": {
                            "decision": dec_str,
                        }
                    });
                    out.to_pty.push(format!("{msg}\n"));
                }
            }
            _ => {}
        }

        // 更新对应工具调用卡片的状态
        let mut updated_entry = None;
        for entry in &mut self.state.entries {
            if let AgentEntry::ToolCall(tc) = entry
                && tc.id == call_id
            {
                tc.permission_pending = false;
                tc.status = if decision == AgentPermissionDecision::Deny {
                    AgentToolStatus::Failed
                } else {
                    AgentToolStatus::InProgress
                };
                if let Some(cmd) = updated_command {
                    tc.title = cmd.clone();
                    for c in &mut tc.content {
                        if let AgentToolContent::Terminal { command, .. } = c {
                            *command = cmd.clone();
                        }
                    }
                }
                updated_entry = Some(AgentEntry::ToolCall(tc.clone()));
                break;
            }
        }
        if let Some(entry) = updated_entry {
            out.messages.push(ServerMessage::AgentEntry { entry });
        }

        let still_waiting = self.state.entries.iter().any(|e| {
            matches!(e, AgentEntry::ToolCall(tc) if tc.permission_pending)
        });
        self.state.status = if still_waiting {
            AgentTurnStatus::AwaitingPermission
        } else {
            AgentTurnStatus::Running
        };
        out.status_changed = true;
        out.messages.push(self.meta_message(false));
        out
    }

    /// 只在轮次之间修改参数。Claude 等控制确认，Codex 将选项交给下一次 turn/start。
    pub fn set_model(&mut self, model: &str) -> AgentDriverOutput {
        let mut out = AgentDriverOutput::default();
        if !self.can_change_settings(&mut out) {
            return out;
        }
        let model = model.trim();
        if !self.state.model_options.iter().any(|m| m.id == model) {
            self.notice("该模型不在 CLI 返回的可用模型列表中。", &mut out);
            return out;
        }
        if self.state.agent == SessionAgent::Claude {
            let req_id = self.alloc_id("model");
            self.pending_settings.insert(req_id.clone(), PendingSetting::Model(model.to_string()));
            let msg = json!({
                "type": "control_request", "request_id": req_id,
                "request": { "subtype": "set_model", "model": model }
            });
            out.to_pty.push(format!("{msg}\n"));
        } else if self.state.agent == SessionAgent::Codex {
            self.state.model = Some(model.to_string());
            self.state.reasoning_effort = None;
            out.messages.push(self.meta_message(false));
        }
        out
    }

    pub fn set_reasoning_effort(&mut self, effort: Option<&str>) -> AgentDriverOutput {
        let mut out = AgentDriverOutput::default();
        if !self.can_change_settings(&mut out) {
            return out;
        }
        let effort = effort.map(str::trim);
        if self.state.selected_model_option().is_none_or(|m| {
            m.reasoning_efforts.is_empty() || effort.is_some_and(|e| !m.reasoning_efforts.iter().any(|v| v == e))
        }) {
            self.notice("当前模型不支持该思考程度。", &mut out);
            return out;
        }
        if self.state.agent == SessionAgent::Claude {
            let req_id = self.alloc_id("effort");
            self.pending_settings.insert(req_id.clone(), PendingSetting::ReasoningEffort(effort.map(str::to_string)));
            // effort 是模型的推理程度，不能用 set_max_thinking_tokens 的 token 预算替代。
            let msg = json!({
                "type": "control_request", "request_id": req_id,
                "request": { "subtype": "apply_flag_settings", "settings": { "effortLevel": effort } }
            });
            out.to_pty.push(format!("{msg}\n"));
        } else if self.state.agent == SessionAgent::Codex {
            self.state.reasoning_effort = effort.map(str::to_string);
            out.messages.push(self.meta_message(false));
        }
        out
    }

    fn can_change_settings(&mut self, out: &mut AgentDriverOutput) -> bool {
        if !self.state.active || !self.initialized {
            self.notice("Agent 尚未就绪，暂时不能修改模型设置。", out);
            return false;
        }
        if self.transport_uncertain || self.session_mismatch || self.pending_turns != 0
            || matches!(self.state.status, AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission | AgentTurnStatus::Unknown) {
            self.notice("请等待当前轮次结束后再修改模型或思考程度。", out);
            return false;
        }
        if !self.pending_settings.is_empty() {
            self.notice("正在确认上一项模型设置，请稍后重试。", out);
            return false;
        }
        true
    }

    fn notice(&mut self, text: &str, out: &mut AgentDriverOutput) {
        let entry = AgentEntry::Notice { id: self.alloc_id("notice"), level: AgentNoticeLevel::Warning, text: text.to_string() };
        self.state.entries.push(entry.clone());
        out.messages.push(ServerMessage::AgentEntry { entry });
    }

    fn request_claude_initialize(&mut self, out: &mut AgentDriverOutput) {
        self.pending_controls.insert("init-1".to_string());
        let init = json!({
            "type": "control_request",
            "request_id": "init-1",
            "request": { "subtype": "initialize" }
        });
        out.to_pty.push(format!("{init}\n"));
    }

    fn request_claude_settings(&mut self, out: &mut AgentDriverOutput) {
        let req_id = self.alloc_id("settings");
        self.pending_controls.insert(req_id.clone());
        let msg = json!({
            "type": "control_request", "request_id": req_id,
            "request": { "subtype": "get_settings" }
        });
        out.to_pty.push(format!("{msg}\n"));
    }

    /// 兼容旧客户端的切换消息：只补发快照，绝不退出 CLI。
    /// `--resume` 只能加载已保存的对话，不能迁移正在执行的模型请求或权限审批。
    pub fn switch_to_terminal(&mut self) -> AgentDriverOutput {
        AgentDriverOutput {
            messages: vec![ServerMessage::AgentSnapshot { state: self.state.clone() }],
            ..Default::default()
        }
    }

    /// 处理从 PTY 读到的字节块。
    pub fn on_data(&mut self, chunk: &str) -> AgentDriverOutput {
        let mut out = AgentDriverOutput::default();
        if !self.should_intercept_pty() {
            out.terminal_passthrough.push_str(chunk);
            return out;
        }

        self.line_buf.push_str(chunk);
        while let Some(pos) = self.line_buf.find('\n') {
            let mut line = self.line_buf[..pos].to_string();
            self.line_buf.drain(..=pos);
            if line.ends_with('\r') {
                line.pop();
            }
            self.handle_line(&line, &mut out);
            if !self.should_intercept_pty() {
                if !self.line_buf.is_empty() {
                    out.terminal_passthrough.push_str(&std::mem::take(&mut self.line_buf));
                }
                break;
            }
        }
        out
    }

    fn handle_line(&mut self, raw_line: &str, out: &mut AgentDriverOutput) {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            return;
        }

        if self.stopping_for_terminal && trimmed != AGENT_EXIT_SENTINEL { return; }
        if !self.started && self.state.active
            && let Some(pid) = trimmed.strip_prefix(AGENT_PROCESS_SENTINEL_PREFIX)
        {
            self.native_cli_pid = pid.parse::<u32>().ok().filter(|p| *p > 1);
            return;
        }

        if trimmed == AGENT_READY_SENTINEL && self.state.active && !self.started {
            out.to_pty.push(format!("{AGENT_CONNECT_SENTINEL}\n"));
            return;
        }

        if let Some(agent_str) = trimmed.strip_prefix(AGENT_START_SENTINEL_PREFIX) {
            if !self.state.active || (agent_str != self.state.agent.as_str()
                && agent_str.strip_suffix(":guarded") != Some(self.state.agent.as_str())) {
                return;
            }
            self.guarded_exit = agent_str.ends_with(":guarded");
            self.started = true;
            self.initialized = false;
            // 若是 `--resume` 失败后自动回退到 `--session-id` 的第二次启动，清掉前一次的未找到对话提示
            self.state.entries.retain(|e| {
                !matches!(e, AgentEntry::Notice { text, .. } if text.contains("No conversation found"))
            });
            match self.state.agent {
                SessionAgent::Claude => {
                    self.claude_catalog_recovery_requested = false;
                    self.request_claude_initialize(out);
                }
                SessionAgent::Codex => {
                    let init = json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "clientInfo": {
                                "name": "falcon",
                                "version": falcon_proto::sessiond::FALCON_VERSION,
                            }
                        }
                    });
                    out.to_pty.push(format!("{init}\n"));
                }
                _ => {}
            }
            return;
        }

        if trimmed == AGENT_EXIT_SENTINEL {
            let was_active = self.state.active;
            self.terminal_exit_received = self.stopping_for_terminal;
            if !self.stopping_for_terminal {
                self.state.active = false;
                self.state.status = AgentTurnStatus::Idle;
            }
            self.last_finished_state = None;
            out.exited_native = true;
            // START 和失败 EXIT 可能在同一块输出里；此前排队的 initialize 不能在 shell 中执行。
            out.to_pty.clear();
            if self.guarded_exit { out.to_pty.push(format!("{AGENT_EXIT_ACK}\n")); }
            if was_active {
                out.status_changed = true;
                if !self.stopping_for_terminal { out.messages.push(self.meta_message(false)); }
            }
            return;
        }

        if !self.state.active || !self.started {
            return;
        }

        // 提取行内的 JSON 对象（剥除可能出现在行首的终端控制序列，如 `\x1b[?25h`）
        let Some(json_slice) = extract_json_object(trimmed) else {
            return;
        };
        let Ok(v) = serde_json::from_str::<Value>(json_slice) else {
            return;
        };

        match self.state.agent {
            SessionAgent::Claude => self.handle_claude_json(&v, out),
            SessionAgent::Codex => self.handle_codex_json(&v, out),
            _ => {}
        }
    }

    fn flush_pending_prompts(&mut self, out: &mut AgentDriverOutput) {
        let prompts = std::mem::take(&mut self.pending_prompts);
        for p in prompts {
            self.encode_prompt_to_pty(&p, out);
        }
    }

    fn meta_message(&self, include_catalog: bool) -> ServerMessage {
        ServerMessage::AgentMeta {
            active: self.state.active,
            status: self.state.status,
            model: self.state.model.clone(),
            permission_mode: self.state.permission_mode.clone(),
            cwd: self.state.cwd.clone(),
            commands: include_catalog.then(|| self.state.commands.clone()),
            models: include_catalog.then(|| self.state.models.clone()),
            model_options: include_catalog.then(|| self.state.model_options.clone()),
            reasoning_effort: self.state.reasoning_effort.clone(),
        }
    }

    fn handle_claude_setting_response(&mut self, response: &Value, out: &mut AgentDriverOutput) -> bool {
        let Some(request_id) = response["request_id"].as_str() else { return false };
        if let Some(setting) = self.pending_settings.remove(request_id) {
            if response["subtype"].as_str() != Some("success") {
                let error = response["error"].as_str().unwrap_or("CLI 拒绝了模型设置");
                self.notice(error, out);
                self.flush_pending_prompts(out);
                return true;
            }
            match setting {
                PendingSetting::Model(requested) => {
                    self.state.model = Some(response["response"]["model"].as_str().unwrap_or(&requested).to_string());
                }
                PendingSetting::ReasoningEffort(effort) => self.state.reasoning_effort = effort,
            }
            out.messages.push(self.meta_message(false));
            self.request_claude_settings(out);
            self.flush_pending_prompts(out);
            return true;
        }
        if request_id.starts_with("settings-") {
            if response["subtype"].as_str() == Some("success") {
                // max 等会话级覆盖不一定存在于 flagSettings；applied 才是实际执行值。
                let applied = &response["response"]["applied"];
                if let Some(model) = applied["model"].as_str() {
                    self.state.model = Some(model.to_string());
                }
                self.state.reasoning_effort = applied["effort"].as_str().map(str::to_string);
                out.messages.push(self.meta_message(false));
            }
            return true;
        }
        false
    }

    // ---------------- Claude `stream-json` ----------------

    fn claude_stream_id(&mut self, v: &Value, message_id: Option<&str>) -> String {
        // 子 agent 的流可以穿插在主流中；没有 api_message_id 的旧版本按 parent 分开记录。
        let parent = v["parent_tool_use_id"].as_str().map(str::to_string);
        let id = message_id.or_else(|| v["api_message_id"].as_str())
            .map(str::to_string)
            .or_else(|| self.claude_stream_ids.get(&parent).cloned())
            .unwrap_or_else(|| self.alloc_id("claude-message"));
        self.claude_stream_ids.insert(parent, id.clone());
        self.claude_messages.entry(id.clone()).or_default();
        id
    }

    fn claude_block(&mut self, message_id: &str, index: u64, kind: AgentDeltaKind) -> (&ClaudeBlock, bool) {
        let message = self.claude_messages.entry(message_id.to_string()).or_default();
        let is_new = !message.blocks.contains_key(&index);
        if is_new {
            // 2.1.322 会把相邻 text 块合成一个 assistant 帧；流式时也必须指向同一条正文，
            // 否则完成帧覆盖第一块后，其余块仍留在时间线上，形成重复内容。
            let joined_id = (message.join_text_runs && kind == AgentDeltaKind::Message)
                .then(|| index.checked_sub(1).and_then(|previous| message.blocks.get(&previous)))
                .flatten()
                .filter(|block| block.kind == kind && !block.finalized)
                .map(|block| block.id.clone());
            let prefix = if kind == AgentDeltaKind::Thought { "claude-thought" } else { "claude-msg" };
            let id = joined_id.unwrap_or_else(|| format!("{prefix}-{message_id}-{index}"));
            message.blocks.insert(index, ClaudeBlock { kind, id, finalized: false });
        }
        (message.blocks.get(&index).unwrap(), is_new)
    }

    fn claude_final_block_id(&mut self, message_id: &str, kind: AgentDeltaKind) -> String {
        let message = self.claude_messages.entry(message_id.to_string()).or_default();
        // assistant.content 是当前已完成块的局部数组，不是 API message 的完整 content。
        // 例如 thinking 在原始索引 0、text 在 1，两个 assistant 帧的局部下标却都是 0。
        let index = message.blocks.iter()
            .find(|(_, block)| block.kind == kind && !block.finalized)
            .map(|(index, _)| *index)
            .unwrap_or_else(|| message.blocks.last_key_value().map_or(0, |(index, _)| index + 1));
        let id = self.claude_block(message_id, index, kind).0.id.clone();
        for block in self.claude_messages.get_mut(message_id).unwrap().blocks.values_mut() {
            if block.id == id {
                block.finalized = true;
            }
        }
        id
    }

    fn handle_claude_json(&mut self, v: &Value, out: &mut AgentDriverOutput) {
        let Some(msg_type) = v["type"].as_str() else { return };
        if let (Some(expected), Some(actual)) = (&self.expected_session_id, v["session_id"].as_str()) {
            if expected != actual {
                self.session_mismatch = true;
            }
        }
        // CLI 重发的是同一个 UUID 的帧，不按文本去重：不同块与不同轮次可以有相同正文。
        if matches!(msg_type, "assistant" | "user" | "result")
            && let Some(uuid) = v["uuid"].as_str().filter(|s| !s.is_empty())
            && !self.claude_seen_uuids.insert(uuid.to_string())
        {
            return;
        }
        match msg_type {
            "control_response" => {
                let resp = &v["response"];
                if let Some(id) = resp["request_id"].as_str() { self.pending_controls.remove(id); }
                if self.handle_claude_setting_response(resp, out) {
                    return;
                }
                if resp["request_id"].as_str() == Some("init-1") {
                    if resp["subtype"].as_str() != Some("success") {
                        let error = resp["error"].as_str().unwrap_or("CLI 未返回初始化结果");
                        self.notice(&format!("Claude 初始化失败：{error}"), out);
                        return;
                    }
                    let inner = &resp["response"];
                    if let Some(cmds) = inner["commands"].as_array() {
                        self.state.commands = cmds
                            .iter()
                            .filter_map(|c| {
                                let name = c["name"].as_str()?.trim();
                                if name.is_empty() {
                                    return None;
                                }
                                let desc = c["description"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .lines()
                                    .next()
                                    .unwrap_or_default()
                                    .chars()
                                    .take(100)
                                    .collect();
                                Some(AgentSlashCommand {
                                    name: name.to_string(),
                                    description: desc,
                                })
                            })
                            .take(64)
                            .collect();
                    }
                    if let Some(models) = inner["models"].as_array() {
                        self.state.model_options = models.iter().filter_map(|m| {
                            let id = m["value"].as_str().filter(|s| !s.is_empty())?.to_string();
                            let reasoning_efforts = if m["supportsEffort"].as_bool() == Some(true) {
                                string_list(&m["supportedEffortLevels"])
                            } else { Vec::new() };
                            Some(AgentModelOption {
                                label: m["displayName"].as_str().unwrap_or(&id).to_string(),
                                resolved_model: m["resolvedModel"].as_str().map(str::to_string),
                                id, reasoning_efforts, default_reasoning_effort: None,
                            })
                        }).collect();
                        self.state.models = self.state.model_options.iter().map(|m| m.id.clone()).collect();
                    }
                    self.initialized = true;
                    out.messages.push(self.meta_message(true));
                    self.request_claude_settings(out);
                    self.flush_pending_prompts(out);
                }
            }
            "system" => {
                if v["subtype"].as_str() == Some("init") {
                    if let Some(m) = v["model"].as_str().filter(|s| !s.is_empty()) {
                        self.state.model = Some(m.to_string());
                    }
                    if let Some(cwd) = v["cwd"].as_str().filter(|s| !s.is_empty()) {
                        self.state.cwd = Some(cwd.to_string());
                    }
                    if let Some(pm) = v["permissionMode"].as_str().filter(|s| !s.is_empty()) {
                        self.state.permission_mode = Some(pm.to_string());
                    }
                    if self.state.model_options.is_empty() && !self.claude_catalog_recovery_requested {
                        // system/init 只有当前型号，不能代替 initialize 的能力目录。2.1.322 实测
                        // 重复 initialize（包括同一 request_id）会重新返回目录；每次 CLI 启动最多
                        // 补发一次，避免旧版本拒绝重入或目录本来为空时反复请求。
                        self.claude_catalog_recovery_requested = true;
                        self.request_claude_initialize(out);
                    }
                    self.initialized = true;
                    out.messages.push(self.meta_message(false));
                    self.flush_pending_prompts(out);
                }
            }
            "stream_event" => {
                let ev = &v["event"];
                match ev["type"].as_str() {
                    Some("message_start") => {
                        let id = self.claude_stream_id(v, ev["message"]["id"].as_str());
                        self.claude_messages.get_mut(&id).unwrap().join_text_runs =
                            v["text_runs_joined"].as_bool().unwrap_or(false);
                    }
                    Some("content_block_start") => {
                        let idx = ev["index"].as_u64().unwrap_or(0);
                        let cb = &ev["content_block"];
                        let (kind, text) = match cb["type"].as_str() {
                            Some("thinking") => (AgentDeltaKind::Thought, cb["thinking"].as_str()),
                            Some("text") => (AgentDeltaKind::Message, cb["text"].as_str()),
                            _ => return,
                        };
                        let message_id = self.claude_stream_id(v, None);
                        let (block, is_new) = self.claude_block(&message_id, idx, kind);
                        if !is_new { return; }
                        let id = block.id.clone();
                        let text = text.unwrap_or_default();
                        if self.state.entries.iter().any(|entry| entry.id() == Some(&id)) {
                            if !text.is_empty() {
                                self.state.apply_delta(&id, kind, text);
                                out.messages.push(ServerMessage::AgentDelta { id, kind, delta: text.to_string() });
                            }
                        } else {
                            let entry = if kind == AgentDeltaKind::Thought {
                                AgentEntry::Thought { id, text: text.to_string() }
                            } else {
                                AgentEntry::Message { id, text: text.to_string() }
                            };
                            self.state.upsert_entry(entry.clone());
                            out.messages.push(ServerMessage::AgentEntry { entry });
                        }
                    }
                    Some("content_block_delta") => {
                        let idx = ev["index"].as_u64().unwrap_or(0);
                        let delta = &ev["delta"];
                        let (kind, text) = match delta["type"].as_str() {
                            Some("thinking_delta") => (AgentDeltaKind::Thought, delta["thinking"].as_str()),
                            Some("text_delta") => (AgentDeltaKind::Message, delta["text"].as_str()),
                            _ => return,
                        };
                        if let Some(text) = text.filter(|s| !s.is_empty()) {
                            let message_id = self.claude_stream_id(v, None);
                            let (block, _) = self.claude_block(&message_id, idx, kind);
                            if block.finalized { return; }
                            let id = block.id.clone();
                            self.state.apply_delta(&id, kind, text);
                            out.messages.push(ServerMessage::AgentDelta { id, kind, delta: text.to_string() });
                        }
                    }
                    _ => {}
                }
            }
            "assistant" => {
                if let Some(m) = v["message"]["model"].as_str().filter(|s| !s.is_empty()) {
                    self.state.model = Some(m.to_string());
                }
                let Some(contents) = v["message"]["content"].as_array() else { return };
                let message_id = v["message"]["id"].as_str().map(str::to_string)
                    .unwrap_or_else(|| self.claude_stream_id(v, None));
                for block in contents {
                    match block["type"].as_str() {
                        Some("thinking") => {
                            let Some(thinking) = block["thinking"].as_str().filter(|s| !s.is_empty()) else {
                                continue;
                            };
                            let id = self.claude_final_block_id(&message_id, AgentDeltaKind::Thought);
                            let entry = AgentEntry::Thought {
                                id,
                                text: thinking.to_string(),
                            };
                            self.state.upsert_entry(entry.clone());
                            out.messages.push(ServerMessage::AgentEntry { entry });
                        }
                        Some("text") => {
                            let Some(text) = block["text"].as_str().filter(|s| !s.is_empty()) else {
                                continue;
                            };
                            let id = self.claude_final_block_id(&message_id, AgentDeltaKind::Message);
                            let entry = AgentEntry::Message {
                                id,
                                text: text.to_string(),
                            };
                            self.state.upsert_entry(entry.clone());
                            out.messages.push(ServerMessage::AgentEntry { entry });
                        }
                        Some("tool_use") => {
                            let id = block["id"]
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| self.alloc_id("tool"));
                            let name = block["name"].as_str().unwrap_or("tool");
                            let input = &block["input"];
                            if name == "TodoWrite" {
                                if let Some(todos) = input["todos"].as_array() {
                                    let items = todos
                                        .iter()
                                        .filter_map(|t| {
                                            let content = t["content"].as_str()?.to_string();
                                            let status = match t["status"].as_str() {
                                                Some("completed") => AgentPlanStatus::Completed,
                                                Some("in_progress") => AgentPlanStatus::InProgress,
                                                _ => AgentPlanStatus::Pending,
                                            };
                                            Some(AgentPlanItem { content, status })
                                        })
                                        .collect();
                                    let entry = AgentEntry::Plan { id, items };
                                    self.state.upsert_entry(entry.clone());
                                    out.messages.push(ServerMessage::AgentEntry { entry });
                                    continue;
                                }
                            }
                            let tc = claude_tool_call(&id, name, input, self.state.cwd.as_deref());
                            let entry = AgentEntry::ToolCall(tc);
                            self.state.upsert_entry(entry.clone());
                            out.messages.push(ServerMessage::AgentEntry { entry });
                        }
                        _ => {}
                    }
                }
            }
            "control_request" => {
                let req = &v["request"];
                if req["subtype"].as_str() == Some("can_use_tool") {
                    let request_id = v["request_id"].as_str().unwrap_or_default().to_string();
                    let tool_name = req["tool_name"].as_str().unwrap_or("tool");
                    let input = req["input"].clone();
                    if tool_name == "AskUserQuestion" {
                        // 该请求需要结构化答案，不能把“允许工具”冒充用户回答。
                        let error = "Falcon 图形界面暂不支持此提问表单，请改用普通消息向用户提问。";
                        let response = json!({"type":"control_response","response":{
                            "subtype":"success","request_id":request_id,
                            "response":{"behavior":"deny","message":error}
                        }});
                        out.to_pty.push(format!("{response}\n"));
                        self.notice(error, out);
                        return;
                    }
                    let call_id = req["tool_use_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| request_id.clone());
                    let reason = req["decision_reason"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);

                    self.claude_permissions.insert(
                        call_id.clone(),
                        ClaudePermission {
                            request_id,
                            input: input.clone(),
                        },
                    );

                    let mut tc = self
                        .state
                        .entries
                        .iter()
                        .find_map(|e| match e {
                            AgentEntry::ToolCall(existing) if existing.id == call_id => {
                                Some(existing.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| {
                            claude_tool_call(&call_id, tool_name, &input, self.state.cwd.as_deref())
                        });
                    tc.permission_pending = true;
                    tc.permission_reason = reason;
                    tc.status = AgentToolStatus::Pending;

                    let entry = AgentEntry::ToolCall(tc);
                    self.state.upsert_entry(entry.clone());
                    self.state.status = AgentTurnStatus::AwaitingPermission;
                    out.status_changed = true;
                    out.messages.push(ServerMessage::AgentEntry { entry });
                    out.messages.push(self.meta_message(false));
                } else {
                    let subtype = req["subtype"].as_str().unwrap_or("unknown");
                    let error = format!("Falcon 图形界面暂不支持控制请求 {subtype}，已明确拒绝该请求。");
                    if let Some(request_id) = v["request_id"].as_str() {
                        let response = json!({"type":"control_response","response":{
                            "subtype":"error","request_id":request_id,"error":error
                        }});
                        out.to_pty.push(format!("{response}\n"));
                    }
                    self.notice(&error, out);
                }
            }
            "user" => {
                let Some(contents) = v["message"]["content"].as_array() else { return };
                for item in contents {
                    if item["type"].as_str() != Some("tool_result") {
                        continue;
                    }
                    let Some(tool_use_id) = item["tool_use_id"].as_str() else { continue };
                    let is_error = item["is_error"].as_bool().unwrap_or(false);
                    let result_text = extract_claude_tool_result(item, &v["tool_use_result"]);

                    let mut updated = None;
                    for entry in &mut self.state.entries {
                        if let AgentEntry::ToolCall(tc) = entry
                            && tc.id == tool_use_id
                        {
                            tc.permission_pending = false;
                            tc.status = if is_error {
                                AgentToolStatus::Failed
                            } else {
                                AgentToolStatus::Completed
                            };
                            if !result_text.is_empty() {
                                let truncated = truncate_output(&result_text, 16_384);
                                if let Some(AgentToolContent::Terminal { output, .. }) =
                                    tc.content.iter_mut().find(|c| matches!(c, AgentToolContent::Terminal { .. }))
                                {
                                    *output = truncated;
                                } else if !matches!(tc.kind, AgentToolKind::Edit | AgentToolKind::Write) {
                                    if let Some(AgentToolContent::Text { text }) =
                                        tc.content.iter_mut().find(|c| matches!(c, AgentToolContent::Text { .. }))
                                    {
                                        // tool_result 是完整快照，重发时替换已有输出，不能再追加一块。
                                        *text = truncated;
                                    } else {
                                        tc.content.push(AgentToolContent::Text { text: truncated });
                                    }
                                }
                            }
                            updated = Some(AgentEntry::ToolCall(tc.clone()));
                            break;
                        }
                    }
                    if let Some(entry) = updated {
                        out.messages.push(ServerMessage::AgentEntry { entry });
                    }
                }
            }
            "result" => {
                self.pending_turns = self.pending_turns.saturating_sub(1);
                self.transport_uncertain = false;
                let is_error = v["is_error"].as_bool().unwrap_or(false);
                if is_error {
                    let err_text = v["errors"]
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .or_else(|| v["result"].as_str())
                        .unwrap_or("执行出错")
                        .to_string();
                    let notice_id = self.alloc_id("notice");
                    let entry = AgentEntry::Notice {
                        id: notice_id,
                        level: AgentNoticeLevel::Error,
                        text: err_text.clone(),
                    };
                    self.state.entries.push(entry.clone());
                    out.messages.push(ServerMessage::AgentEntry { entry });
                    self.state.status = AgentTurnStatus::Error;
                    self.last_finished_state = Some((ProgramState::Error, Some(err_text)));
                } else {
                    self.state.status = AgentTurnStatus::Idle;
                    let summary = v["result"]
                        .as_str()
                        .map(|s| s.lines().next().unwrap_or_default().chars().take(60).collect::<String>())
                        .filter(|s| !s.is_empty());
                    self.last_finished_state = Some((ProgramState::Done, summary));
                }
                out.status_changed = true;
                out.messages.push(self.meta_message(false));
            }
            _ => {}
        }
    }

    fn request_codex_models(&mut self, cursor: Option<&str>, out: &mut AgentDriverOutput) {
        let id = self.alloc_id("models");
        let msg = json!({
            "jsonrpc": "2.0", "id": id, "method": "model/list",
            "params": { "cursor": cursor, "limit": 100, "includeHidden": false }
        });
        out.to_pty.push(format!("{msg}\n"));
    }

    fn receive_codex_models(&mut self, result: &Value, out: &mut AgentDriverOutput) {
        if let Some(models) = result["data"].as_array() {
            for model in models {
                if model["hidden"].as_bool() == Some(true) { continue; }
                let Some(id) = model["model"].as_str().filter(|s| !s.is_empty()) else { continue };
                let option = AgentModelOption {
                    id: id.to_string(),
                    label: model["displayName"].as_str().unwrap_or(id).to_string(),
                    resolved_model: None,
                    reasoning_efforts: model["supportedReasoningEfforts"].as_array().map(|efforts| {
                        efforts.iter().filter_map(|e| e["reasoningEffort"].as_str()).map(str::to_string).collect()
                    }).unwrap_or_default(),
                    default_reasoning_effort: model["defaultReasoningEffort"].as_str().map(str::to_string),
                };
                if let Some(existing) = self.state.model_options.iter_mut().find(|m| m.id == id) {
                    *existing = option;
                } else {
                    self.state.model_options.push(option);
                }
            }
        }
        self.state.models = self.state.model_options.iter().map(|m| m.id.clone()).collect();
        out.messages.push(self.meta_message(true));
        if let Some(cursor) = result["nextCursor"].as_str().filter(|s| !s.is_empty())
            && self.codex_model_cursors.len() < 32
            && self.codex_model_cursors.insert(cursor.to_string())
        {
            self.request_codex_models(Some(cursor), out);
        }
    }

    // ---------------- Codex `app-server` ----------------

    fn handle_codex_json(&mut self, v: &Value, out: &mut AgentDriverOutput) {
        // 1. JSON-RPC 服务端请求（同时带 id 和 method，如权限审批）
        if let (Some(method), Some(rpc_id)) = (v["method"].as_str(), v.get("id")) {
            let params = &v["params"];
            match method {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    let item_id = params["itemId"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| self.alloc_id("codex-item"));
                    self.codex_permissions.insert(item_id.clone(), rpc_id.clone());
                    let reason = params["reason"].as_str().map(str::to_string);
                    let command = params["command"].as_str().unwrap_or("command");
                    let cwd = params["cwd"].as_str().map(str::to_string);

                    let mut tc = self
                        .state
                        .entries
                        .iter()
                        .find_map(|e| match e {
                            AgentEntry::ToolCall(existing) if existing.id == item_id => {
                                Some(existing.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| AgentToolCall {
                            id: item_id.clone(),
                            title: command.to_string(),
                            description: None,
                            file_path: None,
                            kind: if method.contains("fileChange") {
                                AgentToolKind::Edit
                            } else {
                                AgentToolKind::Execute
                            },
                            status: AgentToolStatus::Pending,
                            content: if method.contains("commandExecution") {
                                vec![AgentToolContent::Terminal {
                                    command: command.to_string(),
                                    cwd,
                                    output: String::new(),
                                    exit_code: None,
                                }]
                            } else {
                                Vec::new()
                            },
                            permission_pending: true,
                            permission_reason: reason.clone(),
                        });
                    tc.permission_pending = true;
                    tc.permission_reason = reason;
                    tc.status = AgentToolStatus::Pending;

                    let entry = AgentEntry::ToolCall(tc);
                    self.state.upsert_entry(entry.clone());
                    self.state.status = AgentTurnStatus::AwaitingPermission;
                    out.status_changed = true;
                    out.messages.push(ServerMessage::AgentEntry { entry });
                    out.messages.push(self.meta_message(false));
                }
                _ => {
                    let error = format!("Falcon 图形界面暂不支持交互请求 {method}，已明确拒绝该请求。");
                    let response = json!({"jsonrpc":"2.0","id":rpc_id,
                        "error":{"code":-32601,"message":error}});
                    out.to_pty.push(format!("{response}\n"));
                    self.notice(&error, out);
                }
            }
            return;
        }

        // 2. JSON-RPC 响应（无 method，有 id + result/error）
        if v.get("method").is_none() && v.get("id").is_some() {
            if let Some(error) = v.get("error") {
                self.notice(error["message"].as_str().unwrap_or("Codex 请求失败"), out);
                if v["id"].as_str().is_some_and(|id| id.starts_with("turn-")) || v["id"].as_i64().is_some() {
                    self.state.status = AgentTurnStatus::Error;
                    out.status_changed = true;
                    out.messages.push(self.meta_message(false));
                }
                return;
            }
            if v["id"].as_str().is_some_and(|id| id.starts_with("models-")) {
                self.receive_codex_models(&v["result"], out);
                return;
            }
            if v["id"].as_i64() == Some(1) && v.get("result").is_some() {
                out.to_pty.push("{\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}\n".to_string());
                let start_thread = json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "thread/start",
                    "params": {
                        "cwd": self.state.cwd,
                    }
                });
                out.to_pty.push(format!("{start_thread}\n"));
                self.request_codex_models(None, out);
            } else if v["id"].as_i64() == Some(2) {
                let res = &v["result"];
                if let Some(tid) = res["thread"]["id"].as_str() {
                    self.codex_thread_id = Some(tid.to_string());
                }
                if let Some(m) = res["model"].as_str().filter(|s| !s.is_empty()) {
                    self.state.model = Some(m.to_string());
                }
                self.state.reasoning_effort = res["reasoningEffort"].as_str().map(str::to_string);
                if let Some(cwd) = res["cwd"].as_str().filter(|s| !s.is_empty()) {
                    self.state.cwd = Some(cwd.to_string());
                }
                self.initialized = true;
                out.messages.push(self.meta_message(false));
                self.flush_pending_prompts(out);
            }
            return;
        }

        // 3. JSON-RPC 通知（有 method，无 id）
        let Some(method) = v["method"].as_str() else { return };
        let params = &v["params"];
        match method {
            "turn/started" => {
                if let Some(tid) = params["turn"]["id"].as_str() {
                    self.codex_turn_id = Some(tid.to_string());
                }
                self.state.status = AgentTurnStatus::Running;
                out.status_changed = true;
                out.messages.push(self.meta_message(false));
            }
            "item/started" | "item/completed" => {
                let item = &params["item"];
                let Some(item_id) = item["id"].as_str() else { return };
                let completed = method == "item/completed";
                if completed {
                    self.codex_completed_items.insert(item_id.to_string());
                } else if self.codex_completed_items.contains(item_id)
                    || self.state.entries.iter().any(|entry| entry.id() == Some(item_id))
                {
                    // started 的重发不能清空已经累积的文本，也不能把完成条目重新变成运行中。
                    return;
                }
                match item["type"].as_str() {
                    Some("agentMessage") => {
                        let text = item["text"].as_str().unwrap_or_default().to_string();
                        if !text.is_empty() || !completed {
                            let entry = AgentEntry::Message {
                                id: item_id.to_string(),
                                text,
                            };
                            self.state.upsert_entry(entry.clone());
                            out.messages.push(ServerMessage::AgentEntry { entry });
                        }
                    }
                    Some("reasoning") => {
                        let summary = string_list(&item["summary"]).join("\n");
                        let summary = if summary.is_empty() {
                            string_list(&item["content"]).join("\n")
                        } else {
                            self.codex_reasoning_summaries.insert(item_id.to_string());
                            summary
                        };
                        if !summary.is_empty() || !completed {
                            let entry = AgentEntry::Thought {
                                id: item_id.to_string(),
                                text: summary,
                            };
                            self.state.upsert_entry(entry.clone());
                            out.messages.push(ServerMessage::AgentEntry { entry });
                        }
                    }
                    Some("commandExecution") => {
                        let command = item["command"].as_str().unwrap_or("command").to_string();
                        let cwd = item["cwd"].as_str().map(str::to_string);
                        let output = item["aggregatedOutput"]
                            .as_str()
                            .map(|s| truncate_output(s, 16_384))
                            .unwrap_or_default();
                        let exit_code = item["exitCode"].as_i64().map(|c| c as i32);
                        let status = match item["status"].as_str() {
                            Some("completed") => AgentToolStatus::Completed,
                            Some("failed") | Some("declined") => AgentToolStatus::Failed,
                            _ => AgentToolStatus::InProgress,
                        };
                        let tc = AgentToolCall {
                            id: item_id.to_string(),
                            title: command.clone(),
                            description: None,
                            file_path: None,
                            kind: AgentToolKind::Execute,
                            status,
                            content: vec![AgentToolContent::Terminal {
                                command,
                                cwd,
                                output,
                                exit_code,
                            }],
                            permission_pending: false,
                            permission_reason: None,
                        };
                        let entry = AgentEntry::ToolCall(tc);
                        self.state.upsert_entry(entry.clone());
                        out.messages.push(ServerMessage::AgentEntry { entry });
                    }
                    Some("fileChange") => {
                        let status = match item["status"].as_str() {
                            Some("completed") => AgentToolStatus::Completed,
                            Some("failed") | Some("declined") => AgentToolStatus::Failed,
                            _ => AgentToolStatus::InProgress,
                        };
                        let mut diffs = Vec::new();
                        let mut first_path = None;
                        if let Some(changes) = item["changes"].as_array() {
                            for ch in changes {
                                let path = ch["path"].as_str().unwrap_or("file").to_string();
                                if first_path.is_none() {
                                    first_path = Some(path.clone());
                                }
                                let diff = ch["diff"].as_str().map(str::to_string);
                                diffs.push(AgentToolContent::Diff {
                                    path,
                                    old_text: None,
                                    new_text: None,
                                    unified_diff: diff,
                                });
                            }
                        }
                        let title = first_path
                            .as_deref()
                            .map(|p| short_path(p, self.state.cwd.as_deref()))
                            .unwrap_or_else(|| "文件修改".into());
                        let tc = AgentToolCall {
                            id: item_id.to_string(),
                            title,
                            description: None,
                            file_path: first_path,
                            kind: AgentToolKind::Edit,
                            status,
                            content: diffs,
                            permission_pending: false,
                            permission_reason: None,
                        };
                        let entry = AgentEntry::ToolCall(tc);
                        self.state.upsert_entry(entry.clone());
                        out.messages.push(ServerMessage::AgentEntry { entry });
                    }
                    Some("webSearch") => {
                        let query = item["query"].as_str().unwrap_or("web search").to_string();
                        let tc = AgentToolCall {
                            id: item_id.to_string(),
                            title: query,
                            description: None,
                            file_path: None,
                            kind: AgentToolKind::Fetch,
                            status: if completed {
                                AgentToolStatus::Completed
                            } else {
                                AgentToolStatus::InProgress
                            },
                            content: Vec::new(),
                            permission_pending: false,
                            permission_reason: None,
                        };
                        let entry = AgentEntry::ToolCall(tc);
                        self.state.upsert_entry(entry.clone());
                        out.messages.push(ServerMessage::AgentEntry { entry });
                    }
                    _ => {}
                }
            }
            "item/agentMessage/delta" => {
                if let (Some(id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str())
                    && !delta.is_empty()
                    && !self.codex_completed_items.contains(id)
                {
                    self.state.apply_delta(id, AgentDeltaKind::Message, delta);
                    out.messages.push(ServerMessage::AgentDelta {
                        id: id.to_string(),
                        kind: AgentDeltaKind::Message,
                        delta: delta.to_string(),
                    });
                }
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                if let (Some(id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str())
                    && !delta.is_empty()
                    && !self.codex_completed_items.contains(id)
                {
                    // summary 与原始 reasoning content 是两个独立文本流，不能拼成一段。
                    // 先显示原文；一旦有摘要便替换为摘要，后续原文不再追加到摘要后面。
                    let is_summary = method == "item/reasoning/summaryTextDelta";
                    if is_summary && self.codex_reasoning_summaries.insert(id.to_string()) {
                        let entry = AgentEntry::Thought { id: id.to_string(), text: delta.to_string() };
                        self.state.upsert_entry(entry.clone());
                        out.messages.push(ServerMessage::AgentEntry { entry });
                    } else if is_summary || !self.codex_reasoning_summaries.contains(id) {
                        self.state.apply_delta(id, AgentDeltaKind::Thought, delta);
                        out.messages.push(ServerMessage::AgentDelta {
                            id: id.to_string(),
                            kind: AgentDeltaKind::Thought,
                            delta: delta.to_string(),
                        });
                    }
                }
            }
            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
                if let (Some(id), Some(delta)) = (params["itemId"].as_str(), params["delta"].as_str())
                    && !delta.is_empty()
                    && !self.codex_completed_items.contains(id)
                {
                    self.state.apply_delta(id, AgentDeltaKind::ToolOutput, delta);
                    out.messages.push(ServerMessage::AgentDelta {
                        id: id.to_string(),
                        kind: AgentDeltaKind::ToolOutput,
                        delta: delta.to_string(),
                    });
                }
            }
            "turn/completed" => {
                let turn_status = params["turn"]["status"].as_str();
                if turn_status == Some("failed") {
                    let err_msg = params["turn"]["error"]["message"]
                        .as_str()
                        .unwrap_or("Codex 执行失败")
                        .to_string();
                    let notice_id = params["turn"]["id"].as_str()
                        .map(|id| format!("codex-turn-error-{id}"))
                        .unwrap_or_else(|| self.alloc_id("notice"));
                    let entry = AgentEntry::Notice {
                        id: notice_id,
                        level: AgentNoticeLevel::Error,
                        text: err_msg.clone(),
                    };
                    self.state.upsert_entry(entry.clone());
                    out.messages.push(ServerMessage::AgentEntry { entry });
                    self.state.status = AgentTurnStatus::Error;
                    self.last_finished_state = Some((ProgramState::Error, Some(err_msg)));
                } else {
                    self.state.status = AgentTurnStatus::Idle;
                    self.last_finished_state = Some((ProgramState::Done, Some("本轮完成".into())));
                }
                out.status_changed = true;
                out.messages.push(self.meta_message(false));
            }
            _ => {}
        }
    }
}

fn string_list(value: &Value) -> Vec<String> {
    value.as_array().map(|values| values.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

fn extract_json_object(line: &str) -> Option<&str> {
    let start = line.find('{')?;
    let end = line.rfind('}')?;
    (end >= start).then_some(&line[start..=end])
}

fn short_path(path: &str, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        let prefix = cwd.trim_end_matches('/');
        if let Some(rest) = path.strip_prefix(prefix) {
            let trimmed = rest.trim_start_matches('/');
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    path.to_string()
}

fn truncate_output(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…（输出已截断）", &s[..end])
}

fn extract_claude_tool_result(item: &Value, tool_use_result: &Value) -> String {
    if let Some(s) = item["content"].as_str() {
        return s.to_string();
    }
    if let Some(arr) = item["content"].as_array() {
        let joined: Vec<&str> = arr
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect();
        if !joined.is_empty() {
            return joined.join("\n");
        }
    }
    if let Some(stdout) = tool_use_result["stdout"].as_str() {
        let stderr = tool_use_result["stderr"].as_str().unwrap_or_default();
        if stderr.is_empty() {
            return stdout.to_string();
        }
        if stdout.is_empty() {
            return stderr.to_string();
        }
        return format!("{stdout}\n{stderr}");
    }
    String::new()
}

/// transcript 只复用工具卡片的纯转换；不进入驱动器的双向控制流。历史记录不能证明
/// 工具此刻仍在执行，且文件镜像不能沿用 stream 视图的内容预览截断。
pub(super) fn claude_transcript_tool_call(id: &str, name: &str, input: &Value, cwd: Option<&str>) -> AgentToolCall {
    let mut call = claude_tool_call(id, name, input, cwd);
    call.status = AgentToolStatus::Unknown;
    if name == "Write" {
        for content in &mut call.content {
            if let AgentToolContent::Diff { new_text, .. } = content {
                *new_text = input["content"].as_str().map(str::to_string);
            }
        }
    }
    if !matches!(name, "Bash" | "Edit" | "MultiEdit" | "Write" | "Read" | "Grep" | "Glob" | "WebFetch" | "WebSearch") {
        call.description = input["description"].as_str().or_else(|| input["prompt"].as_str()).map(str::to_string);
        if !input.is_null() {
            call.content.push(AgentToolContent::Text { text: input.to_string() });
        }
    }
    call
}

fn claude_tool_call(id: &str, name: &str, input: &Value, cwd: Option<&str>) -> AgentToolCall {
    match name {
        "Bash" => {
            let cmd = input["command"].as_str().unwrap_or(name).to_string();
            let desc = input["description"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            AgentToolCall {
                id: id.to_string(),
                title: cmd.clone(),
                description: desc,
                file_path: None,
                kind: AgentToolKind::Execute,
                status: AgentToolStatus::InProgress,
                content: vec![AgentToolContent::Terminal {
                    command: cmd,
                    cwd: cwd.map(str::to_string),
                    output: String::new(),
                    exit_code: None,
                }],
                permission_pending: false,
                permission_reason: None,
            }
        }
        "Edit" | "MultiEdit" => {
            let file_path = input["file_path"]
                .as_str()
                .or_else(|| input["path"].as_str())
                .unwrap_or(name)
                .to_string();
            let title = short_path(&file_path, cwd);
            let mut content = Vec::new();
            if let (Some(old_s), Some(new_s)) =
                (input["old_string"].as_str(), input["new_string"].as_str())
            {
                content.push(AgentToolContent::Diff {
                    path: title.clone(),
                    old_text: Some(old_s.to_string()),
                    new_text: Some(new_s.to_string()),
                    unified_diff: None,
                });
            } else if let Some(edits) = input["edits"].as_array() {
                for e in edits {
                    if let (Some(old_s), Some(new_s)) =
                        (e["old_string"].as_str(), e["new_string"].as_str())
                    {
                        content.push(AgentToolContent::Diff {
                            path: title.clone(),
                            old_text: Some(old_s.to_string()),
                            new_text: Some(new_s.to_string()),
                            unified_diff: None,
                        });
                    }
                }
            }
            AgentToolCall {
                id: id.to_string(),
                title,
                description: None,
                file_path: Some(file_path),
                kind: AgentToolKind::Edit,
                status: AgentToolStatus::InProgress,
                content,
                permission_pending: false,
                permission_reason: None,
            }
        }
        "Write" => {
            let file_path = input["file_path"]
                .as_str()
                .or_else(|| input["path"].as_str())
                .unwrap_or(name)
                .to_string();
            let title = short_path(&file_path, cwd);
            let new_text = input["content"].as_str().map(|s| truncate_output(s, 16_384));
            AgentToolCall {
                id: id.to_string(),
                title: title.clone(),
                description: None,
                file_path: Some(file_path),
                kind: AgentToolKind::Write,
                status: AgentToolStatus::InProgress,
                content: vec![AgentToolContent::Diff {
                    path: title,
                    old_text: Some(String::new()),
                    new_text,
                    unified_diff: None,
                }],
                permission_pending: false,
                permission_reason: None,
            }
        }
        "Read" => {
            let file_path = input["file_path"]
                .as_str()
                .or_else(|| input["path"].as_str())
                .unwrap_or(name)
                .to_string();
            let title = short_path(&file_path, cwd);
            AgentToolCall {
                id: id.to_string(),
                title,
                description: None,
                file_path: Some(file_path),
                kind: AgentToolKind::Read,
                status: AgentToolStatus::InProgress,
                content: Vec::new(),
                permission_pending: false,
                permission_reason: None,
            }
        }
        "Grep" | "Glob" => {
            let pattern = input["pattern"].as_str().unwrap_or(name).to_string();
            let path = input["path"].as_str().map(|p| short_path(p, cwd));
            AgentToolCall {
                id: id.to_string(),
                title: format!("{name}: {pattern}"),
                description: path,
                file_path: None,
                kind: AgentToolKind::Search,
                status: AgentToolStatus::InProgress,
                content: Vec::new(),
                permission_pending: false,
                permission_reason: None,
            }
        }
        "WebFetch" | "WebSearch" => {
            let target = input["url"]
                .as_str()
                .or_else(|| input["query"].as_str())
                .unwrap_or(name)
                .to_string();
            AgentToolCall {
                id: id.to_string(),
                title: target,
                description: None,
                file_path: None,
                kind: AgentToolKind::Fetch,
                status: AgentToolStatus::InProgress,
                content: Vec::new(),
                permission_pending: false,
                permission_reason: None,
            }
        }
        other => {
            let kind = if other.starts_with("mcp__") {
                AgentToolKind::Mcp
            } else {
                AgentToolKind::Other
            };
            let desc = input["description"]
                .as_str()
                .or_else(|| input["prompt"].as_str())
                .map(|s| s.lines().next().unwrap_or_default().chars().take(80).collect());
            AgentToolCall {
                id: id.to_string(),
                title: other.to_string(),
                description: desc,
                file_path: None,
                kind,
                status: AgentToolStatus::InProgress,
                content: Vec::new(),
                permission_pending: false,
                permission_reason: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(drv: &mut AgentDriver, viewer: &mut AgentViewState, event: Value) -> AgentDriverOutput {
        let out = drv.on_data(&format!("{event}\n"));
        for message in &out.messages {
            match message {
                ServerMessage::AgentEntry { entry } => viewer.upsert_entry(entry.clone()),
                ServerMessage::AgentDelta { id, kind, delta } => viewer.apply_delta(id, *kind, delta),
                _ => {}
            }
        }
        assert_eq!(viewer.entries, drv.state.entries, "增量客户端与重新连接取得的快照必须一致");
        out
    }

    #[test]
    fn claude_single_block_assistant_frames_replace_their_streamed_blocks() {
        let mut drv = ready_claude();
        let mut viewer = drv.state.clone();
        feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
            "type":"message_start","message":{"id":"api-1"}
        }}));
        for (index, kind, field, delta_kind, text) in [
            (0, "thinking", "thinking", "thinking_delta", "先检查实现"),
            (1, "text", "text", "text_delta", "检查完成"),
        ] {
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"content_block_start","index":index,"content_block":{"type":kind,field:""}
            }}));
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"content_block_delta","index":index,"delta":{"type":delta_kind,field:text}
            }}));
            // 本机 2.1.322 在每个块结束时发送单块 assistant，两个帧的 content 下标均为 0。
            // assistant 先于对应的 content_block_stop 被转发。
            let complete = json!({"type":"assistant","uuid":format!("frame-{index}"),"message":{
                "id":"api-1","content":[{"type":kind,field:text}]
            }});
            feed(&mut drv, &mut viewer, complete.clone());
            assert!(feed(&mut drv, &mut viewer, complete).messages.is_empty());
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"content_block_stop","index":index
            }}));
            assert!(feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"content_block_delta","index":index,"delta":{"type":delta_kind,field:text}
            }})).messages.is_empty());
        }
        feed(&mut drv, &mut viewer, json!({"type":"result","uuid":"result-1","is_error":false,"result":"检查完成"}));
        assert_eq!(drv.state.entries.len(), 2, "result 只能更新完成状态，不能再添加一次正文");
        assert!(matches!(&drv.state.entries[0], AgentEntry::Thought { text, .. } if text == "先检查实现"));
        assert!(matches!(&drv.state.entries[1], AgentEntry::Message { text, .. } if text == "检查完成"));
    }

    #[test]
    fn claude_joined_text_runs_share_one_streamed_entry() {
        let mut drv = ready_claude();
        let mut viewer = drv.state.clone();
        feed(&mut drv, &mut viewer, json!({"type":"stream_event","text_runs_joined":true,"event":{
            "type":"message_start","message":{"id":"joined"}
        }}));
        for (index, text) in [(0, "重复"), (1, "后半段")] {
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"content_block_start","index":index,"content_block":{"type":"text","text":""}
            }}));
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}
            }}));
            if index == 0 {
                // 相同文本的连续增量本身完全合法，不能按文本去重。
                feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                    "type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}
                }}));
            }
        }
        feed(&mut drv, &mut viewer, json!({"type":"assistant","uuid":"joined-frame","message":{
            "id":"joined","content":[{"type":"text","text":"重复重复后半段"}]
        }}));
        assert_eq!(drv.state.entries.len(), 1);
        assert!(matches!(&drv.state.entries[0], AgentEntry::Message { text, .. } if text == "重复重复后半段"));
    }

    #[test]
    fn claude_equal_text_blocks_survive_single_block_and_whole_message_snapshots() {
        for single_frames in [false, true] {
            let mut drv = ready_claude();
            let mut viewer = drv.state.clone();
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                "type":"message_start","message":{"id":"equal-blocks"}
            }}));
            for index in 0..2 {
                feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                    "type":"content_block_start","index":index,"content_block":{"type":"text","text":""}
                }}));
                feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                    "type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":"相同正文"}
                }}));
                if single_frames {
                    feed(&mut drv, &mut viewer, json!({"type":"assistant","uuid":format!("equal-{index}"),"message":{
                        "id":"equal-blocks","content":[{"type":"text","text":"相同正文"}]
                    }}));
                }
            }
            if !single_frames {
                feed(&mut drv, &mut viewer, json!({"type":"assistant","uuid":"equal-full","message":{
                    "id":"equal-blocks","content":[{"type":"text","text":"相同正文"},{"type":"text","text":"相同正文"}]
                }}));
            }
            assert_eq!(drv.state.entries.len(), 2);
            assert_ne!(drv.state.entries[0].id(), drv.state.entries[1].id());
            assert!(drv.state.entries.iter().all(|entry| matches!(entry, AgentEntry::Message { text, .. } if text == "相同正文")));
        }
    }

    #[test]
    fn claude_message_ids_and_parent_streams_keep_identical_replies_distinct() {
        let mut drv = ready_claude();
        let mut viewer = drv.state.clone();
        // 一次工具执行前后会产生不同 message_start；子 agent 的消息还能穿插其中。
        for (message_id, parent) in [("main-1", None), ("child-1", Some("tool-1"))] {
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","parent_tool_use_id":parent,"event":{
                "type":"message_start","message":{"id":message_id}
            }}));
        }
        for (message_id, parent) in [("main-1", None), ("child-1", Some("tool-1")), ("main-2", None)] {
            if message_id == "main-2" {
                feed(&mut drv, &mut viewer, json!({"type":"stream_event","event":{
                    "type":"message_start","message":{"id":message_id}
                }}));
            }
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","parent_tool_use_id":parent,"event":{
                "type":"content_block_start","index":0,"content_block":{"type":"text","text":""}
            }}));
            feed(&mut drv, &mut viewer, json!({"type":"stream_event","parent_tool_use_id":parent,"event":{
                "type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"相同回答"}
            }}));
            feed(&mut drv, &mut viewer, json!({"type":"assistant","parent_tool_use_id":parent,
                "uuid":format!("frame-{message_id}"),"message":{
                    "id":message_id,"content":[{"type":"text","text":"相同回答"}]
                }
            }));
        }
        drv.send_prompt("下一轮");
        viewer = drv.state.clone();
        feed(&mut drv, &mut viewer, json!({"type":"assistant","uuid":"frame-main-3","message":{
            "id":"main-3","content":[{"type":"text","text":"相同回答"}]
        }}));
        let replies: Vec<_> = drv.state.entries.iter().filter_map(|entry| match entry {
            AgentEntry::Message { id, text } => { assert_eq!(text, "相同回答"); Some(id) }
            _ => None,
        }).collect();
        assert_eq!(replies.len(), 4);
        assert_eq!(replies.into_iter().collect::<HashSet<_>>().len(), 4);
    }

    #[test]
    fn claude_unstreamed_blocks_and_replayed_tool_results_are_snapshots() {
        let mut drv = ready_claude();
        let mut viewer = drv.state.clone();
        let assistant = json!({"type":"assistant","uuid":"full-frame","message":{
            "id":"unstreamed","content":[
                {"type":"thinking","thinking":"思考"},
                {"type":"text","text":"正文"},
                {"type":"tool_use","id":"read-1","name":"Read","input":{"file_path":"a.rs"}}
            ]
        }});
        feed(&mut drv, &mut viewer, assistant.clone());
        let result = json!({"type":"user","message":{"content":[{
            "type":"tool_result","tool_use_id":"read-1","content":"文件内容"
        }]}});
        feed(&mut drv, &mut viewer, result.clone());
        feed(&mut drv, &mut viewer, result);
        assert!(feed(&mut drv, &mut viewer, assistant).messages.is_empty());
        assert_eq!(drv.state.entries.len(), 3);
        assert!(matches!(&drv.state.entries[0], AgentEntry::Thought { text, .. } if text == "思考"));
        assert!(matches!(&drv.state.entries[1], AgentEntry::Message { text, .. } if text == "正文"));
        let AgentEntry::ToolCall(tool) = &drv.state.entries[2] else { panic!("missing Read") };
        assert_eq!(tool.status, AgentToolStatus::Completed);
        assert_eq!(tool.content, vec![AgentToolContent::Text { text: "文件内容".into() }]);
    }

    #[test]
    fn codex_completed_items_replace_deltas_and_ignore_late_replays() {
        let mut drv = AgentDriver::new(SessionAgent::Codex, None);
        drv.on_data("__FALCON_AGENT_START__:codex\n");
        let mut viewer = drv.state.clone();
        for item_id in ["answer-1", "answer-2"] {
            let started = json!({"method":"item/started","params":{"item":{
                "type":"agentMessage","id":item_id,"text":""
            }}});
            let delta = json!({"method":"item/agentMessage/delta","params":{"itemId":item_id,"delta":"重复"}});
            feed(&mut drv, &mut viewer, started.clone());
            feed(&mut drv, &mut viewer, delta.clone());
            assert!(feed(&mut drv, &mut viewer, started.clone()).messages.is_empty());
            feed(&mut drv, &mut viewer, delta.clone());
            assert!(matches!(drv.state.entries.last(), Some(AgentEntry::Message { text, .. }) if text == "重复重复"));
            let completed = json!({"method":"item/completed","params":{"item":{
                "type":"agentMessage","id":item_id,"text":"重复重复"
            }}});
            feed(&mut drv, &mut viewer, completed.clone());
            feed(&mut drv, &mut viewer, completed);
            assert!(feed(&mut drv, &mut viewer, started).messages.is_empty());
            assert!(feed(&mut drv, &mut viewer, delta).messages.is_empty());
            feed(&mut drv, &mut viewer, json!({"method":"turn/completed","params":{"turn":{
                "id":format!("turn-{item_id}"),"status":"completed"
            }}}));
        }
        assert_eq!(drv.state.entries.len(), 2, "不同 item 即使文本相同也必须保留");
        for entry in &drv.state.entries {
            assert!(matches!(entry, AgentEntry::Message { text, .. } if text == "重复重复"));
        }
    }

    #[test]
    fn codex_reasoning_summary_replaces_raw_content_instead_of_appending() {
        let mut drv = AgentDriver::new(SessionAgent::Codex, None);
        drv.on_data("__FALCON_AGENT_START__:codex\n");
        let mut viewer = drv.state.clone();
        for summary_first in [false, true] {
            let id = format!("reasoning-{summary_first}");
            feed(&mut drv, &mut viewer, json!({"method":"item/started","params":{"item":{
                "type":"reasoning","id":id,"summary":[],"content":[]
            }}}));
            let raw = json!({"method":"item/reasoning/textDelta","params":{"itemId":id,"contentIndex":0,"delta":"同一思考"}});
            let summary = json!({"method":"item/reasoning/summaryTextDelta","params":{"itemId":id,"summaryIndex":0,"delta":"同一思考"}});
            let events = if summary_first { [summary, raw] } else { [raw, summary] };
            for event in events { feed(&mut drv, &mut viewer, event); }
            assert!(matches!(drv.state.entries.last(), Some(AgentEntry::Thought { text, .. }) if text == "同一思考"));
            let completed = json!({"method":"item/completed","params":{"item":{
                "type":"reasoning","id":id,"summary":["同一思考"],"content":["同一思考"]
            }}});
            feed(&mut drv, &mut viewer, completed.clone());
            feed(&mut drv, &mut viewer, completed);
            let out = feed(&mut drv, &mut viewer, json!({"method":"item/reasoning/summaryTextDelta","params":{
                "itemId":id,"summaryIndex":0,"delta":"同一思考"
            }}));
            assert!(out.messages.is_empty());
        }
        assert_eq!(drv.state.entries.len(), 2);
        assert!(drv.state.entries.iter().all(|entry| matches!(entry, AgentEntry::Thought { text, .. } if text == "同一思考")));
    }

    #[test]
    fn repeated_error_results_are_deduplicated_by_event_or_turn_id() {
        for agent in [SessionAgent::Claude, SessionAgent::Codex] {
            let mut drv = AgentDriver::new(agent, None);
            drv.on_data(&format!("__FALCON_AGENT_START__:{}\n", agent.as_str()));
            let mut viewer = drv.state.clone();
            for index in 0..2 {
                let event = if agent == SessionAgent::Claude {
                    json!({"type":"result","uuid":format!("failed-{index}"),"is_error":true,"errors":["相同错误"]})
                } else {
                    json!({"method":"turn/completed","params":{"turn":{
                        "id":format!("failed-{index}"),"status":"failed","error":{"message":"相同错误"}
                    }}})
                };
                feed(&mut drv, &mut viewer, event.clone());
                feed(&mut drv, &mut viewer, event);
            }
            assert_eq!(drv.state.entries.len(), 2, "同一事件重发只留一条，不同轮次相同报错各保留一条");
        }
    }

    #[test]
    fn codex_command_completion_replaces_streamed_output() {
        let mut drv = AgentDriver::new(SessionAgent::Codex, None);
        drv.on_data("__FALCON_AGENT_START__:codex\n");
        let mut viewer = drv.state.clone();
        feed(&mut drv, &mut viewer, json!({"method":"item/started","params":{"item":{
            "type":"commandExecution","id":"command-1","command":"pwd","status":"inProgress"
        }}}));
        let delta = json!({"method":"item/commandExecution/outputDelta","params":{"itemId":"command-1","delta":"/work\n"}});
        feed(&mut drv, &mut viewer, delta.clone());
        let complete = json!({"method":"item/completed","params":{"item":{
            "type":"commandExecution","id":"command-1","command":"pwd","status":"completed",
            "aggregatedOutput":"/work\n","exitCode":0
        }}});
        feed(&mut drv, &mut viewer, complete.clone());
        feed(&mut drv, &mut viewer, complete);
        assert!(feed(&mut drv, &mut viewer, delta).messages.is_empty());
        assert_eq!(drv.state.entries.len(), 1);
        let AgentEntry::ToolCall(tool) = &drv.state.entries[0] else { panic!("missing command") };
        assert_eq!(tool.status, AgentToolStatus::Completed);
        assert!(matches!(&tool.content[0], AgentToolContent::Terminal { output, .. } if output == "/work\n"));
    }

    /// 旧客户端也不能通过切换样式打断正在流式输出的轮次。
    #[test]
    fn view_switch_keeps_the_running_turn_and_partial_message() {
        for agent in [SessionAgent::Claude, SessionAgent::Codex] {
            let mut drv = AgentDriver::new(agent, None);
            drv.state.status = AgentTurnStatus::Running;
            drv.state.apply_delta("answer", AgentDeltaKind::Message, "前半段");
            let before = drv.state.clone();
            for _ in 0..4 {
                let out = drv.switch_to_terminal();
                assert!(out.to_pty.is_empty(), "切换不能向 CLI 发送退出或中断命令");
                assert!(!out.status_changed);
                assert_eq!(drv.state, before);
                assert!(drv.should_intercept_pty());
            }
            drv.state.apply_delta("answer", AgentDeltaKind::Message, "后半段");
            assert!(matches!(&drv.state.entries[0], AgentEntry::Message { text, .. } if text == "前半段后半段"));
        }
    }

    #[test]
    fn view_switch_keeps_permission_request_answerable() {
        let mut drv = AgentDriver::new(SessionAgent::Claude, None);
        drv.on_data("__FALCON_AGENT_START__:claude\n");
        drv.on_data(concat!(
            r#"{"type":"control_request","request_id":"request-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-1","input":{"command":"pwd"}}}"#,
            "\n"
        ));
        let out = drv.switch_to_terminal();
        assert!(out.to_pty.is_empty());
        assert_eq!(drv.state.status, AgentTurnStatus::AwaitingPermission);
        assert_eq!(drv.program_status_records()[0].state, ProgramState::Blocked);
        let out = drv.answer_permission("tool-1", AgentPermissionDecision::Allow, None);
        let response: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(response["response"]["request_id"], "request-1");
        assert_eq!(response["response"]["response"]["behavior"], "allow");
    }

    #[test]
    fn claude_stream_lifecycle_and_permission() {
        let mut drv = AgentDriver::new(SessionAgent::Claude, Some("/work/repo".into()));
        let out = drv.on_data("__FALCON_AGENT_START__:claude\n");
        assert_eq!(out.to_pty.len(), 1);
        assert!(out.to_pty[0].contains("\"subtype\":\"initialize\""));

        let out = drv.on_data(
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-4-6","cwd":"/work/repo","permissionMode":"default"}"#,
        );
        assert!(out.messages.is_empty());
        let out = drv.on_data("\n");
        assert_eq!(drv.state.model.as_deref(), Some("claude-sonnet-4-6"));
        assert!(!out.messages.is_empty());

        // 用户提问
        let out = drv.send_prompt("Run cargo check");
        assert_eq!(out.title.as_deref(), Some("Run cargo check"));
        assert_eq!(out.to_pty.len(), 1);
        assert!(out.to_pty[0].contains("Run cargo check"));
        assert_eq!(drv.state.status, AgentTurnStatus::Running);

        // 工具权限请求
        let perm_line = r#"{"type":"control_request","request_id":"req-99","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{"command":"cargo check","description":"Check workspace"},"decision_reason":"Shell execution"}}"#;
        let out = drv.on_data(&format!("{perm_line}\n"));
        assert_eq!(drv.state.status, AgentTurnStatus::AwaitingPermission);
        assert!(out.status_changed);
        let records = drv.program_status_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].state, ProgramState::Blocked);

        // 批准权限
        let out = drv.answer_permission("toolu_1", AgentPermissionDecision::Allow, None);
        assert_eq!(out.to_pty.len(), 1);
        assert!(out.to_pty[0].contains("\"behavior\":\"allow\""));
        assert_eq!(drv.state.status, AgentTurnStatus::Running);

        // 收到工具输出与结果
        let res_line = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"Finished dev profile","is_error":false}]}}"#;
        let _ = drv.on_data(&format!("{res_line}\n"));
        let done_line = r#"{"type":"result","subtype":"success","is_error":false,"result":"All checks passed."}"#;
        let out = drv.on_data(&format!("{done_line}\n"));
        assert_eq!(drv.state.status, AgentTurnStatus::Idle);
        assert!(out.status_changed);
    }

    #[test]
    fn claude_initialize_errors_are_visible_without_discarding_capabilities() {
        for initialized in [false, true] {
            let mut drv = if initialized {
                ready_claude()
            } else {
                let mut drv = AgentDriver::new(SessionAgent::Claude, None);
                drv.on_data("__FALCON_AGENT_START__:claude\n");
                drv
            };
            let catalog = drv.state.model_options.clone();
            let out = drv.on_data(concat!(
                r#"{"type":"control_response","response":{"subtype":"error","request_id":"init-1","error":"initialization rejected"}}"#,
                "\n"
            ));
            assert!(out.to_pty.is_empty());
            assert!(out.messages.iter().any(|msg| matches!(msg,
                ServerMessage::AgentEntry { entry: AgentEntry::Notice { level: AgentNoticeLevel::Warning, text, .. } }
                    if text.contains("Claude 初始化失败") && text.contains("initialization rejected")
            )));
            assert_eq!(drv.state.model_options, catalog);
            assert_eq!(drv.initialized, initialized);
            assert!(drv.state.active);
        }
    }

    #[test]
    fn claude_system_init_recovers_missing_catalog_once() {
        let mut drv = AgentDriver::new(SessionAgent::Claude, None);
        drv.on_data("__FALCON_AGENT_START__:claude\n");
        let system_init = concat!(
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-5-5","cwd":"/work/repo"}"#,
            "\n"
        );
        let out = drv.on_data(system_init);
        assert_eq!(out.to_pty.len(), 1);
        let request: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(request["request"]["subtype"], "initialize");
        assert!(drv.state.model_options.is_empty());
        assert!(drv.on_data(system_init).to_pty.is_empty(), "目录仍为空时不能无限补发");

        let out = drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response": { "subtype":"success", "request_id":request["request_id"],
                "response":{"models":[{"value":"sonnet","resolvedModel":"claude-sonnet-5-5",
                    "supportsEffort":true,"supportedEffortLevels":["low","high","max"]}]}}
        })));
        let selected = drv.state.selected_model_option().unwrap();
        assert_eq!(selected.id, "sonnet");
        assert_eq!(selected.reasoning_efforts, ["low", "high", "max"]);
        assert!(out.messages.iter().any(|msg| matches!(msg,
            ServerMessage::AgentMeta { model_options: Some(options), .. } if options.len() == 1
        )));
        let settings: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(settings["request"]["subtype"], "get_settings");
        assert!(drv.on_data(system_init).to_pty.is_empty());
    }

    #[test]
    fn claude_system_init_keeps_an_existing_catalog_without_reinitializing() {
        let mut drv = ready_claude();
        let catalog = drv.state.model_options.clone();
        let out = drv.on_data(concat!(
            r#"{"type":"system","subtype":"init","model":"claude-sonnet-5-5"}"#,
            "\n"
        ));
        assert!(out.to_pty.is_empty());
        assert_eq!(drv.state.model_options, catalog);
    }

    #[test]
    fn guarded_start_and_exit_do_not_send_initialization_to_the_fallback_shell() {
        let mut drv = AgentDriver::new(SessionAgent::Claude, None);
        assert_eq!(drv.on_data(&format!("{AGENT_READY_SENTINEL}\n")).to_pty, vec![format!("{AGENT_CONNECT_SENTINEL}\n")]);
        let out = drv.on_data(&format!("{AGENT_START_SENTINEL_PREFIX}claude:guarded\n{AGENT_EXIT_SENTINEL}\n"));
        assert_eq!(out.to_pty, vec![format!("{AGENT_EXIT_ACK}\n")], "启动失败只允许发送退出确认，不能发送此前排队的 initialize");
        assert!(!drv.is_active());
        assert!(drv.send_prompt("late prompt").to_pty.is_empty());

        let mut drv = ready_claude();
        let out = drv.on_data(&format!("{}\n", json!({"type":"assistant","message":{"id":"m","content":[{"type":"text","text":AGENT_EXIT_SENTINEL}]}})));
        assert!(drv.is_active(), "正文中的内部哨兵不能改变进程状态");
        assert!(out.to_pty.is_empty());
    }

    #[test]
    fn unknown_interactions_are_visibly_refused_instead_of_waiting_forever() {
        let mut drv = ready_claude();
        let out = drv.on_data("{\"type\":\"control_request\",\"request_id\":\"other-1\",\"request\":{\"subtype\":\"elicitation\"}}\n");
        let response: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(response["response"]["subtype"], "error");
        assert_eq!(response["response"]["request_id"], "other-1");
        assert!(out.messages.iter().any(|m| matches!(m, ServerMessage::AgentEntry { entry: AgentEntry::Notice { text, .. } } if text.contains("elicitation"))));
        let out = drv.on_data("{\"type\":\"control_request\",\"request_id\":\"question-1\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"AskUserQuestion\",\"input\":{\"questions\":[]}}}\n");
        let response: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(response["response"]["response"]["behavior"], "deny");
        let mut codex = AgentDriver::new(SessionAgent::Codex, None);
        codex.on_data("__FALCON_AGENT_START__:codex\n");
        let out = codex.on_data("{\"jsonrpc\":\"2.0\",\"id\":44,\"method\":\"item/tool/requestUserInput\",\"params\":{}}\n");
        let response: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(response["id"], 44);
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn initialization_timeout_does_not_allow_any_further_control_writes() {
        let mut drv = ready_claude();
        let out = drv.initialization_failed("handshake timeout");
        assert!(!out.messages.is_empty());
        assert!(!drv.control_ready());
        assert!(drv.send_prompt("must not write").to_pty.is_empty());
        assert!(drv.interrupt().to_pty.is_empty());
        assert!(drv.set_model("sonnet").to_pty.is_empty());
        assert!(drv.set_reasoning_effort(Some("high")).to_pty.is_empty());
        assert!(drv.answer_permission("unknown", AgentPermissionDecision::Allow, None).to_pty.is_empty());
    }

    fn ready_claude() -> AgentDriver {
        let mut drv = AgentDriver::new(SessionAgent::Claude, None);
        drv.on_data("__FALCON_AGENT_PROCESS__:12345\n__FALCON_AGENT_START__:claude:guarded\n");
        let out = drv.on_data(concat!(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"models":[{"value":"sonnet","resolvedModel":"claude-sonnet-5-5","displayName":"Sonnet","supportsEffort":true,"supportedEffortLevels":["low","high","max"]},{"value":"plain","supportsEffort":false}]}}}"#,
            "\n"
        ));
        let query: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response": { "subtype":"success", "request_id":query["request_id"],
                "response":{"applied":{"model":"claude-sonnet-5-5","effort":"high"}} }
        })));
        drv
    }

    #[test]
    fn terminal_conversion_requires_live_process_evidence_and_keeps_viewers_blocked_until_manager_confirms_cli() {
        let mut drv = ready_claude();
        assert!(drv.terminal_cli_pid("fixed-id").is_none(), "没有绑定 UUID 不能退出 CLI");
        drv.pin_session_id("fixed-id");
        assert_eq!(drv.terminal_cli_pid("fixed-id"), Some(12345));
        drv.on_data("__FALCON_AGENT_PROCESS__:99999\n");
        assert_eq!(drv.terminal_cli_pid("fixed-id"), Some(12345), "启动后的普通输出不得改写 CLI 身份");
        let out = drv.begin_terminal_conversion();
        assert!(matches!(&out.messages[0], ServerMessage::AgentSnapshot { state } if state.active && state.status == AgentTurnStatus::Unknown));
        let late = drv.on_data("{\"type\":\"control_request\",\"request_id\":\"late\",\"request\":{\"subtype\":\"unsupported\"}}\n");
        assert!(late.to_pty.is_empty(), "退出中的 CLI 不得触发迟到 JSON 回复");
        let out = drv.on_data("__FALCON_AGENT_EXIT__\n");
        assert_eq!(out.to_pty, vec![format!("{AGENT_EXIT_ACK}\n")]);
        assert!(out.messages.is_empty(), "EXIT 不能提前广播 active:false");
        assert!(!drv.is_active() && !drv.should_intercept_pty());
        assert!(drv.state.active && drv.state.status == AgentTurnStatus::Unknown, "新 Viewer 必须继续禁用输入");
        assert_eq!(drv.on_data("official terminal bytes").terminal_passthrough, "official terminal bytes");
        assert!(drv.send_prompt("late prompt").to_pty.is_empty());
    }

    #[test]
    fn mode_switch_requires_result_and_no_pending_permission_or_controls() {
        let mut drv = ready_claude();
        assert!(drv.ready_to_switch());
        drv.send_prompt("run a tool");
        drv.on_data("{\"type\":\"control_request\",\"request_id\":\"req-99\",\"request\":{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"tool_use_id\":\"tool-1\",\"input\":{\"command\":\"true\"}}}\n");
        assert!(!drv.ready_to_switch());
        drv.interrupt();
        assert_eq!(drv.state.status, AgentTurnStatus::Idle);
        assert!(!drv.ready_to_switch(), "interrupt 不能替代 result");
        drv.on_data("{\"type\":\"result\",\"is_error\":false}\n");
        assert!(!drv.ready_to_switch(), "仍有未确认审批时不能切换");
        drv.answer_permission("tool-1", AgentPermissionDecision::Deny, None);
        drv.on_data("{\"type\":\"result\",\"is_error\":true}\n");
        assert!(drv.ready_to_switch());
        let out = drv.set_model("plain");
        let request: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert!(!drv.ready_to_switch());
        let out = drv.on_data(&format!("{}\n", json!({"type":"control_response","response":{"request_id":request["request_id"],"subtype":"success","response":{}}})));
        let query: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert!(!drv.ready_to_switch(), "get_settings 仍在确认中");
        drv.on_data(&format!("{}\n", json!({"type":"control_response","response":{"request_id":query["request_id"],"subtype":"success","response":{"applied":{}}}})));
        assert!(drv.ready_to_switch());
    }

    #[test]
    fn changed_conversation_uuid_and_disconnected_stream_are_not_safe_to_switch() {
        let mut drv = ready_claude();
        drv.pin_session_id("expected");
        drv.on_data("{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"another\"}\n");
        assert!(!drv.ready_to_switch());
        assert!(drv.send_prompt("must not start").to_pty.is_empty());
        let mut drv = ready_claude();
        assert!(drv.reconnect_stream().to_pty.is_empty(), "不向未知 PTY 注入 JSON");
        assert!(!drv.ready_to_switch());
        assert!(drv.send_prompt("must not start").to_pty.is_empty());
    }

    #[test]
    fn claude_settings_follow_capabilities_and_control_acknowledgements() {
        let mut drv = ready_claude();
        assert_eq!(drv.state.selected_model_option().unwrap().id, "sonnet");
        assert_eq!(drv.state.reasoning_effort.as_deref(), Some("high"));
        let out = drv.set_reasoning_effort(Some("max"));
        let request: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(request["request"]["subtype"], "apply_flag_settings");
        assert_eq!(request["request"]["settings"]["effortLevel"], "max");
        assert_eq!(drv.state.reasoning_effort.as_deref(), Some("high"), "未确认前不能显示成新值");
        drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response":{"subtype":"error", "request_id":request["request_id"], "error":"policy rejected"}
        })));
        assert_eq!(drv.state.reasoning_effort.as_deref(), Some("high"));
        assert!(drv.set_reasoning_effort(Some("ultra")).to_pty.is_empty(), "不能发送模型不支持的选项");

        let out = drv.set_reasoning_effort(None);
        let request: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert!(request["request"]["settings"]["effortLevel"].is_null());
        let out = drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response":{"subtype":"success", "request_id":request["request_id"], "response":{}}
        })));
        let query: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(query["request"]["subtype"], "get_settings");
        drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response":{"subtype":"success", "request_id":query["request_id"],
                "response":{"applied":{"model":"claude-sonnet-5-5","effort":"low"}}}
        })));
        assert_eq!(drv.state.reasoning_effort.as_deref(), Some("low"));

        let out = drv.set_model("plain");
        let request: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(drv.state.model.as_deref(), Some("claude-sonnet-5-5"));
        drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response":{"subtype":"success", "request_id":request["request_id"], "response":{"model":"plain"}}
        })));
        assert_eq!(drv.state.model.as_deref(), Some("plain"));
        assert!(drv.set_reasoning_effort(Some("high")).to_pty.is_empty());
    }

    #[test]
    fn prompt_waits_for_model_ack_and_interrupt_clears_the_queue() {
        let mut drv = ready_claude();
        let request = drv.set_model("plain");
        let request: Value = serde_json::from_str(&request.to_pty[0]).unwrap();
        assert!(drv.send_prompt("keep this input").to_pty.is_empty());
        let out = drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response":{"subtype":"success", "request_id":request["request_id"], "response":{"model":"plain"}}
        })));
        assert!(out.to_pty.iter().any(|line| line.contains("keep this input")));
        assert!(drv.pending_prompts.is_empty());

        let mut drv = ready_claude();
        let request = drv.set_model("plain");
        let request: Value = serde_json::from_str(&request.to_pty[0]).unwrap();
        drv.send_prompt("cancel this input");
        drv.interrupt();
        let out = drv.on_data(&format!("{}\n", json!({
            "type":"control_response", "response":{"subtype":"success", "request_id":request["request_id"], "response":{"model":"plain"}}
        })));
        assert!(!out.to_pty.iter().any(|line| line.contains("cancel this input")));
    }

    #[test]
    fn setting_changes_cannot_interrupt_a_running_turn_or_permission() {
        for status in [AgentTurnStatus::Running, AgentTurnStatus::AwaitingPermission] {
            let mut drv = ready_claude();
            drv.state.status = status;
            assert!(drv.set_model("plain").to_pty.is_empty());
            assert!(drv.set_reasoning_effort(Some("low")).to_pty.is_empty());
            assert_eq!(drv.state.status, status);
            assert_eq!(drv.state.model.as_deref(), Some("claude-sonnet-5-5"));
        }
    }

    #[test]
    fn codex_model_catalog_is_paged_and_selected_options_reach_the_next_turn() {
        let mut drv = AgentDriver::new(SessionAgent::Codex, None);
        drv.on_data("__FALCON_AGENT_START__:codex\n");
        let out = drv.on_data("{\"id\":1,\"result\":{}}\n");
        let requests: Vec<Value> = out.to_pty.iter().map(|s| serde_json::from_str(s).unwrap()).collect();
        assert_eq!(requests[0]["method"], "initialized");
        let list = requests.iter().find(|r| r["method"] == "model/list").unwrap();
        let out = drv.on_data(&format!("{}\n", json!({
            "id":list["id"], "result":{"data":[
                {"model":"model-a","displayName":"A","defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"ultra"}]},
                {"model":"hidden","hidden":true}
            ],"nextCursor":"page-2"}
        })));
        let page: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(page["params"]["cursor"], "page-2");
        drv.on_data(&format!("{}\n", json!({"id":page["id"],"result":{"data":[],"nextCursor":null}})));
        drv.on_data("{\"id\":2,\"result\":{\"thread\":{\"id\":\"thread-1\"},\"model\":\"model-a\"}}\n");
        assert_eq!(drv.state.models, vec!["model-a"]);
        drv.set_model("model-a");
        drv.set_reasoning_effort(Some("ultra"));
        let out = drv.send_prompt("Continue");
        let turn: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(turn["params"]["model"], "model-a");
        assert_eq!(turn["params"]["effort"], "ultra");
        drv.state.status = AgentTurnStatus::Idle;
        drv.set_reasoning_effort(None);
        let out = drv.send_prompt("Default effort");
        let turn: Value = serde_json::from_str(&out.to_pty[0]).unwrap();
        assert_eq!(turn["params"]["effort"], "low");
    }

    #[test]
    fn codex_app_server_lifecycle() {
        let mut drv = AgentDriver::new(SessionAgent::Codex, Some("/work/repo".into()));
        let out = drv.on_data("__FALCON_AGENT_START__:codex\n");
        assert_eq!(out.to_pty.len(), 1);
        assert!(out.to_pty[0].contains("\"method\":\"initialize\""));

        let out = drv.on_data("{\"id\":1,\"result\":{\"userAgent\":\"falcon\"}}\n");
        assert_eq!(out.to_pty.len(), 3);
        assert!(out.to_pty[0].contains("\"method\":\"initialized\""));
        assert!(out.to_pty[1].contains("\"method\":\"thread/start\""));
        assert!(out.to_pty[2].contains("\"method\":\"model/list\""));

        let _ = drv.on_data(
            "{\"id\":2,\"result\":{\"thread\":{\"id\":\"thr-1\"},\"model\":\"gpt-5.4\",\"cwd\":\"/work/repo\"}}\n",
        );
        assert_eq!(drv.codex_thread_id.as_deref(), Some("thr-1"));
        assert_eq!(drv.state.model.as_deref(), Some("gpt-5.4"));

        let out = drv.send_prompt("Hello codex");
        assert_eq!(out.to_pty.len(), 1);
        assert!(out.to_pty[0].contains("\"method\":\"turn/start\""));

        let _ = drv.on_data("{\"method\":\"item/started\",\"params\":{\"item\":{\"type\":\"agentMessage\",\"id\":\"msg-1\",\"text\":\"\"}}}\n");
        let _ = drv.on_data("{\"method\":\"item/agentMessage/delta\",\"params\":{\"itemId\":\"msg-1\",\"delta\":\"Hi there!\"}}\n");
        assert!(drv.state.entries.iter().any(|e| matches!(e, AgentEntry::Message { id, text } if id == "msg-1" && text == "Hi there!")));
    }
}
