//! AI 助手面板（docs/design/ai-assistant.md）：侧栏「设置」旁的按钮、⌘J、命令面板、原生菜单都开它。
//!
//! - 面板浮在主区左下角、贴着侧栏（按钮就在侧栏左下角），不是模态对话框：开着时照样能点画布里
//!   的终端，工作区的快捷键也都在。⤢ 铺满主区，— 收起；
//! - 一段**对话**就是一个会话（像 Warp 的一个标签）：一个终端 + 一段 AI 上下文，你敲的命令与
//!   agent 跑的命令都在这个 shell 里、记成同一条时间线上的命令块。「＋」开新对话 = 新终端 +
//!   新上下文；切回旧对话，它的终端还活着；删对话 = 终止它的终端；
//! - 视图常驻在 [`Workspace::assistant`] 上，收起面板不卸载：每段打开过的对话的 WS 一直连着，再
//!   打开是原样。收起本来就只是 Detach，终端与在跑的一轮照旧；
//! - ⌘⇧I（浏览器里另有 Alt+I）在 Shell / Agent 两种输入之间切，绑在 key context "Assistant"
//!   上：keymap 先于终端的 key_down 匹配，终端聚焦时也切得动。

mod blocks;
mod pane;

use std::collections::HashMap;

use falcon_core::assistant::{self as core, LOCAL};
use falcon_core::pane_key::{PaneItem, parse_pane_key};
use falcon_proto::assistant::{AssistantSettings, CreateThreadRequest, ThreadStatus, ThreadSummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, FocusHandle, Focusable, IntoElement, Render, SharedString, Window, div};
use rust_i18n::t;

use crate::actions::assistant::ToggleMode;
use crate::labels::local_word;
use crate::menus::{MenuItemSpec, to_popup};
use crate::theme::{TerminalLook, Ui, radius};
use crate::ui::{icon, icon_button};
use crate::workspace::Workspace;
use crate::zoom::zpx;
use pane::{PaneEvent, ThreadPane};

/// 面板平常的大小（贴左下角）；主区放不下时缩到主区
const PANEL_W: f32 = 560.;
const PANEL_H: f32 = 680.;
const HEADER_H: f32 = 40.;
/// 面板离主区边缘的那道缝
const INSET: f32 = 8.;

pub struct AssistantPanel {
    ws: Entity<Workspace>,
    pub open: bool,
    /// ⤢：铺满主区
    expanded: bool,
    /// 全部对话（跨机器），新的在前
    threads: Vec<ThreadSummary>,
    /// 列表拿到过没有。没拿到之前不知道有没有对话，不自动开新的
    loaded: bool,
    current: Option<String>,
    /// 打开过的对话：切走不卸载，WS 与终端一直连着
    panes: HashMap<String, Entity<ThreadPane>>,
    settings: Option<AssistantSettings>,
    /// 新对话的请求在路上：别连点出两个
    creating: bool,
    focus: FocusHandle,
}

/// 打开 / 收起助手。第一次打开时建视图并挂到工作区上，之后一直复用
pub fn toggle(ws: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let panel = ensure(ws, cx);
    if panel.read(cx).open {
        panel.update(cx, |p, cx| p.close(window, cx));
    } else {
        panel.update(cx, |p, cx| p.show(window, cx));
    }
}

fn ensure(ws: &Entity<Workspace>, cx: &mut App) -> Entity<AssistantPanel> {
    match ws.read(cx).assistant.clone() {
        Some(panel) => panel,
        None => {
            let panel = cx.new(|cx| AssistantPanel::new(ws.clone(), cx));
            ws.update(cx, |w, _| w.assistant = Some(panel.clone()));
            panel
        }
    }
}

/// 收起面板时焦点还给工作区里活动的那扇终端（面板不是对话框，没有「关掉就回到原处」这回事）
fn focus_active_terminal(ws: &Entity<Workspace>, window: &mut Window, cx: &mut App) {
    let w = ws.read(cx);
    if let Some(key) = w.active_key()
        && let Some(PaneItem::Terminal { id, .. }) = parse_pane_key(&key)
        && let Some(view) = w.terminals.get(&id).cloned()
    {
        let handle = view.read(cx).focus_handle().clone();
        window.focus(&handle, cx);
    }
}

impl AssistantPanel {
    fn new(ws: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        // 主机删了 / 改了名：菜单与底栏跟着变（删主机时服务端已把那台机器上的对话一起删掉）
        cx.observe(&ws, |this, ws, cx| {
            let hosts = ws.read(cx).hosts.clone();
            let before = this.threads.len();
            this.threads.retain(|t| core::machine_exists(&t.machine, &hosts));
            if this.threads.len() != before {
                this.panes.retain(|id, _| this.threads.iter().any(|t| &t.id == id));
                if this.current.as_ref().is_some_and(|id| !this.panes.contains_key(id) && !this.threads.iter().any(|t| &t.id == id)) {
                    this.current = this.threads.first().map(|t| t.id.clone());
                }
            }
            cx.notify();
        })
        .detach();
        Self {
            ws,
            open: false,
            expanded: false,
            threads: Vec::new(),
            loaded: false,
            current: None,
            panes: HashMap::new(),
            settings: None,
            creating: false,
            focus: cx.focus_handle(),
        }
    }

    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.reload(window, cx);
        self.focus_current(window, cx);
        // 面板画在工作区窗口的主区里：让窗口重画一次
        self.ws.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = false;
        focus_active_terminal(&self.ws, window, cx);
        self.ws.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// 设置页存了新的设置（填了 API key、加了 ACP agent）：各段对话的底栏跟着变
    pub fn set_settings(&mut self, settings: AssistantSettings, cx: &mut Context<Self>) {
        self.settings = Some(settings);
        for pane in self.panes.values() {
            let s = self.settings.clone();
            pane.update(cx, |p, cx| p.set_settings(s, cx));
        }
        cx.notify();
    }

    /// 拉对话列表与设置。一段对话都没有时在本机开一段（打开助手默认就是本机的一个终端）
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.ws.read(cx).client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let threads = client.assistant_threads().await;
            let settings = client.assistant_settings().await;
            this.update_in(cx, |this, window, cx| {
                if let Ok(s) = settings {
                    this.set_settings(s, cx);
                }
                match threads {
                    Ok(list) => {
                        this.threads = list;
                        this.loaded = true;
                        let alive = this.current.as_ref().is_some_and(|id| this.threads.iter().any(|t| &t.id == id));
                        if !alive {
                            this.current = this.threads.first().map(|t| t.id.clone());
                        }
                        match this.current.clone() {
                            Some(id) => this.select(id, window, cx),
                            None if this.open => this.create(LOCAL.to_string(), window, cx),
                            None => {}
                        }
                    }
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn pane(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<ThreadPane>> {
        if let Some(p) = self.panes.get(id) {
            return Some(p.clone());
        }
        let summary = self.threads.iter().find(|t| t.id == id)?.clone();
        let (ws, settings) = (self.ws.clone(), self.settings.clone());
        let pane = cx.new(|cx| ThreadPane::new(ws, summary, settings, window, cx));
        cx.subscribe(&pane, |this, pane, ev: &PaneEvent, cx| match ev {
            PaneEvent::Changed => {
                let s = pane.read(cx).summary.clone();
                if let Some(t) = this.threads.iter_mut().find(|t| t.id == s.id) {
                    *t = s;
                }
                cx.notify();
            }
        })
        .detach();
        self.panes.insert(id.to_string(), pane.clone());
        Some(pane)
    }

    fn current_pane(&self) -> Option<Entity<ThreadPane>> {
        self.current.as_ref().and_then(|id| self.panes.get(id)).cloned()
    }

    fn focus_current(&self, window: &mut Window, cx: &mut App) {
        match self.current_pane() {
            Some(p) => p.update(cx, |p, cx| p.focus_input(window, cx)),
            None => self.focus.focus(window, cx),
        }
    }

    fn select(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.current = Some(id.clone());
        self.pane(&id, window, cx);
        if self.open {
            self.focus_current(window, cx);
        }
        cx.notify();
    }

    /// 新对话 = 在这台机器上开一个新终端 + 一段新的 AI 上下文
    fn create(&mut self, machine: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        self.creating = true;
        let client = self.ws.read(cx).client.clone();
        let hint = TerminalLook::global(cx).appearance.clone();
        cx.spawn_in(window, async move |this, cx| {
            let request = CreateThreadRequest {
                agent: None,
                appearance: hint.appearance,
                background: hint.background.clone(),
                foreground: hint.foreground.clone(),
            };
            let res = client.create_assistant_thread(&machine, &request).await;
            this.update_in(cx, |this, window, cx| {
                this.creating = false;
                match res {
                    Ok(summary) => {
                        let id = summary.id.clone();
                        this.threads.insert(0, summary);
                        this.select(id, window, cx);
                    }
                    Err(err) => this.ws.update(cx, |w, cx| w.handle_error(&err, cx)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// 删对话：在跑的一轮先停、它的终端终止（服务端做）。删的是当前这段就换到最近的另一段
    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.threads.retain(|t| t.id != id);
        self.panes.remove(&id);
        if self.current.as_deref() == Some(id.as_str()) {
            self.current = None;
            if let Some(next) = self.threads.first().map(|t| t.id.clone()) {
                self.select(next, window, cx);
            } else {
                self.focus.focus(window, cx);
            }
        }
        let client = self.ws.read(cx).client.clone();
        cx.spawn(async move |this, cx| {
            if let Err(err) = client.delete_assistant_thread(&id).await {
                this.update(cx, |this, cx| this.ws.update(cx, |w, cx| w.handle_error(&err, cx))).ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn set_expanded(&mut self, on: bool, cx: &mut Context<Self>) {
        self.expanded = on;
        cx.notify();
    }

    /// 自动化脚本的 `type:`：面板开着、停在 Shell 输入时往当前对话的终端里打字
    #[cfg(feature = "automation")]
    pub fn automation_terminal(&self, cx: &App) -> Option<Entity<crate::terminal::view::TerminalView>> {
        let pane = self.current_pane().filter(|_| self.open)?;
        let p = pane.read(cx);
        (p.mode == pane::Mode::Shell).then(|| p.terminal()).flatten()
    }

    /// 自动化脚本的 `assistant-agent:`：当前对话换这个 agent（`builtin` 或 `acp:<id>`）
    #[cfg(feature = "automation")]
    pub fn automation_pick_agent(&mut self, agent: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.current_pane() {
            let agent = agent.to_string();
            p.update(cx, |p, cx| {
                p.set_mode(pane::Mode::Agent, window, cx);
                p.set_agent(agent, cx);
            });
        }
    }

    /// 自动化脚本的 `assistant-prompt:`：切到 Agent 输入、填字、发送
    #[cfg(feature = "automation")]
    pub fn automation_prompt(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.current_pane() {
            p.update(cx, |p, cx| p.automation_prompt(text, window, cx));
        }
    }

    /// 自动化脚本的 `assistant-new:<机器>`：开新对话（`local` 或 hostId）
    #[cfg(feature = "automation")]
    pub fn automation_new(&mut self, machine: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.create(machine.to_string(), window, cx);
    }

    /// 自动化脚本的 `assistant-expand`：⤢ 开合
    #[cfg(feature = "automation")]
    pub fn automation_toggle_expand(&mut self, cx: &mut Context<Self>) {
        self.set_expanded(!self.expanded, cx);
    }

    // ---------------- 渲染 ----------------

    /// 一段对话在菜单与标题上叫什么：起了名（第一句话 / 第一条命令）就用它，没有就是「新对话」
    fn thread_title(t: &ThreadSummary) -> String {
        if t.title.is_empty() { t!("assistant.newThread").to_string() } else { t.title.clone() }
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let hosts = self.ws.read(cx).hosts.clone();
        let word = local_word();
        let this = cx.entity();
        let current = self.current.as_ref().and_then(|id| self.threads.iter().find(|t| &t.id == id));

        // 左边：当前对话 ▾ —— 全部对话（跨机器，带机器名），末尾是删掉当前这段
        let mut items: Vec<MenuItemSpec> = Vec::new();
        for t in &self.threads {
            let this = this.clone();
            let id = t.id.clone();
            let machine = core::machine_label(&t.machine, &hosts, &word);
            let busy = matches!(t.status, ThreadStatus::Running | ThreadStatus::AwaitingPermission);
            let mark = if busy { " ●" } else { "" };
            items.push(
                MenuItemSpec::new(format!("{}{mark}  ·  {machine}", Self::thread_title(t)), move |window, cx| {
                    let id = id.clone();
                    this.update(cx, |p, cx| p.select(id, window, cx))
                })
                .checked(Some(&t.id) == self.current.as_ref()),
            );
        }
        if let Some(id) = self.current.clone() {
            let this = this.clone();
            items.push(
                MenuItemSpec::new(t!("assistant.deleteThread").to_string(), move |window, cx| {
                    let id = id.clone();
                    this.update(cx, |p, cx| p.delete(id, window, cx))
                })
                .sep()
                .danger(),
            );
        }
        let (title, machine_icon) = match current {
            Some(t) => (Self::thread_title(t), if t.machine == LOCAL { IconName::Monitor } else { IconName::Server }),
            None => (t!("assistant.title").to_string(), IconName::Sparkles),
        };
        let switcher = Button::new("assistant-threads")
            .ghost()
            .small()
            .icon(machine_icon)
            .label(SharedString::from(title))
            .dropdown_caret(true)
            .disabled(self.threads.is_empty())
            .tooltip(t!("assistant.switchThread").to_string())
            .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone()));

        // ＋：只有本机时一点就开；有 SSH Host 时先挑机器
        let machines = core::machines(&hosts, &word);
        let new_button = if machines.len() <= 1 {
            let this = this.clone();
            Button::new("assistant-new")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .tooltip(t!("assistant.newThreadHint").to_string())
                .disabled(self.creating)
                .on_click(move |_, window, cx| this.update(cx, |p, cx| p.create(LOCAL.to_string(), window, cx)))
                .into_any_element()
        } else {
            let choices: Vec<MenuItemSpec> = machines
                .into_iter()
                .map(|m| {
                    let this = this.clone();
                    let text = match &m.detail {
                        Some(detail) => format!("{}  ·  {detail}", m.label),
                        None => m.label.clone(),
                    };
                    let key = m.key.clone();
                    MenuItemSpec::new(text, move |window, cx| {
                        let key = key.clone();
                        this.update(cx, |p, cx| p.create(key, window, cx))
                    })
                })
                .collect();
            Button::new("assistant-new")
                .ghost()
                .small()
                .icon(IconName::Plus)
                .tooltip(t!("assistant.newThreadOn").to_string())
                .disabled(self.creating)
                .dropdown_menu(move |menu, _, _| to_popup(menu, choices.clone()))
                .into_any_element()
        };

        let ws = self.ws.clone();
        let expand_this = this.clone();
        let expanded = self.expanded;
        let close_this = this.clone();
        div()
            .h(zpx(HEADER_H))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .pl(zpx(6.))
            .pr(zpx(8.))
            .child(div().min_w_0().flex_shrink(1.).overflow_hidden().child(switcher))
            .child(div().flex_1())
            .child(new_button)
            .child(icon_button(
                "assistant-settings",
                IconName::Settings,
                t!("assistant.openSettings").to_string(),
                zpx(26.),
                cx,
                move |_, window, cx| crate::dialogs::settings::open(&ws, crate::dialogs::settings::Tab::Assistant, window, cx),
            ))
            .child(icon_button(
                "assistant-expand",
                if expanded { IconName::Minimize } else { IconName::Maximize },
                if expanded { t!("assistant.restore").to_string() } else { t!("assistant.expand").to_string() },
                zpx(26.),
                cx,
                move |_, _, cx| expand_this.update(cx, |p, cx| p.set_expanded(!expanded, cx)),
            ))
            .child(icon_button(
                "assistant-close",
                IconName::Minus,
                format!("{} · {}（{}）", t!("assistant.close"), crate::ui::chord("toggleAssistant"), t!("assistant.detachHint")),
                zpx(26.),
                cx,
                move |_, window, cx| close_this.update(cx, |p, cx| p.close(window, cx)),
            ))
            .text_color(ui.foreground)
    }

    /// 一段对话都没有（全删了）：中间一句话 + 在哪台机器上开
    fn empty(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let hosts = self.ws.read(cx).hosts.clone();
        let word = local_word();
        let mut col = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .child(icon(IconName::Sparkles).size(zpx(28.)).text_color(ui.tint_strong))
            .child(
                div()
                    .max_w(zpx(360.))
                    .text_sm()
                    .text_center()
                    .text_color(ui.muted_foreground)
                    .whitespace_normal()
                    .child(if self.loaded { t!("assistant.empty").to_string() } else { t!("assistant.settings.loading").to_string() }),
            );
        if self.loaded {
            let mut row = div().flex().flex_wrap().justify_center().gap_2();
            for m in core::machines(&hosts, &word) {
                let this = cx.entity();
                let key = m.key.clone();
                row = row.child(
                    Button::new(SharedString::from(format!("assistant-empty-{}", m.key)))
                        .small()
                        .icon(if m.key == LOCAL { IconName::Monitor } else { IconName::Server })
                        .label(m.label.clone())
                        .disabled(self.creating)
                        .on_click(move |_, window, cx| {
                            let key = key.clone();
                            this.update(cx, |p, cx| p.create(key, window, cx))
                        }),
                );
            }
            col = col.child(row);
        }
        col
    }
}

impl Focusable for AssistantPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AssistantPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        // 平常贴左下角；主区（画布那一格）比它小就缩到主区。窗口外框、标题栏留出余量
        let vp = window.viewport_size();
        let height = if self.expanded { None } else { Some(zpx(PANEL_H).min(vp.height - zpx(104.))) };
        // 实时区域终端的格子数照面板的高度定（切 Shell / Agent 不改它，见 pane.rs 顶部）
        let body = height.unwrap_or(vp.height - zpx(104.)) - zpx(HEADER_H) - zpx(56.);
        let pane = self.current_pane();
        if let Some(p) = &pane {
            p.update(cx, |p, _| p.set_term_height(body.max(zpx(160.))));
        }
        // 面板浮在画布上，底下多半就是终端，两者同一个底色：岛的「只靠底色分块、不画边框」在这里
        // 分不出轮廓。所以它像弹出层一样带一圈细边与阴影，并离主区的边留一道缝
        let mut root = div()
            .id("assistant-panel")
            .key_context("Assistant")
            .track_focus(&self.focus)
            .absolute()
            .left(zpx(INSET))
            .bottom(zpx(INSET))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(radius::LG)
            .bg(ui.background)
            .border_1()
            .border_color(ui.input)
            .shadow_lg()
            .occlude()
            .text_color(ui.foreground)
            .on_action(cx.listener(|this, _: &ToggleMode, window, cx| {
                if let Some(p) = this.current_pane() {
                    p.update(cx, |p, cx| p.toggle_mode(window, cx));
                }
            }));
        root = match height {
            Some(h) => root.w(zpx(PANEL_W)).max_w_full().h(h),
            None => root.top(zpx(INSET)).right(zpx(INSET)),
        };
        root.child(self.header(cx)).child(
            div().flex_1().min_h_0().overflow_hidden().child(match pane {
                Some(p) => p.into_any_element(),
                None => self.empty(cx).into_any_element(),
            }),
        )
    }
}
