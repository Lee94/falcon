//! 项目所属节点运行 Claude Code。窗口关闭或网络断开时回收其子进程。
use std::{collections::HashMap, process::Stdio};

use anyhow::{Context as _, Result, bail};
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
    claude::{ClaudeEvent as Event, ClaudeInput},
};
use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio::{
    io::AsyncWriteExt as _,
    process::{ChildStdin, Command},
};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::AppState;
use crate::{
    no_window::NoWindow,
    sessions::{agent_detect::local_agent_command, machine::owner_row},
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
    socket
        .send(Message::Text(serde_json::to_string(&event)?.into()))
        .await?;
    Ok(())
}

async fn write(stdin: &mut ChildStdin, value: Value) -> Result<()> {
    let mut buf = serde_json::to_vec(&value)?;
    buf.push(b'\n');
    stdin.write_all(&buf).await?;
    Ok(())
}

#[derive(Default)]
struct Conversation {
    ready: bool,
    busy: bool,
    permissions: HashMap<String, Value>,
}

impl Conversation {
    fn input(&mut self, input: ClaudeInput) -> Option<Value> {
        match input {
            ClaudeInput::Prompt { text } if self.ready && !self.busy && !text.trim().is_empty() => {
                self.busy = true;
                Some(json!({"type":"user","message":{"role":"user","content":text}}))
            }
            ClaudeInput::Permission { id, allow } => {
                let input = self.permissions.remove(&id)?;
                let answer = if allow {
                    json!({"behavior":"allow","updatedInput":input})
                } else {
                    json!({"behavior":"deny","message":"User denied this request"})
                };
                Some(
                    json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":answer}}),
                )
            }
            _ => None,
        }
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
                Some(Event::Ready)
            }
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
                let mut tools = Vec::new();
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => text.extend(block["text"].as_str()),
                        Some("tool_use") => tools.extend(block["name"].as_str().map(str::to_owned)),
                        _ => {}
                    }
                }
                Some(Event::Assistant {
                    text: text.join("\n\n"),
                    tools,
                })
            }
            Some("result") => {
                self.busy = false;
                self.permissions.clear();
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
                Some(Event::Done { error })
            }
            _ => None,
        })
    }
}

async fn run(state: AppState, id: String, socket: &mut WebSocket) -> Result<()> {
    let row = owner_row(&state.db, &id).context("项目不存在")?;
    anyhow::ensure!(
        row.project_type == "local" && row.worktree_archived_at.is_none(),
        "请在所属节点的活动项目中打开对话"
    );
    let cwd = if let Some(dir) = row.working_dir.clone() {
        dir.into()
    } else if row.multi_repos.is_some() && row.source_project_id.is_none() {
        state
            .engine
            .call(move |e| async move { e.sessions.ensure_virtual_dir(&row, true).await })
            .await??
            .into()
    } else {
        crate::config::home_dir()
    };
    let (program, env) = local_agent_command(falcon_proto::SessionAgent::Claude)
        .await
        .context("宿主机未找到 Claude Code")?;
    let mut child = Command::new(program)
        .no_window()
        .current_dir(cwd)
        .envs(env)
        .env_remove("CLAUDECODE")
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
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("启动 Claude Code 失败")?;
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = FramedRead::new(
        child.stdout.take().unwrap(),
        LinesCodec::new_with_max_length(16 * 1024 * 1024),
    );
    write(&mut stdin, json!({"type":"control_request","request_id":"falcon-init","request":{"subtype":"initialize"}})).await?;
    let mut chat = Conversation::default();
    let startup = tokio::time::sleep(std::time::Duration::from_secs(45));
    tokio::pin!(startup);
    loop {
        tokio::select! {
            _ = &mut startup, if !chat.ready => bail!("等待 Claude Code 初始化超时"),
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(msg))) => {
                    if let Some(input) = chat.input(serde_json::from_str(&msg)?) { write(&mut stdin, input).await?; }
                }
                None | Some(Ok(Message::Close(_))) => return Ok(()),
                Some(Err(err)) => return Err(err.into()),
                _ => {}
            },
            line = lines.next() => {
                let Some(line) = line else { bail!("Claude Code 已退出（{}）", child.wait().await?); };
                let v: Value = serde_json::from_str(&line?)?;
                if v["type"] == "control_request" && v["request"]["subtype"] != "can_use_tool" {
                    write(&mut stdin, json!({"type":"control_response","response":{"subtype":"error","request_id":v["request_id"],"error":"Unsupported control request"}})).await?;
                } else if let Some(event) = chat.event(&v)? { send(socket, event).await?; }
            }
        }
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
        };
        assert!(chat.input(prompt()).is_none());
        chat.event(&json!({"type":"control_response","response":{"request_id":"falcon-init","subtype":"success"}})).unwrap();
        assert_eq!(chat.input(prompt()).unwrap()["message"]["content"], "hello");
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
            let response = chat.input(decision()).unwrap();
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
            Some(Event::Done { error: None })
        ));
        assert!(chat.input(prompt()).is_some());
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
            matches!(chat.event(&message).unwrap(), Some(Event::Assistant { text, tools }) if text == "hi" && tools == ["Read"])
        );
    }
}
