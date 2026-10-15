//! AI 助手（docs/design/ai-assistant.md 第 5、8 节）：对话、条目、WS 消息、设置。
//!
//! 条目的形状**照 ACP 的 `session/update`**（消息块、思考块、工具调用及其增量、计划），这样内置
//! agent 直接产出这些，ACP agent 做一次映射，界面只认这一种。没有直接用 ACP 的 schema crate：
//! 它一周发三个大版本、带一堆 unstable 变体，客户端协议要稳、要编进 wasm；映射只在服务端。
//! `ToolKind` / `ToolStatus` / `PermissionKind` 的取值与 ACP 一致。
//!
//! 线上都是 camelCase；枚举标签是 snake_case（与 ACP 一致）。每个带 tag 的枚举都有 `Unknown`
//! 兜底：服务端比客户端新时，看不懂的条目 / 消息安静忽略，而不是整条 JSON 解析失败。

use serde::{Deserialize, Serialize};

/// 内置 agent 的 id（`Thread.agent`）。ACP agent 是 `acp:<配置 id>`
pub const BUILTIN_AGENT: &str = "builtin";

// ---------------------------------------------------------------- 对话

/// 对话在跑没跑
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    #[default]
    Idle,
    Running,
    /// 有工具调用在等你批准
    AwaitingPermission,
    /// 上一轮出错停下了（错误原文在对话里的 notice 条目上）
    Error,
    #[serde(other)]
    Unknown,
}

/// 对话列表里的一行
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: String,
    /// `local` 或 SSH Host 的 id（与 `/api/assistant/machines/:machine` 同形）
    pub machine: String,
    /// [`BUILTIN_AGENT`] 或 `acp:<id>`
    pub agent: String,
    /// 第一句话（或第一条命令）的前 40 字；还没开始时为空
    pub title: String,
    /// 这段对话的终端会话（一段对话 = 一个 shell + 一段 AI 上下文，像 Warp 的一个标签）。
    /// 会话丢了（shell 退出）时 `POST /api/assistant/threads/:id/shell` 再开一个，这里随之换掉
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub status: ThreadStatus,
    /// unix 毫秒
    pub created_at: i64,
    pub updated_at: i64,
}

/// `POST /api/assistant/machines/:machine/threads`：新对话（连同它的终端）。配色的深浅与新建
/// 终端会话一样，写 COLORFGBG 等用
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreateThreadRequest {
    /// 缺省是内置 agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<crate::TermAppearance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
}

/// `PATCH /api/assistant/threads/:id`：换这段对话的 agent（之后的话交给新的那个；终端与命令块不动）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

// ---------------------------------------------------------------- 条目

/// 对话里的一条。`id` 在对话内唯一，增量（[`AssistantServerMessage::Append`]）与整条替换
/// （[`AssistantServerMessage::Entry`]）都按它找。标签是 `type`，不能叫 `kind`——ToolCall 自己
/// 有个 `kind` 字段，摊平之后会把标签盖掉
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Entry {
    /// 终端里跑完的一条命令（Warp 的命令块）：命令行、当时的目录、原始输出（带 ANSI，过长时
    /// 掐了中间）、退出码。你自己敲的，或者内置 agent 打进去的（`by_agent`，接在它的工具卡片
    /// 后面——卡片只画命令与状态，输出在这里，与终端里看到的一样）
    Command {
        id: String,
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        /// agent 跑的：下一句话不再当成「你在终端里干了什么」带给它
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        by_agent: bool,
    },
    /// 你对 agent 说的话，`cwd` 是说的时候终端的目录（时间线上画成 `~/dir 这句话`，像在命令行里敲的）。
    /// `context` 是旧版附上的终端输出（现在改成自动带最近的命令块），留着读旧数据
    User {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// AI 的回复（markdown，流式追加）
    Agent { id: String, text: String },
    /// 思考的摘要（流式追加，界面直接画出来，长了折起）
    Thought { id: String, text: String },
    ToolCall(ToolCall),
    /// 计划（ACP 的 `plan`）。内置 agent 不出
    Plan { id: String, items: Vec<PlanItem> },
    /// 系统的话：出错、被拒、已停止、上下文已重置……
    Notice { id: String, level: NoticeLevel, text: String },
    #[serde(other)]
    Unknown,
}

impl Entry {
    pub fn id(&self) -> Option<&str> {
        match self {
            Entry::User { id, .. }
            | Entry::Command { id, .. }
            | Entry::Agent { id, .. }
            | Entry::Thought { id, .. }
            | Entry::Plan { id, .. }
            | Entry::Notice { id, .. } => Some(id),
            Entry::ToolCall(t) => Some(&t.id),
            Entry::Unknown => None,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlanItem {
    pub content: String,
    /// `pending` / `in_progress` / `completed`
    pub status: String,
}

/// 工具调用的卡片
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    /// 卡片标题：命令本身、`读取 src/main.rs`……
    pub title: String,
    /// 卡片上面那一行「这一步要做什么」（内置 agent 的 run_command 带的 description）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: ToolKind,
    pub status: ToolStatus,
    #[serde(default)]
    pub content: Vec<ToolContent>,
    /// 等你批准时有：卡片底部的按钮
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<Vec<PermissionOption>>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ToolContent {
    Text { text: String },
    /// 文件改动。`old_text` 为空 = 新文件
    Diff {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old_text: Option<String>,
        new_text: String,
    },
    /// 命令的输出：原始字节流（带 ANSI），界面按终端的习惯处理 `\r` 与转义后显示。
    /// `exit_code` 在结束后才有
    Terminal {
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub id: String,
    /// 按钮上的字（ACP agent 给的原样用）
    pub name: String,
    pub kind: PermissionKind,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------- WS

/// 服务端 → 客户端（`/ws/assistant/threads/:id`，JSON 文本帧）。连上先收一份 `snapshot`，
/// 再收增量——与终端「先回放再接实时流」同一个模型，多个窗口同时看同一段对话也成立
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AssistantServerMessage {
    Snapshot { thread: ThreadSummary, entries: Vec<Entry> },
    /// 新条目，或同 id 的整条替换（工具调用换状态、等批准、出结果）
    Entry { entry: Entry },
    /// 同 id 条目的文字追加：Agent / Thought 追加正文，ToolCall 追加到它的终端输出
    Append { id: String, text: String },
    Status { status: ThreadStatus },
    /// 对话的摘要变了：第一条命令 / 第一句话给它起了名、换了 agent、终端丢了另开了一个。
    /// 面板的对话列表靠它跟着变，不用重拉
    Thread { thread: ThreadSummary },
    #[serde(other)]
    Unknown,
}

/// 客户端 → 服务端
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AssistantClientMessage {
    Prompt {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Cancel,
    /// 答卡片上的审批。`command`：你在卡片上把命令改了（「修改」），按改过的跑——只有内置
    /// agent 的 run_command 认，别的忽略
    Permission {
        tool_call_id: String,
        option_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------- 设置

/// `GET /api/assistant/settings`。API key 永不下发，只给有没有、末四位
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSettings {
    /// 空 = 官方 `https://api.anthropic.com`
    pub base_url: String,
    pub model: String,
    /// `low` / `medium` / `high` / `xhigh` / `max`
    pub effort: String,
    pub has_api_key: bool,
    /// `sk-ant-…1a2b`；key 来自服务端的环境变量时是「环境变量」之类的说明
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_hint: Option<String>,
    /// 命令与写文件不再询问
    pub auto_approve: bool,
    /// 经 ACP 接入的本地 agent（预设三家 + 自定义）。只在本机起
    #[serde(default)]
    pub acp_agents: Vec<AcpAgentConfig>,
}

/// 一个 ACP agent 怎么起：命令 + 参数 + 额外的环境变量（与 Zed 的 `agent_servers` 同形）。
/// 进程在 falcon 后端本机跑，环境是登录 shell 解析出来的那份（npx / nvm 找得到）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentConfig {
    /// 预设是固定的 `claude-code` / `codex` / `gemini`；自定义的是随机 id
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub enabled: bool,
}

impl AcpAgentConfig {
    /// 线上的 agent id（`Thread.agent`）
    pub fn agent_id(&self) -> String {
        format!("{ACP_AGENT_PREFIX}{}", self.id)
    }

    pub fn is_preset(&self) -> bool {
        acp_presets().iter().any(|p| p.id == self.id)
    }
}

/// ACP agent 在 `Thread.agent` 上的前缀
pub const ACP_AGENT_PREFIX: &str = "acp:";

/// 预设：命令是 ACP registry 里的写法（docs/design/ai-assistant.md 第 7 节）。缺省都开着——没装的会在
/// 第一次用时报「找不到命令」并给安装命令
pub fn acp_presets() -> Vec<AcpAgentConfig> {
    let preset = |id: &str, name: &str, command: &str, args: &[&str]| AcpAgentConfig {
        id: id.into(),
        name: name.into(),
        command: command.into(),
        args: args.iter().map(|a| a.to_string()).collect(),
        env: Vec::new(),
        enabled: true,
    };
    vec![
        preset("claude-code", "Claude Code", "npx", &["-y", "@agentclientprotocol/claude-agent-acp"]),
        preset("codex", "Codex", "npx", &["-y", "@agentclientprotocol/codex-acp"]),
        preset("gemini", "Gemini CLI", "gemini", &["--acp"]),
    ]
}

/// `PUT /api/assistant/settings`：只改带了的字段；`api_key` 给空串 = 清掉
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssistantSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_approve: Option<bool>,
    /// 整份替换 ACP agent 列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_agents: Option<Vec<AcpAgentConfig>>,
}

/// 设置页里可选的模型（也能手填）。第一项是缺省
pub const MODELS: &[&str] = &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1"];
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const DEFAULT_EFFORT: &str = "high";

#[cfg(test)]
mod tests {
    use super::*;

    /// 值 → JSON 字符串 → 值，再与 JSON 样本互相对得上（test_util::roundtrip 吃 JSON 样本）
    fn roundtrip<T>(v: &T)
    where
        T: serde::de::DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(v).unwrap();
        let back: T = crate::test_util::roundtrip(&json);
        assert_eq!(&back, v);
    }

    #[test]
    fn entries_roundtrip_with_acp_shaped_tags() {
        let call = Entry::ToolCall(ToolCall {
            id: "t1".into(),
            title: "ls -la".into(),
            description: None,
            kind: ToolKind::Execute,
            status: ToolStatus::InProgress,
            content: vec![ToolContent::Terminal { output: "a\r\n".into(), exit_code: None }],
            permission: Some(vec![PermissionOption {
                id: "allow".into(),
                name: "允许".into(),
                kind: PermissionKind::AllowOnce,
            }]),
        });
        let v = serde_json::to_value(&call).unwrap();
        assert_eq!(v["type"], "tool_call");
        assert_eq!(v["kind"], "execute");
        assert_eq!(v["status"], "in_progress");
        assert_eq!(v["content"][0]["type"], "terminal");
        assert_eq!(v["permission"][0]["kind"], "allow_once");
        roundtrip(&call);
        roundtrip(&Entry::User { id: "u".into(), text: "hi".into(), context: Some("$ ls".into()), cwd: Some("/tmp".into()) });
        roundtrip(&AssistantServerMessage::Append { id: "a".into(), text: "x".into() });
        roundtrip(&AssistantClientMessage::Permission { tool_call_id: "t1".into(), option_id: "allow".into(), command: None });
        // 旧客户端不带 command；by_agent 缺省 false、为 false 时不写出去
        let p: AssistantClientMessage =
            serde_json::from_str(r#"{"type":"permission","toolCallId":"t1","optionId":"allow"}"#).unwrap();
        assert_eq!(p, AssistantClientMessage::Permission { tool_call_id: "t1".into(), option_id: "allow".into(), command: None });
        let block = Entry::Command { id: "c".into(), command: "ls".into(), cwd: None, output: String::new(), exit_code: Some(0), by_agent: false };
        assert!(serde_json::to_value(&block).unwrap().get("byAgent").is_none());
        roundtrip(&block);
        roundtrip(&Entry::Command { id: "c".into(), command: "ls".into(), cwd: None, output: String::new(), exit_code: None, by_agent: true });
    }

    #[test]
    fn newer_server_variants_are_unknown_not_errors() {
        let e: Entry = serde_json::from_str(r#"{"type":"subagent","id":"x"}"#).unwrap();
        assert_eq!(e, Entry::Unknown);
        let m: AssistantServerMessage = serde_json::from_str(r#"{"type":"usage","tokens":3}"#).unwrap();
        assert_eq!(m, AssistantServerMessage::Unknown);
        let s: ThreadStatus = serde_json::from_str(r#""paused""#).unwrap();
        assert_eq!(s, ThreadStatus::Unknown);
    }
}
