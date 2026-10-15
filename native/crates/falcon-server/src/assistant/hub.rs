//! AI 助手的对话中枢（docs/design/ai-assistant.md 第 4–7 节）：对话的状态、观看者、持久化、审批，
//! 以及内置 agent 的循环。
//!
//! 跑在会话引擎线程上（`Rc` / `RefCell`，与 SessionManager 同一个执行模型）：命令要进对话终端，
//! 那些状态本来就在这条线程上。HTTP 流与工具执行都是 `spawn_local` 起的任务；RefCell 的借用只在
//! 同步小段里持有，不跨 await、不在借着时调别的方法（CLAUDE.md 的约定）。
//!
//! 一段对话 = 一个 [`Thread`]：界面条目（`entries`，照 ACP 的形状）与内置 agent 的 API 历史
//! （`messages`，原样回传）分开存——界面要的是好看，API 要的是逐字节不变。客户端连上 WS 先收
//! 一份快照再收增量，多个窗口同时看同一段对话也成立；收起面板、断网都不打断在跑的一轮。
//!
//! 内置 agent 的一轮：发请求 → 流式收（文字 / 思考 / 工具调用进卡片）→ `tool_use` 就按顺序跑工具
//! （要批准的先挂起等你点）→ 结果回给模型 → 直到 `end_turn`。被拒绝、被停止就停在那里，等你的
//! 下一句话；悬着的 tool_use 在下一句话之前补上「没有执行」的结果（claude.rs）。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use falcon_core::assistant::{plain_output, truncate_middle};
use falcon_proto::assistant::{
    AcpAgentConfig, AssistantClientMessage, AssistantServerMessage, BUILTIN_AGENT, Entry, NoticeLevel,
    PermissionKind, PermissionOption, ThreadStatus, ThreadSummary, ToolCall, ToolContent, ToolKind, ToolStatus,
};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::acp::{AUTH_REQUIRED, AcpConn, EntrySink, NotifyHandler, RequestHandler, RpcError, UpdateMapper, auth_hint, initialize_params, patch_call, slice_lines};
use super::claude::{
    Accumulator, ApiConfig, Delta, SseParser, close_dangling_tool_uses, echo_content, error_message, headers,
    push_user_content, request_body, retryable,
};
use super::settings;
use super::tools::{
    self, FileHost, MAX_TOOL_OUTPUT, MachineFacts, ToolInput, apply_edit, numbered, read_text, resolve_path,
    write_text,
};
use crate::askpass::hub::uuid_v4;
use crate::auth::now_ms;
use crate::crypto::SecretBox;
use crate::db::{AssistantThreadRow, Db};
use crate::sessions::machine::{Machine, owner_row};
use crate::sessions::manager::{CANCELLED, SessionManager};
use crate::sessions::shell_marks::CommandBlock;
use falcon_proto::OscColorHint;

/// 一句话里最多来回这么多轮工具调用，防止跑飞
const MAX_ROUNDS: usize = 60;
/// 流式响应多久没动静就算断了
const STREAM_IDLE: Duration = Duration::from_secs(300);
/// 流式文字落库的节流
const SAVE_EVERY: Duration = Duration::from_secs(2);
const TITLE_CHARS: usize = 40;

pub struct AssistantHub {
    db: Arc<Db>,
    secrets: Arc<SecretBox>,
    sessions: Rc<SessionManager>,
    threads: RefCell<HashMap<String, Rc<Thread>>>,
    next_viewer: Cell<u64>,
    me: Weak<AssistantHub>,
}

pub struct Thread {
    pub id: String,
    pub machine: String,
    pub created_at: i64,
    /// 删掉了：还在跑的那一轮收尾时别再把它写回库里（删了又冒出来）
    deleted: Cell<bool>,
    s: RefCell<ThreadState>,
}

#[derive(Default)]
struct ThreadState {
    /// [`BUILTIN_AGENT`] 或 `acp:<id>`；可以换（之后的话交给新的那个）
    agent: String,
    /// 这段对话的终端会话（一段对话一个 shell）
    session_id: Option<String>,
    title: String,
    updated_at: i64,
    status: ThreadStatus,
    entries: Vec<Entry>,
    /// 内置 agent 的 API 历史（原样回传）
    messages: Vec<Value>,
    viewers: Vec<(u64, mpsc::UnboundedSender<String>)>,
    cancel: Option<CancellationToken>,
    /// 在等你批准的那张卡片：(工具调用 id, 回话)
    /// 在等批准的那张卡片，与答复的去处：(选项 id, 你在卡片上改过的命令)
    permission: Option<(String, oneshot::Sender<(String, Option<String>)>)>,
    /// 「本对话内总是允许」过的工具
    always_allow: HashSet<String>,
    last_save: Option<Instant>,
    // ---- ACP agent 的对话 ----
    /// agent 进程（不落库：服务端重启后下一句话再起，能 session/load 就接回）
    acp: Option<Rc<AcpConn>>,
    acp_session: Option<String>,
    acp_mapper: UpdateMapper,
    /// session/load 期间 agent 会把历史重放一遍：界面上本来就有，丢掉
    acp_loading: bool,
}

/// 审批的结果
enum Approval {
    Once,
    Always,
    Reject,
    Stopped,
}

const OPT_ALLOW: &str = "allow";
const OPT_ALWAYS: &str = "always";
const OPT_REJECT: &str = "reject";

fn permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption { id: OPT_ALLOW.into(), name: "允许".into(), kind: PermissionKind::AllowOnce },
        PermissionOption { id: OPT_ALWAYS.into(), name: "本对话内总是允许".into(), kind: PermissionKind::AllowAlways },
        PermissionOption { id: OPT_REJECT.into(), name: "拒绝".into(), kind: PermissionKind::RejectOnce },
    ]
}

impl Thread {
    fn new(id: String, machine: String, agent: String, created_at: i64) -> Rc<Self> {
        Rc::new(Thread {
            id,
            machine,
            created_at,
            deleted: Cell::new(false),
            s: RefCell::new(ThreadState { agent, updated_at: created_at, ..Default::default() }),
        })
    }

    pub fn agent(&self) -> String {
        self.s.borrow().agent.clone()
    }

    pub fn session_id(&self) -> Option<String> {
        self.s.borrow().session_id.clone()
    }

    fn from_row(row: AssistantThreadRow) -> Rc<Self> {
        let t = Thread::new(row.id, row.machine, row.agent, row.created_at);
        {
            let mut s = t.s.borrow_mut();
            s.session_id = row.session_id;
            s.title = row.title;
            s.updated_at = row.updated_at;
            s.entries = serde_json::from_str(&row.entries).unwrap_or_default();
            let provider: Value = serde_json::from_str(&row.provider).unwrap_or(Value::Null);
            s.messages = provider["messages"].as_array().cloned().unwrap_or_default();
            s.acp_session = provider["acpSessionId"].as_str().map(str::to_string);
            // 收在库里的「等批准」是上次进程留下的：那一轮已经没了，按钮不能再按
            for e in s.entries.iter_mut() {
                if let Entry::ToolCall(c) = e
                    && (c.permission.is_some() || matches!(c.status, ToolStatus::Pending | ToolStatus::InProgress))
                {
                    c.permission = None;
                    c.status = ToolStatus::Failed;
                }
            }
        }
        t
    }

    fn to_row(&self) -> AssistantThreadRow {
        let s = self.s.borrow();
        AssistantThreadRow {
            id: self.id.clone(),
            machine: self.machine.clone(),
            agent: s.agent.clone(),
            title: s.title.clone(),
            created_at: self.created_at,
            updated_at: s.updated_at,
            entries: serde_json::to_string(&s.entries).unwrap_or_else(|_| "[]".into()),
            provider: json!({ "messages": s.messages, "acpSessionId": s.acp_session }).to_string(),
            session_id: s.session_id.clone(),
        }
    }

    pub fn summary(&self) -> ThreadSummary {
        let s = self.s.borrow();
        ThreadSummary {
            id: self.id.clone(),
            machine: self.machine.clone(),
            agent: s.agent.clone(),
            title: s.title.clone(),
            status: s.status,
            created_at: self.created_at,
            updated_at: s.updated_at,
            session_id: s.session_id.clone(),
        }
    }

    /// 摘要（名字、agent、终端）变了：推给在看的
    fn announce(&self) {
        self.send_all(&AssistantServerMessage::Thread { thread: self.summary() });
    }

    fn send_all(&self, msg: &AssistantServerMessage) {
        let text = serde_json::to_string(msg).unwrap_or_default();
        self.s.borrow_mut().viewers.retain(|(_, tx)| tx.send(text.clone()).is_ok());
    }

    fn touch(&self) {
        self.s.borrow_mut().updated_at = now_ms();
    }

    fn push(&self, entry: Entry) {
        self.s.borrow_mut().entries.push(entry.clone());
        self.touch();
        self.send_all(&AssistantServerMessage::Entry { entry });
    }

    /// 同 id 的整条替换（没有就追加）
    fn replace(&self, entry: Entry) {
        {
            let mut s = self.s.borrow_mut();
            let id = entry.id().map(str::to_string);
            match s.entries.iter_mut().find(|e| e.id().map(str::to_string) == id) {
                Some(slot) => *slot = entry.clone(),
                None => s.entries.push(entry.clone()),
            }
        }
        self.touch();
        self.send_all(&AssistantServerMessage::Entry { entry });
    }

    /// 文字追加：Agent / Thought 追加正文，ToolCall 追加到终端输出
    fn append(&self, id: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        {
            let mut s = self.s.borrow_mut();
            if let Some(e) = s.entries.iter_mut().find(|e| e.id() == Some(id)) {
                match e {
                    Entry::Agent { text: t, .. } | Entry::Thought { text: t, .. } => t.push_str(text),
                    Entry::ToolCall(c) => match c.content.iter_mut().find(|c| matches!(c, ToolContent::Terminal { .. })) {
                        Some(ToolContent::Terminal { output, .. }) => output.push_str(text),
                        _ => c.content.push(ToolContent::Terminal { output: text.to_string(), exit_code: None }),
                    },
                    _ => {}
                }
            }
        }
        self.send_all(&AssistantServerMessage::Append { id: id.to_string(), text: text.to_string() });
    }

    fn tool_call(&self, id: &str) -> Option<ToolCall> {
        self.s.borrow().entries.iter().find_map(|e| match e {
            Entry::ToolCall(c) if c.id == id => Some(c.clone()),
            _ => None,
        })
    }

    fn update_call(&self, id: &str, f: impl FnOnce(&mut ToolCall)) {
        if let Some(mut c) = self.tool_call(id) {
            f(&mut c);
            self.replace(Entry::ToolCall(c));
        }
    }

    fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.push(Entry::Notice { id: uuid_v4(), level, text: text.into() });
    }

    fn set_status(&self, status: ThreadStatus) {
        let changed = {
            let mut s = self.s.borrow_mut();
            std::mem::replace(&mut s.status, status) != status
        };
        if changed {
            self.send_all(&AssistantServerMessage::Status { status });
        }
    }

    fn status(&self) -> ThreadStatus {
        self.s.borrow().status
    }
}

/// ACP 的 `session/update` 映射直接写进对话
impl EntrySink for Thread {
    fn push(&self, entry: Entry) {
        Thread::push(self, entry)
    }
    fn append(&self, id: &str, text: &str) {
        Thread::append(self, id, text)
    }
    fn update_call(&self, id: &str, f: &mut dyn FnMut(&mut ToolCall)) -> bool {
        match self.tool_call(id) {
            Some(mut c) => {
                f(&mut c);
                self.replace(Entry::ToolCall(c));
                true
            }
            None => false,
        }
    }
    fn replace(&self, entry: Entry) {
        Thread::replace(self, entry)
    }
    fn new_id(&self) -> String {
        uuid_v4()
    }
}

impl AssistantHub {
    pub fn new(db: Arc<Db>, secrets: Arc<SecretBox>, sessions: Rc<SessionManager>) -> Rc<Self> {
        let hub = Rc::new_cyclic(|me| AssistantHub {
            db,
            secrets,
            sessions: sessions.clone(),
            threads: RefCell::default(),
            next_viewer: Cell::new(1),
            me: me.clone(),
        });
        // 终端里用户自己敲完的命令：记进它所属的那段对话（Warp 的命令块）
        let weak = Rc::downgrade(&hub);
        sessions.set_block_listener(Rc::new(move |session_id, block| {
            if let Some(hub) = weak.upgrade() {
                hub.on_block(session_id, block);
            }
        }));
        hub
    }

    /// 这个终端会话属于哪段对话（内存里的优先，没有就从库里读回来）
    fn thread_of_session(&self, session_id: &str) -> Option<Rc<Thread>> {
        let loaded = self.threads.borrow().values().find(|t| t.session_id().as_deref() == Some(session_id)).cloned();
        if loaded.is_some() {
            return loaded;
        }
        let row = self.db.find_assistant_thread_by_session(session_id)?;
        self.thread(&row.id)
    }

    fn on_block(&self, session_id: &str, block: CommandBlock) {
        let Some(t) = self.thread_of_session(session_id) else { return };
        let named = {
            let mut s = t.s.borrow_mut();
            let name = s.title.is_empty() && !block.command.trim().is_empty();
            if name {
                s.title = block.command.trim().chars().take(TITLE_CHARS).collect();
            }
            name
        };
        if named {
            t.announce();
        }
        t.push(Entry::Command {
            id: uuid_v4(),
            command: block.command,
            cwd: block.cwd,
            output: block.output,
            exit_code: block.exit_code,
            by_agent: false,
        });
        self.save(&t);
    }

    fn me(&self) -> Rc<Self> {
        self.me.upgrade().expect("AssistantHub 还活着")
    }

    fn save(&self, thread: &Thread) {
        if thread.deleted.get() {
            return;
        }
        thread.s.borrow_mut().last_save = Some(Instant::now());
        self.db.upsert_assistant_thread(&thread.to_row());
    }

    /// 流式文字的落库节流：崩了最多丢 2s 的字
    fn maybe_save(&self, thread: &Thread) {
        let due = thread.s.borrow().last_save.is_none_or(|t| t.elapsed() >= SAVE_EVERY);
        if due {
            self.save(thread);
        }
    }

    /// 内存里的，或者从库里读回来的
    pub fn thread(&self, id: &str) -> Option<Rc<Thread>> {
        if let Some(t) = self.threads.borrow().get(id) {
            return Some(t.clone());
        }
        let t = Thread::from_row(self.db.get_assistant_thread(id)?);
        self.threads.borrow_mut().insert(id.to_string(), t.clone());
        Some(t)
    }

    // ---------------- 对话的增删查 ----------------

    /// 全部对话，新的在前（面板的对话列表跨机器）
    pub fn list(&self) -> Vec<ThreadSummary> {
        self.db
            .list_all_assistant_threads()
            .into_iter()
            .map(|row| match self.threads.borrow().get(&row.id) {
                Some(t) => t.summary(),
                None => Thread::from_row(row).summary(),
            })
            .collect()
    }

    /// agent 这台机器用不用得了：ACP agent 只在本机起（ADR 0021 决定六），而且要在设置里开着
    fn check_agent(&self, machine: &str, agent: &str) -> Result<(), String> {
        if agent == BUILTIN_AGENT {
            return Ok(());
        }
        if settings::acp_agent(&self.db, agent).is_none() {
            return Err(format!("不认识的 agent，或者它在设置里关掉了：{agent}"));
        }
        if machine != "local" {
            return Err("ACP agent 只能在本机用；SSH 主机上请用内置 agent".into());
        }
        Ok(())
    }

    /// 新对话 = 新的终端 + 新的 AI 上下文（像 Warp 的一个标签）
    pub async fn create(&self, machine: &str, agent: Option<&str>, hint: OscColorHint) -> Result<ThreadSummary, String> {
        let agent = agent.filter(|a| !a.is_empty()).unwrap_or(BUILTIN_AGENT);
        self.check_agent(machine, agent)?;
        let session = self
            .sessions
            .create_machine_session(&Machine::from_param(machine), hint)
            .await
            .map_err(|e| format!("终端没开起来：{e}"))?;
        let t = Thread::new(uuid_v4(), machine.to_string(), agent.to_string(), now_ms());
        t.s.borrow_mut().session_id = Some(session.id);
        self.threads.borrow_mut().insert(t.id.clone(), t.clone());
        self.save(&t);
        Ok(t.summary())
    }

    /// 对话的终端：活着就交出去，丢了（shell 退出了）就在同一台机器上另开一个
    pub async fn shell(&self, id: &str, hint: OscColorHint) -> Result<falcon_proto::Session, String> {
        let t = self.thread(id).ok_or("对话不存在")?;
        let session = self
            .sessions
            .ensure_machine_session(&t.id, t.session_id().as_deref(), &Machine::from_param(&t.machine), hint)
            .await
            .map_err(|e| format!("终端没开起来：{e}"))?;
        if t.session_id().as_deref() != Some(session.id.as_str()) {
            t.s.borrow_mut().session_id = Some(session.id.clone());
            self.save(&t);
            t.announce();
        }
        Ok(session)
    }

    /// 换这段对话的 agent：之后的话交给新的那个（终端与命令块不动）。ACP 的进程与会话丢掉；
    /// 内置 agent 的历史留着，换回来接着聊
    pub fn set_agent(&self, id: &str, agent: &str) -> Result<ThreadSummary, String> {
        let t = self.thread(id).ok_or("对话不存在")?;
        if matches!(t.status(), ThreadStatus::Running | ThreadStatus::AwaitingPermission) {
            return Err("这一轮还没结束，先停下再换".into());
        }
        self.check_agent(&t.machine, agent)?;
        if t.agent() != agent {
            {
                let mut s = t.s.borrow_mut();
                s.agent = agent.to_string();
                s.acp = None;
                s.acp_session = None;
            }
            self.save(&t);
            t.announce();
        }
        Ok(t.summary())
    }

    /// 删对话：在跑的停下、终端终止（一段对话一个 shell，对话没了它也没用了）
    pub async fn delete(&self, id: &str) {
        let session = self.thread(id).and_then(|t| t.session_id());
        if let Some(t) = self.threads.borrow_mut().remove(id) {
            t.deleted.set(true);
            let mut s = t.s.borrow_mut();
            if let Some(c) = s.cancel.take() {
                c.cancel();
            }
            // 观看者的发送端丢掉，WS 那头随之结束；agent 进程随连接一起收掉（kill_on_drop）
            s.viewers.clear();
            s.acp = None;
        }
        self.db.delete_assistant_thread(id);
        if let Some(sid) = session {
            self.sessions.terminate(&sid, false).await;
        }
    }

    /// 主机删掉了：它上面的对话跟着删（终端由 SessionManager::forget_machine 终止）
    pub async fn forget_machine(&self, machine: &str) {
        let ids: Vec<String> = self.db.list_assistant_threads(machine).into_iter().map(|r| r.id).collect();
        for id in ids {
            self.delete(&id).await;
        }
    }

    // ---------------- 观看者 ----------------

    /// 接上一个观看者：先给快照。对话不存在给 None
    pub fn add_viewer(&self, id: &str, tx: mpsc::UnboundedSender<String>) -> Option<u64> {
        let t = self.thread(id)?;
        let vid = self.next_viewer.get();
        self.next_viewer.set(vid + 1);
        let snapshot = {
            let s = t.s.borrow();
            AssistantServerMessage::Snapshot { thread: t.summary(), entries: s.entries.clone() }
        };
        let _ = tx.send(serde_json::to_string(&snapshot).unwrap_or_default());
        t.s.borrow_mut().viewers.push((vid, tx));
        Some(vid)
    }

    pub fn remove_viewer(&self, id: &str, vid: u64) {
        if let Some(t) = self.threads.borrow().get(id) {
            t.s.borrow_mut().viewers.retain(|(v, _)| *v != vid);
        }
    }

    /// 观看者发来的消息
    pub fn handle(&self, id: &str, msg: AssistantClientMessage) {
        let Some(t) = self.thread(id) else { return };
        match msg {
            AssistantClientMessage::Prompt { text, context } => {
                if let Err(e) = self.prompt(&t, text, context) {
                    t.notice(NoticeLevel::Warning, e);
                }
            }
            AssistantClientMessage::Cancel => {
                let token = t.s.borrow().cancel.clone();
                if let Some(c) = token {
                    c.cancel();
                }
            }
            AssistantClientMessage::Permission { tool_call_id, option_id, command } => {
                let pending = t.s.borrow_mut().permission.take();
                match pending {
                    Some((id, tx)) if id == tool_call_id => {
                        let _ = tx.send((option_id, command.filter(|c| !c.trim().is_empty())));
                    }
                    other => t.s.borrow_mut().permission = other,
                }
            }
            AssistantClientMessage::Unknown => {}
        }
    }

    // ---------------- 一轮 ----------------

    fn prompt(&self, t: &Rc<Thread>, text: String, context: Option<String>) -> Result<(), String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }
        if matches!(t.status(), ThreadStatus::Running | ThreadStatus::AwaitingPermission) {
            return Err("上一轮还没结束，先等它跑完或者点停止".into());
        }
        let context = context.filter(|c| !c.trim().is_empty());
        // 上一次跟 agent 说话之后，你在终端里敲过的命令：自动带上（Warp 的「agent 看得见你的终端」）。
        // 要在记下这句话之前取——「上一次说话」指的是上一句，不是这一句
        let recent = recent_commands(&t.s.borrow().entries);
        // 说这句话时终端在哪个目录：时间线上画成 `~/dir 这句话`，像在命令行里敲的
        let cwd = t.session_id().and_then(|sid| self.sessions.session_shell_info(&sid)).and_then(|(cwd, _, _)| cwd);
        t.push(Entry::User { id: uuid_v4(), text: text.clone(), context: context.clone(), cwd: cwd.clone() });
        let named = {
            let mut s = t.s.borrow_mut();
            let name = s.title.is_empty();
            if name {
                s.title = text.chars().take(TITLE_CHARS).collect();
            }
            name
        };
        if named {
            t.announce();
        }

        let agent = t.agent();
        if agent != BUILTIN_AGENT {
            let cfg = settings::acp_agent(&self.db, &agent).ok_or("这个 agent 在设置里被删掉或关掉了")?;
            let context = recent.or(context);
            return Ok(self.start_turn(t, move |me, t, cancel| {
                Box::pin(async move { me.run_acp_turn(&t, &cfg, text, context, &cancel).await })
            }));
        }

        // 这一轮发给模型的用户消息：当前目录（会变，不进系统提示词）、最近的命令、正文、附带的终端输出
        let mut blocks = Vec::new();
        if let Some(pre) = tools::turn_preamble(cwd.as_deref()) {
            blocks.push(json!({ "type": "text", "text": pre }));
        }
        if let Some(r) = &recent {
            blocks.push(json!({ "type": "text", "text": r }));
        }
        blocks.push(json!({ "type": "text", "text": text }));
        if let Some(ctx) = &context {
            blocks.push(json!({
                "type": "text",
                "text": format!("<terminal_output>\n{}\n</terminal_output>", truncate_middle(&plain_output(ctx), MAX_TOOL_OUTPUT)),
            }));
        }
        {
            let mut s = t.s.borrow_mut();
            close_dangling_tool_uses(&mut s.messages, "Not executed: the turn ended before this tool ran.");
            push_user_content(&mut s.messages, blocks);
        }

        self.start_turn(t, |me, t, cancel| Box::pin(async move { me.run_turn(&t, &cancel).await }));
        Ok(())
    }

    /// 起一轮：标 running、挂上停止用的 token，跑完按返回的状态收尾
    fn start_turn(
        &self,
        t: &Rc<Thread>,
        run: impl FnOnce(Rc<Self>, Rc<Thread>, CancellationToken) -> crate::exec::LocalBoxFuture<'static, ThreadStatus> + 'static,
    ) {
        let cancel = CancellationToken::new();
        t.s.borrow_mut().cancel = Some(cancel.clone());
        t.set_status(ThreadStatus::Running);
        self.save(t);
        let me = self.me();
        let t = t.clone();
        tokio::task::spawn_local(async move {
            let status = run(me.clone(), t.clone(), cancel).await;
            t.s.borrow_mut().cancel = None;
            t.set_status(status);
            me.save(&t);
        });
    }

    /// 内置 agent 的一轮，返回结束时的状态
    async fn run_turn(&self, t: &Rc<Thread>, cancel: &CancellationToken) -> ThreadStatus {
        let cfg = match settings::api_config(&self.db, &self.secrets) {
            Ok(c) => c,
            Err(e) => {
                t.notice(NoticeLevel::Error, e);
                return ThreadStatus::Error;
            }
        };
        let client = match super::claude::http_client() {
            Ok(c) => c,
            Err(e) => {
                t.notice(NoticeLevel::Error, e);
                return ThreadStatus::Error;
            }
        };
        let machine = Machine::from_param(&t.machine);
        let facts = self.machine_facts(&machine, t.session_id().as_deref()).await;
        let system = tools::system_prompt(&facts);
        let defs = tools::definitions();

        for _ in 0..MAX_ROUNDS {
            let messages = t.s.borrow().messages.clone();
            let body = request_body(&cfg, &system, &defs, &messages);
            let acc = match self.stream_once(t, &cfg, &client, &body, cancel).await {
                Ok(acc) => acc,
                Err(None) => {
                    t.notice(NoticeLevel::Info, CANCELLED);
                    return ThreadStatus::Idle;
                }
                Err(Some(e)) => {
                    t.notice(NoticeLevel::Error, e);
                    return ThreadStatus::Error;
                }
            };
            if let Some(e) = &acc.error {
                t.notice(NoticeLevel::Error, format!("模型的响应中途出错：{e}"));
                return ThreadStatus::Error;
            }
            let stop = acc.stop_reason.clone().unwrap_or_default();
            if stop == "refusal" {
                // 被拒的这一段不进历史（文档：丢掉部分输出），工具一个都不跑
                self.fail_cards(t, &acc, "模型拒绝了这个请求，没有执行");
                t.notice(NoticeLevel::Warning, "模型拒绝了这个请求。换个说法，或者到设置里换一个模型再试");
                return ThreadStatus::Idle;
            }
            let content = echo_content(&acc.blocks);
            if !content.is_empty() {
                t.s.borrow_mut().messages.push(json!({ "role": "assistant", "content": content }));
            }
            match stop.as_str() {
                "tool_use" => {
                    let (results, halted) = self.run_tools(t, &machine, &acc, cancel).await;
                    push_user_content(&mut t.s.borrow_mut().messages, results);
                    self.save(t);
                    if halted {
                        return ThreadStatus::Idle;
                    }
                }
                "max_tokens" => {
                    // 被截断的工具参数不可信，不执行
                    self.fail_cards(t, &acc, "回复太长被截断，没有执行");
                    t.notice(NoticeLevel::Warning, "回复太长被截断了。可以让它分几步做");
                    return ThreadStatus::Idle;
                }
                // 只有服务端工具会暂停，我们没用；照样接着发
                "pause_turn" => {}
                _ => return ThreadStatus::Idle,
            }
        }
        t.notice(NoticeLevel::Warning, format!("这一轮已经来回了 {MAX_ROUNDS} 次工具调用，先停下。要继续就再说一句"));
        ThreadStatus::Idle
    }

    /// 发一次请求、收完整个流。`Err(None)` = 被停止；`Err(Some(话))` = 出错。
    /// 限流 / 过载 / 5xx 在还没开始收的时候退避重试三次
    async fn stream_once(
        &self,
        t: &Rc<Thread>,
        cfg: &ApiConfig,
        client: &reqwest::Client,
        body: &Value,
        cancel: &CancellationToken,
    ) -> Result<Accumulator, Option<String>> {
        let bytes = serde_json::to_vec(body).map_err(|e| Some(e.to_string()))?;
        let mut attempt = 0;
        let mut resp = loop {
            let mut req = client.post(cfg.endpoint()).body(bytes.clone());
            for (k, v) in headers(cfg) {
                req = req.header(k, v);
            }
            let sent = tokio::select! {
                r = req.send() => r,
                _ = cancel.cancelled() => return Err(None),
            };
            let resp = sent.map_err(|e| Some(format!("连不上模型服务（{}）：{e}", cfg.endpoint())))?;
            let status = resp.status().as_u16();
            if resp.status().is_success() {
                break resp;
            }
            let text = resp.text().await.unwrap_or_default();
            if retryable(status) && attempt < 3 {
                attempt += 1;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1 << attempt)) => continue,
                    _ = cancel.cancelled() => return Err(None),
                }
            }
            return Err(Some(error_message(status, &text)));
        };

        let mut parser = SseParser::default();
        let mut utf8 = Utf8Carry::default();
        let mut acc = Accumulator::default();
        // 块序号 → 界面条目 id
        let mut ui: HashMap<usize, String> = HashMap::new();
        loop {
            let chunk = tokio::select! {
                c = tokio::time::timeout(STREAM_IDLE, resp.chunk()) => c,
                _ = cancel.cancelled() => return Err(None),
            };
            let chunk = match chunk {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(Some(format!("模型的响应断了：{e}"))),
                Err(_) => return Err(Some("模型的响应 5 分钟没有动静，断开了".into())),
            };
            for data in parser.push(&utf8.push(&chunk)) {
                for delta in acc.apply(&data) {
                    self.on_delta(t, &mut ui, &acc, delta);
                }
            }
            self.maybe_save(t);
        }
        Ok(acc)
    }

    fn on_delta(&self, t: &Rc<Thread>, ui: &mut HashMap<usize, String>, acc: &Accumulator, delta: Delta) {
        match delta {
            Delta::TextStart { index } => {
                let id = uuid_v4();
                ui.insert(index, id.clone());
                t.push(Entry::Agent { id, text: String::new() });
            }
            Delta::ThinkingStart { index } => {
                let id = uuid_v4();
                ui.insert(index, id.clone());
                t.push(Entry::Thought { id, text: String::new() });
            }
            Delta::Text { index, text } | Delta::Thinking { index, text } => {
                if let Some(id) = ui.get(&index) {
                    t.append(id, &text);
                }
            }
            Delta::ToolStart { index, id, name } => {
                ui.insert(index, id.clone());
                let kind = match name.as_str() {
                    "run_command" => ToolKind::Execute,
                    "read_file" => ToolKind::Read,
                    _ => ToolKind::Edit,
                };
                t.push(Entry::ToolCall(ToolCall {
                    id,
                    title: String::new(),
                    description: None,
                    kind,
                    status: ToolStatus::Pending,
                    content: Vec::new(),
                    permission: None,
                }));
            }
            Delta::ToolDone { index } => {
                let Some(block) = acc.blocks.get(index) else { return };
                let id = block.value["id"].as_str().unwrap_or("").to_string();
                let name = block.value["name"].as_str().unwrap_or("");
                if let Ok(input) = ToolInput::parse(name, &block.value["input"]) {
                    t.update_call(&id, |c| {
                        c.title = input.title();
                        c.description = input.description();
                        c.kind = input.kind();
                        c.content = input.preview();
                    });
                }
            }
            Delta::Fallback { .. } => {
                let model = acc.blocks.last().and_then(|b| b.value["to"]["model"].as_str()).unwrap_or("另一个模型");
                t.notice(NoticeLevel::Info, format!("这个请求转给了 {model} 接着回答"));
            }
        }
    }

    /// 这一段里没执行的工具卡片标成失败（被拒、被截断、换模型之前的）
    fn fail_cards(&self, t: &Rc<Thread>, acc: &Accumulator, why: &str) {
        for b in &acc.blocks {
            if b.kind() == "tool_use"
                && let Some(id) = b.value["id"].as_str()
            {
                t.update_call(id, |c| {
                    c.status = ToolStatus::Failed;
                    c.content.push(ToolContent::Text { text: why.into() });
                });
            }
        }
    }

    /// 按顺序跑这一段的工具，返回 (tool_result 块, 是否就此停下)。一个被拒绝 / 停止之后，
    /// 剩下的都不跑，各给一个「没有执行」的结果——每个 tool_use 都必须有结果
    async fn run_tools(
        &self,
        t: &Rc<Thread>,
        machine: &Machine,
        acc: &Accumulator,
        cancel: &CancellationToken,
    ) -> (Vec<Value>, bool) {
        // 换模型之前的那些 tool_use 不跑、也不进历史（echo_content 已经剔掉了）
        if let Some(boundary) = acc.fallback_boundary() {
            for b in &acc.blocks[..boundary] {
                if b.kind() == "tool_use"
                    && let Some(id) = b.value["id"].as_str()
                {
                    t.update_call(id, |c| c.status = ToolStatus::Failed);
                }
            }
        }
        let auto = settings::auto_approve(&self.db);
        let mut results = Vec::new();
        let mut halted = false;
        for block in acc.tool_uses() {
            let id = block.value["id"].as_str().unwrap_or("").to_string();
            let name = block.value["name"].as_str().unwrap_or("").to_string();
            let result = |content: String, is_error: bool| {
                json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error })
            };
            if halted || cancel.is_cancelled() {
                halted = true;
                t.update_call(&id, |c| c.status = ToolStatus::Failed);
                results.push(result("Not executed: the user stopped this turn.".into(), true));
                continue;
            }
            if let Some(raw) = &block.invalid_input {
                t.update_call(&id, |c| c.status = ToolStatus::Failed);
                results.push(result(json!({ "INVALID_JSON": raw }).to_string(), true));
                continue;
            }
            let mut input = match ToolInput::parse(&name, &block.value["input"]) {
                Ok(i) => i,
                Err(e) => {
                    t.update_call(&id, |c| {
                        c.status = ToolStatus::Failed;
                        c.content.push(ToolContent::Text { text: e.clone() });
                    });
                    results.push(result(format!("Invalid input: {e}"), true));
                    continue;
                }
            };

            let allowed = auto || !input.needs_approval() || t.s.borrow().always_allow.contains(input.approval_key());
            // 你在卡片上把命令改了：按改过的跑，并告诉模型（不然它以为跑的是自己那条）
            let mut edited = None;
            if !allowed {
                let (approval, command) = self.ask_permission(t, &id, cancel).await;
                if let (Some(new), ToolInput::Run { command, .. }) = (command, &mut input)
                    && new != *command
                {
                    *command = new.clone();
                    edited = Some(new);
                    let title = input.title();
                    t.update_call(&id, |c| c.title = title.clone());
                }
                match approval {
                    Approval::Once => {}
                    Approval::Always => {
                        t.s.borrow_mut().always_allow.insert(input.approval_key().to_string());
                    }
                    Approval::Reject => {
                        t.update_call(&id, |c| c.status = ToolStatus::Failed);
                        results.push(result("The user declined this action. Wait for their next message.".into(), true));
                        halted = true;
                        continue;
                    }
                    Approval::Stopped => {
                        t.update_call(&id, |c| c.status = ToolStatus::Failed);
                        results.push(result("Not executed: the user stopped this turn.".into(), true));
                        halted = true;
                        continue;
                    }
                }
            }

            t.update_call(&id, |c| c.status = ToolStatus::InProgress);
            match self.execute(t, machine, &id, &input, cancel).await {
                Ok(text) => {
                    t.update_call(&id, |c| c.status = ToolStatus::Completed);
                    let text = match &edited {
                        Some(cmd) => format!("(The user edited the command before approving it. What actually ran: `{cmd}`)\n{text}"),
                        None => text,
                    };
                    results.push(result(text, false));
                }
                Err(e) if e == CANCELLED => {
                    t.update_call(&id, |c| c.status = ToolStatus::Failed);
                    results.push(result("Stopped by the user before it finished.".into(), true));
                    halted = true;
                }
                Err(e) => {
                    t.update_call(&id, |c| {
                        c.status = ToolStatus::Failed;
                        c.content.push(ToolContent::Text { text: e.clone() });
                    });
                    results.push(result(e, true));
                }
            }
        }
        if halted {
            t.notice(NoticeLevel::Info, CANCELLED);
        }
        (results, halted)
    }

    /// 卡片上挂按钮等你点，返回 (选中的选项 id, 你改过的命令)；停止了给 None
    async fn ask(
        &self,
        t: &Rc<Thread>,
        id: &str,
        options: Vec<PermissionOption>,
        cancel: &CancellationToken,
    ) -> Option<(String, Option<String>)> {
        let (tx, rx) = oneshot::channel();
        t.s.borrow_mut().permission = Some((id.to_string(), tx));
        t.update_call(id, |c| c.permission = Some(options));
        t.set_status(ThreadStatus::AwaitingPermission);
        self.save(t);
        let answer = tokio::select! {
            r = rx => r.ok(),
            _ = cancel.cancelled() => None,
        };
        t.s.borrow_mut().permission = None;
        t.update_call(id, |c| c.permission = None);
        t.set_status(ThreadStatus::Running);
        answer
    }

    async fn ask_permission(&self, t: &Rc<Thread>, id: &str, cancel: &CancellationToken) -> (Approval, Option<String>) {
        let (option, command) = self.ask(t, id, permission_options(), cancel).await.unzip();
        let approval = match option.as_deref() {
            Some(OPT_ALLOW) => Approval::Once,
            Some(OPT_ALWAYS) => Approval::Always,
            Some(OPT_REJECT) => Approval::Reject,
            _ => Approval::Stopped,
        };
        (approval, command.flatten())
    }

    /// 跑一个工具，返回给模型的文字
    async fn execute(
        &self,
        t: &Rc<Thread>,
        machine: &Machine,
        id: &str,
        input: &ToolInput,
        cancel: &CancellationToken,
    ) -> Result<String, String> {
        match input {
            ToolInput::Run { command, timeout_secs, .. } => {
                // 这段对话的终端（丢了就另开一个）：命令打进去，用户在时间线底部的实时区域看着它跑。
                // 卡片上只有命令与状态；跑完的输出记成一个命令块接在卡片后面，与你自己敲的一样
                let session = self.shell(&t.id, OscColorHint::default()).await?;
                let cwd = self.sessions.session_shell_info(&session.id).and_then(|(cwd, _, _)| cwd);
                let on_wait: Rc<dyn Fn()> = {
                    let t = t.clone();
                    let id = id.to_string();
                    Rc::new(move || {
                        t.update_call(&id, |c| {
                            c.content.insert(0, ToolContent::Text { text: "Shell 正忙，等它空出来……".into() });
                        })
                    })
                };
                let on_chunk: Rc<dyn Fn(&str)> = Rc::new(|_: &str| {});
                let run = self
                    .sessions
                    .run_in_session(&session.id, command, Duration::from_secs(*timeout_secs), cancel, on_wait, on_chunk)
                    .await;
                t.update_call(id, |c| c.content.retain(|x| !matches!(x, ToolContent::Text { .. })));
                let run = run?;
                // 超时的也记：到这一刻为止的输出，退出码空着（命令还在终端里跑）
                t.push(Entry::Command {
                    id: uuid_v4(),
                    command: command.clone(),
                    cwd,
                    output: run.output.clone(),
                    exit_code: run.exit_code,
                    by_agent: true,
                });
                let mut text = String::new();
                if !run.in_shell {
                    text.push_str("(This shell has no integration, so the command ran as a separate process.)\n");
                }
                if run.timed_out {
                    text.push_str(&format!(
                        "(Still running after {timeout_secs}s. It keeps running in the user's terminal; the output so far is below.)\n"
                    ));
                } else {
                    text.push_str(&format!("exit code: {}\n", run.exit_code.map_or("unknown".into(), |c| c.to_string())));
                }
                text.push_str(&truncate_middle(&plain_output(&run.output), MAX_TOOL_OUTPUT));
                Ok(text)
            }
            ToolInput::Read { path, offset, limit } => {
                let (host, cwd, home) = self.file_context(machine, t.session_id().as_deref()).await?;
                let abs = resolve_path(path, cwd.as_deref(), home.as_deref())?;
                let content = read_text(&host, &abs).await?.ok_or_else(|| format!("{abs} does not exist"))?;
                Ok(truncate_middle(&numbered(&content, *offset, *limit), MAX_TOOL_OUTPUT * 4))
            }
            ToolInput::Write { path, content } => {
                let (host, cwd, home) = self.file_context(machine, t.session_id().as_deref()).await?;
                let abs = resolve_path(path, cwd.as_deref(), home.as_deref())?;
                let old = read_text(&host, &abs).await.ok().flatten();
                write_text(&host, &abs, content).await?;
                t.update_call(id, |c| {
                    c.title = abs.clone();
                    c.content = vec![ToolContent::Diff { path: abs.clone(), old_text: old.clone(), new_text: content.clone() }];
                });
                Ok(format!("{} {abs} ({} bytes)", if old.is_some() { "Overwrote" } else { "Created" }, content.len()))
            }
            ToolInput::Edit { path, old_string, new_string, replace_all } => {
                let (host, cwd, home) = self.file_context(machine, t.session_id().as_deref()).await?;
                let abs = resolve_path(path, cwd.as_deref(), home.as_deref())?;
                let content = read_text(&host, &abs).await?.ok_or_else(|| format!("{abs} does not exist"))?;
                let next = apply_edit(&content, old_string, new_string, *replace_all)?;
                write_text(&host, &abs, &next).await?;
                t.update_call(id, |c| c.title = abs.clone());
                Ok(format!("Edited {abs}"))
            }
        }
    }

    /// 文件工具落到哪、相对路径按哪解析
    async fn file_context(
        &self,
        machine: &Machine,
        session: Option<&str>,
    ) -> Result<(FileHost, Option<String>, Option<String>), String> {
        let (cwd, _, mut home) = session.and_then(|sid| self.sessions.session_shell_info(sid)).unwrap_or_default();
        let host = match machine {
            Machine::Local => {
                if home.is_none() {
                    home = Some(crate::config::home_dir().to_string_lossy().into_owned());
                }
                if cfg!(windows) { FileHost::Unsupported } else { FileHost::Local }
            }
            Machine::Host(_) => {
                let project = owner_row(&self.db, &machine.owner_id()).ok_or("这台主机已经不在了")?;
                let link = self.sessions.get_link(&project);
                let facts = link.host_facts().await.map_err(|e| format!("连不上这台主机：{e}"))?;
                if home.is_none() {
                    home = Some(facts.home.clone());
                }
                tools::file_host_for(Some(facts.kind), Some(link))
            }
        };
        Ok((host, cwd, home))
    }

    // ---------------- ACP agent ----------------

    /// ACP agent 的一轮：起进程（或用现成的）、发 session/prompt、等它说完。过程中的输出走
    /// session/update 通知（映射进对话），权限与读写文件是它反过来的请求（[`Self::acp_request`]）
    async fn run_acp_turn(
        &self,
        t: &Rc<Thread>,
        cfg: &AcpAgentConfig,
        text: String,
        context: Option<String>,
        cancel: &CancellationToken,
    ) -> ThreadStatus {
        let conn = match self.ensure_acp(t, cfg).await {
            Ok(c) => c,
            Err(e) => {
                t.notice(NoticeLevel::Error, e);
                return ThreadStatus::Error;
            }
        };
        let Some(session) = t.s.borrow().acp_session.clone() else { return ThreadStatus::Error };
        t.s.borrow_mut().acp_mapper.reset();
        let mut prompt = vec![json!({ "type": "text", "text": text })];
        // 最近的命令（已经排好版）放在前面，正文在后
        if let Some(ctx) = &context {
            prompt.insert(0, json!({ "type": "text", "text": ctx }));
        }
        let req = conn.request("session/prompt", json!({ "sessionId": session, "prompt": prompt }));
        tokio::pin!(req);
        let res = tokio::select! {
            r = &mut req => r,
            _ = cancel.cancelled() => {
                // 请它停下，等它把这一轮收尾（它会回 stopReason: cancelled）；不理人就不等了
                conn.notify("session/cancel", json!({ "sessionId": session }));
                match tokio::time::timeout(Duration::from_secs(10), &mut req).await {
                    Ok(r) => r,
                    Err(_) => {
                        t.notice(NoticeLevel::Info, CANCELLED);
                        return ThreadStatus::Idle;
                    }
                }
            }
        };
        match res {
            Ok(v) => match v["stopReason"].as_str().unwrap_or("end_turn") {
                "cancelled" => {
                    t.notice(NoticeLevel::Info, CANCELLED);
                    ThreadStatus::Idle
                }
                "refusal" => {
                    t.notice(NoticeLevel::Warning, format!("{} 拒绝了这个请求", cfg.name));
                    ThreadStatus::Idle
                }
                "max_tokens" | "max_turn_requests" => {
                    t.notice(NoticeLevel::Warning, format!("{} 这一轮到上限停下了，要继续就再说一句", cfg.name));
                    ThreadStatus::Idle
                }
                _ => ThreadStatus::Idle,
            },
            Err(e) => {
                let tail = if conn.is_alive() { String::new() } else { conn.stderr_tail() };
                let tail = if tail.is_empty() { tail } else { format!("\n{tail}") };
                t.notice(NoticeLevel::Error, format!("{} 出错了：{e}{tail}", cfg.name));
                ThreadStatus::Error
            }
        }
    }

    /// 这段对话的 agent 进程：活着就用，没有就起一个、握手、开（或接回）会话
    async fn ensure_acp(&self, t: &Rc<Thread>, cfg: &AcpAgentConfig) -> Result<Rc<AcpConn>, String> {
        let existing = t.s.borrow().acp.clone();
        if let Some(c) = existing.filter(|c| c.is_alive()) {
            return Ok(c);
        }
        let env = self.sessions.local.base_env().await;
        let (cwd, _, home) = t.session_id().and_then(|sid| self.sessions.session_shell_info(&sid)).unwrap_or_default();
        let cwd = cwd.or(home).unwrap_or_else(|| crate::config::home_dir().to_string_lossy().into_owned());

        let on_notify: NotifyHandler = {
            let (weak_t, weak_hub) = (Rc::downgrade(t), self.me.clone());
            Rc::new(move |method, params| {
                if method != "session/update" {
                    return;
                }
                let Some(t) = weak_t.upgrade() else { return };
                if t.s.borrow().acp_loading {
                    return;
                }
                // 映射会回头改对话的状态：先把映射器拿出来，别借着 t.s 调它
                let mut mapper = std::mem::take(&mut t.s.borrow_mut().acp_mapper);
                mapper.apply(&params["update"], &*t);
                t.s.borrow_mut().acp_mapper = mapper;
                if let Some(hub) = weak_hub.upgrade() {
                    hub.maybe_save(&t);
                }
            })
        };
        let on_request: RequestHandler = {
            let (weak_t, weak_hub) = (Rc::downgrade(t), self.me.clone());
            Rc::new(move |method, params| {
                let (t, hub) = (weak_t.upgrade(), weak_hub.upgrade());
                Box::pin(async move {
                    let (Some(t), Some(hub)) = (t, hub) else {
                        return Err(RpcError { code: -32603, message: "对话已经没了".into(), data: None });
                    };
                    hub.acp_request(&t, &method, params).await
                })
            })
        };
        let conn = Rc::new(AcpConn::spawn(cfg, &env, &cwd, on_request, on_notify)?);
        let failed = |what: &str, e: RpcError| {
            let tail = conn.stderr_tail();
            let tail = if tail.is_empty() { tail } else { format!("\n{tail}") };
            format!("{} {what}失败：{e}{tail}", cfg.name)
        };
        let init = conn.request("initialize", initialize_params()).await.map_err(|e| failed("握手", e))?;
        let can_load = init["agentCapabilities"]["loadSession"].as_bool().unwrap_or(false);
        let auth = init["authMethods"].clone();
        let new_session = || async {
            match conn.request("session/new", json!({ "cwd": cwd, "mcpServers": [] })).await {
                Ok(v) => v["sessionId"].as_str().map(str::to_string).ok_or_else(|| format!("{} 没给会话 id", cfg.name)),
                Err(e) if e.code == AUTH_REQUIRED => Err(auth_hint(cfg, &auth)),
                Err(e) => Err(failed("开会话", e)),
            }
        };
        let stored = t.s.borrow().acp_session.clone();
        let session = match stored {
            Some(sid) if can_load => {
                t.s.borrow_mut().acp_loading = true;
                let loaded = conn.request("session/load", json!({ "sessionId": sid, "cwd": cwd, "mcpServers": [] })).await;
                t.s.borrow_mut().acp_loading = false;
                match loaded {
                    Ok(_) => sid,
                    Err(_) => {
                        let fresh = new_session().await?;
                        t.notice(NoticeLevel::Info, format!("{} 接不回上一次的上下文，开了一个新会话", cfg.name));
                        fresh
                    }
                }
            }
            Some(_) => {
                let fresh = new_session().await?;
                t.notice(NoticeLevel::Info, format!("{} 不支持接回会话，上下文已重置", cfg.name));
                fresh
            }
            None => new_session().await?,
        };
        {
            let mut s = t.s.borrow_mut();
            s.acp = Some(conn.clone());
            s.acp_session = Some(session);
        }
        self.save(t);
        Ok(conn)
    }

    /// agent 反过来的请求：权限、读写本机文件。没声明的能力（terminal/*）一律「没有这个方法」
    async fn acp_request(&self, t: &Rc<Thread>, method: &str, params: Value) -> Result<Value, RpcError> {
        let err = |code: i64, message: String| RpcError { code, message, data: None };
        match method {
            "session/request_permission" => {
                let call = &params["toolCall"];
                let id = call["toolCallId"].as_str().unwrap_or("").to_string();
                if !EntrySink::update_call(&**t, &id, &mut |c| patch_call(c, call)) {
                    let mut c = ToolCall {
                        id: id.clone(),
                        title: String::new(),
                        description: None,
                        kind: ToolKind::Other,
                        status: ToolStatus::Pending,
                        content: Vec::new(),
                        permission: None,
                    };
                    patch_call(&mut c, call);
                    t.push(Entry::ToolCall(c));
                }
                let options: Vec<PermissionOption> = params["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| PermissionOption {
                        id: o["optionId"].as_str().unwrap_or("").to_string(),
                        name: o["name"].as_str().unwrap_or("").to_string(),
                        kind: serde_json::from_value(o["kind"].clone()).unwrap_or(PermissionKind::Unknown),
                    })
                    .collect();
                // 设置里关了询问：替你选「允许一次」
                if settings::auto_approve(&self.db)
                    && let Some(o) = options.iter().find(|o| o.kind == PermissionKind::AllowOnce)
                {
                    return Ok(json!({ "outcome": { "outcome": "selected", "optionId": o.id } }));
                }
                let cancel = t.s.borrow().cancel.clone().unwrap_or_default();
                Ok(match self.ask(t, &id, options, &cancel).await {
                    // ACP 的审批没有「改了再跑」，改过的命令丢掉（界面上 ACP 的卡片也不给「修改」）
                    Some((opt, _)) => json!({ "outcome": { "outcome": "selected", "optionId": opt } }),
                    None => json!({ "outcome": { "outcome": "cancelled" } }),
                })
            }
            "fs/read_text_file" => {
                let path = params["path"].as_str().ok_or_else(|| err(-32602, "缺 path".into()))?;
                let content = read_text(&FileHost::Local, path)
                    .await
                    .map_err(|e| err(-32603, e))?
                    .ok_or_else(|| err(-32002, format!("{path} does not exist")))?;
                Ok(json!({ "content": slice_lines(&content, params["line"].as_u64(), params["limit"].as_u64()) }))
            }
            "fs/write_text_file" => {
                let path = params["path"].as_str().ok_or_else(|| err(-32602, "缺 path".into()))?;
                let content = params["content"].as_str().unwrap_or("");
                write_text(&FileHost::Local, path, content).await.map_err(|e| err(-32603, e))?;
                Ok(Value::Null)
            }
            other => Err(err(-32601, format!("falcon 没有实现 {other}"))),
        }
    }

    async fn machine_facts(&self, machine: &Machine, session: Option<&str>) -> MachineFacts {
        let (_, user, home) = session.and_then(|sid| self.sessions.session_shell_info(sid)).unwrap_or_default();
        match machine {
            Machine::Local => MachineFacts {
                label: "the computer running the Falcon server".into(),
                os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
                shell: crate::sessions::local::default_local_shell(),
                user: user.or_else(|| std::env::var("USER").ok()),
                home: home.or_else(|| Some(crate::config::home_dir().to_string_lossy().into_owned())),
                windows: cfg!(windows),
            },
            Machine::Host(id) => {
                let host = self.db.get_host(id);
                let label = host
                    .as_ref()
                    .map(|h| format!("{} (SSH host {}@{}:{})", h.name, h.username, h.host, h.port))
                    .unwrap_or_else(|| "an SSH host".into());
                let facts = match owner_row(&self.db, &machine.owner_id()) {
                    Some(p) => self.sessions.get_link(&p).host_facts().await.ok(),
                    None => None,
                };
                MachineFacts {
                    label,
                    os: facts.as_ref().and_then(|f| f.uname.clone()).unwrap_or_else(|| "unknown".into()),
                    shell: facts.as_ref().map(|f| f.shell.clone()).unwrap_or_default(),
                    user: user.or_else(|| host.map(|h| h.username)),
                    home: home.or_else(|| facts.as_ref().map(|f| f.home.clone())),
                    windows: facts.is_some_and(|f| f.kind == crate::host::HostKind::Windows),
                }
            }
        }
    }
}

/// 最近几条命令最多带几条、每条输出最多多少字
const RECENT_COMMANDS: usize = 5;
const RECENT_OUTPUT_CHARS: usize = 3000;

/// 上一次跟 agent 说话之后，用户在终端里敲过的命令（最多最近 5 条），排成一段给模型看的文字。
/// 一条都没有就是 None
fn recent_commands(entries: &[Entry]) -> Option<String> {
    let since = entries.iter().rposition(|e| matches!(e, Entry::User { .. })).map_or(0, |i| i + 1);
    // agent 自己跑的不算：它知道自己跑了什么，结果也已经在它的历史里
    let cmds: Vec<&Entry> =
        entries[since..].iter().filter(|e| matches!(e, Entry::Command { by_agent: false, .. })).collect();
    if cmds.is_empty() {
        return None;
    }
    let mut out = String::from("<terminal_activity>\nCommands the user ran in this terminal since your last turn:\n");
    for e in cmds.iter().rev().take(RECENT_COMMANDS).rev() {
        let Entry::Command { command, cwd, output, exit_code, .. } = e else { continue };
        out.push_str(&format!(
            "\n$ {command}\n(cwd: {}, exit code: {})\n{}\n",
            cwd.as_deref().unwrap_or("?"),
            exit_code.map_or("unknown".into(), |c| c.to_string()),
            truncate_middle(plain_output(output).trim_end(), RECENT_OUTPUT_CHARS),
        ));
    }
    out.push_str("</terminal_activity>");
    Some(out)
}

/// 字节流按 UTF-8 解码：一个汉字可能被切在两块之间，不全的尾巴留到下一块
#[derive(Default)]
struct Utf8Carry {
    pending: Vec<u8>,
}

impl Utf8Carry {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        match std::str::from_utf8(&self.pending) {
            Ok(s) => {
                let s = s.to_string();
                self.pending.clear();
                s
            }
            Err(e) if e.error_len().is_none() => {
                // 尾巴不全：先交出前面完整的部分
                let valid = e.valid_up_to();
                let s = String::from_utf8_lossy(&self.pending[..valid]).into_owned();
                self.pending.drain(..valid);
                s
            }
            Err(_) => {
                let s = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                s
            }
        }
    }
}

#[cfg(test)]
mod tests;
