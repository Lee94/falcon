//! 原生 Coding Agent 画布视图的统一协议（`Claude Code` `stream-json` 与 `Codex` `app-server`）。
//!
//! 服务端在 `SessionManager` 内置的 `AgentDriver` 中将 CLI 的结构化双向流解析为本模块的
//! 统一模型（[`AgentViewState`]、[`AgentEntry`] 等），通过会话通道 `/ws/sessions/:id` 的
//! JSON 控制消息（[`crate::ServerMessage`] / [`crate::ClientMessage`]）与客户端双向同步。

use serde::{Deserialize, Serialize};

use crate::session::SessionAgent;
use crate::wire::wire_enum;

wire_enum! {
    /// 当前轮次的运行状态。
    pub enum AgentTurnStatus open {
        Idle => "idle",
        Running => "running",
        AwaitingPermission => "awaiting_permission",
        Error => "error",
    }
}

wire_enum! {
    /// 工具调用的类别，供原生视图决定图标与卡片样式。
    pub enum AgentToolKind open {
        Execute => "execute",
        Edit => "edit",
        Write => "write",
        Read => "read",
        Search => "search",
        Fetch => "fetch",
        Mcp => "mcp",
        Other => "other",
    }
}

wire_enum! {
    /// 工具调用的生命周期状态。
    pub enum AgentToolStatus open {
        Pending => "pending",
        InProgress => "in_progress",
        Completed => "completed",
        Failed => "failed",
    }
}

wire_enum! {
    /// 流式增量对应的目标字段。
    pub enum AgentDeltaKind open {
        Message => "message",
        Thought => "thought",
        ToolOutput => "tool_output",
    }
}

wire_enum! {
    /// 用户对工具调用权限审批的决定。
    pub enum AgentPermissionDecision {
        Allow => "allow",
        AllowSession => "allow_session",
        Deny => "deny",
    }
}

wire_enum! {
    /// 系统提示或通知条目的级别。
    pub enum AgentNoticeLevel open {
        Info => "info",
        Warning => "warning",
        Error => "error",
    }
}

wire_enum! {
    /// 计划清单条目的状态。
    pub enum AgentPlanStatus open {
        Pending => "pending",
        InProgress => "in_progress",
        Completed => "completed",
    }
}

/// 斜杠命令描述（由 CLI 初始化握手上报）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSlashCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// CLI 实际提供的模型及其思考程度，避免客户端把不支持的选项显示成可用。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<String>,
}

/// 计划清单中的单条任务。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentPlanItem {
    pub content: String,
    pub status: AgentPlanStatus,
}

/// 工具调用卡片内嵌的结构化内容。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AgentToolContent {
    /// 普通文本输出或文件读取内容。
    #[serde(rename = "text")]
    Text { text: String },
    /// 文件修改对比：既支持旧文本/新文本逐行对比（Claude `Edit` / `Write`），也支持
    /// 统一 diff 文本（Codex `fileChange`）。
    #[serde(rename = "diff")]
    Diff {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        new_text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unified_diff: Option<String>,
    },
    /// 终端命令执行及输出。
    #[serde(rename = "terminal")]
    Terminal {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    #[serde(rename = "unknown", other)]
    Unknown,
}

/// 一次工具调用及其审批状态。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCall {
    /// 工具调用 ID（在单次会话中唯一）。
    pub id: String,
    /// 简短标题（例如命令内容、相对文件路径或工具名）。
    pub title: String,
    /// 补充说明（例如 Claude Bash 工具的 `description`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 关联的文件路径（用于点击跳转到工作区编辑器）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    pub kind: AgentToolKind,
    pub status: AgentToolStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<AgentToolContent>,
    /// 是否正在等待用户审批权限。
    #[serde(default)]
    pub permission_pending: bool,
    /// 请求审批的原因说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_reason: Option<String>,
}

/// 时间线上的单条对话或操作记录。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AgentEntry {
    /// 用户发送的提示词。
    #[serde(rename = "user")]
    User { id: String, text: String },
    /// 模型的思考过程（默认折叠，流式更新时展示摘要）。
    #[serde(rename = "thought")]
    Thought { id: String, text: String },
    /// 模型的 Markdown 回复正文。
    #[serde(rename = "message")]
    Message { id: String, text: String },
    /// 工具调用卡片。
    #[serde(rename = "tool_call")]
    ToolCall(AgentToolCall),
    /// 任务计划清单（TodoWrite / update_plan）。
    #[serde(rename = "plan")]
    Plan {
        id: String,
        items: Vec<AgentPlanItem>,
    },
    /// 系统提示或报错信息。
    #[serde(rename = "notice")]
    Notice {
        id: String,
        level: AgentNoticeLevel,
        text: String,
    },
    #[serde(rename = "unknown", other)]
    Unknown,
}

impl AgentEntry {
    /// 获取条目的唯一 ID。
    pub fn id(&self) -> Option<&str> {
        match self {
            AgentEntry::User { id, .. }
            | AgentEntry::Thought { id, .. }
            | AgentEntry::Message { id, .. }
            | AgentEntry::Plan { id, .. }
            | AgentEntry::Notice { id, .. } => Some(id),
            AgentEntry::ToolCall(tc) => Some(&tc.id),
            AgentEntry::Unknown => None,
        }
    }
}

/// 单个 Agent 会话的完整原生视图快照。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentViewState {
    /// 结构化视图是否可用；本地切换显示不改变底层 CLI 的生命周期。
    pub active: bool,
    /// 对话记录来自仍在运行的官方终端；图形时间线只读，输入与审批继续走原 PTY。
    /// 旧结构化协议缺省为 false，不能把它的 JSON 输入误送给交互式终端。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal_backed: bool,
    /// 会话对应的 Agent 类型。
    pub agent: SessionAgent,
    /// 当前轮次状态。
    pub status: AgentTurnStatus,
    /// 当前生效的模型名称。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 当前权限模式（如 `default`、`acceptEdits`、`plan` 等）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// 当前工作目录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// 可用的斜杠命令列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<AgentSlashCommand>,
    /// 可选的模型列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// 模型能力目录，来自 CLI 握手或 model/list。
    #[serde(default)]
    pub model_options: Vec<AgentModelOption>,
    /// 已确认或下一轮将使用的思考程度；None 使用 CLI 默认设置。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 时间线条目（按时间顺序排列）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<AgentEntry>,
}

impl AgentViewState {
    /// 创建初始状态。
    pub fn new(agent: SessionAgent, active: bool) -> Self {
        Self {
            active,
            terminal_backed: false,
            agent,
            status: AgentTurnStatus::Idle,
            model: None,
            permission_mode: None,
            cwd: None,
            commands: Vec::new(),
            models: Vec::new(),
            model_options: Vec::new(),
            reasoning_effort: None,
            entries: Vec::new(),
        }
    }

    /// 兼容只返回 `models` 名称列表的旧服务端；缺失的思考能力不能按名称推测。
    pub fn model_choices(&self) -> Vec<AgentModelOption> {
        if !self.model_options.is_empty() {
            return self.model_options.clone();
        }
        self.models.iter().filter(|model| !model.trim().is_empty()).map(|model| AgentModelOption {
            id: model.clone(),
            label: model.clone(),
            resolved_model: None,
            reasoning_efforts: Vec::new(),
            default_reasoning_effort: None,
        }).collect()
    }

    /// 返回当前模型的能力；未知模型不猜测它是否支持思考程度。
    pub fn selected_model_option(&self) -> Option<&AgentModelOption> {
        let model = self.model.as_deref()?;
        self.model_options.iter().find(|option| option.id == model || option.resolved_model.as_deref() == Some(model))
    }

    /// 按条目 ID 插入或整体更新一条记录。
    pub fn upsert_entry(&mut self, entry: AgentEntry) {
        if let Some(target_id) = entry.id() {
            if let Some(existing) = self
                .entries
                .iter_mut()
                .find(|e| e.id() == Some(target_id))
            {
                *existing = entry;
                return;
            }
        }
        self.entries.push(entry);
    }

    /// 对指定 ID 的条目应用流式文本增量。
    pub fn apply_delta(&mut self, id: &str, kind: AgentDeltaKind, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id() == Some(id)) {
            match (entry, kind) {
                (AgentEntry::Message { text, .. }, AgentDeltaKind::Message)
                | (AgentEntry::Thought { text, .. }, AgentDeltaKind::Thought) => {
                    text.push_str(delta);
                    return;
                }
                (AgentEntry::ToolCall(tc), AgentDeltaKind::ToolOutput) => {
                    if let Some(AgentToolContent::Terminal { output, .. }) =
                        tc.content.iter_mut().find(|c| matches!(c, AgentToolContent::Terminal { .. }))
                    {
                        output.push_str(delta);
                        return;
                    }
                    if let Some(AgentToolContent::Text { text }) =
                        tc.content.iter_mut().find(|c| matches!(c, AgentToolContent::Text { .. }))
                    {
                        text.push_str(delta);
                        return;
                    }
                    tc.content.push(AgentToolContent::Text {
                        text: delta.to_string(),
                    });
                    return;
                }
                _ => {}
            }
        }
        // 若尚未收到起始条目，按增量类型自动补齐
        match kind {
            AgentDeltaKind::Message => self.entries.push(AgentEntry::Message {
                id: id.to_string(),
                text: delta.to_string(),
            }),
            AgentDeltaKind::Thought => self.entries.push(AgentEntry::Thought {
                id: id.to_string(),
                text: delta.to_string(),
            }),
            AgentDeltaKind::ToolOutput | AgentDeltaKind::Unknown => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn agent_entry_roundtrip() {
        let tc = AgentEntry::ToolCall(AgentToolCall {
            id: "tc-1".into(),
            title: "cargo test".into(),
            description: Some("Run unit tests".into()),
            file_path: None,
            kind: AgentToolKind::Execute,
            status: AgentToolStatus::InProgress,
            content: vec![AgentToolContent::Terminal {
                command: "cargo test".into(),
                cwd: Some("/workspace".into()),
                output: "running 3 tests\n".into(),
                exit_code: None,
            }],
            permission_pending: true,
            permission_reason: Some("Requires shell execution".into()),
        });
        let json = serde_json::to_string(&tc).unwrap();
        let back = roundtrip::<AgentEntry>(&json);
        assert_eq!(back, tc);
    }

    #[test]
    fn legacy_model_names_remain_selectable_without_inventing_effort_support() {
        // 在用旧服务端的快照只有模型名称，没有 modelOptions / reasoningEffort。
        let state: AgentViewState = serde_json::from_value(serde_json::json!({
            "active": true, "agent": "claude", "status": "idle",
            "model": "remote/codex/gpt-6.1-sol",
            "models": ["remote/codex/gpt-6.1-sol", "sonnet", ""]
        })).unwrap();
        let choices = state.model_choices();
        assert_eq!(choices.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["remote/codex/gpt-6.1-sol", "sonnet"]);
        assert!(choices.iter().all(|m| m.label == m.id && m.reasoning_efforts.is_empty() && m.default_reasoning_effort.is_none()));
        assert!(state.selected_model_option().is_none());
        assert!(state.reasoning_effort.is_none());
    }

    #[test]
    fn detailed_model_catalog_takes_precedence_over_legacy_names() {
        let state: AgentViewState = serde_json::from_value(serde_json::json!({
            "active": true, "agent": "claude", "status": "idle", "model": "actual-model",
            "models": ["old-model"],
            "modelOptions": [{"id": "alias", "label": "Model", "resolvedModel": "actual-model",
                "reasoningEfforts": ["low", "ultra"], "defaultReasoningEffort": "low"}]
        })).unwrap();
        assert_eq!(state.model_choices(), state.model_options);
        assert_eq!(state.selected_model_option().map(|m| m.id.as_str()), Some("alias"));
        assert_eq!(state.model_choices()[0].reasoning_efforts, ["low", "ultra"]);
    }

    #[test]
    fn terminal_backed_snapshot_is_explicit_and_legacy_stream_defaults_to_false() {
        let old: AgentViewState = serde_json::from_value(serde_json::json!({
            "active": true, "agent": "claude", "status": "idle"
        })).unwrap();
        assert!(!old.terminal_backed);
        assert!(serde_json::to_value(&old).unwrap().get("terminalBacked").is_none());
        let mut mirror = old;
        mirror.terminal_backed = true;
        let value = serde_json::to_value(&mirror).unwrap();
        assert_eq!(value["terminalBacked"], true);
        assert_eq!(serde_json::from_value::<AgentViewState>(value).unwrap(), mirror);
    }

    #[test]
    fn apply_delta_appends_and_creates() {
        let mut state = AgentViewState::new(SessionAgent::Claude, true);
        state.apply_delta("msg-1", AgentDeltaKind::Message, "Hello");
        state.apply_delta("msg-1", AgentDeltaKind::Message, " world");
        assert_eq!(
            state.entries,
            vec![AgentEntry::Message {
                id: "msg-1".into(),
                text: "Hello world".into(),
            }]
        );
    }
}
