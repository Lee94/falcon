//! 项目所属节点运行 Claude Code。线程保存对话，窗口关闭时回收其子进程。
use std::{
    collections::HashMap,
    fs::File,
    path::{Path as FsPath, PathBuf},
    process::Stdio,
    time::Duration,
};

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

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum HistoryEntry {
    User { text: String },
    Assistant { text: String, tools: Vec<String> },
    Partial { text: String },
    Done { error: Option<String> },
}

impl From<HistoryEntry> for Event {
    fn from(entry: HistoryEntry) -> Self {
        match entry {
            HistoryEntry::User { text } => Self::User { text },
            HistoryEntry::Assistant { text, tools } => Self::Assistant { text, tools },
            HistoryEntry::Partial { text } => Self::Assistant {
                text,
                tools: Vec::new(),
            },
            HistoryEntry::Done { error } => Self::Done { error },
        }
    }
}

struct ThreadHistory {
    id: String,
    file: fs::File,
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
        Ok(Self {
            id,
            file,
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
            HistoryEntry::Partial { .. } => {}
            HistoryEntry::Done { error } => {
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
            };
            self.append(&entry).await?;
            send(socket, entry.into()).await?;
        }
        Ok(())
    }

    async fn record(&mut self, event: &Event) -> Result<()> {
        match event {
            Event::Delta { text } => self.draft.push_str(text),
            Event::Assistant { text, tools } => {
                self.draft.clear();
                self.append(&HistoryEntry::Assistant {
                    text: text.clone(),
                    tools: tools.clone(),
                })
                .await?;
            }
            Event::Done { error } => {
                self.save_partial().await?;
                self.append(&HistoryEntry::Done {
                    error: error.clone(),
                })
                .await?;
            }
            _ => {}
        }
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
            let event = self.receive(&value).await?;
            if let Some(Event::Ready) = event {
                return Ok(true);
            }
            if let Some(Event::Done { error: Some(error) }) = event {
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

    async fn receive(&mut self, value: &Value) -> Result<Option<Event>> {
        if value["type"] == "control_request" && value["request"]["subtype"] != "can_use_tool" {
            write(&mut self.stdin, json!({"type":"control_response","response":{"subtype":"error","request_id":value["request_id"],"error":"Unsupported control request"}})).await?;
            return Ok(None);
        }
        self.chat.event(value)
    }

    async fn drive(
        &mut self,
        socket: &mut WebSocket,
        mut history: Option<&mut ThreadHistory>,
    ) -> Result<()> {
        send(socket, Event::Ready).await?;
        loop {
            tokio::select! {
                frame = socket.recv() => match frame {
                    Some(Ok(Message::Text(msg))) => {
                        if let Some(input) = self.chat.input(serde_json::from_str(&msg)?) {
                            if let Some(history) = history.as_deref_mut()
                                && let Some(text) = input["message"]["content"].as_str()
                            {
                                let entry = HistoryEntry::User { text: text.to_owned() };
                                history.append(&entry).await?;
                                write(&mut self.stdin, input).await?;
                                if send(socket, entry.into()).await.is_err() {
                                    return Ok(());
                                }
                            } else {
                                write(&mut self.stdin, input).await?;
                            }
                        }
                    }
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
                    if let Some(event) = self.receive(&value).await? {
                        if let Some(history) = history.as_deref_mut() {
                            history.record(&event).await?;
                        }
                        if let Err(err) = send(socket, event).await {
                            return if history.is_some() { Ok(()) } else { Err(err) };
                        }
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
                    if let Some(event) = self.chat.event(&value)? {
                        history.record(&event).await?;
                        if !history.pending {
                            break;
                        }
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
    let environment_context = row
        .thread_environment_id
        .as_ref()
        .and_then(|id| state.db.get_project(id))
        .and_then(|environment| environment.environment_context)
        .filter(|text| !text.trim().is_empty());
    let cwd: PathBuf = if let Some(dir) = row.working_dir.clone() {
        dir.into()
    } else if history.is_some() {
        bail!("线程缺少环境工作目录");
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
    let command = || {
        let mut command = Command::new(&program);
        command
            .no_window()
            .current_dir(&cwd)
            .envs(env.iter().cloned())
            .env_remove("CLAUDECODE");
        if let Some(context) = &environment_context {
            command.arg(format!("--append-system-prompt={context}"));
        }
        command
    };
    run_cli(socket, history, command).await
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
            let input = ClaudeInput::Prompt { text: text.into() };
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
        assert!(matches!(receive(&mut other).await, Event::User { text } if text == "interrupt"));
        assert!(matches!(receive(&mut other).await, Event::Delta { .. }));
        other.close(None).await.unwrap();

        prompt(&mut writer, "first").await;
        assert!(matches!(receive(&mut writer).await, Event::User { text } if text == "first"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == format!("--session-id {first}"))
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: None }
        ));
        prompt(&mut writer, "interrupt").await;
        assert!(matches!(receive(&mut writer).await, Event::User { text } if text == "interrupt"));
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
        assert!(matches!(receive(&mut writer).await, Event::User { text } if text == "first"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == format!("--session-id {first}"))
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: None }
        ));
        assert!(matches!(receive(&mut writer).await, Event::User { text } if text == "interrupt"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == "partial")
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: Some(_) }
        ));
        assert!(matches!(receive(&mut writer).await, Event::Ready));
        prompt(&mut writer, "follow-up").await;
        assert!(matches!(receive(&mut writer).await, Event::User { text } if text == "follow-up"));
        assert!(
            matches!(receive(&mut writer).await, Event::Assistant { text, .. } if text == format!("--resume {first}"))
        );
        assert!(matches!(
            receive(&mut writer).await,
            Event::Done { error: None }
        ));
        writer.close(None).await.unwrap();

        let mut other = connect(address, second).await;
        assert!(matches!(receive(&mut other).await, Event::ThreadHistory));
        assert!(matches!(receive(&mut other).await, Event::User { text } if text == "interrupt"));
        assert!(
            matches!(receive(&mut other).await, Event::Assistant { text, .. } if text == "partial")
        );
        assert!(matches!(
            receive(&mut other).await,
            Event::Done { error: Some(_) }
        ));
        assert!(matches!(receive(&mut other).await, Event::Ready));
        prompt(&mut other, "retry manually").await;
        assert!(
            matches!(receive(&mut other).await, Event::User { text } if text == "retry manually")
        );
        assert!(
            matches!(receive(&mut other).await, Event::Assistant { text, .. } if text == format!("--session-id {second}"))
        );
        assert!(matches!(
            receive(&mut other).await,
            Event::Done { error: None }
        ));
        other.close(None).await.unwrap();
        await_release(root.path(), first).await;
        await_release(root.path(), second).await;
        server.abort();
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
