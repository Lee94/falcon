//! 一段对话（docs/design/ai-assistant.md 2.2）：一条时间线 + 底部的输入。
//!
//! 一段对话就是一个会话（像 Warp 的一个标签）：自己的终端、自己的命令块、自己的 AI 上下文。
//! 时间线从上往下是服务端存的条目（你敲完的命令块、你对 agent 说的话、AI 的话与工具卡片……），
//! 最底下是**实时区域**——这段对话的终端，只露出「当前提示符到光标」那几行（[`TerminalView::live_slice`]）。
//! 命令在跑时它跟着长，跑完那一块由服务端记成命令块接到上面，实时区域又只剩新的提示符。全屏程序
//! （vim、claude、top）开着时整块露出来。
//!
//! 输入有两种模式，⌘⇧I 切换，都在时间线最底下那一行「命令行」上：
//! - **Shell**：那一行就是实时终端的提示符，键盘在终端里，补全、历史都是你自己 shell 的；
//! - **Agent**：那一行换成 `✦ 目录 ▌`（同样的字体字号），Enter 交给 agent。不另起一个输入框：
//!   你对 agent 说的话与你敲的命令在同一条时间线、同一个位置。命令在跑时实时终端照样露着。
//!
//! AI 的一轮收在一道竖线里：思考（直接画出来）、回复、每一步的说明与卡片；在跑时末尾一行
//! 「AI 正在思考… | 终止」。内置 agent 的命令输出不在卡片上，跑完记成卡片后面的命令块。
//!
//! 实时区域的终端按面板的高度定格子数（[`ThreadPane::set_term_height`]），切模式不改它：PTY 一
//! resize，正在跑的全屏程序就整屏重画。

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use falcon_client::{AssistantEvent, AssistantSink, AssistantSocket};
use falcon_core::assistant::shell_title;
use falcon_proto::CreateSessionRequest;
use falcon_proto::assistant::{
    AssistantClientMessage, AssistantServerMessage, AssistantSettings, BUILTIN_AGENT, Entry, PermissionKind,
    ThreadPatch, ThreadStatus, ThreadSummary, ToolContent,
};
use futures::StreamExt as _;
use futures::channel::mpsc::unbounded;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Enter, InputEvent, TextareaState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Pixels, Render,
    ScrollHandle, SharedString, Task, Window, div,
};
use rust_i18n::t;

use super::blocks;
use crate::labels::local_word;
use crate::menus::{MenuItemSpec, to_popup};
use crate::terminal::view::{TerminalView, TerminalViewEvent};
use crate::theme::{TerminalLook, Ui, radius};
use crate::ui::icon;
use crate::workspace::{AuthPhase, Workspace};
use crate::zoom::zpx;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Shell,
    Agent,
}

pub enum PaneEvent {
    /// 标题、状态、agent 变了：面板的对话列表跟着刷新
    Changed,
}

impl EventEmitter<PaneEvent> for ThreadPane {}

/// 实时区域的终端
enum Term {
    Opening,
    Ready(Entity<TerminalView>),
    Failed(String),
    /// 终端结束了（`exit`、被终止）。不自动另开：rc 文件有问题时 shell 一起来就退，自动另开会
    /// 停不下来。底下给一个「新终端」按钮，命令块与 AI 上下文都还在
    Ended,
}

pub struct ThreadPane {
    ws: Entity<Workspace>,
    pub summary: ThreadSummary,
    socket: Option<AssistantSocket>,
    _pump: Option<Task<()>>,
    entries: Vec<Entry>,
    status: ThreadStatus,
    connected: bool,
    term: Term,
    /// 实时区域终端的高度（面板给的）
    term_height: Pixels,
    pub mode: Mode,
    input: Entity<TextareaState>,
    /// 点开了的思考、展开了的命令块
    expanded: HashSet<String>,
    /// Agent 输入那一行的输入框（自己一个视图，见 [`blocks::InputView`]）
    input_view: Entity<blocks::InputView>,
    /// 正在卡片上改的命令：(卡片 id, 输入框, 画它的视图)。「修改」点开，答了或取消就收起
    editing: Option<(String, Entity<TextareaState>, Entity<blocks::InputView>)>,
    /// 「在跑」那一行的转圈（自己一个视图，见 [`blocks::Spinner`]）
    spinner: Entity<blocks::Spinner>,
    scroll: ScrollHandle,
    settings: Option<AssistantSettings>,
    focus: FocusHandle,
}

impl ThreadPane {
    pub fn new(
        ws: Entity<Workspace>,
        summary: ThreadSummary,
        settings: Option<AssistantSettings>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let chord = if crate::ui::mac_keys() { "⌘⇧I" } else { "Ctrl+Shift+I" };
            TextareaState::new(window, cx).auto_grow(1, 8).placeholder(t!("assistant.placeholder", chord = chord).to_string())
        });
        cx.subscribe(&input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        })
        .detach();
        let input_view = cx.new(|_| blocks::InputView::new(input.clone()));
        let mut this = Self {
            ws,
            summary,
            socket: None,
            _pump: None,
            entries: Vec::new(),
            status: ThreadStatus::Idle,
            connected: false,
            term: Term::Opening,
            term_height: zpx(400.),
            mode: Mode::Shell,
            input,
            expanded: HashSet::new(),
            input_view,
            editing: None,
            spinner: cx.new(|_| blocks::Spinner),
            scroll: ScrollHandle::new(),
            settings,
            focus: cx.focus_handle(),
        };
        this.connect(cx);
        this.open_shell(window, cx);
        this
    }

    pub fn set_settings(&mut self, settings: Option<AssistantSettings>, cx: &mut Context<Self>) {
        self.settings = settings;
        cx.notify();
    }

    pub fn set_term_height(&mut self, h: Pixels) {
        self.term_height = h;
    }

    /// 条目的 WS：连上先收快照、再收增量
    fn connect(&mut self, cx: &mut Context<Self>) {
        let (tx, mut rx) = unbounded::<AssistantEvent>();
        let sink: Arc<dyn AssistantSink> = Arc::new(move |ev: AssistantEvent| {
            let _ = tx.unbounded_send(ev);
        });
        let client = self.ws.read(cx).client.clone();
        self.socket = Some(AssistantSocket::open(&client, &self.summary.id, sink));
        self._pump = Some(cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.next().await {
                if this.update(cx, |this, cx| this.on_event(ev, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// 这段对话的终端：活着就接上，丢了服务端另开一个
    fn open_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.term = Term::Opening;
        let client = self.ws.read(cx).client.clone();
        let hint = TerminalLook::global(cx).appearance.clone();
        let id = self.summary.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let request = CreateSessionRequest {
                name: None,
                agent: None,
                appearance: hint.appearance,
                background: hint.background.clone(),
                foreground: hint.foreground.clone(),
            };
            let result = client.assistant_thread_shell(&id, &request).await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(session) => this.attach(session.id, session.durable, window, cx),
                Err(err) => {
                    this.ws.update(cx, |w, cx| w.handle_error(&err, cx));
                    this.term = Term::Failed(err.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn attach(&mut self, session_id: String, durable: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.summary.session_id = Some(session_id.clone());
        let ws = self.ws.read(cx);
        let client = ws.client.clone();
        let local_server = ws.profile.is_loopback() && self.summary.machine == falcon_core::assistant::LOCAL;
        let view = cx.new(|cx| TerminalView::new_live(session_id, client, local_server, Some(durable), window, cx));
        cx.subscribe_in(&view, window, |this, _, ev: &TerminalViewEvent, window, cx| match ev {
            TerminalViewEvent::NewTerminal | TerminalViewEvent::ClearRecord => this.open_shell(window, cx),
            TerminalViewEvent::State { state: falcon_proto::SessionState::Dead, .. } => {
                this.term = Term::Ended;
                cx.notify();
            }
            TerminalViewEvent::Askpass { id, prompt } => {
                let (id, prompt) = (id.clone(), prompt.clone());
                this.ws.update(cx, |w, cx| w.push_askpass(id, prompt, cx));
            }
            TerminalViewEvent::Unauthorized => this.ws.update(cx, |w, cx| {
                if w.auth_phase == AuthPhase::Ready {
                    w.auth_phase = AuthPhase::NeedLogin;
                    cx.notify();
                }
            }),
            TerminalViewEvent::Toast { kind, title, body } => {
                let (kind, title, body) = (*kind, title.clone(), body.clone());
                this.ws.update(cx, |w, cx| w.toast(kind, title, body, cx));
            }
            // 实时区域长了 / 缩了：人停在底部就跟着往下走
            TerminalViewEvent::LiveChanged => {
                if this.at_bottom() {
                    this.scroll.scroll_to_bottom();
                }
                cx.notify();
            }
            _ => cx.notify(),
        })
        .detach();
        self.term = Term::Ready(view);
        self.scroll.scroll_to_bottom();
        if self.mode == Mode::Shell {
            self.focus_input(window, cx);
        }
        cx.notify();
    }

    #[cfg(feature = "automation")]
    pub fn terminal(&self) -> Option<Entity<TerminalView>> {
        match &self.term {
            Term::Ready(v) => Some(v.clone()),
            _ => None,
        }
    }

    /// 焦点交给当前模式的输入：Shell 是实时区域的终端，Agent 是输入框
    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        match (self.mode, &self.term) {
            (Mode::Shell, Term::Ready(view)) => {
                let handle = view.read(cx).focus_handle().clone();
                handle.focus(window, cx);
            }
            (Mode::Agent, _) => {
                let input = self.input.clone();
                input.update(cx, |i, cx| i.focus(window, cx));
            }
            _ => self.focus.focus(window, cx),
        }
    }

    pub fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.focus_input(window, cx);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    pub fn toggle_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let next = if self.mode == Mode::Shell { Mode::Agent } else { Mode::Shell };
        self.set_mode(next, window, cx);
    }

    fn on_event(&mut self, ev: AssistantEvent, cx: &mut Context<Self>) {
        match ev {
            AssistantEvent::Connected => self.connected = true,
            AssistantEvent::Disconnected => self.connected = false,
            AssistantEvent::Unauthorized => self.ws.update(cx, |w, cx| {
                if w.auth_phase == AuthPhase::Ready {
                    w.auth_phase = AuthPhase::NeedLogin;
                    cx.notify();
                }
            }),
            AssistantEvent::Message(msg) => self.on_message(msg, cx),
        }
        cx.notify();
    }

    fn on_message(&mut self, msg: AssistantServerMessage, cx: &mut Context<Self>) {
        let at_bottom = self.at_bottom();
        match msg {
            AssistantServerMessage::Snapshot { thread, entries } => {
                self.entries = entries;
                self.status = thread.status;
                let changed = self.summary.title != thread.title;
                self.summary = ThreadSummary { session_id: self.summary.session_id.clone(), ..thread };
                if changed {
                    cx.emit(PaneEvent::Changed);
                }
                self.scroll.scroll_to_bottom();
                return;
            }
            AssistantServerMessage::Entry { entry } => {
                let id = entry.id().map(str::to_string);
                match self.entries.iter_mut().find(|e| id.is_some() && e.id().map(str::to_string) == id) {
                    Some(slot) => *slot = entry,
                    None => self.entries.push(entry),
                }
            }
            AssistantServerMessage::Append { id, text } => {
                if let Some(e) = self.entries.iter_mut().find(|e| e.id() == Some(id.as_str())) {
                    match e {
                        Entry::Agent { text: t, .. } | Entry::Thought { text: t, .. } => t.push_str(&text),
                        Entry::ToolCall(c) => match c.content.iter_mut().find(|x| matches!(x, ToolContent::Terminal { .. })) {
                            Some(ToolContent::Terminal { output, .. }) => output.push_str(&text),
                            _ => c.content.push(ToolContent::Terminal { output: text, exit_code: None }),
                        },
                        _ => {}
                    }
                }
            }
            AssistantServerMessage::Status { status } => {
                self.status = status;
                self.summary.status = status;
                cx.emit(PaneEvent::Changed);
            }
            AssistantServerMessage::Thread { thread } => {
                // 终端那一格以我们自己接上的为准（attach 时记过）；名字、agent 跟服务端
                self.summary = ThreadSummary { session_id: self.summary.session_id.clone(), ..thread };
                cx.emit(PaneEvent::Changed);
            }
            AssistantServerMessage::Unknown => {}
        }
        if at_bottom {
            self.scroll.scroll_to_bottom();
        }
    }

    fn at_bottom(&self) -> bool {
        let max = self.scroll.max_offset().y;
        let off = -self.scroll.offset().y;
        max - off < zpx(48.)
    }

    fn busy(&self) -> bool {
        matches!(self.status, ThreadStatus::Running | ThreadStatus::AwaitingPermission)
    }

    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() || self.busy() {
            return;
        }
        self.input.update(cx, |i, cx| i.set_value("", window, cx));
        if let Some(socket) = &self.socket {
            socket.send(&AssistantClientMessage::Prompt { text, context: None });
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    #[cfg(feature = "automation")]
    pub fn automation_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(Mode::Agent, window, cx);
        self.input.update(cx, |i, cx| i.set_value(text.to_string(), window, cx));
        self.send(window, cx);
    }

    fn stop(&self) {
        if let Some(s) = &self.socket {
            s.send(&AssistantClientMessage::Cancel);
        }
    }

    /// 答卡片上的审批；`command` 是在卡片上改过的命令
    fn answer(&mut self, call_id: String, option_id: String, command: Option<String>, cx: &mut Context<Self>) {
        if self.editing.as_ref().is_some_and(|(id, _, _)| *id == call_id) {
            self.editing = None;
        }
        if let Some(s) = &self.socket {
            s.send(&AssistantClientMessage::Permission { tool_call_id: call_id, option_id, command });
        }
        cx.notify();
    }

    /// 最上面那张在等批准的卡片：(卡片 id, 这一种选项的 id)
    fn pending(&self, kind: PermissionKind) -> Option<(String, String)> {
        self.entries.iter().find_map(|e| match e {
            Entry::ToolCall(c) => c
                .permission
                .as_ref()
                .and_then(|opts| opts.iter().find(|o| o.kind == kind).map(|o| (c.id.clone(), o.id.clone()))),
            _ => None,
        })
    }

    /// ⌘↵ / ⌥⌘Y / ⌥⌘Z：答最上面那张在等批准的卡片。正在改它的命令时，⌘↵ 按改过的跑
    fn answer_by_kind(&mut self, kind: PermissionKind, cx: &mut Context<Self>) {
        let Some((call, opt)) = self.pending(kind) else { return };
        let edited = match &self.editing {
            Some((id, input, _)) if *id == call && kind == PermissionKind::AllowOnce => {
                Some(input.read(cx).value().trim().to_string()).filter(|c| !c.is_empty())
            }
            _ => None,
        };
        self.answer(call, opt, edited, cx);
    }

    /// 「修改」：卡片上的命令变成输入框，焦点给它
    fn start_edit(&mut self, call_id: String, command: String, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut i = TextareaState::new(window, cx).auto_grow(1, 6);
            i.set_value(command, window, cx);
            i
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        let view = cx.new(|_| blocks::InputView::new(input.clone()));
        self.editing = Some((call_id, input, view));
        cx.notify();
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.focus_input(window, cx);
        cx.notify();
    }

    /// 换 agent（之后的话交给它；终端与命令块不动）
    pub fn set_agent(&mut self, agent: String, cx: &mut Context<Self>) {
        let client = self.ws.read(cx).client.clone();
        let id = self.summary.id.clone();
        cx.spawn(async move |this, cx| {
            let res = client.update_assistant_thread(&id, &ThreadPatch { agent: Some(agent) }).await;
            this.update(cx, |this, cx| {
                match res {
                    Ok(s) => {
                        this.summary.agent = s.agent;
                        cx.emit(PaneEvent::Changed);
                    }
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn agent_name(&self, agent: &str) -> String {
        if agent == BUILTIN_AGENT {
            return t!("assistant.builtinAgent").to_string();
        }
        let id = agent.strip_prefix(falcon_proto::assistant::ACP_AGENT_PREFIX).unwrap_or(agent);
        self.settings
            .as_ref()
            .and_then(|s| s.acp_agents.iter().find(|a| a.id == id))
            .map(|a| a.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    /// 可选的 agent：内置；本机再加上设置里开着的 ACP agent（ACP 只在本机起）
    fn agent_choices(&self) -> Vec<String> {
        let mut out = vec![BUILTIN_AGENT.to_string()];
        if self.summary.machine == falcon_core::assistant::LOCAL
            && let Some(s) = &self.settings
        {
            out.extend(s.acp_agents.iter().filter(|a| a.enabled).map(|a| a.agent_id()));
        }
        out
    }

    /// 底栏左边那一格：`用户@机器:~/目录`（来自终端的 shell integration）
    pub fn location(&self, cx: &App) -> String {
        let hosts = &self.ws.read(cx).hosts;
        let word = local_word();
        match &self.term {
            Term::Ready(view) => {
                let v = view.read(cx);
                shell_title(
                    &self.summary.machine,
                    hosts,
                    &word,
                    v.shell_user.as_deref(),
                    v.shell_cwd.as_deref(),
                    v.shell_home.as_deref(),
                )
            }
            _ => falcon_core::assistant::machine_label(&self.summary.machine, hosts, &word),
        }
    }

    fn home(&self, cx: &App) -> Option<String> {
        match &self.term {
            Term::Ready(view) => view.read(cx).shell_home.clone(),
            _ => None,
        }
    }

    // ---------------- 渲染 ----------------

    fn toggle(&self, id: &str, cx: &mut Context<Self>) -> blocks::Click {
        let (this, id) = (cx.entity(), id.to_string());
        Rc::new(move |_, cx| {
            this.update(cx, |v, cx| {
                if !v.expanded.remove(&id) {
                    v.expanded.insert(id.clone());
                }
                cx.notify();
            })
        })
    }

    /// 时间线：命令块与你说的话各占一行（像终端），AI 连着的几条（思考、回复、卡片、计划、系统的话）
    /// 收进一道竖线里。agent 跑完的命令块把一轮切成两段——它的输出是终端里的东西，不归 AI 那一栏
    fn timeline(&self, home: Option<&str>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let entries = self.entries.clone();
        let builtin = self.summary.agent == BUILTIN_AGENT;
        let mut out = Vec::new();
        let mut group: Vec<AnyElement> = Vec::new();
        let flush = |group: &mut Vec<AnyElement>, out: &mut Vec<AnyElement>, cx: &mut Context<Self>| {
            if !group.is_empty() {
                out.push(blocks::ai_group(std::mem::take(group), cx));
            }
        };
        for e in &entries {
            // 一段里头一条带字的（思考或回复）前面画 ✦
            let lead = group.is_empty();
            match e {
                Entry::Command { id, command, cwd, output, exit_code, .. } => {
                    flush(&mut group, &mut out, cx);
                    let toggle = self.toggle(id, cx);
                    out.push(blocks::command_block(
                        id,
                        command,
                        cwd.as_deref(),
                        home,
                        output,
                        *exit_code,
                        self.expanded.contains(id),
                        toggle,
                        cx,
                    ));
                }
                Entry::User { text, cwd, .. } => {
                    flush(&mut group, &mut out, cx);
                    out.push(blocks::user_prompt(text, cwd.as_deref(), home, cx));
                }
                Entry::Agent { id, text } => group.extend(blocks::agent_text(id, text, lead, cx)),
                Entry::Thought { id, text } => {
                    let toggle = self.toggle(id, cx);
                    group.extend(blocks::thought(id, text, lead, self.expanded.contains(id), toggle, cx));
                }
                Entry::ToolCall(call) => group.push(self.step(call, builtin, cx)),
                Entry::Notice { text, level, .. } => group.push(blocks::notice(text, *level, cx)),
                Entry::Plan { items, .. } => group.push(blocks::plan(items, cx)),
                Entry::Unknown => {}
            }
        }
        flush(&mut group, &mut out, cx);
        out
    }

    /// 一步：说明 + 卡片。「修改」只给内置 agent 的单行命令（标题就是整条命令；多行的标题是
    /// 第一行加「…」，改不全）
    fn step(&self, call: &falcon_proto::assistant::ToolCall, builtin: bool, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        let answer: Rc<dyn Fn(String, String, Option<String>, &mut App)> = {
            let this = this.clone();
            Rc::new(move |call, opt, cmd, cx| this.update(cx, |v, cx| v.answer(call, opt, cmd, cx)))
        };
        let editable = builtin
            && call.kind == falcon_proto::assistant::ToolKind::Execute
            && call.permission.is_some()
            && !call.title.is_empty()
            && !call.title.ends_with(" …");
        let modify: Option<Rc<dyn Fn(&mut Window, &mut App)>> = editable.then(|| {
            let (this, id, cmd) = (this.clone(), call.id.clone(), call.title.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                this.update(cx, |v, cx| v.start_edit(id.clone(), cmd.clone(), window, cx))
            }) as Rc<dyn Fn(&mut Window, &mut App)>
        });
        let cancel_edit: blocks::Click = {
            let this = this.clone();
            Rc::new(move |window, cx| this.update(cx, |v, cx| v.cancel_edit(window, cx)))
        };
        let edit = self
            .editing
            .as_ref()
            .filter(|(id, _, _)| *id == call.id && call.permission.is_some())
            .map(|(_, input, view)| blocks::CardEdit { input, view });
        let el = blocks::tool_step(call, builtin, edit, blocks::CardActions { answer, modify, cancel_edit }, cx);
        // 改命令的输入框里：Enter / ⌘↵ 都是按改过的跑（单行命令，没有换行要插）
        match &self.editing {
            Some((id, _, _)) if *id == call.id => div()
                .capture_action(cx.listener(|this, e: &Enter, _, cx| {
                    if !e.shift {
                        cx.stop_propagation();
                        this.answer_by_kind(PermissionKind::AllowOnce, cx);
                    }
                }))
                .child(el)
                .into_any_element(),
            _ => el,
        }
    }

    /// 在跑时时间线末尾那一行：在干什么 + 终止
    fn status_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.busy() {
            return None;
        }
        let running_call = self.entries.iter().rev().find_map(|e| match e {
            Entry::ToolCall(c) if c.status == falcon_proto::assistant::ToolStatus::InProgress => Some(c.kind),
            _ => None,
        });
        let label = match (self.status, running_call) {
            (ThreadStatus::AwaitingPermission, _) => t!("assistant.status.awaiting"),
            (_, Some(falcon_proto::assistant::ToolKind::Execute)) => t!("assistant.status.executing"),
            (_, Some(_)) => t!("assistant.status.working"),
            _ => t!("assistant.status.thinking"),
        };
        let this = cx.entity();
        let stop: blocks::Click = Rc::new(move |_, cx| this.update(cx, |v, _| v.stop()));
        Some(blocks::status_row(&self.summary.id, self.spinner.clone().into_any_element(), label.to_string(), stop, cx))
    }

    /// Agent 输入那一行（在时间线最底下，替掉终端的提示符）。Enter 交给 agent、⇧Enter 换行、
    /// ⌘↵ 批准最上面那张卡片。在捕获阶段拦：输入框就不会顺手插一个换行
    fn agent_line(&self, home: Option<&str>, cx: &mut Context<Self>) -> AnyElement {
        let cwd = match &self.term {
            Term::Ready(view) => view.read(cx).shell_cwd.clone(),
            _ => None,
        };
        blocks::agent_input_line(&self.input_view, cwd.as_deref(), home, cx)
            .capture_action(cx.listener(|this, e: &Enter, window, cx| {
                if e.secondary {
                    cx.stop_propagation();
                    this.answer_by_kind(PermissionKind::AllowOnce, cx);
                } else if !e.shift {
                    cx.stop_propagation();
                    this.send(window, cx);
                }
            }))
            .into_any_element()
    }

    /// 内置 agent 没有 API key：时间线底下一条提示 + 去设置
    fn no_key_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let no_key = self.summary.agent == BUILTIN_AGENT && self.settings.as_ref().is_some_and(|s| !s.has_api_key);
        if !no_key || self.mode != Mode::Agent {
            return None;
        }
        let ui = Ui::global(cx).clone();
        let ws = self.ws.clone();
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .p_2()
                .rounded(radius::MD)
                .bg(ui.muted)
                .text_xs()
                .child(icon(IconName::TriangleAlert).size(zpx(14.)).text_color(ui.warning))
                .child(div().flex_1().child(t!("assistant.noKey").to_string()))
                .child(
                    Button::new("pane-open-settings")
                        .xsmall()
                        .primary()
                        .label(t!("assistant.openSettings").to_string())
                        .on_click(move |_, window, cx| {
                            crate::dialogs::settings::open(&ws, crate::dialogs::settings::Tab::Assistant, window, cx)
                        }),
                )
                .into_any_element(),
        )
    }

    /// 实时区域：终端只露出当前提示符到光标那几行（全屏程序开着时整块露出来）。终端本身按
    /// `term_height` 排版（PTY 的格子数跟着它），外面一层裁掉不要露的行
    fn live_region(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = Ui::global(cx).clone();
        let view = match &self.term {
            Term::Ready(v) => v.clone(),
            Term::Opening => return Some(blocks::hint(t!("assistant.opening").to_string(), cx).into_any_element()),
            Term::Failed(msg) => {
                let text = format!("{}：{msg}", t!("assistant.shellFailed"));
                return Some(self.reopen_row(text, ui.destructive, cx));
            }
            Term::Ended => {
                return Some(self.reopen_row(t!("assistant.shellEnded").to_string(), ui.muted_foreground, cx));
            }
        };
        let v = view.read(cx);
        let running = v.live_running();
        let slice = v.live_slice();
        // Agent 模式下 shell 闲着（只有一个提示符）就不露：输入在下面的输入框里
        if self.mode == Mode::Agent && !running && slice.is_some() {
            return None;
        }
        let look = TerminalLook::global(cx);
        let lh = v.line_height().unwrap_or(look.text.font_size * look.text.line_height_factor);
        let full = self.term_height;
        // 网格画在元素里留白之下（element.rs 的 PADDING_Y）：要露出的第一行在 留白 + 行号 × 行高 处
        let pad = gpui_kit::px(crate::terminal::element::PADDING_Y);
        let (top, height) = match slice {
            Some((first, rows)) => (-(pad + lh * first as f32), (lh * rows as f32).min(full)),
            None => (zpx(0.), full),
        };
        Some(
            div()
                .relative()
                .w_full()
                .h(height)
                .overflow_hidden()
                .child(div().absolute().left_0().right_0().top(top).h(full).child(view))
                .into_any_element(),
        )
    }

    /// 终端没打开 / 结束了：一行说明 + 「新终端」
    fn reopen_row(&self, text: String, color: gpui_kit::Hsla, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().text_xs().text_color(color).whitespace_normal().child(text))
            .child(
                Button::new("pane-reopen")
                    .xsmall()
                    .icon(IconName::SquareTerminal)
                    .label(t!("assistant.reopenShell").to_string())
                    .on_click(move |_, window, cx| this.update(cx, |v, cx| v.open_shell(window, cx))),
            )
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let this = cx.entity();
        let chord = if crate::ui::mac_keys() { "⌘⇧I" } else { "Ctrl+Shift+I" };
        // Shell / Agent 小分段
        let mut seg = crate::ui::sunken(cx).flex().flex_none().gap(zpx(2.)).p(zpx(2.));
        for (mode, label, glyph) in [(Mode::Shell, "Shell", IconName::SquareTerminal), (Mode::Agent, "Agent", IconName::Sparkles)] {
            let on = self.mode == mode;
            let this = this.clone();
            let tip: SharedString = format!("{} · {chord}", t!("assistant.switchMode")).into();
            let mut b = div()
                .id(SharedString::from(format!("mode-{label}")))
                .flex()
                .items_center()
                .gap_1()
                .h(zpx(22.))
                .px(zpx(8.))
                .rounded(radius::SM)
                .text_xs()
                .cursor_pointer()
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
                .on_click(move |_, window, cx| this.update(cx, |v, cx| v.set_mode(mode, window, cx)));
            b = if on {
                b.bg(ui.background).text_color(ui.foreground).font_weight(FontWeight::MEDIUM)
            } else {
                let fg = ui.foreground;
                b.text_color(ui.muted_foreground).hover(move |s| s.text_color(fg))
            };
            let glyph = icon(glyph).size(zpx(12.));
            let glyph = if mode == Mode::Agent { glyph.text_color(ui.tint_strong) } else { glyph };
            seg = seg.child(b.child(glyph).child(label));
        }

        let location = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_xs()
            .text_color(ui.muted_foreground)
            .child(self.location(cx));

        let mut row = div().flex_none().flex().items_center().gap_2().px_3().py_2().child(seg).child(location);
        if self.mode == Mode::Shell {
            return row.child(
                div().flex_none().text_xs().text_color(ui.muted_foreground).child(t!("assistant.toAgentHint", chord = chord).to_string()),
            );
        }
        // Agent：怎么发、怎么回 Shell 写在输入行的占位字里，这里放「交给哪个 agent」
        {
            // 这段对话交给哪个 agent（在跑时不能换）
            let choices: Vec<MenuItemSpec> = self
                .agent_choices()
                .into_iter()
                .map(|a| {
                    let this = this.clone();
                    let checked = a == self.summary.agent;
                    MenuItemSpec::new(self.agent_name(&a), move |_, cx| {
                        let a = a.clone();
                        this.update(cx, |v, cx| v.set_agent(a, cx))
                    })
                    .checked(checked)
                })
                .collect();
            row = row.child(
                Button::new("pane-agent")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Bot)
                    .label(self.agent_name(&self.summary.agent))
                    .tooltip(t!("assistant.pickAgent").to_string())
                    .disabled(self.busy())
                    .dropdown_menu(move |menu, _, _| to_popup(menu, choices.clone())),
            );
        }
        row
    }
}

impl Focusable for ThreadPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ThreadPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let home = self.home(cx);
        let mut list = div().w_full().flex().flex_col().gap_3().px_3().py_3().children(self.timeline(home.as_deref(), cx));
        // 最底下：实时终端（Shell 输入、或者命令在跑）、在跑时的那一行、Agent 输入那一行
        if let Some(live) = self.live_region(cx) {
            list = list.child(live);
        }
        if let Some(row) = self.status_row(cx) {
            list = list.child(row);
        }
        if let Some(banner) = self.no_key_banner(cx) {
            list = list.child(banner);
        }
        if self.mode == Mode::Agent {
            list = list.child(self.agent_line(home.as_deref(), cx));
        }
        let this = cx.entity();
        div()
            .key_context("AssistantAgent")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &crate::actions::assistant::AllowOnce, _, cx| {
                this.answer_by_kind(PermissionKind::AllowOnce, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::actions::assistant::AllowAlways, _, cx| {
                this.answer_by_kind(PermissionKind::AllowAlways, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::actions::assistant::Reject, _, cx| {
                this.answer_by_kind(PermissionKind::RejectOnce, cx)
            }))
            .child(
                div()
                    .id("pane-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    // Shell 模式下点时间线的空白处：键盘回到终端（Agent 模式不抢：时间线里的字要能选）
                    .on_click(move |_, window, cx| {
                        this.update(cx, |v, cx| {
                            if v.mode == Mode::Shell && v.editing.is_none() {
                                v.focus_input(window, cx);
                            }
                        })
                    })
                    .child(list),
            )
            .child(self.footer(cx))
    }
}
