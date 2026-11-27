//! 画布上的 Claude 对话窗口（pane key `c:<项目 id>`）。原先是一扇独立的系统窗口，现在与终端、
//! 文件窗口一样排在线程的画布里，一个线程最多一扇。
//!
//! 对话跑在项目所属节点的 Claude Code 上（`/ws/projects/:id/claude`，服务端 api/claude.rs）：线程的
//! 历史存在宿主机，连上先回放再 Ready；窗口关掉（视图释放）socket 跟着断，宿主机回收子进程，
//! 下次打开照样接回。视图挂在工作区的 `chats` 上，切到别的线程不卸载，和终端一样。
//!
//! 新建线程时选了「Claude 对话」：第一条消息在连上之后自动发出（只在对话还空着时，免得接回
//! 一段已有历史时把它当成新消息再发一遍）。

use falcon_client::{ClaudeSocket, FalconClient};
use falcon_proto::claude::{ClaudeEvent, ClaudeInput};
use futures::StreamExt as _;
use gpui_kit::component::{
    Disableable,
    button::{Button, ButtonVariants},
    input::{InputEvent, Textarea, TextareaState},
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, ScrollHandle, SharedString, Subscription, Task,
    Window, div,
};
use rust_i18n::t;

use crate::{theme::Ui, zoom::zpx};

/// 对话窗口状态变化（处理中 / 待授权 / 断开），供工作区通知侧栏刷新线程状态过滤与排序。
pub enum ChatViewEvent {
    StateChanged,
}

impl EventEmitter<ChatViewEvent> for ChatView {}

pub struct ChatView {
    socket: ClaudeSocket,
    input: Entity<TextareaState>,
    history: Vec<(Option<String>, SharedString)>,
    draft: String,
    permissions: Vec<(String, String, String)>,
    ready: bool,
    durable: bool,
    busy: bool,
    closed: bool,
    scroll: ScrollHandle,
    _events: Task<()>,
    _enter: Subscription,
}

impl ChatView {
    /// `initial`：新建线程时写的第一条消息，连上之后自动发出
    pub fn new(
        client: FalconClient,
        project: &str,
        initial: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (socket, mut events) = ClaudeSocket::open(&client, project);
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
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
                            if this.history.is_empty() {
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
        Self {
            socket,
            input,
            history: Vec::new(),
            permissions: Vec::new(),
            draft: String::new(),
            ready: false,
            durable: false,
            busy: false,
            closed: false,
            scroll: ScrollHandle::new(),
            _events: task,
            _enter: enter,
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
        if !self.ready || self.busy || text.trim().is_empty() {
            return;
        }
        if self.socket.send(ClaudeInput::Prompt { text: text.clone() }) {
            // 线程的消息以宿主机的回显为准（见 event 的 User），这里不先记
            if !self.durable {
                self.push_user(text);
                self.input.update(cx, |i, cx| i.set_value("", window, cx));
            }
            self.busy = true;
            self.scroll.scroll_to_bottom();
            cx.emit(ChatViewEvent::StateChanged);
            cx.notify();
        }
    }

    fn push_user(&mut self, text: String) {
        self.history.push((Some(t!("claudeChat.you").to_string()), text.into()));
    }

    fn event(&mut self, event: ClaudeEvent, window: &mut Window, cx: &mut Context<Self>) {
        let prev = (self.is_busy(), self.has_pending_permission());
        match event {
            ClaudeEvent::ThreadHistory => self.durable = true,
            ClaudeEvent::User { text } => {
                // 用户可能已在写下一条草稿，回显只清除刚刚发送的内容。
                if self.ready && self.input.read(cx).value().as_ref() == text {
                    self.input.update(cx, |i, cx| i.set_value("", window, cx));
                }
                self.push_user(text);
            }
            ClaudeEvent::Ready => self.ready = true,
            ClaudeEvent::Delta { text } => self.draft.push_str(&text),
            ClaudeEvent::Assistant { text, tools } => {
                self.draft.clear();
                if !text.is_empty() {
                    self.history.push((None, text.into()));
                }
                if !tools.is_empty() {
                    self.history.push((Some(t!("claudeChat.tools").to_string()), tools.join(" · ").into()));
                }
            }
            ClaudeEvent::Permission { id, tool, input } => {
                self.permissions.push((id, tool, serde_json::to_string_pretty(&input).unwrap_or_default()));
            }
            ClaudeEvent::CancelPermission { id } => self.permissions.retain(|p| p.0 != id),
            ClaudeEvent::Done { error } => {
                self.busy = false;
                self.permissions.clear();
                if !self.draft.is_empty() {
                    self.history.push((None, std::mem::take(&mut self.draft).into()));
                }
                if let Some(e) = error {
                    self.history.push((Some(t!("claudeChat.error").to_string()), e.into()));
                }
            }
            ClaudeEvent::Closed { reason } => {
                self.ready = false;
                self.closed = true;
                self.busy = false;
                self.permissions.clear();
                self.history.push((Some(t!("claudeChat.closed").to_string()), reason.into()));
            }
        }
        if prev != (self.is_busy(), self.has_pending_permission()) {
            cx.emit(ChatViewEvent::StateChanged);
        }
        if self.scroll.max_offset().y + self.scroll.offset().y <= zpx(3.) {
            self.scroll.scroll_to_bottom();
        }
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

fn reply(index: usize, text: SharedString) -> impl IntoElement {
    gpui_kit::component::text::TextView::markdown(SharedString::from(format!("claude-reply-{index}")), text)
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
        let mut log = div().id("claude-history").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll);
        for (index, (role, body)) in self.history.iter().enumerate() {
            log = log.child(
                div()
                    .mb_3()
                    .child(div().text_color(ui.muted_foreground).child(role.as_deref().unwrap_or("Claude").to_string()))
                    .child(if role.is_none() {
                        reply(index, body.clone()).into_any_element()
                    } else {
                        body.clone().into_any_element()
                    }),
            );
        }
        if !self.draft.is_empty() {
            log = log.child(reply(self.history.len(), self.draft.clone().into()));
        }
        let mut content = div()
            .size_full()
            .flex()
            .flex_col()
            .p_4()
            .gap_2()
            .bg(ui.background)
            .text_color(ui.foreground)
            .child(log);
        if let Some((id, tool, args)) = self.permissions.first() {
            let mut actions = div().flex().gap_2();
            for (allow, label) in [(false, t!("claudeChat.deny")), (true, t!("claudeChat.allow"))] {
                let id = id.clone();
                actions = actions.child(
                    Button::new(if allow { "allow" } else { "deny" }).label(label.to_string()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if this.socket.send(ClaudeInput::Permission { id: id.clone(), allow }) {
                                this.permissions.retain(|p| p.0 != id);
                                cx.emit(ChatViewEvent::StateChanged);
                                cx.notify();
                            }
                        },
                    )),
                );
            }
            content = content.child(
                div()
                    .child(t!("claudeChat.permission", tool = tool.clone()).to_string())
                    .child(div().id("permission-input").overflow_y_scroll().max_h(zpx(160.)).child(args.clone()))
                    .child(actions),
            );
        } else if !self.closed && (!self.ready || self.busy) {
            content = content.child(
                div()
                    .text_color(ui.muted_foreground)
                    .child(if self.ready { t!("claudeChat.working") } else { t!("claudeChat.connecting") }.to_string()),
            );
        }
        content.child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().child(Textarea::new(&self.input)))
                .child(
                    Button::new("send")
                        .primary()
                        .label(t!("claudeChat.send").to_string())
                        .disabled(!self.ready || self.busy)
                        .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                ),
        )
    }
}
