//! 画布上的 Claude 对话窗口（pane key `c:<项目 id>`）。原先是一扇独立的系统窗口，现在与终端、
//! 文件窗口一样排在线程的画布里，一个线程最多一扇。
//!
//! 对话跑在项目所属节点的 Claude Code 上（`/ws/projects/:id/claude`，服务端 api/claude.rs）：线程的
//! 历史存在宿主机，连上先回放再 Ready；窗口关掉（视图释放）socket 跟着断，宿主机回收子进程，
//! 下次打开照样接回。视图挂在工作区的 `chats` 上，切到别的线程不卸载，和终端一样。
//!
//! 新建线程时选了「Claude 对话」：第一条消息在连上之后自动发出（只在对话还空着时，免得接回
//! 一段已有历史时把它当成新消息再发一遍）。
//!
//! 画法：事件流先交给 falcon-core 的 `claude_transcript` 整理成一轮一轮，这里每轮画成
//! 「右侧的提示词气泡 → 过程（文字与工具步骤卡片）→ 最终回复」。跑着的一轮过程全展开、带计时；
//! 结束了的一轮把过程收进「工作了 8m 0s ›」，点开再看（没有最终回复的一轮默认展开，不然什么都看不到）。
//!
//! 输入框照 Claude 桌面版的式样：左下「＋」添加附件（也可以拖进来、直接粘贴截图）、盾牌选权限
//! 模式；右下是模型与推理强度、上下文用量圆环、发送 / 停止。底下一行是这条线程在哪：机器、
//! 环境（主工作树 / 工作树）、分支。模型列表、effort 档位、当前权限模式都由宿主机的 Claude Code
//! 报上来（`options` 事件），线程上选过的存在宿主机，接回时恢复；不做语音输入。

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use falcon_client::{ClaudeSocket, FalconClient};
use falcon_core::claude_transcript::{
    AttachmentReject, Entry, Step, ToolStatus, ToolStep, Transcript, Turn, attachment_media_type, context_percent,
    current_model, effort_levels, format_elapsed, format_tokens, model_label, model_name,
};
use falcon_core::project_tree::thread_environment;
use falcon_proto::ProjectType;
use falcon_proto::claude::{ClaudeAttachment, ClaudeEvent, ClaudeInput, ClaudeOptions};
use futures::StreamExt as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Disableable, Sizable,
    button::{Button, ButtonVariants},
    input::{InputEvent, Paste, Textarea, TextareaState},
    menu::DropdownMenu,
    progress::ProgressCircle,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, ClipboardEntry, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable,
    FontWeight, PathPromptOptions, Render, ScrollHandle, SharedString, Subscription, Task, WeakEntity, Window, div,
};
use rust_i18n::t;
use web_time::Instant;

use crate::{
    menus::{MenuItemSpec, to_popup},
    theme::{Ui, radius},
    ui::{disclosure_chevron, icon, spinner},
    workspace::Workspace,
    zoom::zpx,
};

/// 输入框上可选的权限模式，顺序即菜单顺序（宿主机 `api/claude.rs` 的 PERMISSION_MODES 同一组）
const PERMISSION_MODES: [&str; 5] = ["default", "acceptEdits", "plan", "auto", "bypassPermissions"];

fn mode_icon(mode: &str) -> IconName {
    match mode {
        "acceptEdits" => IconName::PencilLine,
        "plan" => IconName::Map,
        "auto" => IconName::Sparkles,
        "bypassPermissions" => IconName::ShieldOff,
        _ => IconName::Shield,
    }
}

/// 对话所在的位置：输入框底下那一行（机器 · 环境 · 分支）。跟着工作区刷新（主机改名、分支切换）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChatLocation {
    machine: Option<(IconName, String)>,
    folder: Option<String>,
    /// 完整工作目录，悬停时看
    dir: Option<String>,
    /// 「主工作树」/「工作树」：知道是 git 仓库时才写
    worktree: Option<String>,
    branch: Option<String>,
}

/// 机器的口径与新建线程的机器选择一致：本地是本机，SSH 是绑定的主机（存量项目退回 ssh.host）
pub fn chat_location(w: &Workspace, id: &str) -> ChatLocation {
    let Some(p) = w.project(id) else { return ChatLocation::default() };
    let machine = match p.project_type {
        ProjectType::Local => (IconName::Monitor, crate::labels::local_word()),
        ProjectType::Ssh => {
            let host = p.host_id.as_deref().and_then(|hid| w.hosts.iter().find(|h| h.id == hid));
            let name = host.map(|h| h.name.clone()).or_else(|| p.ssh.as_ref().map(|s| s.host.clone()));
            (IconName::Server, name.unwrap_or_default())
        }
    };
    let env = thread_environment(p).and_then(|env| w.project(env));
    let branch = p
        .worktree
        .as_ref()
        .map(|wt| wt.branch.clone())
        .filter(|b| !b.is_empty())
        .or_else(|| w.heads.get(&p.id).and_then(|h| h.branch.clone()));
    let worktree = if p.worktree.is_some() {
        Some(t!("claudeChat.worktree").to_string())
    } else if branch.is_some() && p.multi.is_none() {
        Some(t!("claudeChat.mainWorktree").to_string())
    } else {
        None
    };
    ChatLocation {
        machine: Some(machine),
        folder: Some(env.map(|e| e.name.clone()).unwrap_or_else(|| p.name.clone())),
        dir: p.working_dir.clone(),
        worktree,
        branch,
    }
}

/// 待发送的附件：读进内存，发送时才分段传给宿主机
struct Attachment {
    id: String,
    name: String,
    media_type: &'static str,
    bytes: Vec<u8>,
}

/// 对话正文一栏的最大宽度：窗口拉得很宽时不让一行字横穿整个画布
const COLUMN_MAX_W: f32 = 760.;

/// 对话窗口状态变化（处理中 / 待授权 / 断开），供工作区通知侧栏刷新线程状态过滤与排序。
pub enum ChatViewEvent {
    StateChanged,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

pub struct ChatView {
    socket: ClaudeSocket,
    input: Entity<TextareaState>,
    transcript: Transcript,
    permissions: Vec<(String, String, String)>,
    ready: bool,
    durable: bool,
    busy: bool,
    closed: bool,
    /// 这一轮是本窗口亲眼看着开始的：从这时起计时。回放出来的、没收尾的旧一轮不计
    turn_started: Option<Instant>,
    /// 点过折叠开关的轮次（下标）：与默认展开状态相反
    toggled_turns: HashSet<usize>,
    /// 展开了输出的工具步骤（轮次下标，步骤下标）
    expanded_tools: HashSet<(usize, usize)>,
    scroll: ScrollHandle,
    attachments: Vec<Attachment>,
    next_attachment: u64,
    /// 模型 / effort / 权限模式：Ready 之后宿主机推来，没推来（旧节点）就不画这几个按钮
    options: Option<ClaudeOptions>,
    /// 上下文用量（已用，上限）
    context: Option<(u64, u64)>,
    /// 设置没改成、附件读不了之类：输入框上方一行，点掉或下次发送时清掉
    warning: Option<String>,
    location: ChatLocation,
    project_id: String,
    _events: Task<()>,
    _tick: Task<()>,
    _enter: Subscription,
    _location: Subscription,
}

impl ChatView {
    /// `initial`：新建线程时写的第一条消息，连上之后自动发出。`location` 由工作区先算好：
    /// 构造时工作区正在 update，这里读不了它，之后的变化经 observe 跟上
    pub fn new(
        client: FalconClient,
        ws: &Entity<Workspace>,
        project: &str,
        location: ChatLocation,
        initial: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let observe = cx.observe(ws, |this: &mut Self, ws, cx| {
            let location = chat_location(ws.read(cx), &this.project_id);
            if location != this.location {
                this.location = location;
                cx.notify();
            }
        });
        let (socket, mut events) = ClaudeSocket::open(&client, project);
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(t!("claudeChat.placeholder").to_string())
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        let enter = cx.subscribe_in(&input, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                this.submit(window, cx);
            }
        });
        let mut initial = initial.filter(|text| !text.trim().is_empty());
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                let ready = matches!(event, ClaudeEvent::Ready);
                if this
                    .update_in(cx, |this, window, cx| {
                        this.event(event, window, cx);
                        if ready && let Some(text) = initial.take() {
                            this.input.update(cx, |i, cx| i.set_value(text, window, cx));
                            // 已有历史（接回了一段旧对话）只填进输入框，不替用户发
                            if this.transcript.is_empty() {
                                this.submit(window, cx);
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        // 跑着的一轮每秒重画一次计时。只有计时这一处要它，没在跑就不重画
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(()) = this.update(cx, |this, cx| {
                    if this.turn_started.is_some() {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });
        Self {
            socket,
            input,
            transcript: Transcript::default(),
            permissions: Vec::new(),
            ready: false,
            durable: false,
            busy: false,
            closed: false,
            turn_started: None,
            toggled_turns: HashSet::new(),
            expanded_tools: HashSet::new(),
            scroll: ScrollHandle::new(),
            attachments: Vec::new(),
            next_attachment: 0,
            options: None,
            context: None,
            warning: None,
            location,
            project_id: project.to_owned(),
            _events: task,
            _tick: tick,
            _enter: enter,
            _location: observe,
        }
    }

    /// 对话是否正在连接或处理回复中
    pub fn is_busy(&self) -> bool {
        !self.closed && (!self.ready || self.busy)
    }

    /// 是否有待用户审批的工具权限请求
    pub fn has_pending_permission(&self) -> bool {
        !self.permissions.is_empty()
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if !self.ready || self.busy || (text.trim().is_empty() && self.attachments.is_empty()) {
            return;
        }
        // 附件先分段传过去，提示词按 id 引用。哪一段排不进队列就整条不发，附件留着
        let attachments = std::mem::take(&mut self.attachments);
        if !attachments.iter().all(|a| self.socket.upload(&a.id, &a.bytes)) {
            self.attachments = attachments;
            self.warning = Some(t!("claudeChat.attachFailed").to_string());
            cx.notify();
            return;
        }
        let refs = attachments
            .iter()
            .map(|a| ClaudeAttachment { id: a.id.clone(), name: a.name.clone(), media_type: a.media_type.to_owned() })
            .collect();
        if self.socket.send(ClaudeInput::Prompt { text: text.clone(), attachments: refs }) {
            // 线程的消息以宿主机的回显为准（见 event 的 User），这里不先记
            if !self.durable {
                let attachments = attachments.iter().map(|a| a.name.clone()).collect();
                self.transcript.apply(&ClaudeEvent::User { text, attachments });
                self.input.update(cx, |i, cx| i.set_value("", window, cx));
            }
            self.warning = None;
            self.busy = true;
            self.turn_started = Some(Instant::now());
            self.scroll.scroll_to_bottom();
            cx.emit(ChatViewEvent::StateChanged);
            cx.notify();
        } else {
            self.attachments = attachments;
        }
    }

    /// 「＋」：系统的文件选择框，可多选
    fn pick_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 自动化验证时系统的文件框没法点，与文件面板的上传共用同一个替身
        if let Some(paths) = crate::panels::files::transfer::test_paths("FALCON_TEST_PICK") {
            self.attach_paths(paths, cx);
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            this.update(cx, |this, cx| this.attach_paths(paths, cx)).ok();
        })
        .detach();
    }

    /// 本机文件读进内存（后台线程读，大 PDF 不卡界面）
    fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let read = cx.background_spawn(async move {
            paths
                .into_iter()
                .map(|path| {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    // 文件夹读不了，和读不了的文件一样提示
                    let bytes = std::fs::read(&path).ok();
                    (name, bytes)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let files = read.await;
            this.update(cx, |this, cx| {
                for (name, bytes) in files {
                    match bytes {
                        Some(bytes) => this.attach_bytes(name, bytes),
                        None => this.warning = Some(t!("claudeChat.attachReadFailed", name = name).to_string()),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn attach_bytes(&mut self, name: String, bytes: Vec<u8>) {
        match attachment_media_type(&name, &bytes) {
            Ok(media_type) => {
                self.next_attachment += 1;
                let id = self.next_attachment.to_string();
                self.attachments.push(Attachment { id, name, media_type, bytes });
            }
            Err(AttachmentReject::TooLarge(max)) => {
                self.warning = Some(t!("claudeChat.attachTooLarge", name = name, max = max / 1024 / 1024).to_string());
            }
            Err(AttachmentReject::Unsupported) => {
                self.warning = Some(t!("claudeChat.attachUnsupported", name = name).to_string());
            }
        }
    }

    /// 粘贴：复制的文件、纯图片（截图）变成附件；带文本的照常交给输入框（Excel、网页复制常
    /// 同时带位图，贴文本才对，与终端的口径一致）。返回 true = 这次粘贴被附件吃掉了
    fn paste_attachments(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_clipboard() else { return false };
        let paths: Vec<PathBuf> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !paths.is_empty() {
            self.attach_paths(paths, cx);
            return true;
        }
        if item.entries().iter().any(|e| matches!(e, ClipboardEntry::String(_))) {
            return false;
        }
        let Some(image) = item.entries().iter().find_map(|e| match e {
            ClipboardEntry::Image(img) => Some(img.clone()),
            _ => None,
        }) else {
            return false;
        };
        let ext = image.format.mime_type().rsplit('/').next().unwrap_or("png").to_owned();
        self.attach_bytes(format!("image-{}.{ext}", self.next_attachment + 1), image.bytes.clone());
        cx.notify();
        true
    }

    fn remove_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        self.attachments.retain(|a| a.id != id);
        cx.notify();
    }

    /// 改设置：等宿主机的 `options` 回来才变，按钮上不先改
    fn set_option(&mut self, input: ClaudeInput, cx: &mut Context<Self>) {
        if self.ready && self.socket.send(input) {
            self.warning = None;
            cx.notify();
        }
    }

    /// 自动化脚本的发送口：填进输入框再照常提交（对话还没 Ready 时同样不发）
    #[cfg(feature = "automation")]
    pub fn automation_send(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_value(text.to_string(), window, cx));
        self.submit(window, cx);
    }

    /// 自动化脚本：把每一轮的过程与每个工具的输出都展开（截图验卡片用，等同于挨个点开）
    #[cfg(feature = "automation")]
    pub fn automation_expand_all(&mut self, cx: &mut Context<Self>) {
        for (index, entry) in self.transcript.entries.iter().enumerate() {
            if let Entry::Turn(turn) = entry {
                if !self.turn_expanded(index, turn) {
                    self.toggled_turns.insert(index);
                }
                for step in 0..turn.steps.len() {
                    self.expanded_tools.insert((index, step));
                }
            }
        }
        cx.notify();
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if self.busy && self.socket.send(ClaudeInput::Interrupt) {
            cx.notify();
        }
    }

    fn event(&mut self, event: ClaudeEvent, window: &mut Window, cx: &mut Context<Self>) {
        let prev = (self.is_busy(), self.has_pending_permission());
        // 贴着底部才跟着新内容滚；用户往上翻着看时不打扰
        let at_bottom = self.at_bottom();
        match &event {
            ClaudeEvent::ThreadHistory => self.durable = true,
            ClaudeEvent::User { text, .. } => {
                // 用户可能已在写下一条草稿，回显只清除刚刚发送的内容。
                if self.ready && self.input.read(cx).value().as_ref() == text.as_str() {
                    self.input.update(cx, |i, cx| i.set_value("", window, cx));
                }
                if self.ready && self.turn_started.is_none() {
                    self.turn_started = Some(Instant::now());
                }
            }
            ClaudeEvent::Ready => self.ready = true,
            ClaudeEvent::Permission { id, tool, input } => {
                self.permissions.push((id.clone(), tool.clone(), serde_json::to_string_pretty(input).unwrap_or_default()));
            }
            ClaudeEvent::CancelPermission { id } => self.permissions.retain(|p| p.0 != *id),
            ClaudeEvent::Done { .. } => {
                self.busy = false;
                self.turn_started = None;
                self.permissions.clear();
            }
            ClaudeEvent::Closed { .. } => {
                self.ready = false;
                self.closed = true;
                self.busy = false;
                self.turn_started = None;
                self.permissions.clear();
            }
            ClaudeEvent::Options(options) => self.options = Some(options.clone()),
            ClaudeEvent::Context { used, max } => self.context = Some((*used, *max)),
            ClaudeEvent::Warning { message } => self.warning = Some(message.clone()),
            ClaudeEvent::Delta { .. } | ClaudeEvent::Assistant { .. } | ClaudeEvent::ToolResult { .. } => {}
        }
        self.transcript.apply(&event);
        if prev != (self.is_busy(), self.has_pending_permission()) {
            cx.emit(ChatViewEvent::StateChanged);
        }
        if at_bottom {
            self.scroll.scroll_to_bottom();
        }
    }

    fn at_bottom(&self) -> bool {
        self.scroll.max_offset().y + self.scroll.offset().y <= zpx(3.)
    }

    fn turn_expanded(&self, index: usize, turn: &Turn) -> bool {
        let default = !turn.done || turn.reply_index().is_none();
        default != self.toggled_turns.contains(&index)
    }

    fn toggle_turn(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.toggled_turns.remove(&index) {
            self.toggled_turns.insert(index);
        }
        cx.notify();
    }

    fn toggle_tool(&mut self, key: (usize, usize), cx: &mut Context<Self>) {
        if !self.expanded_tools.remove(&key) {
            self.expanded_tools.insert(key);
        }
        cx.notify();
    }

    fn render_turn(&self, index: usize, turn: &Turn, live: bool, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let mut col = div().flex().flex_col().gap_3().pb(zpx(20.));
        if !turn.attachments.is_empty() {
            col = col.child(
                div()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .pl(zpx(48.))
                    .children(turn.attachments.iter().map(|name| attachment_chip(name, None, &ui))),
            );
        }
        if let Some(prompt) = turn.prompt.as_ref().filter(|p| !p.is_empty()) {
            col = col.child(
                div().flex().justify_end().pl(zpx(48.)).child(
                    div()
                        .max_w_full()
                        .px(zpx(14.))
                        .py(zpx(9.))
                        .rounded(radius::LG)
                        .bg(ui.muted)
                        .text_color(ui.foreground)
                        .child(prompt.clone()),
                ),
            );
        }
        let process = turn.process();
        let expanded = self.turn_expanded(index, turn);
        if !turn.done {
            // 跑着的一轮：头上是计时，过程全展开
            let elapsed = live.then(|| self.turn_started.map(|t| t.elapsed().as_millis() as u64)).flatten();
            let label = match elapsed {
                Some(ms) => t!("claudeChat.workingFor", time = format_elapsed(ms)).to_string(),
                None => t!("claudeChat.working").to_string(),
            };
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(zpx(13.))
                    .text_color(ui.muted_foreground)
                    .child(div().text_color(ui.warning).child(spinner(
                        ("claude-turn-spin", index),
                        IconName::LoaderCircle,
                        zpx(13.),
                    )))
                    .child(label),
            );
        } else if !process.is_empty() {
            let steps = turn.tool_count();
            let label = match turn.duration_ms {
                Some(ms) => t!("claudeChat.workedFor", time = format_elapsed(ms)).to_string(),
                None => t!("claudeChat.steps", count = steps.max(process.len())).to_string(),
            };
            col = col.child(
                div()
                    .id(("claude-turn-head", index))
                    .flex()
                    .items_center()
                    .gap_1()
                    .pb(zpx(6.))
                    .border_b_1()
                    .border_color(ui.border)
                    .text_size(zpx(13.))
                    .text_color(ui.muted_foreground)
                    .cursor_pointer()
                    .hover(|s| s.text_color(ui.foreground))
                    .child(label)
                    .child(disclosure_chevron(("claude-turn-chevron", index), expanded, zpx(12.)))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_turn(index, cx))),
            );
        }
        if expanded || !turn.done {
            for (step_index, step) in process.iter().enumerate() {
                col = col.child(match step {
                    Step::Text(text) => reply(format!("claude-step-{index}-{step_index}"), text.clone()).into_any_element(),
                    Step::Tool(tool) => self.render_tool((index, step_index), tool, cx),
                });
            }
        }
        if !turn.draft.is_empty() {
            col = col.child(reply(format!("claude-draft-{index}"), turn.draft.clone()));
        }
        if let Some(i) = turn.reply_index()
            && let Step::Text(text) = &turn.steps[i]
        {
            col = col.child(reply(format!("claude-reply-{index}"), text.clone()));
        }
        if let Some(error) = &turn.error {
            col = col.child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .px(zpx(12.))
                    .py(zpx(8.))
                    .rounded(radius::MD)
                    .bg(ui.destructive.opacity(0.08))
                    .text_size(zpx(13.))
                    .text_color(ui.destructive)
                    .child(icon(IconName::CircleAlert).size(zpx(14.)).mt(zpx(2.)).flex_none())
                    .child(div().flex_1().min_w_0().child(format!("{}：{error}", t!("claudeChat.error")))),
            );
        }
        col.into_any_element()
    }

    /// 一次工具调用：工具图标、状态、标题，右边「完成 · 42s」；下面是补充行（命令）与输出开头，
    /// 点一下展开整段输出
    fn render_tool(&self, key: (usize, usize), tool: &ToolStep, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let expanded = self.expanded_tools.contains(&key);
        let status_icon = match tool.status {
            ToolStatus::Running => div()
                .text_color(ui.warning)
                .child(spinner(("claude-tool-spin", key.0 * 10_000 + key.1), IconName::LoaderCircle, zpx(13.)))
                .into_any_element(),
            ToolStatus::Ok => icon(IconName::CircleCheck).size(zpx(13.)).text_color(ui.success).into_any_element(),
            ToolStatus::Failed => icon(IconName::CircleX).size(zpx(13.)).text_color(ui.destructive).into_any_element(),
            ToolStatus::Interrupted => {
                icon(IconName::CircleAlert).size(zpx(13.)).text_color(ui.muted_foreground).into_any_element()
            }
        };
        let status_text = match tool.status {
            ToolStatus::Running => t!("claudeChat.toolRunning").to_string(),
            ToolStatus::Ok => t!("claudeChat.toolDone").to_string(),
            ToolStatus::Failed => t!("claudeChat.toolFailed").to_string(),
            ToolStatus::Interrupted => t!("claudeChat.toolInterrupted").to_string(),
        };
        let status_text = match tool.elapsed_ms {
            Some(ms) if tool.status != ToolStatus::Running => format!("{status_text} · {}", format_elapsed(ms)),
            _ => status_text,
        };
        let title = if tool.call.title.is_empty() { tool.call.name.clone() } else { tool.call.title.clone() };
        let mut head = div()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(icon(tool_icon(&tool.call.name)).size(zpx(14.)).text_color(ui.muted_foreground).flex_none())
            .child(div().flex_none().child(status_icon))
            .child(div().flex_1().min_w_0().truncate().text_color(ui.foreground).child(title));
        if !tool.call.title.is_empty() {
            // 有标题时工具名退到一个小标签里，扫一眼知道是哪类操作
            head = head.child(
                div()
                    .flex_none()
                    .px(zpx(6.))
                    .rounded(zpx(4.))
                    .bg(ui.muted)
                    .text_size(zpx(11.))
                    .text_color(ui.muted_foreground)
                    .child(tool.call.name.clone()),
            );
        }
        head = head.child(
            div()
                .flex_none()
                .text_size(zpx(12.))
                .text_color(if tool.status == ToolStatus::Failed { ui.destructive } else { ui.muted_foreground })
                .child(status_text),
        );
        let mut card = div()
            .id(("claude-tool", key.0 * 10_000 + key.1))
            .flex()
            .flex_col()
            .gap_1()
            .px(zpx(10.))
            .py(zpx(8.))
            .rounded(radius::MD)
            .text_size(zpx(13.))
            .hover(|s| s.bg(ui.muted.opacity(0.5)))
            .child(head);
        // 补充行与输出缩进到标题下面（两个图标的宽度），和参考里一样对齐正文
        let body = || div().pl(zpx(44.)).min_w_0().font_family(crate::fonts::BERKELEY).text_size(zpx(12.)).text_color(ui.muted_foreground);
        if let Some(detail) = &tool.call.detail {
            card = card.child(body().truncate().child(format!("$ {detail}")));
        }
        if !tool.output.is_empty() {
            let output = if expanded {
                tool.output.clone()
            } else {
                // 收着只露两行，第二行之后的折掉
                let mut lines = tool.output.lines();
                let head = lines.by_ref().take(2).collect::<Vec<_>>().join("\n");
                if lines.next().is_some() { format!("{head} …") } else { head }
            };
            let mut out = body();
            for line in output.lines() {
                out = out.child(div().when(!expanded, |d| d.truncate()).child(line.to_string()));
            }
            card = card
                .cursor_pointer()
                .child(out)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_tool(key, cx)));
        }
        card.into_any_element()
    }

    fn render_permission(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (id, tool, args) = self.permissions.first()?;
        let ui = Ui::global(cx).clone();
        let mut actions = div().flex().justify_end().gap_2();
        for (allow, label) in [(false, t!("claudeChat.deny")), (true, t!("claudeChat.allow"))] {
            let id = id.clone();
            let mut button = Button::new(if allow { "allow" } else { "deny" }).label(label.to_string());
            if allow {
                button = button.primary();
            }
            actions = actions.child(button.on_click(cx.listener(move |this, _, _, cx| {
                if this.socket.send(ClaudeInput::Permission { id: id.clone(), allow }) {
                    this.permissions.retain(|p| p.0 != id);
                    cx.emit(ChatViewEvent::StateChanged);
                    cx.notify();
                }
            })));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p(zpx(12.))
                .rounded(radius::LG)
                .bg(ui.warning.opacity(0.08))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .font_weight(FontWeight::MEDIUM)
                        .child(icon(IconName::ShieldCheck).size(zpx(15.)).text_color(ui.warning))
                        .child(t!("claudeChat.permission", tool = tool.clone()).to_string()),
                )
                .child(
                    crate::ui::sunken(cx)
                        .id("permission-input")
                        .overflow_y_scroll()
                        .max_h(zpx(160.))
                        .px(zpx(10.))
                        .py(zpx(8.))
                        .font_family(crate::fonts::BERKELEY)
                        .text_size(zpx(12.))
                        .child(args.clone()),
                )
                .child(actions)
                .into_any_element(),
        )
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let me = cx.entity().downgrade();
        let running = self.ready && self.busy;
        let can_send = self.ready && !self.busy && (!self.input.read(cx).value().trim().is_empty() || !self.attachments.is_empty());
        // 连着、能用的时候底栏全是按钮，不再写回车怎么用（占位文字里写着）；连接中 / 断开写在左边
        let status = if self.closed {
            Some(t!("claudeChat.closed").to_string())
        } else if !self.ready {
            Some(t!("claudeChat.connecting").to_string())
        } else {
            None
        };
        // 发送 / 停止共用一个圆钮：跑着的时候是方块（打断这一轮），否则是向上箭头
        let (name, enabled) = if running { (IconName::Square, true) } else { (IconName::ArrowUp, can_send) };
        let send = div()
            .id("claude-send")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(zpx(30.))
            .rounded_full()
            .bg(if enabled { ui.primary } else { ui.muted })
            .text_color(if enabled { ui.primary_foreground } else { ui.muted_foreground })
            .when(enabled, |b| b.cursor_pointer().hover(|s| s.opacity(0.85)).active(|s| s.opacity(0.7)))
            .child(icon(name).size(if running { zpx(12.) } else { zpx(16.) }))
            .tooltip(move |window, cx| {
                let label = if running { t!("claudeChat.stop") } else { t!("claudeChat.send") };
                gpui_kit::component::tooltip::Tooltip::new(label.to_string()).build(window, cx)
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if running {
                    this.interrupt(cx);
                } else {
                    this.submit(window, cx);
                }
            }));

        let attach = Button::new("claude-attach")
            .ghost()
            .small()
            .icon(IconName::Plus)
            .tooltip(t!("claudeChat.attach").to_string())
            .disabled(self.closed)
            .on_click(cx.listener(|this, _, window, cx| this.pick_attachments(window, cx)));

        let mut left = div().flex().flex_none().items_center().gap_1().child(attach);
        let mut right = div().flex().flex_1().min_w_0().justify_end().items_center().gap_2();
        if let Some(options) = self.options.as_ref().filter(|_| self.ready) {
            left = left.child(self.mode_button(options, &me));
            if let Some(model) = self.model_button(options, &me, &ui) {
                right = right.child(model);
            }
        }
        if let Some(status) = status {
            left = left.child(div().pl(zpx(4.)).truncate().text_size(zpx(12.)).text_color(ui.muted_foreground).child(status));
        }
        if let Some(ring) = self.context_ring(&ui) {
            right = right.child(ring);
        }
        right = right.child(send);

        let mut composer = crate::ui::sunken(cx)
            .id("claude-composer")
            .rounded(radius::LG)
            .flex()
            .flex_col()
            .gap_1()
            .px(zpx(12.))
            .pt(zpx(8.))
            .pb(zpx(8.))
            // 拖进来的文件当附件；粘贴的文件与截图同理（带文本的粘贴照常进输入框），
            // 必须在捕获阶段截：冒泡阶段输入框已经把剪贴板里的文件名贴进去了
            .drag_over::<ExternalPaths>({
                let tint = ui.primary.opacity(0.08);
                move |s, _, _, _| s.bg(tint)
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.attach_paths(paths.paths().to_vec(), cx)))
            .capture_action(cx.listener(|this, _: &Paste, _, cx| {
                if this.paste_attachments(cx) {
                    cx.stop_propagation();
                }
            }));
        if !self.attachments.is_empty() {
            composer = composer.child(div().flex().flex_wrap().gap_2().pt(zpx(4.)).children(self.attachments.iter().map(|a| {
                let id = a.id.clone();
                let remove = div()
                    .id(SharedString::from(format!("claude-attachment-remove-{}", a.id)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(zpx(16.))
                    .rounded_full()
                    .cursor_pointer()
                    .text_color(ui.muted_foreground)
                    .hover(|s| s.bg(ui.muted).text_color(ui.foreground))
                    .child(icon(IconName::X).size(zpx(11.)))
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(t!("claudeChat.removeAttachment").to_string()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(&id, cx)));
                attachment_chip(&a.name, Some(remove.into_any_element()), &ui)
            })));
        }
        composer
            .child(Textarea::new(&self.input).appearance(false))
            .child(div().flex().items_center().gap_2().child(left).child(right))
            .into_any_element()
    }

    /// 盾牌：权限模式。默认模式只露图标（和参考一样），别的模式把名字也写出来，免得忘了自己
    /// 开着计划模式或跳过了权限
    fn mode_button(&self, options: &ClaudeOptions, me: &WeakEntity<Self>) -> AnyElement {
        let current = options.permission_mode.clone();
        let items: Vec<MenuItemSpec> = PERMISSION_MODES
            .iter()
            .map(|mode| {
                let me = me.clone();
                let value = mode.to_string();
                let item = MenuItemSpec::new(t!(format!("claudeChat.mode.{mode}")).to_string(), move |_, cx| {
                    let input = ClaudeInput::SetPermissionMode { mode: value.clone() };
                    me.update(cx, |this, cx| this.set_option(input, cx)).ok();
                })
                .icon(icon(mode_icon(mode)))
                .checked(current == *mode);
                // 跳过权限与其余几项隔开，标成危险色
                if *mode == "bypassPermissions" { item.sep().danger() } else { item }
            })
            .collect();
        let known = PERMISSION_MODES.contains(&current.as_str());
        let tooltip = if known {
            format!(
                "{}：{} — {}",
                t!("claudeChat.permissionMode"),
                t!(format!("claudeChat.mode.{current}")),
                t!(format!("claudeChat.mode.{current}Desc"))
            )
        } else {
            format!("{}：{current}", t!("claudeChat.permissionMode"))
        };
        // 跳过权限整块标成危险色：外层 div 的 text_color 管不到 Button 自己的配色，只能换变体
        let button = Button::new("claude-mode");
        let mut button = if current == "bypassPermissions" { button.danger() } else { button.ghost() }
            .small()
            .icon(icon(mode_icon(&current)))
            .tooltip(tooltip);
        if current != "default" {
            let label = if known { t!(format!("claudeChat.mode.{current}")).to_string() } else { current.clone() };
            button = button.label(label);
        }
        div()
            .flex_none()
            .child(button.dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| to_popup(menu, items.clone())))
            .into_any_element()
    }

    /// 模型与推理强度：一个按钮「模型名 XHigh ⌄」，菜单上半是模型、下半是这个模型支持的档位
    fn model_button(&self, options: &ClaudeOptions, me: &WeakEntity<Self>, ui: &Ui) -> Option<AnyElement> {
        let label = model_label(options)?;
        let current = current_model(options).map(|m| m.value.clone());
        let mut items: Vec<MenuItemSpec> = options
            .models
            .iter()
            .map(|m| {
                let me = me.clone();
                let value = m.value.clone();
                MenuItemSpec::new(model_name(m), move |_, cx| {
                    let input = ClaudeInput::SetModel { model: value.clone() };
                    me.update(cx, |this, cx| this.set_option(input, cx)).ok();
                })
                .checked(current.as_deref() == Some(m.value.as_str()))
            })
            .collect();
        let levels = effort_levels(options);
        for (i, level) in levels.iter().enumerate() {
            let me = me.clone();
            let value = level.clone();
            let item = MenuItemSpec::new(effort_label(level), move |_, cx| {
                let input = ClaudeInput::SetEffort { effort: value.clone() };
                me.update(cx, |this, cx| this.set_option(input, cx)).ok();
            })
            .checked(options.effort.as_deref() == Some(level.as_str()));
            items.push(if i == 0 { item.sep() } else { item });
        }
        if !levels.is_empty() {
            // 档位前面加一行不能点的小标题，和模型分开
            let header = MenuItemSpec::new(t!("claudeChat.effort").to_string(), |_, _| {}).disabled(true).sep();
            let at = items.len() - levels.len();
            items[at].separated = false;
            items.insert(at, header);
        }
        let effort = options.effort.as_deref().filter(|_| !levels.is_empty()).map(effort_label);
        let tooltip = format!("{}：{label}", t!("claudeChat.model"));
        let button = Button::new("claude-model")
            .ghost()
            .small()
            .dropdown_caret(true)
            .tooltip(tooltip)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .min_w_0()
                    .child(div().max_w(zpx(240.)).truncate().font_weight(FontWeight::MEDIUM).child(label))
                    .when_some(effort, |d, effort| d.child(div().flex_none().text_color(ui.muted_foreground).child(effort))),
            );
        Some(
            div()
                .min_w_0()
                .child(button.dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| to_popup(menu, items.clone())))
                .into_any_element(),
        )
    }

    /// 上下文用量圆环：越满越醒目，悬停看具体数字
    fn context_ring(&self, ui: &Ui) -> Option<AnyElement> {
        if !self.ready {
            return None;
        }
        let (percent, tooltip) = match self.context {
            Some((used, max)) => {
                let percent = context_percent(used, max);
                let text = t!(
                    "claudeChat.context",
                    percent = percent.round() as u32,
                    used = format_tokens(used),
                    max = format_tokens(max)
                )
                .to_string();
                (percent, text)
            }
            None => (0., t!("claudeChat.contextUnknown").to_string()),
        };
        let color = if percent >= 90. {
            ui.destructive
        } else if percent >= 75. {
            ui.warning
        } else {
            ui.muted_foreground
        };
        Some(
            div()
                .id("claude-context")
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(zpx(26.))
                .child(ProgressCircle::new("claude-context-ring").value(percent).color(color).size(zpx(18.)))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx))
                .into_any_element(),
        )
    }

    /// 输入框底下一行：在哪台机器、哪个环境（主工作树 / 工作树）、哪个分支。只是看，不能在这里换
    fn render_location(&self, ui: &Ui) -> Option<AnyElement> {
        let loc = &self.location;
        let item = |id: &'static str, glyph: IconName, text: String, tip: String| {
            div()
                .id(id)
                .flex()
                .min_w_0()
                .items_center()
                .gap_1p5()
                .child(icon(glyph).size(zpx(13.)).flex_none())
                .child(div().min_w_0().truncate().child(text))
                .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
        };
        let mut row = div()
            .flex()
            .items_center()
            .gap_4()
            .px(zpx(6.))
            .text_size(zpx(12.))
            .text_color(ui.muted_foreground);
        let mut any = false;
        if let Some((glyph, name)) = loc.machine.clone().filter(|(_, n)| !n.is_empty()) {
            row = row.child(item("claude-loc-machine", glyph, name, t!("claudeChat.machine").to_string()));
            any = true;
        }
        if let Some(folder) = loc.folder.clone() {
            let text = match &loc.worktree {
                Some(kind) => format!("{folder} · {kind}"),
                None => folder.clone(),
            };
            let tip = loc.dir.clone().unwrap_or(folder);
            row = row.child(item("claude-loc-folder", IconName::Folder, text, tip));
            any = true;
        }
        if let Some(branch) = loc.branch.clone() {
            row = row.child(
                item("claude-loc-branch", IconName::GitBranch, branch, t!("claudeChat.branch").to_string())
                    .font_family(crate::fonts::BERKELEY),
            );
            any = true;
        }
        any.then(|| row.into_any_element())
    }

    fn render_warning(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let message = self.warning.clone()?;
        let ui = Ui::global(cx).clone();
        Some(
            div()
                .flex()
                .items_start()
                .gap_2()
                .px(zpx(12.))
                .py(zpx(6.))
                .rounded(radius::MD)
                .bg(ui.warning.opacity(0.08))
                .text_size(zpx(12.))
                .text_color(ui.foreground)
                .child(icon(IconName::CircleAlert).size(zpx(13.)).mt(zpx(2.)).flex_none().text_color(ui.warning))
                .child(div().flex_1().min_w_0().child(message))
                .child(
                    div()
                        .id("claude-warning-close")
                        .flex_none()
                        .cursor_pointer()
                        .text_color(ui.muted_foreground)
                        .hover(|s| s.text_color(ui.foreground))
                        .child(icon(IconName::X).size(zpx(13.)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.warning = None;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

fn effort_label(level: &str) -> String {
    match level {
        "low" | "medium" | "high" | "xhigh" | "max" => t!(format!("claudeChat.effortLevel.{level}")).to_string(),
        other => other.to_owned(),
    }
}

/// 附件的小标签：图标 + 文件名，输入框里的带移除钮，提示词气泡上面的不带
fn attachment_chip(name: &str, remove: Option<AnyElement>, ui: &Ui) -> AnyElement {
    let glyph = if attachment_media_type(name, b"").is_ok_and(|m| m.starts_with("image/")) {
        IconName::Image
    } else {
        IconName::FileText
    };
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .max_w(zpx(240.))
        .pl(zpx(8.))
        .pr(zpx(if remove.is_some() { 4. } else { 8. }))
        .py(zpx(3.))
        .rounded(radius::MD)
        .bg(ui.muted)
        .text_size(zpx(12.))
        .text_color(ui.foreground)
        .child(icon(glyph).size(zpx(13.)).flex_none().text_color(ui.muted_foreground))
        .child(div().min_w_0().truncate().child(name.to_owned()))
        .children(remove)
        .into_any_element()
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

fn tool_icon(name: &str) -> IconName {
    match name {
        "Bash" | "PowerShell" | "BashOutput" | "KillShell" => IconName::SquareTerminal,
        "Read" | "NotebookRead" => IconName::FileText,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => IconName::PencilLine,
        "Grep" | "Glob" => IconName::Search,
        "WebFetch" | "WebSearch" => IconName::Globe,
        "Task" | "Agent" => IconName::Bot,
        "TodoWrite" => IconName::ListTodo,
        _ => IconName::Wrench,
    }
}

fn reply(id: String, text: String) -> impl IntoElement {
    gpui_kit::component::text::TextView::markdown(SharedString::from(id), text)
        .selectable(true)
        // 消息里的图片路径属于宿主机，不能交给桌面客户端当本地路径读取。
        .image_source(|_| "icons/image.svg".into())
        .on_link_click(|href, _, _, cx| {
            if let Some(url) = falcon_core::md_link::external_href(href) {
                cx.open_url(&url);
            }
        })
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let live = self.transcript.open_turn().is_some() && self.turn_started.is_some();
        let mut column = div().w_full().max_w(zpx(COLUMN_MAX_W)).mx_auto().px(zpx(20.)).pt(zpx(16.)).flex().flex_col();
        if self.transcript.is_empty() && self.ready {
            column = column.child(
                div()
                    .pt(zpx(48.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_color(ui.muted_foreground)
                    .child(icon(IconName::Bot).size(zpx(28.)))
                    .child(t!("claudeChat.empty").to_string()),
            );
        }
        let last = self.transcript.entries.len().saturating_sub(1);
        for (index, entry) in self.transcript.entries.iter().enumerate() {
            column = column.child(match entry {
                Entry::Turn(turn) => self.render_turn(index, turn, live && index == last, cx),
                Entry::Notice(reason) => div()
                    .pb(zpx(20.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(zpx(12.))
                    .text_color(ui.muted_foreground)
                    .child(div().flex_1().h(zpx(1.)).bg(ui.border))
                    .child(format!("{}：{reason}", t!("claudeChat.closed")))
                    .child(div().flex_1().h(zpx(1.)).bg(ui.border))
                    .into_any_element(),
            });
        }
        let log = div()
            .id("claude-history")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(column);
        // 往上翻着看时，底部浮一个回到最新的圆钮
        let jump = (!self.at_bottom()).then(|| {
            div().absolute().bottom(zpx(10.)).left_0().right_0().flex().justify_center().child(
                div()
                    .id("claude-jump")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(zpx(30.))
                    .rounded_full()
                    .bg(ui.popover)
                    .text_color(ui.popover_foreground)
                    .shadow_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(ui.muted))
                    .child(icon(IconName::ArrowDown).size(zpx(15.)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.scroll.scroll_to_bottom();
                        cx.notify();
                    })),
            )
        });
        let mut footer = div().w_full().max_w(zpx(COLUMN_MAX_W)).mx_auto().px(zpx(20.)).pb(zpx(14.)).flex().flex_col().gap_2();
        if let Some(permission) = self.render_permission(cx) {
            footer = footer.child(permission);
        }
        if let Some(warning) = self.render_warning(cx) {
            footer = footer.child(warning);
        }
        footer = footer.child(self.render_composer(cx));
        if let Some(location) = self.render_location(&ui) {
            footer = footer.child(location);
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(ui.background)
            .text_color(ui.foreground)
            .child(div().relative().flex_1().min_h_0().child(log).children(jump))
            .child(footer)
    }
}
