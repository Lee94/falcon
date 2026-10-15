//! ACP（Agent Client Protocol）客户端：falcon 是 client，Claude Code / Codex / Gemini 的 ACP
//! 适配器是 agent（docs/design/ai-assistant.md 第 7 节）。
//!
//! 传输就是子进程 stdio 上一行一条的 JSON-RPC 2.0。没用 `agent-client-protocol` crate：3.x 的
//! 处理器要求 `Send`，跟引擎线程的 `Rc` / `RefCell` 合不来，而收发循环本身几十行；也没用它的
//! schema crate：一周三个大版本、一堆 unstable 变体，这里只读用得上的那几个字段，按 JSON 宽松地读
//! （与 `api/input.rs` 的 LenientJson 同一个思路：某个字段长得不对，不该让整条更新作废）。
//!
//! 分两层：
//! - [`AcpConn`]：收发与请求 / 应答配对，对任意读写流都成立（测试里用内存管道）；agent 发来的
//!   请求（权限、读写文件）交给调用方给的处理器，通知（`session/update`）交给另一个；
//! - [`UpdateMapper`]：`session/update` → 对话条目，纯逻辑（写进 [`EntrySink`]）。
//!
//! 命令输出：Claude / Codex 的适配器在自己的进程里跑命令，经 Zed 定的 `_meta` 扩展把输出流给
//! 客户端（`terminal_info` / `terminal_output_delta` / `terminal_exit`）。客户端在 initialize 里声明
//! `_meta.terminal_output_delta` 才给，卡片的实时输出就靠它。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use falcon_proto::assistant::{AcpAgentConfig, Entry, PlanItem, ToolCall, ToolContent, ToolKind, ToolStatus};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::exec::LocalBoxFuture;

pub const PROTOCOL_VERSION: u64 = 1;
/// agent 说「要先登录」时用的错误码（ACP 的 auth_required）
pub const AUTH_REQUIRED: i64 = -32000;

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}（{}）", self.message, self.code)
    }
}

/// agent 发来的请求：(方法, 参数) → 结果
pub type RequestHandler = Rc<dyn Fn(String, Value) -> LocalBoxFuture<'static, Result<Value, RpcError>>>;
/// agent 发来的通知
pub type NotifyHandler = Rc<dyn Fn(String, Value)>;

type Pending = Rc<RefCell<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

pub struct AcpConn {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    next_id: Cell<u64>,
    alive: Rc<Cell<bool>>,
    /// 进程 stderr 的最后几行：起不来 / 中途死掉时给人看
    stderr: Rc<RefCell<VecDeque<String>>>,
    tasks: Vec<tokio::task::AbortHandle>,
    _child: Option<tokio::process::Child>,
}

impl Drop for AcpConn {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl AcpConn {
    /// 在一对读写流上跑起收发循环
    pub fn connect(
        reader: impl AsyncRead + Unpin + 'static,
        mut writer: impl AsyncWrite + Unpin + 'static,
        on_request: RequestHandler,
        on_notify: NotifyHandler,
    ) -> Self {
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Rc::default();
        let alive = Rc::new(Cell::new(true));
        let writer_task = tokio::task::spawn_local(async move {
            while let Some(line) = out_rx.recv().await {
                if writer.write_all(line.as_bytes()).await.is_err()
                    || writer.write_all(b"\n").await.is_err()
                    || writer.flush().await.is_err()
                {
                    break;
                }
            }
        });
        let reader_task = {
            let (pending, alive, out) = (pending.clone(), alive.clone(), out.clone());
            tokio::task::spawn_local(async move {
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                    if let Some(method) = msg["method"].as_str().map(str::to_string) {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        match msg.get("id").cloned() {
                            Some(id) if !id.is_null() => {
                                let (handler, out) = (on_request.clone(), out.clone());
                                tokio::task::spawn_local(async move {
                                    let reply = match handler(method, params).await {
                                        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                                        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": {
                                            "code": e.code, "message": e.message, "data": e.data,
                                        }}),
                                    };
                                    let _ = out.send(reply.to_string());
                                });
                            }
                            _ => on_notify(method, params),
                        }
                        continue;
                    }
                    let Some(id) = msg["id"].as_u64() else { continue };
                    let Some(tx) = pending.borrow_mut().remove(&id) else { continue };
                    let res = match msg.get("error").filter(|e| !e.is_null()) {
                        Some(e) => Err(RpcError {
                            code: e["code"].as_i64().unwrap_or(-1),
                            message: e["message"].as_str().unwrap_or("agent 报错").to_string(),
                            data: e.get("data").cloned(),
                        }),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(res);
                }
                // 进程退出 / 管道断了：在等的请求一起失败
                alive.set(false);
                for (_, tx) in pending.borrow_mut().drain() {
                    let _ = tx.send(Err(RpcError { code: -1, message: "agent 进程退出了".into(), data: None }));
                }
            })
        };
        AcpConn {
            out,
            pending,
            next_id: Cell::new(1),
            alive,
            stderr: Rc::default(),
            tasks: vec![writer_task.abort_handle(), reader_task.abort_handle()],
            _child: None,
        }
    }

    /// 起 agent 进程并接上。`env` 是登录 shell 解析出来的整份环境（npx / nvm 在 PATH 上），
    /// 配置里的额外环境变量叠在上面；Command 按这份 env 里的 PATH 找程序
    pub fn spawn(
        cfg: &AcpAgentConfig,
        env: &[(String, String)],
        cwd: &str,
        on_request: RequestHandler,
        on_notify: NotifyHandler,
    ) -> Result<Self, String> {
        let mut cmd = tokio::process::Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .env_clear()
            .envs(env.iter().map(|(k, v)| (k, v)))
            .envs(cfg.env.iter().map(|(k, v)| (k, v)))
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("找不到命令 `{}`：先装好它（或到设置里改命令）再试", cfg.command),
            _ => format!("{} 起不来：{e}", cfg.name),
        })?;
        let stdin = child.stdin.take().ok_or("拿不到 agent 的 stdin")?;
        let stdout = child.stdout.take().ok_or("拿不到 agent 的 stdout")?;
        let stderr = child.stderr.take();
        let mut conn = AcpConn::connect(stdout, stdin, on_request, on_notify);
        if let Some(err) = stderr {
            let tail = conn.stderr.clone();
            let task = tokio::task::spawn_local(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log::debug!("acp stderr: {line}");
                    let mut t = tail.borrow_mut();
                    t.push_back(line);
                    if t.len() > 20 {
                        t.pop_front();
                    }
                }
            });
            conn.tasks.push(task.abort_handle());
        }
        conn._child = Some(child);
        Ok(conn)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.get()
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr.borrow().iter().cloned().collect::<Vec<_>>().join("\n")
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if !self.is_alive() {
            return Err(RpcError { code: -1, message: "agent 进程已经退出了".into(), data: None });
        }
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().insert(id, tx);
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string());
        rx.await.unwrap_or_else(|_| Err(RpcError { code: -1, message: "agent 连接断了".into(), data: None }))
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.out.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string());
    }
}

/// initialize 的参数：v1；声明 fs（Gemini 用）、不声明 terminal（没有 agent 用它）、声明终端输出
/// 扩展（按增量给）；`auth.terminal`：允许 agent 提「在终端里跑登录命令」这种认证方式
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": true, "writeTextFile": true },
            "terminal": false,
            "auth": { "terminal": true },
            "_meta": { "terminal_output": true, "terminal_output_delta": true },
        },
        "clientInfo": { "name": "falcon", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// agent 要求先登录（session/new 报 auth_required）时给人看的话：终端类的认证方式拼成一条命令，
/// 让人在对话的终端里跑；agent 自己处理的列出名字
pub fn auth_hint(cfg: &AcpAgentConfig, auth_methods: &Value) -> String {
    let mut lines = vec![format!("{} 需要先登录。", cfg.name)];
    for m in auth_methods.as_array().into_iter().flatten() {
        let name = m["name"].as_str().or(m["id"].as_str()).unwrap_or("");
        if m["type"] == "terminal" {
            let args: Vec<String> = m["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(shell_word)).collect();
            let mut cmd = vec![shell_word(&cfg.command)];
            cmd.extend(cfg.args.iter().map(|a| shell_word(a)));
            cmd.extend(args);
            lines.push(format!("- {name}：在终端里运行 `{}`", cmd.join(" ")));
        } else if !name.is_empty() {
            lines.push(format!("- {name}"));
        }
    }
    lines.push("登好了再发一次。".into());
    lines.join("\n")
}

fn shell_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./@:=+,".contains(c)) {
        s.to_string()
    } else {
        crate::host::quote_posix(s)
    }
}

// ---------------------------------------------------------------- session/update → 条目

/// 映射写进去的地方（对话；测试里是一个 Vec）
pub trait EntrySink {
    fn push(&self, entry: Entry);
    fn append(&self, id: &str, text: &str);
    /// 改一张工具卡片；没有这张卡片返回 false
    fn update_call(&self, id: &str, f: &mut dyn FnMut(&mut ToolCall)) -> bool;
    fn replace(&self, entry: Entry);
    fn new_id(&self) -> String;
}

/// 流式的文字块要接着往同一条上追加：记着当前那条 AI 话 / 思考
#[derive(Debug, Default)]
pub struct UpdateMapper {
    agent: Option<String>,
    thought: Option<String>,
}

impl UpdateMapper {
    /// 新的一轮：之后的文字另起一条
    pub fn reset(&mut self) {
        self.agent = None;
        self.thought = None;
    }

    pub fn apply(&mut self, update: &Value, sink: &dyn EntrySink) {
        match update["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => {
                let Some(text) = chunk_text(&update["content"]) else { return };
                self.thought = None;
                let id = self.agent.get_or_insert_with(|| {
                    let id = sink.new_id();
                    sink.push(Entry::Agent { id: id.clone(), text: String::new() });
                    id
                });
                sink.append(id, &text);
            }
            "agent_thought_chunk" => {
                let Some(text) = chunk_text(&update["content"]) else { return };
                self.agent = None;
                let id = self.thought.get_or_insert_with(|| {
                    let id = sink.new_id();
                    sink.push(Entry::Thought { id: id.clone(), text: String::new() });
                    id
                });
                sink.append(id, &text);
            }
            "tool_call" => {
                self.reset();
                let Some(id) = update["toolCallId"].as_str() else { return };
                let mut call = ToolCall {
                    id: id.to_string(),
                    title: String::new(),
                    description: None,
                    kind: ToolKind::Other,
                    status: ToolStatus::Pending,
                    content: Vec::new(),
                    permission: None,
                };
                patch_call(&mut call, update);
                if !sink.update_call(id, &mut |c| patch_call(c, update)) {
                    sink.push(Entry::ToolCall(call));
                }
            }
            "tool_call_update" => {
                let Some(id) = update["toolCallId"].as_str() else { return };
                if !sink.update_call(id, &mut |c| patch_call(c, update)) {
                    let mut call = ToolCall {
                        id: id.to_string(),
                        title: String::new(),
                        description: None,
                        kind: ToolKind::Other,
                        status: ToolStatus::InProgress,
                        content: Vec::new(),
                        permission: None,
                    };
                    patch_call(&mut call, update);
                    sink.push(Entry::ToolCall(call));
                }
            }
            "plan" => {
                let items = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| PlanItem {
                        content: e["content"].as_str().unwrap_or("").to_string(),
                        status: e["status"].as_str().unwrap_or("pending").to_string(),
                    })
                    .collect();
                // 一段对话只有一份计划，每次整份替换
                sink.replace(Entry::Plan { id: "plan".into(), items });
            }
            _ => {}
        }
    }
}

fn chunk_text(content: &Value) -> Option<String> {
    (content["type"] == "text").then(|| content["text"].as_str().unwrap_or("").to_string())
}

/// 工具调用（及其增量）里带了的字段改到卡片上；没带的不动
pub fn patch_call(c: &mut ToolCall, u: &Value) {
    if let Some(t) = u["title"].as_str() {
        c.title = clean_title(t);
    }
    if let Some(k) = u["kind"].as_str() {
        c.kind = match k {
            "read" => ToolKind::Read,
            "edit" => ToolKind::Edit,
            "delete" => ToolKind::Delete,
            "move" => ToolKind::Move,
            "search" => ToolKind::Search,
            "execute" => ToolKind::Execute,
            "think" => ToolKind::Think,
            "fetch" => ToolKind::Fetch,
            _ => ToolKind::Other,
        };
    }
    if let Some(s) = u["status"].as_str() {
        c.status = match s {
            "pending" => ToolStatus::Pending,
            "in_progress" => ToolStatus::InProgress,
            "completed" => ToolStatus::Completed,
            "failed" => ToolStatus::Failed,
            _ => c.status,
        };
    }
    if let Some(items) = u["content"].as_array() {
        // 整份替换，但终端已经收到的输出留着（内容里的 terminal 只是个占位）
        let output = terminal_output(c);
        c.content = items
            .iter()
            .filter_map(|it| match it["type"].as_str()? {
                "content" => chunk_text(&it["content"]).map(|text| ToolContent::Text { text }),
                "diff" => Some(ToolContent::Diff {
                    path: it["path"].as_str().unwrap_or("").to_string(),
                    old_text: it["oldText"].as_str().map(str::to_string),
                    new_text: it["newText"].as_str().unwrap_or("").to_string(),
                }),
                "terminal" => Some(ToolContent::Terminal { output: output.0.clone(), exit_code: output.1 }),
                _ => None,
            })
            .collect();
    }
    let meta = &u["_meta"];
    if meta.get("terminal_info").is_some() && terminal_mut(c).is_none() {
        c.content.push(ToolContent::Terminal { output: String::new(), exit_code: None });
    }
    if let Some(data) = meta["terminal_output_delta"]["data"].as_str() {
        ensure_terminal(c).0.push_str(data);
    } else if let Some(data) = meta["terminal_output"]["data"].as_str() {
        // 不是增量的那种：整份
        *ensure_terminal(c).0 = data.to_string();
    }
    if let Some(exit) = meta.get("terminal_exit") {
        let code = exit["exit_code"].as_i64().or(exit["exitCode"].as_i64()).map(|c| c as i32);
        *ensure_terminal(c).1 = code;
    }
}

/// ACP 的标题常把命令包在反引号里（「`npm test`」）：卡片自己用等宽字画，去掉
fn clean_title(t: &str) -> String {
    let t = t.trim();
    match t.strip_prefix('`').and_then(|x| x.strip_suffix('`')) {
        Some(inner) if !inner.contains('`') => inner.to_string(),
        _ => t.to_string(),
    }
}

fn terminal_output(c: &ToolCall) -> (String, Option<i32>) {
    c.content
        .iter()
        .find_map(|x| match x {
            ToolContent::Terminal { output, exit_code } => Some((output.clone(), *exit_code)),
            _ => None,
        })
        .unwrap_or_default()
}

fn terminal_mut(c: &mut ToolCall) -> Option<(&mut String, &mut Option<i32>)> {
    c.content.iter_mut().find_map(|x| match x {
        ToolContent::Terminal { output, exit_code } => Some((output, exit_code)),
        _ => None,
    })
}

fn ensure_terminal(c: &mut ToolCall) -> (&mut String, &mut Option<i32>) {
    if terminal_mut(c).is_none() {
        c.content.push(ToolContent::Terminal { output: String::new(), exit_code: None });
    }
    terminal_mut(c).expect("刚补上")
}

/// fs/read_text_file 的 line / limit：从第 line 行（1 起）取 limit 行
pub fn slice_lines(content: &str, line: Option<u64>, limit: Option<u64>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_string();
    }
    let start = line.unwrap_or(1).max(1) as usize - 1;
    let lines = content.split_inclusive('\n').skip(start);
    match limit {
        Some(n) => lines.take(n as usize).collect(),
        None => lines.collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct VecSink {
        entries: RefCell<Vec<Entry>>,
        n: Cell<u32>,
    }

    impl EntrySink for VecSink {
        fn push(&self, entry: Entry) {
            self.entries.borrow_mut().push(entry);
        }
        fn append(&self, id: &str, text: &str) {
            for e in self.entries.borrow_mut().iter_mut() {
                if let Entry::Agent { id: i, text: t } | Entry::Thought { id: i, text: t } = e
                    && i == id
                {
                    t.push_str(text);
                }
            }
        }
        fn update_call(&self, id: &str, f: &mut dyn FnMut(&mut ToolCall)) -> bool {
            for e in self.entries.borrow_mut().iter_mut() {
                if let Entry::ToolCall(c) = e
                    && c.id == id
                {
                    f(c);
                    return true;
                }
            }
            false
        }
        fn replace(&self, entry: Entry) {
            let mut es = self.entries.borrow_mut();
            match es.iter_mut().find(|e| e.id() == entry.id()) {
                Some(slot) => *slot = entry,
                None => es.push(entry),
            }
        }
        fn new_id(&self) -> String {
            self.n.set(self.n.get() + 1);
            format!("e{}", self.n.get())
        }
    }

    fn up(v: Value) -> Value {
        v
    }

    #[test]
    fn chunks_stream_into_one_entry_until_something_else_happens() {
        let sink = VecSink::default();
        let mut m = UpdateMapper::default();
        for t in ["想", "一想"] {
            m.apply(&up(json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":t}})), &sink);
        }
        for t in ["先看", "看"] {
            m.apply(&up(json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":t}})), &sink);
        }
        m.apply(&json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"`ls -la`","kind":"execute","status":"pending",
            "_meta":{"terminal_info":{"terminal_id":"t1"}}}), &sink);
        m.apply(&json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"好了"}}), &sink);
        let es = sink.entries.borrow();
        assert_eq!(es.len(), 4);
        assert_eq!(es[0], Entry::Thought { id: "e1".into(), text: "想一想".into() });
        assert_eq!(es[1], Entry::Agent { id: "e2".into(), text: "先看看".into() });
        let Entry::ToolCall(c) = &es[2] else { panic!() };
        assert_eq!((c.title.as_str(), c.kind, c.status), ("ls -la", ToolKind::Execute, ToolStatus::Pending));
        assert_eq!(c.content, [ToolContent::Terminal { output: String::new(), exit_code: None }]);
        // 工具调用之后的文字另起一条
        assert_eq!(es[3], Entry::Agent { id: "e3".into(), text: "好了".into() });
    }

    #[test]
    fn terminal_meta_streams_output_and_exit_code() {
        let sink = VecSink::default();
        let mut m = UpdateMapper::default();
        m.apply(&json!({"sessionUpdate":"tool_call","toolCallId":"t1","title":"npm test","kind":"execute","status":"in_progress",
            "content":[{"type":"terminal","terminalId":"t1"}]}), &sink);
        m.apply(&json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","_meta":{"terminal_output_delta":{"terminal_id":"t1","data":"ok 1\n"}}}), &sink);
        m.apply(&json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","_meta":{"terminal_output_delta":{"terminal_id":"t1","data":"ok 2\n"}}}), &sink);
        // 结果到了：内容整份重给（终端占位），已收的输出不能丢
        m.apply(&json!({"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed",
            "content":[{"type":"terminal","terminalId":"t1"}],
            "_meta":{"terminal_exit":{"terminal_id":"t1","exit_code":0,"signal":null}}}), &sink);
        let es = sink.entries.borrow();
        let Entry::ToolCall(c) = &es[0] else { panic!() };
        assert_eq!(c.status, ToolStatus::Completed);
        assert_eq!(c.content, [ToolContent::Terminal { output: "ok 1\nok 2\n".into(), exit_code: Some(0) }]);
    }

    #[test]
    fn diffs_text_and_plans_map_over() {
        let sink = VecSink::default();
        let mut m = UpdateMapper::default();
        m.apply(&json!({"sessionUpdate":"tool_call","toolCallId":"e1","title":"Edit a.rs","kind":"edit",
            "content":[{"type":"diff","path":"/r/a.rs","oldText":"a","newText":"b"},{"type":"content","content":{"type":"text","text":"note"}}]}), &sink);
        m.apply(&json!({"sessionUpdate":"plan","entries":[{"content":"读代码","priority":"high","status":"completed"},{"content":"改","status":"in_progress"}]}), &sink);
        m.apply(&json!({"sessionUpdate":"plan","entries":[{"content":"读代码","status":"completed"},{"content":"改","status":"completed"}]}), &sink);
        m.apply(&json!({"sessionUpdate":"available_commands_update","availableCommands":[]}), &sink);
        let es = sink.entries.borrow();
        assert_eq!(es.len(), 2);
        let Entry::ToolCall(c) = &es[0] else { panic!() };
        assert_eq!(c.kind, ToolKind::Edit);
        assert_eq!(c.content[0], ToolContent::Diff { path: "/r/a.rs".into(), old_text: Some("a".into()), new_text: "b".into() });
        assert_eq!(c.content[1], ToolContent::Text { text: "note".into() });
        let Entry::Plan { items, .. } = &es[1] else { panic!() };
        assert!(items.iter().all(|i| i.status == "completed"));
    }

    #[test]
    fn auth_hint_turns_terminal_methods_into_commands() {
        let cfg = falcon_proto::assistant::acp_presets().remove(0);
        let methods = json!([
            {"type":"terminal","id":"claude-login","name":"Claude 账号","args":["--cli","auth","login","--claudeai"]},
            {"id":"api-key","name":"API key"}
        ]);
        let hint = auth_hint(&cfg, &methods);
        assert!(hint.contains("`npx -y @agentclientprotocol/claude-agent-acp --cli auth login --claudeai`"), "{hint}");
        assert!(hint.contains("- API key"));
    }

    #[test]
    fn line_slices_follow_acp_semantics() {
        let c = "a\nb\nc\n";
        assert_eq!(slice_lines(c, None, None), c);
        assert_eq!(slice_lines(c, Some(2), None), "b\nc\n");
        assert_eq!(slice_lines(c, Some(2), Some(1)), "b\n");
        assert_eq!(slice_lines(c, None, Some(1)), "a\n");
    }

    /// 收发：请求配上应答、agent 发来的请求交给处理器并回话、通知照收、断了的时候在等的请求失败
    #[tokio::test(flavor = "current_thread")]
    async fn rpc_round_trips_over_a_pipe() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (client_side, agent_side) = tokio::io::duplex(64 * 1024);
                let (c_read, c_write) = tokio::io::split(client_side);
                let notes: Rc<RefCell<Vec<String>>> = Rc::default();
                let n2 = notes.clone();
                let conn = AcpConn::connect(
                    c_read,
                    c_write,
                    Rc::new(|method, params| {
                        Box::pin(async move {
                            match method.as_str() {
                                "fs/read_text_file" => Ok(json!({ "content": format!("read {}", params["path"].as_str().unwrap()) })),
                                _ => Err(RpcError { code: -32601, message: "no".into(), data: None }),
                            }
                        })
                    }),
                    Rc::new(move |method, _| n2.borrow_mut().push(method)),
                );

                // 假 agent：读一行请求、回一条通知 + 一个反向请求 + 应答，再读我们对反向请求的回话
                let (a_read, mut a_write) = tokio::io::split(agent_side);
                let agent = tokio::task::spawn_local(async move {
                    let mut lines = BufReader::new(a_read).lines();
                    let req: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                    assert_eq!(req["method"], "initialize");
                    let out = [
                        json!({"jsonrpc":"2.0","method":"session/update","params":{"update":{}}}),
                        json!({"jsonrpc":"2.0","id":"r1","method":"fs/read_text_file","params":{"path":"/x"}}),
                        json!({"jsonrpc":"2.0","id":req["id"],"result":{"protocolVersion":1}}),
                    ];
                    for m in out {
                        a_write.write_all(format!("{m}\n").as_bytes()).await.unwrap();
                    }
                    let reply: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                    assert_eq!(reply["id"], "r1");
                    assert_eq!(reply["result"]["content"], "read /x");
                    // 第二个请求不答，直接断开
                    lines.next_line().await.unwrap();
                });

                let res = conn.request("initialize", initialize_params()).await.unwrap();
                assert_eq!(res["protocolVersion"], 1);
                let pending = conn.request("session/new", json!({}));
                let res = pending.await;
                agent.await.unwrap();
                assert!(res.is_err(), "断开之后在等的请求要失败");
                assert!(!conn.is_alive());
                assert_eq!(notes.borrow().as_slice(), ["session/update"]);
            })
            .await;
    }
}
