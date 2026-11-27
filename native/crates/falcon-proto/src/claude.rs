//! Claude 对话通道。一条连接拥有一个 Claude Code 子进程，线程可接回历史。
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClaudeInput {
    // `attachments` 引用之前用 `upload` 传完的附件；文字可以为空，只要带着附件。
    Prompt {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ClaudeAttachment>,
    },
    Permission { id: String, allow: bool },
    // 打断正在进行的这一轮；Claude Code 随后照常发 result（Done 带错误）。
    Interrupt,
    // 附件的一段 base64。WS 单帧上限 1 MiB（网关同样），附件按 `UPLOAD_CHUNK_CHARS` 分段依次发，
    // 宿主机按 id 拼接，随下一条 Prompt 取走；连接断了就丢。
    Upload { id: String, data: String },
    // 以下三项对应 Claude Code 的控制请求，宿主机成功后推 `options` 回来，线程的选择存在宿主机。
    // `model` 是 `options.models` 里的 value，空串 = 回到默认模型。
    SetModel { model: String },
    SetEffort { effort: String },
    SetPermissionMode { mode: String },
}

/// `upload` 一段 base64 的最大字符数：加上 JSON 外壳也远在 1 MiB 的帧上限以内
pub const UPLOAD_CHUNK_CHARS: usize = 512 * 1024;

/// 提示词带的附件。`mediaType` 决定宿主机怎么交给 Claude：`image/*` 成图片块，
/// `application/pdf` 成文档块，`text/plain` 解成 UTF-8 正文、包上文件名成文字块。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAttachment {
    pub id: String,
    pub name: String,
    pub media_type: String,
}

/// Claude Code 在 `initialize` 里报的一个可选模型
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeModel {
    pub value: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// 选这个模型时实际用的模型名（`default` / `sonnet` 这类别名解析后的）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    /// 支持的 effort 档位（low / medium / high / xhigh / max），空 = 不支持调
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effort_levels: Vec<String>,
}

/// 输入框底栏上的模型、effort、权限模式。每次有变化都推整份。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeOptions {
    pub models: Vec<ClaudeModel>,
    /// 用户选的模型（`models` 里的 value）；None = 没选过，跟 Claude Code 的设置走
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Claude Code 报的实际模型名（选了别名或没选时靠它找显示名）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission_mode: String,
}

/// 一次工具调用，界面画成一张步骤卡片。`title` 是给人看的一行（Bash 的 description、文件路径、
/// 搜索词……），`detail` 是补充的一行（有 description 时的原始命令），都由宿主机按工具名提炼并截断，
/// 客户端不认识具体工具也能画。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClaudeToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ClaudeEvent {
    // 线程先回放历史，再发送 Ready。之后由宿主机回显已收录的提示词。
    ThreadHistory,
    User {
        text: String,
        // 附件只回显文件名，内容不进历史
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    Ready,
    Delta {
        text: String,
    },
    // 完整消息替换当前草稿，兼容没有增量的提供方。`tools` 只剩工具名，留给旧客户端与旧历史；
    // 新客户端有 `calls` 就用 `calls`。
    Assistant {
        text: String,
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        calls: Vec<ClaudeToolCall>,
    },
    // 工具跑完。`output` 是截断过的开头几行；`elapsedMs` 是宿主机从发出调用到收到结果的耗时，
    // 回放历史时照样有（接回时不能拿客户端的时钟算）。
    ToolResult {
        id: String,
        #[serde(default)]
        error: bool,
        #[serde(default)]
        output: String,
        #[serde(default, rename = "elapsedMs", skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
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
        // 这一轮的总耗时（Claude Code 的 result.duration_ms），界面的「工作了 …」。
        #[serde(default, rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    Closed {
        reason: String,
    },
    // Ready 之后推一次，模型 / effort / 权限模式变了（包括 Claude Code 自己退出计划模式）再推。
    Options(ClaudeOptions),
    // 上下文窗口用了多少 token（Claude Code 的 get_context_usage），Ready 后与每轮结束后各报一次
    Context {
        used: u64,
        max: u64,
    },
    // 改设置之类没成功：不属于哪一轮，界面在输入框上方提示一下
    Warning {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_wire_shapes_still_parse() {
        let e: ClaudeEvent = serde_json::from_str(r#"{"type":"assistant","text":"hi","tools":["Read"]}"#).unwrap();
        assert!(matches!(e, ClaudeEvent::Assistant { calls, .. } if calls.is_empty()));
        let e: ClaudeEvent = serde_json::from_str(r#"{"type":"done","error":null}"#).unwrap();
        assert!(matches!(e, ClaudeEvent::Done { duration_ms: None, .. }));
        let s = serde_json::to_string(&ClaudeEvent::ToolResult {
            id: "t".into(),
            error: false,
            output: String::new(),
            elapsed_ms: Some(5),
        })
        .unwrap();
        assert_eq!(s, r#"{"type":"toolResult","id":"t","error":false,"output":"","elapsedMs":5}"#);
        let e: ClaudeEvent = serde_json::from_str(r#"{"type":"user","text":"hi"}"#).unwrap();
        assert!(matches!(e, ClaudeEvent::User { attachments, .. } if attachments.is_empty()));
        let i: ClaudeInput = serde_json::from_str(r#"{"type":"prompt","text":"hi"}"#).unwrap();
        assert!(matches!(i, ClaudeInput::Prompt { attachments, .. } if attachments.is_empty()));
    }

    #[test]
    fn options_and_settings_wire_shapes() {
        let s = serde_json::to_string(&ClaudeEvent::Options(ClaudeOptions {
            models: vec![],
            model: None,
            resolved_model: None,
            effort: Some("high".into()),
            permission_mode: "plan".into(),
        }))
        .unwrap();
        assert_eq!(s, r#"{"type":"options","models":[],"effort":"high","permissionMode":"plan"}"#);
        let s = serde_json::to_string(&ClaudeInput::SetPermissionMode { mode: "plan".into() }).unwrap();
        assert_eq!(s, r#"{"type":"setPermissionMode","mode":"plan"}"#);
    }
}
