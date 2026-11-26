//! 桌面端 Claude 对话窗口。线程历史由宿主机回放。
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
    App, Bounds, Context, Entity, Render, ScrollHandle, SharedString, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, size,
};
use rust_i18n::t;

use crate::{theme::Ui, workspace::Workspace, zoom::zpx};

pub fn open(ws: &Entity<Workspace>, project: &str, cx: &mut App) {
    open_with_prompt(ws, project, None, cx);
}

pub fn open_with_prompt(
    ws: &Entity<Workspace>,
    project: &str,
    initial: Option<&str>,
    cx: &mut App,
) {
    let owner = ws.read(cx);
    let name = owner
        .owner_project(project)
        .map(|p| p.name)
        .unwrap_or_else(|| project.to_owned());
    let client = owner.client.clone();
    let title = format!("{} · {name}", t!("claudeChat.title"));
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(zpx(780.), zpx(620.)),
            cx,
        ))),
        ..Default::default()
    };
    if let Err(err) = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| Chat::new(client, project, initial, window, cx));
        cx.new(|cx| crate::window_background::mount_root(view, window, cx))
    }) {
        ws.update(cx, |w, cx| {
            w.toast(
                crate::workspace::ToastKind::Danger,
                err.to_string(),
                None,
                cx,
            )
        });
    }
}

struct Chat {
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

impl Chat {
    fn new(
        client: FalconClient,
        project: &str,
        initial: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (socket, mut events) = ClaudeSocket::open(&client, project);
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .submit_on_enter(true)
                .placeholder(t!("claudeChat.placeholder").to_string())
        });
        input.update(cx, |i, cx| i.focus(window, cx));
        let enter = cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    this.submit(window, cx);
                }
            },
        );
        let mut initial = initial
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned);
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        if matches!(event, ClaudeEvent::Ready) {
                            if let Some(initial) = initial.take() {
                                if this.durable
                                    && this.history.is_empty()
                                    && this.input.read(cx).value().is_empty()
                                {
                                    this.input
                                        .update(cx, |i, cx| i.set_value(initial, window, cx));
                                }
                            }
                        }
                        this.event(event, window, cx);
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

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if !self.ready || self.busy || text.trim().is_empty() {
            return;
        }
        if self.socket.send(ClaudeInput::Prompt { text: text.clone() }) {
            if !self.durable {
                self.push_user(text);
                self.input.update(cx, |i, cx| i.set_value("", window, cx));
            }
            self.busy = true;
            self.scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    fn push_user(&mut self, text: String) {
        self.history
            .push((Some(t!("claudeChat.you").to_string()), text.into()));
    }

    fn event(&mut self, event: ClaudeEvent, window: &mut Window, cx: &mut Context<Self>) {
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
                    self.history.push((
                        Some(t!("claudeChat.tools").to_string()),
                        tools.join(" · ").into(),
                    ));
                }
            }
            ClaudeEvent::Permission { id, tool, input } => {
                self.permissions.push((
                    id,
                    tool,
                    serde_json::to_string_pretty(&input).unwrap_or_default(),
                ));
            }
            ClaudeEvent::CancelPermission { id } => self.permissions.retain(|p| p.0 != id),
            ClaudeEvent::Done { error } => {
                self.busy = false;
                self.permissions.clear();
                if !self.draft.is_empty() {
                    self.history
                        .push((None, std::mem::take(&mut self.draft).into()));
                }
                if let Some(e) = error {
                    self.history
                        .push((Some(t!("claudeChat.error").to_string()), e.into()));
                }
            }
            ClaudeEvent::Closed { reason } => {
                self.ready = false;
                self.closed = true;
                self.busy = false;
                self.permissions.clear();
                self.history
                    .push((Some(t!("claudeChat.closed").to_string()), reason.into()));
            }
        }
        if self.scroll.max_offset().y + self.scroll.offset().y <= zpx(3.) {
            self.scroll.scroll_to_bottom();
        }
    }
}

fn reply(index: usize, text: SharedString) -> impl IntoElement {
    gpui_kit::component::text::TextView::markdown(
        SharedString::from(format!("claude-reply-{index}")),
        text,
    )
    .selectable(true)
    // 消息里的图片路径属于宿主机，不能交给桌面客户端当本地路径读取。
    .image_source(|_| "icons/image.svg".into())
    .on_link_click(|href, _, _, cx| {
        if let Some(url) = falcon_core::md_link::external_href(href) {
            cx.open_url(&url);
        }
    })
}

impl Render for Chat {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let mut log = div()
            .id("claude-history")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        for (index, (role, body)) in self.history.iter().enumerate() {
            log = log.child(
                div()
                    .mb_3()
                    .child(
                        div()
                            .text_color(ui.muted_foreground)
                            .child(role.as_deref().unwrap_or("Claude").to_string()),
                    )
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
            for (allow, label) in [
                (false, t!("claudeChat.deny")),
                (true, t!("claudeChat.allow")),
            ] {
                let id = id.clone();
                actions = actions.child(
                    Button::new(if allow { "allow" } else { "deny" })
                        .label(label.to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.socket.send(ClaudeInput::Permission {
                                id: id.clone(),
                                allow,
                            }) {
                                this.permissions.retain(|p| p.0 != id);
                                cx.notify();
                            }
                        })),
                );
            }
            content = content.child(
                div()
                    .child(t!("claudeChat.permission", tool = tool.clone()).to_string())
                    .child(
                        div()
                            .id("permission-input")
                            .overflow_y_scroll()
                            .max_h(zpx(160.))
                            .child(args.clone()),
                    )
                    .child(actions),
            );
        } else if !self.closed && (!self.ready || self.busy) {
            content = content.child(
                div().text_color(ui.muted_foreground).child(
                    if self.ready {
                        t!("claudeChat.working")
                    } else {
                        t!("claudeChat.connecting")
                    }
                    .to_string(),
                ),
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
