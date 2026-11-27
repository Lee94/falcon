//! 项目所属节点运行 Claude Code。线程保存对话，窗口关闭时回收其子进程。
use std::{
    collections::HashMap,
    fs::File,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use axum::{
    Router,
    extract::{
        Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::HeaderMap,
    response::Response,
    routing::get,
};
use falcon_proto::{
    WS_MAX_PAYLOAD_BYTES,
    claude::{ClaudeAttachment, ClaudeEvent as Event, ClaudeInput, ClaudeModel, ClaudeOptions, ClaudeToolCall},
};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    fs,
    io::{AsyncBufReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::AppState;
use crate::{
    no_window::NoWindow,
    sessions::{
        agent_detect::local_agent_command,
        login_env::is_inherited_session_env,
        machine::owner_row,
    },
};

pub fn router() -> Router<AppState> {
    Router::new().route("/ws/projects/{id}/claude", get(open))
}

async fn open(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let authed = state.authenticated(&headers);
    ws.max_message_size(WS_MAX_PAYLOAD_BYTES)
        .on_upgrade(move |mut socket| async move {
            if !authed {
                return super::ws::close_unauthorized(socket).await;
            }
            if let Err(err) = run(state, id, &mut socket).await {
                let _ = send(
                    &mut socket,
                    Event::Closed {
                        reason: format!("{err:#}"),
                    },
                )
                .await;
            }
        })
}

async fn send(socket: &mut WebSocket, event: Event) -> Result<()> {
    let text = serde_json::to_string(&event)?;
    tokio::time::timeout(
        Duration::from_secs(15),
        socket.send(Message::Text(text.into())),
    )
    .await
    .context("发送 Claude 对话消息超时")??;
    Ok(())
}

async fn write(stdin: &mut ChildStdin, value: Value) -> Result<()> {
    let mut buf = serde_json::to_vec(&value)?;
    buf.push(b'\n');
    tokio::time::timeout(Duration::from_secs(15), stdin.write_all(&buf))
        .await
        .context("写入 Claude Code 超时")??;
    Ok(())
}

#[derive(Default)]
struct Conversation {
    ready: bool,
    busy: bool,
    permissions: HashMap<String, Value>,
    // 发出调用的时刻，收到结果时算耗时；一轮结束时清掉（被打断的调用不会有结果）。
    tools: HashMap<String, Instant>,
    // 还没随提示词发出去的附件（base64 按段拼起来），连同总字符数，防止客户端无限塞。
    uploads: HashMap<String, String>,
    upload_chars: usize,
    options: ClaudeOptions,
    // 用户在这条线程上选过的设置，存在宿主机（`<id>.options.json`），下次打开照样恢复。
    chosen: SavedOptions,
    // 等 Claude Code 回话的设置请求：request_id → 改了什么。
    pending: HashMap<String, Change>,
    next_request: u64,
    // event() 是同步的，要跟着发的控制请求（每轮后的上下文用量、换模型后的设置）攒在这里，
    // 由 drive 写进 stdin。
    requests: Vec<Value>,
}

#[derive(Clone, Debug)]
enum Change {
    Model(String),
    Effort(String),
    Mode(String),
}

/// 线程上用户选过的设置。没选过的项是 None，跟着 Claude Code 自己的设置走。
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SavedOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_mode: Option<String>,
}

const SETTINGS_REQUEST: &str = "falcon-settings";
const CONTEXT_REQUEST: &str = "falcon-context";
/// 一条连接上没发出去的附件最多攒这么多 base64 字符（约 24 MiB 原始字节）。
const MAX_UPLOAD_CHARS: usize = 32 * 1024 * 1024;
/// 文本附件解码后的上限：整份塞进提示词，再大就该让 Claude 自己去读文件。
const MAX_TEXT_ATTACHMENT_BYTES: usize = 512 * 1024;
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// 能交给 Claude 的权限模式。`bypassPermissions` 要启动时带 `--allow-dangerously-skip-permissions`
/// 才切得过去（实测：不带会报 `Cannot set permission mode to bypassPermissions`）。
const PERMISSION_MODES: [&str; 5] = ["default", "acceptEdits", "plan", "auto", "bypassPermissions"];

fn control(id: &str, request: Value) -> Value {
    json!({"type":"control_request","request_id":id,"request":request})
}

/// 界面发来的一条输入落到哪儿。
enum Outgoing {
    /// 提示词：写给 Claude Code 的消息，连同回显给界面、记进线程历史的那条
    Prompt(Value, HistoryEntry),
    /// 其余写给 Claude Code 的（权限答复、打断、设置）
    Control(Value),
    /// 不用惊动 Claude Code，直接回给界面（附件不完整、设置值不认识……）
    Reply(Event),
}

/// 附件 → 消息内容块。图片、PDF 原样交 base64；文本解码后包上文件名成一段文字。
fn attachment_block(attachment: &ClaudeAttachment, data: String) -> Result<Value> {
    let media = attachment.media_type.as_str();
    Ok(match media {
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" => {
            json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}})
        }
        "application/pdf" => json!({
            "type": "document",
            "title": attachment.name,
            "source": {"type":"base64","media_type":media,"data":data},
        }),
        "text/plain" => {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data.as_bytes())
                .context("附件编码无效")?;
            anyhow::ensure!(
                bytes.len() <= MAX_TEXT_ATTACHMENT_BYTES,
                "文本附件 {} 太大",
                attachment.name
            );
            let text = String::from_utf8(bytes)
                .with_context(|| format!("附件 {} 不是 UTF-8 文本", attachment.name))?;
            let name = attachment.name.replace('"', "'");
            json!({"type":"text","text":format!("<attachment name=\"{name}\">\n{text}\n</attachment>")})
        }
        _ => bail!("不支持的附件类型：{media}"),
    })
}

/// 工具卡片的标题行与补充行最多多长（按字符）。宿主机截好，界面只管画一行。
const TOOL_TITLE_CHARS: usize = 200;
/// 工具结果只带开头这么多行 / 字符：界面默认只露两行，展开也不是用来读整份输出的。
const TOOL_OUTPUT_LINES: usize = 12;
const TOOL_OUTPUT_CHARS: usize = 2000;

fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// 多行的值（多行命令、长描述）只取第一行，后面还有就补省略号。
fn one_line(text: &str) -> String {
    let mut lines = text.trim().lines();
    let first = clip(lines.next().unwrap_or_default().trim(), TOOL_TITLE_CHARS);
    if lines.next().is_some() && !first.ends_with('…') {
        format!("{first} …")
    } else {
        first
    }
}

/// 从 tool_use 块提炼卡片的一行标题。认识的工具取最能说明它在干什么的字段；不认识的
/// （MCP 工具等）取第一个字符串参数，都没有就空着，界面退回只显示工具名。
fn tool_call(block: &Value) -> ClaudeToolCall {
    let name = block["name"].as_str().unwrap_or_default().to_owned();
    let input = &block["input"];
    let field = |key: &str| input[key].as_str().map(one_line).filter(|s| !s.is_empty());
    let (title, detail) = match name.as_str() {
        "Bash" | "PowerShell" => match (field("description"), field("command")) {
            (Some(description), command) => (Some(description), command),
            (None, command) => (command, None),
        },
        "Read" | "Write" | "Edit" | "MultiEdit" => (field("file_path"), None),
        "NotebookEdit" => (field("notebook_path"), None),
        "Grep" | "Glob" => (field("pattern"), field("path")),
        "WebFetch" => (field("url"), None),
        "WebSearch" => (field("query"), None),
        "Task" | "Agent" => (field("description"), field("subagent_type")),
        // 待办清单：标题取正在做的那一项
        "TodoWrite" => (
            input["todos"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|todo| todo["status"] == "in_progress")
                .and_then(|todo| todo["activeForm"].as_str().or(todo["content"].as_str()))
                .map(one_line),
            None,
        ),
        _ => (
            input
                .as_object()
                .and_then(|map| map.values().find_map(Value::as_str))
                .map(one_line)
                .filter(|s| !s.is_empty()),
            None,
        ),
    };
    ClaudeToolCall {
        id: block["id"].as_str().unwrap_or_default().to_owned(),
        name,
        title: title.unwrap_or_default(),
        detail,
    }
}

/// tool_result 的 content 可能是字符串，也可能是块数组（文本、图片……），只取文本。
fn tool_output(content: &Value) -> String {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let mut lines = text.trim().lines();
    let head = lines.by_ref().take(TOOL_OUTPUT_LINES).collect::<Vec<_>>().join("\n");
    let mut out = clip(&head, TOOL_OUTPUT_CHARS);
    if lines.next().is_some() && !out.ends_with('…') {
        out.push_str("\n…");
    }
    out
}

impl Conversation {
    fn input(&mut self, input: ClaudeInput) -> Option<Outgoing> {
        let warn = |message: String| Some(Outgoing::Reply(Event::Warning { message }));
        // 提示词交不出去：界面发出去就进了「工作中」，回一个出错的 Done 让这一轮收尾。
        // 不进历史（没有 User 回显），线程的输入框里那段文字还留着，补上附件再发就行
        let fail = |message: String| {
            Some(Outgoing::Reply(Event::Done {
                error: Some(message),
                duration_ms: None,
            }))
        };
        match input {
            ClaudeInput::Prompt { text, attachments }
                if self.ready
                    && !self.busy
                    && (!text.trim().is_empty() || !attachments.is_empty()) =>
            {
                // 用上的、没用上的附件一并清掉：一条提示词之后不该还有人引用它们
                let mut uploads = std::mem::take(&mut self.uploads);
                self.upload_chars = 0;
                let content = if attachments.is_empty() {
                    json!(text)
                } else {
                    let mut blocks = Vec::new();
                    for attachment in &attachments {
                        let Some(data) = uploads.remove(&attachment.id) else {
                            return fail(format!("附件 {} 没有传完，请重新添加", attachment.name));
                        };
                        match attachment_block(attachment, data) {
                            Ok(block) => blocks.push(block),
                            Err(err) => return fail(format!("{err:#}")),
                        }
                    }
                    if !text.trim().is_empty() {
                        blocks.push(json!({"type":"text","text":text}));
                    }
                    Value::Array(blocks)
                };
                self.busy = true;
                Some(Outgoing::Prompt(
                    json!({"type":"user","message":{"role":"user","content":content}}),
                    HistoryEntry::User {
                        text,
                        attachments: attachments.into_iter().map(|a| a.name).collect(),
                    },
                ))
            }
            ClaudeInput::Upload { id, data } => {
                self.upload_chars += data.len();
                if self.upload_chars > MAX_UPLOAD_CHARS {
                    self.uploads.clear();
                    self.upload_chars = 0;
                    return warn("附件太大，已全部丢弃".into());
                }
                self.uploads.entry(id).or_default().push_str(&data);
                None
            }
            ClaudeInput::Permission { id, allow } => {
                let input = self.permissions.remove(&id)?;
                let answer = if allow {
                    json!({"behavior":"allow","updatedInput":input})
                } else {
                    json!({"behavior":"deny","message":"User denied this request"})
                };
                Some(Outgoing::Control(
                    json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":answer}}),
                ))
            }
            // 没在跑就没什么可打断的；它的 control_response 落到 event 的兜底分支里，不转给界面
            ClaudeInput::Interrupt if self.busy => Some(Outgoing::Control(control(
                "falcon-interrupt",
                json!({"subtype":"interrupt"}),
            ))),
            ClaudeInput::SetModel { model } if self.ready => {
                Some(Outgoing::Control(self.change(Change::Model(model))))
            }
            ClaudeInput::SetEffort { effort } if self.ready => {
                if !EFFORT_LEVELS.contains(&effort.as_str()) {
                    return warn(format!("不认识的 effort：{effort}"));
                }
                Some(Outgoing::Control(self.change(Change::Effort(effort))))
            }
            ClaudeInput::SetPermissionMode { mode } if self.ready => {
                if !PERMISSION_MODES.contains(&mode.as_str()) {
                    return warn(format!("不认识的权限模式：{mode}"));
                }
                Some(Outgoing::Control(self.change(Change::Mode(mode))))
            }
            _ => None,
        }
    }

    /// 设置类的控制请求。Claude Code 答应了（event 里收到 control_response）才改 options，
    /// 不答应推 Warning。
    fn change(&mut self, change: Change) -> Value {
        self.next_request += 1;
        let id = format!("falcon-set-{}", self.next_request);
        let request = match &change {
            // 空串 = 回到默认模型：set_model 不带 model 就是这个意思
            Change::Model(model) if model.is_empty() => json!({"subtype":"set_model"}),
            Change::Model(model) => json!({"subtype":"set_model","model":model}),
            // effort 没有专门的控制请求，走会话级的 flag settings（不写用户的 settings.json）
            Change::Effort(effort) => {
                json!({"subtype":"apply_flag_settings","settings":{"effortLevel":effort}})
            }
            Change::Mode(mode) => json!({"subtype":"set_permission_mode","mode":mode}),
        };
        self.pending.insert(id.clone(), change);
        control(&id, request)
    }

    /// Ready 之后：先把线程上存着的选择恢复回去，再问一遍实际生效的设置与上下文用量。
    /// 恢复走控制请求而不是启动参数：存着的模型下线了也只是一条 Warning，不至于整个线程起不来。
    fn after_ready(&mut self) -> Vec<Value> {
        let chosen = self.chosen.clone();
        let mut out = Vec::new();
        if let Some(model) = chosen.model {
            out.push(self.change(Change::Model(model)));
        }
        if let Some(effort) = chosen.effort {
            out.push(self.change(Change::Effort(effort)));
        }
        if let Some(mode) = chosen
            .permission_mode
            .filter(|mode| *mode != self.options.permission_mode)
        {
            out.push(self.change(Change::Mode(mode)));
        }
        out.push(control(SETTINGS_REQUEST, json!({"subtype":"get_settings"})));
        out.push(control(CONTEXT_REQUEST, json!({"subtype":"get_context_usage"})));
        out
    }

    fn take_requests(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.requests)
    }

    /// 设置请求的答复，返回要推给界面的事件。不是我们发的请求返回 None
    fn settled(&mut self, id: &str, response: &Value) -> Option<Event> {
        let change = self.pending.remove(id)?;
        if response["subtype"] != "success" {
            let error = response["error"].as_str().unwrap_or("未知错误");
            return Some(Event::Warning {
                message: format!("Claude Code 没有接受这项设置：{error}"),
            });
        }
        match change {
            Change::Model(model) => {
                let model = Some(model).filter(|m| !m.is_empty());
                self.options.model = model.clone();
                self.chosen.model = model;
                // 换了模型，实际生效的模型名、effort 与上下文窗口都可能跟着变
                self.requests
                    .push(control(SETTINGS_REQUEST, json!({"subtype":"get_settings"})));
                self.requests
                    .push(control(CONTEXT_REQUEST, json!({"subtype":"get_context_usage"})));
            }
            Change::Effort(effort) => {
                self.options.effort = Some(effort.clone());
                self.chosen.effort = Some(effort);
            }
            Change::Mode(mode) => {
                let mode = response["response"]["mode"].as_str().unwrap_or(&mode).to_owned();
                self.set_mode(mode);
            }
        }
        Some(Event::Options(self.options.clone()))
    }

    fn set_mode(&mut self, mode: String) {
        // 跳过权限只对这一次打开有效：接回线程时不该悄悄就是不问权限的状态
        self.chosen.permission_mode = (mode != "bypassPermissions").then(|| mode.clone());
        self.options.permission_mode = mode;
    }

    /// 一行输出可能对应多个事件：一条 user 消息里可以装着好几个并行工具的结果。
    fn events(&mut self, v: &Value) -> Result<Vec<Event>> {
        if v["type"] == "user" && v["parent_tool_use_id"].is_null() {
            return Ok(v["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|block| block["type"] == "tool_result")
                .filter_map(|block| {
                    let id = block["tool_use_id"].as_str()?.to_owned();
                    let elapsed_ms = self
                        .tools
                        .remove(&id)
                        .map(|start| start.elapsed().as_millis() as u64);
                    Some(Event::ToolResult {
                        id,
                        error: block["is_error"] == true,
                        output: tool_output(&block["content"]),
                        elapsed_ms,
                    })
                })
                .collect());
        }
        Ok(self.event(v)?.into_iter().collect())
    }

    fn event(&mut self, v: &Value) -> Result<Option<Event>> {
        Ok(match v["type"].as_str() {
            Some("control_response") if v["response"]["request_id"] == "falcon-init" => {
                anyhow::ensure!(
                    v["response"]["subtype"] == "success",
                    "Claude Code 初始化失败：{}",
                    v["response"]["error"]
                );
                self.ready = true;
                let init = &v["response"]["response"];
                self.options.models = init["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| {
                        Some(ClaudeModel {
                            value: m["value"].as_str()?.to_owned(),
                            display_name: m["displayName"].as_str().unwrap_or_default().to_owned(),
                            description: m["description"].as_str().unwrap_or_default().to_owned(),
                            resolved_model: m["resolvedModel"].as_str().map(str::to_owned),
                            effort_levels: if m["supportsEffort"] == true {
                                m["supportedEffortLevels"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|l| l.as_str().map(str::to_owned))
                                    .collect()
                            } else {
                                Vec::new()
                            },
                        })
                    })
                    .collect();
                self.options.permission_mode = init["current_permission_mode"]
                    .as_str()
                    .unwrap_or("default")
                    .to_owned();
                Some(Event::Ready)
            }
            Some("control_response") if v["response"]["request_id"] == SETTINGS_REQUEST => {
                if v["response"]["subtype"] != "success" {
                    return Ok(None);
                }
                let applied = &v["response"]["response"]["applied"];
                self.options.resolved_model = applied["model"].as_str().map(str::to_owned);
                self.options.effort = applied["effort"].as_str().map(str::to_owned);
                Some(Event::Options(self.options.clone()))
            }
            Some("control_response") if v["response"]["request_id"] == CONTEXT_REQUEST => {
                let usage = &v["response"]["response"];
                match (usage["totalTokens"].as_u64(), usage["maxTokens"].as_u64()) {
                    (Some(used), Some(max)) if v["response"]["subtype"] == "success" && max > 0 => {
                        Some(Event::Context { used, max })
                    }
                    _ => None,
                }
            }
            Some("control_response") => {
                let id = v["response"]["request_id"].as_str().unwrap_or_default().to_owned();
                self.settled(&id, &v["response"])
            }
            // 权限模式也会由 Claude Code 自己改（计划模式里批准了计划就退出），从状态消息里跟上
            Some("system") if v["subtype"] == "status" => match v["permissionMode"].as_str() {
                Some(mode) if mode != self.options.permission_mode => {
                    self.set_mode(mode.to_owned());
                    Some(Event::Options(self.options.clone()))
                }
                _ => None,
            },
            Some("control_request") if v["request"]["subtype"] == "can_use_tool" => {
                let id = v["request_id"]
                    .as_str()
                    .context("Claude Code 权限请求缺少 ID")?
                    .to_string();
                let input = v["request"]["input"].clone();
                self.permissions.insert(id.clone(), input.clone());
                Some(Event::Permission {
                    id,
                    tool: v["request"]["tool_name"].as_str().unwrap_or("").into(),
                    input,
                })
            }
            Some("control_cancel_request") => {
                let id = v["request_id"].as_str().unwrap_or_default().to_string();
                self.permissions.remove(&id);
                Some(Event::CancelPermission { id })
            }
            Some("stream_event")
                if v["parent_tool_use_id"].is_null()
                    && v["event"]["delta"]["type"] == "text_delta" =>
            {
                v["event"]["delta"]["text"]
                    .as_str()
                    .map(|s| Event::Delta { text: s.into() })
            }
            Some("assistant") if v["parent_tool_use_id"].is_null() => {
                let mut text = Vec::new();
                let mut calls = Vec::new();
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => text.extend(block["text"].as_str()),
                        Some("tool_use") => {
                            let call = tool_call(block);
                            self.tools.insert(call.id.clone(), Instant::now());
                            calls.push(call);
                        }
                        _ => {}
                    }
                }
                Some(Event::Assistant {
                    text: text.join("\n\n"),
                    tools: calls.iter().map(|call| call.name.clone()).collect(),
                    calls,
                })
            }
            Some("result") => {
                self.busy = false;
                self.permissions.clear();
                self.tools.clear();
                if self.ready {
                    self.requests
                        .push(control(CONTEXT_REQUEST, json!({"subtype":"get_context_usage"})));
                }
                let error = (v["is_error"] == true || v["subtype"] != "success").then(|| {
                    v["result"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| {
                            v["errors"].as_array().map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| v["subtype"].to_string())
                });
                Some(Event::Done {
                    error,
                    duration_ms: v["duration_ms"].as_u64(),
                })
            }
            _ => None,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum HistoryEntry {
    User {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    Assistant {
        text: String,
        tools: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        calls: Vec<ClaudeToolCall>,
    },
    ToolResult {
        id: String,
        error: bool,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
    },
    Partial {
        text: String,
    },
    Done {
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
}

impl From<HistoryEntry> for Event {
    fn from(entry: HistoryEntry) -> Self {
        match entry {
            HistoryEntry::User { text, attachments } => Self::User { text, attachments },
            HistoryEntry::Assistant { text, tools, calls } => Self::Assistant { text, tools, calls },
            HistoryEntry::ToolResult {
                id,
                error,
                output,
                elapsed_ms,
            } => Self::ToolResult {
                id,
                error,
                output,
                elapsed_ms,
            },
            HistoryEntry::Partial { text } => Self::Assistant {
                text,
                tools: Vec::new(),
                calls: Vec::new(),
            },
            HistoryEntry::Done { error, duration_ms } => Self::Done { error, duration_ms },
        }
    }
}

struct ThreadHistory {
    id: String,
    file: fs::File,
    // 线程上选过的模型 / effort / 权限模式，与历史放在一起（`<id>.options.json`）
    options_path: PathBuf,
    options: SavedOptions,
    _writer: File,
    started: bool,
    has_reply: bool,
    pending: bool,
    draft: String,
}

pub(super) fn lock_thread(data_dir: &FsPath, id: &str) -> Result<File> {
    anyhow::ensure!(
        id.len() == 36
            && id.bytes().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            }),
        "线程 ID 无效"
    );
    let id = id.to_ascii_lowercase();
    let dir = data_dir.join("claude-threads");
    std::fs::create_dir_all(&dir)?;
    let writer = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{id}.lock")))?;
    match writer.try_lock() {
        Ok(()) => Ok(writer),
        Err(std::fs::TryLockError::WouldBlock) => bail!("该线程已在另一个对话窗口中打开"),
        Err(std::fs::TryLockError::Error(err)) => Err(err).context("无法锁定线程对话"),
    }
}

impl ThreadHistory {
    async fn open(data_dir: &FsPath, id: &str, writer: File) -> Result<Self> {
        let id = id.to_ascii_lowercase();
        let dir = data_dir.join("claude-threads");
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(dir.join(format!("{id}.jsonl"))).await?;
        let options_path = dir.join(format!("{id}.options.json"));
        // 读不出来就当没选过：不值得为一份偏好把整条线程挡在外面
        let saved = fs::read(&options_path)
            .await
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            id,
            file,
            options_path,
            options: saved,
            _writer: writer,
            started: false,
            has_reply: false,
            pending: false,
            draft: String::new(),
        })
    }

    fn observe(&mut self, entry: &HistoryEntry) {
        match entry {
            HistoryEntry::User { .. } => {
                self.started = true;
                self.pending = true;
            }
            HistoryEntry::Assistant { .. } => self.has_reply = true,
            HistoryEntry::ToolResult { .. } | HistoryEntry::Partial { .. } => {}
            HistoryEntry::Done { error, .. } => {
                self.pending = false;
                self.has_reply |= error.is_none();
            }
        }
    }

    async fn replay(&mut self, socket: &mut WebSocket) -> Result<()> {
        send(socket, Event::ThreadHistory).await?;
        let mut reader = BufReader::new(self.file.try_clone().await?);
        let mut line = Vec::new();
        let mut offset = 0;
        loop {
            line.clear();
            reader.read_until(b'\n', &mut line).await?;
            // 进程在追加途中退出时，只丢弃末尾尚未写完的一行。
            if !line.ends_with(b"\n") {
                break;
            }
            offset += line.len() as u64;
            let entry = serde_json::from_slice::<HistoryEntry>(&line)?;
            self.observe(&entry);
            send(socket, entry.into()).await?;
        }
        drop(reader);
        self.file.set_len(offset).await?;
        self.file.seek(std::io::SeekFrom::Start(offset)).await?;
        if self.pending {
            let entry = HistoryEntry::Done {
                error: Some("上一次回复已中断".into()),
                duration_ms: None,
            };
            self.append(&entry).await?;
            send(socket, entry.into()).await?;
        }
        Ok(())
    }

    async fn record(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::Delta { text } => self.draft.push_str(text),
            Event::Assistant { text, tools, calls } => {
                self.draft.clear();
                self.append(&HistoryEntry::Assistant {
                    text: text.clone(),
                    tools: tools.clone(),
                    calls: calls.clone(),
                })
                .await?;
            }
            Event::ToolResult {
                id,
                error,
                output,
                elapsed_ms,
            } => {
                self.append(&HistoryEntry::ToolResult {
                    id: id.clone(),
                    error: *error,
                    output: output.clone(),
                    elapsed_ms: *elapsed_ms,
                })
                .await?;
            }
            Event::Done { error, duration_ms } => {
                self.save_partial().await?;
                self.append(&HistoryEntry::Done {
                    error: error.clone(),
                    duration_ms: *duration_ms,
                })
                .await?;
            }
            _ => {}
        }
        Ok(())
    }

    async fn save_options(&mut self, options: &SavedOptions) -> Result<()> {
        if *options == self.options {
            return Ok(());
        }
        // 先写临时文件再改名，写到一半退出也不会留下半份 JSON
        let tmp = self.options_path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(options)?).await?;
        fs::rename(&tmp, &self.options_path).await?;
        self.options = options.clone();
        Ok(())
    }

    async fn save_partial(&mut self) -> Result<()> {
        if !self.draft.is_empty() {
            let text = std::mem::take(&mut self.draft);
            self.append(&HistoryEntry::Partial { text }).await?;
        }
        Ok(())
    }

    async fn append(&mut self, entry: &HistoryEntry) -> Result<()> {
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        self.file.write_all(&line).await?;
        self.file.flush().await?;
        self.file.sync_data().await?;
        self.observe(entry);
        Ok(())
    }
}

struct Cli {
    child: Child,
    stdin: ChildStdin,
    lines: FramedRead<ChildStdout, LinesCodec>,
    chat: Conversation,
}

impl Cli {
    fn spawn(command: &mut Command, session: Option<(&str, bool)>) -> Result<Self> {
        if let Some((id, resume)) = session {
            command
                .arg(if resume { "--resume" } else { "--session-id" })
                .arg(id);
        }
        let mut child = command
            .args([
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-prompt-tool",
                "stdio",
                // 只是让「跳过权限」能被选上，默认不生效；不带它 set_permission_mode 会拒绝
                "--allow-dangerously-skip-permissions",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("启动 Claude Code 失败")?;
        let stdin = child.stdin.take().unwrap();
        let lines = FramedRead::new(
            child.stdout.take().unwrap(),
            LinesCodec::new_with_max_length(16 * 1024 * 1024),
        );
        Ok(Self {
            child,
            stdin,
            lines,
            chat: Conversation::default(),
        })
    }

    // --resume 找不到记录会在初始化之前退出。这时尚未收到任何用户提示词。
    async fn initialize(&mut self, resume: Option<&str>) -> Result<bool> {
        let init = write(&mut self.stdin, json!({"type":"control_request","request_id":"falcon-init","request":{"subtype":"initialize"}})).await;
        if let Err(err) = init {
            if resume.is_none()
                || !err
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|err| err.kind() == std::io::ErrorKind::BrokenPipe)
            {
                return Err(err);
            }
        }
        while let Some(line) = self.lines.next().await {
            let value = serde_json::from_str::<Value>(&line?)?;
            let event = self.receive(&value).await?.pop();
            if let Some(Event::Ready) = event {
                return Ok(true);
            }
            if let Some(Event::Done {
                error: Some(error), ..
            }) = event
            {
                if let Some(id) = resume {
                    let absent = format!("No conversation found with session ID: {id}");
                    if value["session_id"] == id
                        && value["num_turns"] == 0
                        && value["errors"]
                            .as_array()
                            .is_some_and(|errors| errors.iter().any(|error| error == &absent))
                    {
                        return Ok(false);
                    }
                }
                bail!("Claude Code 初始化失败：{error}");
            }
        }
        self.output_closed()
    }

    fn output_closed<T>(&mut self) -> Result<T> {
        match self.child.try_wait()? {
            Some(status) => bail!("Claude Code 已退出（{status}）"),
            None => bail!("Claude Code 已关闭输出"),
        }
    }

    async fn receive(&mut self, value: &Value) -> Result<Vec<Event>> {
        if value["type"] == "control_request" && value["request"]["subtype"] != "can_use_tool" {
            write(&mut self.stdin, json!({"type":"control_response","response":{"subtype":"error","request_id":value["request_id"],"error":"Unsupported control request"}})).await?;
            return Ok(Vec::new());
        }
        self.chat.events(value)
    }

    async fn drive(
        &mut self,
        socket: &mut WebSocket,
        mut history: Option<&mut ThreadHistory>,
    ) -> Result<()> {
        send(socket, Event::Ready).await?;
        if let Some(history) = history.as_deref() {
            self.chat.chosen = history.options.clone();
        }
        for request in self.chat.after_ready() {
            write(&mut self.stdin, request).await?;
        }
        loop {
            tokio::select! {
                frame = socket.recv() => match frame {
                    Some(Ok(Message::Text(msg))) => match self.chat.input(serde_json::from_str(&msg)?) {
                        Some(Outgoing::Prompt(input, entry)) => {
                            if let Some(history) = history.as_deref_mut() {
                                history.append(&entry).await?;
                                write(&mut self.stdin, input).await?;
                                if send(socket, entry.into()).await.is_err() {
                                    return Ok(());
                                }
                            } else {
                                write(&mut self.stdin, input).await?;
                            }
                        }
                        Some(Outgoing::Control(input)) => write(&mut self.stdin, input).await?,
                        Some(Outgoing::Reply(event)) => {
                            if let Err(err) = send(socket, event).await {
                                return if history.is_some() { Ok(()) } else { Err(err) };
                            }
                        }
                        None => {}
                    },
                    None | Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Err(_)) if history.is_some() => return Ok(()),
                    Some(Err(err)) => return Err(err.into()),
                    _ => {}
                },
                line = self.lines.next() => {
                    let Some(line) = line else {
                        if let Some(history) = history.as_deref_mut() {
                            history.save_partial().await?;
                        }
                        return self.output_closed();
                    };
                    let value: Value = serde_json::from_str(&line?)?;
                    for event in self.receive(&value).await? {
                        if let Some(history) = history.as_deref_mut() {
                            history.record(&event).await?;
                            if matches!(event, Event::Options(_)) {
                                history.save_options(&self.chat.chosen).await?;
                            }
                        }
                        if let Err(err) = send(socket, event).await {
                            return if history.is_some() { Ok(()) } else { Err(err) };
                        }
                    }
                    for request in self.chat.take_requests() {
                        write(&mut self.stdin, request).await?;
                    }
                }
            }
        }
    }

    async fn stop(mut self, history: Option<&mut ThreadHistory>) -> Result<()> {
        // 输入 EOF 后短暂读完已在途的输出，等子进程真正退出再释放写锁。
        drop(self.stdin);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let result = async {
            if let Some(history) = history.filter(|history| history.pending) {
                while let Ok(Some(line)) =
                    tokio::time::timeout_at(deadline, self.lines.next()).await
                {
                    let value = serde_json::from_str::<Value>(&line?)?;
                    for event in self.chat.events(&value)? {
                        history.record(&event).await?;
                    }
                    if !history.pending {
                        break;
                    }
                }
                history.save_partial().await?;
            }
            Ok(())
        }
        .await;
        if tokio::time::timeout_at(deadline, self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        result
    }
}

async fn wait_for_disconnect(socket: &mut WebSocket) {
    while let Some(Ok(frame)) = socket.recv().await {
        if matches!(frame, Message::Close(_)) {
            break;
        }
    }
}

async fn run(state: AppState, id: String, socket: &mut WebSocket) -> Result<()> {
    let mut row = owner_row(&state.db, &id).context("项目不存在")?;
    let writer = if row.thread_environment_id.is_some() {
        let writer = lock_thread(&state.config.data_dir, &row.id)?;
        // 删除和打开用同一把锁，防止读到的线程恰好已被删除。
        row = state
            .db
            .get_project(&id)
            .filter(|row| row.thread_environment_id.is_some())
            .context("线程不存在")?;
        Some(writer)
    } else {
        None
    };
    anyhow::ensure!(
        row.project_type == "local" && row.worktree_archived_at.is_none(),
        "请在所属节点的活动项目中打开对话"
    );
    let history = if let Some(writer) = writer {
        let mut history = ThreadHistory::open(&state.config.data_dir, &row.id, writer).await?;
        history.replay(socket).await?;
        Some(history)
    } else {
        None
    };
    let cwd: PathBuf = if let Some(dir) = row.working_dir.clone() {
        dir.into()
    } else if row.multi_repos.is_some() && row.source_project_id.is_none() {
        let row_for_vdir = row.clone();
        state
            .engine
            .call(move |e| async move { e.sessions.ensure_virtual_dir(&row_for_vdir, true).await })
            .await??
            .into()
    } else if history.is_some() {
        bail!("线程缺少环境工作目录");
    } else {
        crate::config::home_dir()
    };
    let (program, env) = local_agent_command(falcon_proto::SessionAgent::Claude)
        .await
        .context("宿主机未找到 Claude Code")?;
    let command = || claude_command(&program, &cwd, &env);
    run_cli(socket, history, command).await
}

/// 构造启动 Claude Code 子进程的命令。
///
/// 显式 `env_clear()` 后再写入解析出的基底环境并过滤外层会话运行态标记，避免仅移除
/// `CLAUDECODE` 时仍从服务端进程继承 `CLAUDE_CODE_CHILD_SESSION` 等变量，导致 Claude Code
/// 关闭会话记录落盘并影响 `--resume`。
fn claude_command(
    program: &FsPath,
    cwd: &FsPath,
    env: &[(String, String)],
) -> Command {
    let mut command = Command::new(program);
    command
        .no_window()
        .current_dir(cwd)
        .env_clear()
        .envs(env.iter().filter(|(k, _)| !is_inherited_session_env(k)).cloned());
    command
}

async fn run_cli(
    socket: &mut WebSocket,
    mut history: Option<ThreadHistory>,
    command: impl Fn() -> Command,
) -> Result<()> {
    let mut resume = history.as_ref().is_some_and(|history| history.started);
    loop {
        let session = history
            .as_ref()
            .map(|history| (history.id.as_str(), resume));
        let mut cli = Cli::spawn(&mut command(), session)?;
        let initialized = tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(45), cli.initialize(session.filter(|(_, resume)| *resume).map(|(id, _)| id))) => {
                Some(result.context("等待 Claude Code 初始化超时").and_then(|result| result))
            }
            _ = wait_for_disconnect(socket) => None,
        };
        let result = match initialized {
            None => Ok(()),
            Some(Ok(true)) => cli.drive(socket, history.as_mut()).await,
            Some(Ok(false)) if history.as_ref().is_some_and(|history| !history.has_reply) => {
                cli.stop(None).await?;
                resume = false;
                continue;
            }
            Some(Ok(false)) => Err(anyhow::anyhow!("Claude Code 找不到此线程的原始对话记录")),
            Some(Err(err)) => Err(err),
        };
        if history.is_some() {
            let drain = if result.is_ok() {
                history.as_mut()
            } else {
                None
            };
            let stopped = cli.stop(drain).await;
            return result.and(stopped);
        }
        return result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_turns_and_permission_responses() {
        let mut chat = Conversation::default();
        let prompt = || ClaudeInput::Prompt {
            text: "hello".into(),
            attachments: Vec::new(),
        };
        assert!(chat.input(prompt()).is_none());
        chat.event(&json!({"type":"control_response","response":{"request_id":"falcon-init","subtype":"success"}})).unwrap();
        assert!(
            matches!(chat.input(prompt()), Some(Outgoing::Prompt(v, _)) if v["message"]["content"] == "hello")
        );
        assert!(chat.input(prompt()).is_none());
        let request = json!({"type":"control_request","request_id":"a","request":{"subtype":"can_use_tool","tool_name":"Edit","input":{"file_path":"x"}}});
        for allow in [true, false] {
            assert!(matches!(
                chat.event(&request).unwrap(),
                Some(Event::Permission { .. })
            ));
            let decision = || ClaudeInput::Permission {
                id: "a".into(),
                allow,
            };
            let Some(Outgoing::Control(response)) = chat.input(decision()) else {
                panic!("expected control response");
            };
            let response = &response["response"]["response"];
            if allow {
                assert_eq!(response["updatedInput"], request["request"]["input"]);
            } else {
                assert_eq!(response["behavior"], "deny");
            }
            assert!(chat.input(decision()).is_none());
        }
        chat.event(&request).unwrap();
        chat.event(&json!({"type":"control_cancel_request","request_id":"a"}))
            .unwrap();
        assert!(
            chat.input(ClaudeInput::Permission {
                id: "a".into(),
                allow: true
            })
            .is_none()
        );
        assert!(matches!(
            chat.event(&json!({"type":"result","subtype":"success"}))
                .unwrap(),
            Some(Event::Done { error: None, .. })
        ));
        assert!(chat.input(ClaudeInput::Interrupt).is_none());
        assert!(chat.input(prompt()).is_some());
        assert!(chat.input(ClaudeInput::Interrupt).is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn thread_history_reopens_resumes_and_locks_the_writer() {
        use futures::SinkExt as _;
        use tokio::net::{TcpListener, TcpStream};
        use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite};

        type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;
        async fn receive(client: &mut Client) -> Event {
            let message = tokio::time::timeout(Duration::from_secs(5), client.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            serde_json::from_str(message.to_text().unwrap()).unwrap()
        }
        async fn prompt(client: &mut Client, text: &str) {
            let input = ClaudeInput::Prompt { text: text.into(), attachments: Vec::new() };
            client
                .send(tungstenite::Message::Text(
                    serde_json::to_string(&input).unwrap().into(),
                ))
                .await
                .unwrap();
        }
        async fn listen(root: &FsPath) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = Router::new().route("/{id}", get({
                let root = root.to_path_buf();
                move |Path(id): Path<String>, ws: WebSocketUpgrade| {
                    let root = root.clone();
                    async move {
                        ws.on_upgrade(|mut socket| async move {
                            let result = async {
                                let lock = lock_thread(&root, &id)?;
                                let mut history = ThreadHistory::open(&root, &id, lock).await?;
                                history.replay(&mut socket).await?;
                                run_cli(&mut socket, Some(history), || {
                                    let mut command = Command::new("/bin/sh");
                                    command.current_dir(&root).args(["-c", r#"
                                        if [ "$1" = --resume ] && [ ! -f "$2.session" ]; then
                                            printf '{"type":"result","is_error":true,"subtype":"error_during_execution","num_turns":0,"session_id":"%s","errors":["No conversation found with session ID: %s"]}\n' "$2" "$2"
                                            exit 1
                                        fi
                                        IFS= read -r init || exit 1
                                        printf '%s\n' '{"type":"control_response","response":{"request_id":"falcon-init","subtype":"success"}}'
                                        while IFS= read -r input; do
                                            case "$input" in
                                                # Ready 之后问设置与上下文用量的控制请求：不答，界面上什么也不多
                                                *'"type":"control_request"'*)
                                                    ;;
                                                *'"content":"interrupt"'*)
                                                    printf '%s\n' '{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"partial"}}}'
                                                    ;;
                                                *)
                                                    touch "$2.session"
                                                    printf '{"type":"assistant","message":{"content":[{"type":"text","text":"%s %s"}]}}\n' "$1" "$2"
                                                    printf '%s\n' '{"type":"result","subtype":"success"}'
                                                    ;;
                                            esac
                                        done
                                    "#, "fake-claude"]);
                                    command
                                }).await
                            }.await;
                            if let Err(err) = result {
                                let _ = send(&mut socket, Event::Closed { reason: err.to_string() }).await;
                            }
                        })
                    }
                }
            }));
            (
                address,
                tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
            )
        }
        async fn connect(address: std::net::SocketAddr, id: &str) -> Client {
            tokio_tungstenite::connect_async(format!("ws://{address}/{id}"))
                .await
                .unwrap()
                .0
        }

        async fn await_release(root: &FsPath, id: &str) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while lock_thread(root, id).is_err() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }

        let root = tempfile::tempdir().unwrap();
        let first = "d13f3279-478f-44b8-8946-c1cd27d45b9d";
        let second = "3b365aa2-8b07-4dae-a2a1-d73a7445727b";
        let (address, server) = listen(root.path()).await;
        let mut writer = connect(address, first).await;
        assert!(matches!(receive(&mut writer).await, Event::ThreadHistory));
        assert!(matches!(receive(&mut writer).await, Event::Ready));
        let mut duplicate = connect(address, first).await;
        assert!(matches!(
            receive(&mut duplicate).await,
            Event::Closed { .. }
        ));
        let mut other = connect(address, second).await;
        assert!(matches!(receive(&mut other).await, Event::ThreadHistory));
        assert!(matches!(receive(&mut other).await, Event::Ready));
        prompt(&mut other, "interrupt").await;
        assert!(matches!(receive(&mut other).await, Event::User { text, .. } if text == "interrupt"));
        assert!(matches!(receive(&mut other).await, Event::Delta { .. }));
        other.close(None).await.unwrap();

        prompt(&mut writer, "first").await;
        assert!(matches!(receive(&mut writer).await, Event::User { text, .. } if text == "first"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == format!("--session-id {first}"))
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: None, .. }
        ));
        prompt(&mut writer, "interrupt").await;
        assert!(matches!(receive(&mut writer).await, Event::User { text, .. } if text == "interrupt"));
        assert!(matches!(receive(&mut writer).await, Event::Delta { .. }));
        writer.close(None).await.unwrap();
        await_release(root.path(), first).await;
        await_release(root.path(), second).await;
        server.abort();
        let path = root
            .path()
            .join("claude-threads")
            .join(format!("{second}.jsonl"));
        use std::io::Write as _;
        File::options()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"{torn")
            .unwrap();

        let (address, server) = listen(root.path()).await;
        let mut writer = connect(address, first).await;
        assert!(matches!(receive(&mut writer).await, Event::ThreadHistory));
        assert!(matches!(receive(&mut writer).await, Event::User { text, .. } if text == "first"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == format!("--session-id {first}"))
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: None, .. }
        ));
        assert!(matches!(receive(&mut writer).await, Event::User { text, .. } if text == "interrupt"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == "partial")
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: Some(_), .. }
        ));
        assert!(matches!(receive(&mut writer).await, Event::Ready));
        prompt(&mut writer, "follow-up").await;
        assert!(matches!(receive(&mut writer).await, Event::User { text, .. } if text == "follow-up"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == format!("--resume {first}"))
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: None, .. }
        ));
        writer.close(None).await.unwrap();

        let mut other = connect(address, second).await;
        assert!(matches!(receive(&mut other).await, Event::ThreadHistory));
        assert!(matches!(receive(&mut other).await, Event::User { text, .. } if text == "interrupt"));
        assert!(
            matches!(receive(&mut other).await, Event::Assistant { text, .. } if text == "partial")
        );
        assert!(matches!(
            receive(&mut other).await,
            Event::Done { error: Some(_), .. }
        ));
        assert!(matches!(receive(&mut other).await, Event::Ready));
        prompt(&mut other, "retry manually").await;
        assert!(
            matches!(receive(&mut other).await, Event::User { text, .. } if text == "retry manually")
        );
        assert!(
            matches!(receive(&mut other).await, Event::Assistant { text, .. } if text == format!("--session-id {second}"))
        );
        assert!(matches!(
            receive(&mut other).await,
            Event::Done { error: None, .. }
        ));
        other.close(None).await.unwrap();
        await_release(root.path(), first).await;
        await_release(root.path(), second).await;
        server.abort();
    }

    /// 答复的形状照 Claude Code 2.1 实测（initialize / get_settings / get_context_usage /
    /// set_permission_mode 的 control_response 与 system status）
    #[test]
    fn claude_options_attachments_and_context() {
        let mut chat = Conversation {
            chosen: SavedOptions {
                model: Some("sonnet".into()),
                permission_mode: Some("plan".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(matches!(
            chat.event(&json!({"type":"control_response","response":{"request_id":"falcon-init","subtype":"success","response":{
                "current_permission_mode":"default",
                "models":[
                    {"value":"default","displayName":"Default (recommended)","resolvedModel":"m-1","supportsEffort":true,"supportedEffortLevels":["low","high"]},
                    {"value":"haiku","displayName":"Haiku","supportsEffort":false}
                ]
            }}}))
            .unwrap(),
            Some(Event::Ready)
        ));
        assert_eq!(chat.options.models[0].effort_levels, ["low", "high"]);
        assert!(chat.options.models[1].effort_levels.is_empty());

        // 恢复存着的模型与权限模式，再问设置与用量
        let requests = chat.after_ready();
        let subtypes: Vec<_> = requests.iter().map(|r| r["request"]["subtype"].as_str().unwrap()).collect();
        assert_eq!(subtypes, ["set_model", "set_permission_mode", "get_settings", "get_context_usage"]);
        let model_id = requests[0]["request_id"].as_str().unwrap().to_owned();
        assert!(matches!(
            chat.event(&json!({"type":"control_response","response":{"request_id":model_id,"subtype":"success"}})).unwrap(),
            Some(Event::Options(o)) if o.model.as_deref() == Some("sonnet")
        ));
        // 换完模型要重新问一遍实际生效的设置
        assert_eq!(chat.take_requests().len(), 2);
        let settings = chat
            .event(&json!({"type":"control_response","response":{"request_id":"falcon-settings","subtype":"success","response":{"applied":{"model":"m-2","effort":"high"}}}}))
            .unwrap();
        assert!(matches!(settings, Some(Event::Options(o)) if o.resolved_model.as_deref() == Some("m-2") && o.effort.as_deref() == Some("high")));
        assert!(matches!(
            chat.event(&json!({"type":"control_response","response":{"request_id":"falcon-context","subtype":"success","response":{"totalTokens":10307,"maxTokens":1000000}}})).unwrap(),
            Some(Event::Context { used: 10307, max: 1000000 })
        ));

        // 不认识的值不发给 Claude Code；拒绝了推 Warning，选择不变
        assert!(matches!(
            chat.input(ClaudeInput::SetPermissionMode { mode: "yolo".into() }),
            Some(Outgoing::Reply(Event::Warning { .. }))
        ));
        let Some(Outgoing::Control(bypass)) = chat.input(ClaudeInput::SetPermissionMode { mode: "bypassPermissions".into() }) else {
            panic!("expected control request");
        };
        let refused = json!({"type":"control_response","response":{"request_id":bypass["request_id"],"subtype":"error","error":"nope"}});
        assert!(matches!(chat.event(&refused).unwrap(), Some(Event::Warning { .. })));
        let Some(Outgoing::Control(bypass)) = chat.input(ClaudeInput::SetPermissionMode { mode: "bypassPermissions".into() }) else {
            panic!("expected control request");
        };
        let accepted = json!({"type":"control_response","response":{"request_id":bypass["request_id"],"subtype":"success","response":{"mode":"bypassPermissions"}}});
        assert!(matches!(chat.event(&accepted).unwrap(), Some(Event::Options(o)) if o.permission_mode == "bypassPermissions"));
        // 跳过权限不留到下次打开
        assert_eq!(chat.chosen.permission_mode, None);
        // Claude Code 自己换了模式（退出计划模式之类）
        assert!(matches!(
            chat.event(&json!({"type":"system","subtype":"status","permissionMode":"acceptEdits"})).unwrap(),
            Some(Event::Options(o)) if o.permission_mode == "acceptEdits"
        ));
        assert_eq!(chat.chosen.permission_mode.as_deref(), Some("acceptEdits"));

        let Some(Outgoing::Control(effort)) = chat.input(ClaudeInput::SetEffort { effort: "low".into() }) else {
            panic!("expected control request");
        };
        assert_eq!(effort["request"]["settings"]["effortLevel"], "low");

        // 附件：分段上传，随提示词拼成内容块；文本附件解成正文
        let png = base64::engine::general_purpose::STANDARD.encode([0x89, b'P', b'N', b'G']);
        let (a, b) = png.split_at(3);
        chat.input(ClaudeInput::Upload { id: "1".into(), data: a.into() });
        chat.input(ClaudeInput::Upload { id: "1".into(), data: b.into() });
        chat.input(ClaudeInput::Upload {
            id: "2".into(),
            data: base64::engine::general_purpose::STANDARD.encode("fn main() {}"),
        });
        let attachment = |id: &str, name: &str, media: &str| ClaudeAttachment {
            id: id.into(),
            name: name.into(),
            media_type: media.into(),
        };
        let Some(Outgoing::Prompt(message, HistoryEntry::User { text, attachments })) = chat.input(ClaudeInput::Prompt {
            text: String::new(),
            attachments: vec![attachment("1", "a.png", "image/png"), attachment("2", "main.rs", "text/plain")],
        }) else {
            panic!("expected prompt");
        };
        assert!(text.is_empty());
        assert_eq!(attachments, ["a.png", "main.rs"]);
        let content = &message["message"]["content"];
        assert_eq!(content[0]["source"]["data"], png);
        assert_eq!(content[1]["text"], "<attachment name=\"main.rs\">\nfn main() {}\n</attachment>");
        assert_eq!(content.as_array().unwrap().len(), 2);
        // 上一条用掉了附件：再引用就是没传完
        chat.event(&json!({"type":"result","subtype":"success"})).unwrap();
        assert!(matches!(
            chat.input(ClaudeInput::Prompt { text: "again".into(), attachments: vec![attachment("1", "a.png", "image/png")] }),
            Some(Outgoing::Reply(Event::Done { error: Some(_), .. }))
        ));
        assert!(!chat.busy, "没交出去的提示词不占着这一轮");
    }

    #[test]
    fn claude_separates_partial_and_final_messages() {
        let mut chat = Conversation::default();
        let mut partial =
            json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"hi"}}});
        assert!(
            matches!(chat.event(&partial).unwrap(), Some(Event::Delta { text }) if text == "hi")
        );
        partial["parent_tool_use_id"] = json!("nested");
        assert!(chat.event(&partial).unwrap().is_none());
        let message = json!({"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","name":"Read"}]}});
        assert!(
            matches!(chat.event(&message).unwrap(), Some(Event::Assistant { text, tools, .. }) if text == "hi" && tools == ["Read"])
        );
    }

    #[test]
    fn claude_tool_calls_carry_titles_and_results() {
        let mut chat = Conversation::default();
        let message = json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo fetch\n--locked","description":"拉取依赖"}},
            {"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/a/b.rs"}},
            {"type":"tool_use","id":"t3","name":"mcp__x__y","input":{"n":1,"q":"query"}}
        ]}});
        let Some(Event::Assistant { calls, .. }) = chat.event(&message).unwrap() else {
            panic!("expected assistant");
        };
        assert_eq!(calls[0].title, "拉取依赖");
        assert_eq!(calls[0].detail.as_deref(), Some("cargo fetch …"));
        assert_eq!(calls[1].title, "/a/b.rs");
        assert_eq!(calls[2].title, "query");
        let result = json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":"ok"},
            {"type":"tool_result","tool_use_id":"t2","is_error":true,"content":[{"type":"text","text":"missing"}]}
        ]}});
        let events = chat.events(&result).unwrap();
        assert!(matches!(&events[..], [
            Event::ToolResult { id: a, error: false, output: o1, elapsed_ms: Some(_) },
            Event::ToolResult { id: b, error: true, output: o2, .. },
        ] if a == "t1" && b == "t2" && o1 == "ok" && o2 == "missing"));
        let long = (0..40).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        assert!(tool_output(&json!(long)).ends_with("11\n…"));
        let done = chat
            .event(&json!({"type":"result","subtype":"success","duration_ms":4200}))
            .unwrap();
        assert!(matches!(
            done,
            Some(Event::Done {
                error: None,
                duration_ms: Some(4200)
            })
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn claude_command_clears_inherited_claude_child_session_markers() {
        let dir = tempfile::tempdir().unwrap();
        let env = vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("CLAUDECODE".into(), "1".into()),
            ("CLAUDE_CODE_CHILD_SESSION".into(), "1".into()),
            ("CLAUDE_CODE_SESSION_ID".into(), "outer-session".into()),
            ("CLAUDE_PID".into(), "12345".into()),
            ("FALCON_SESSION_ID".into(), "outer-falcon".into()),
            ("CLAUDE_CODE_MAX_OUTPUT_TOKENS".into(), "128000".into()),
        ];
        let output = claude_command(FsPath::new("/usr/bin/env"), dir.path(), &env)
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let keys: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.split_once('=').map(|(k, _)| k))
            .collect();
        for forbidden in [
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_PID",
            "FALCON_SESSION_ID",
        ] {
            assert!(!keys.contains(&forbidden), "leaked {forbidden}: {stdout}");
        }
        assert!(stdout.lines().any(|line| line == "CLAUDE_CODE_MAX_OUTPUT_TOKENS=128000"));
    }
}
