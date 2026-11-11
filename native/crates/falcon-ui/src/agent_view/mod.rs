//! 原生 Coding Agent 画布视图（方案二：基于 `Claude Code` `stream-json` 与 `Codex` `app-server`）。
//!
//! 嵌入在会话窗口（[`crate::terminal::view::TerminalView`]）中，共享同一条 `SessionSocket`，
//! 支持流式 Markdown 渲染、可折叠思考块、行级文件 Diff、ANSI 着色终端输出、原生权限审批按钮、
//! 模型与思考程度选择、斜杠命令补全，以及同一会话的紧凑 / 图形两种原生排版。

pub mod blocks;

use std::collections::HashSet;
use std::rc::Rc;

use falcon_core::agent_output::{TimelineItem, compact_timeline};
use falcon_proto::{
    AgentDeltaKind, AgentEntry, AgentModelOption, AgentPermissionDecision, AgentSlashCommand,
    AgentTurnStatus, AgentViewState,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, Render,
    ScrollHandle, SharedString, Subscription, Window, div,
};
use rust_i18n::t;

use self::blocks::{EntryRenderCallbacks, render_entry};
use crate::session_icon::session_icon;
use crate::theme::{Ui, radius};
use crate::ui::{icon, spinner};
use crate::zoom::zpx;

/// 原生 Agent 面板向所属 `TerminalView` 发出的交互事件。
#[derive(Debug, Clone)]
pub enum AgentPaneEvent {
    Prompt(String),
    Interrupt,
    Permission {
        call_id: String,
        decision: AgentPermissionDecision,
        updated_command: Option<String>,
    },
    SetModel(String),
    SetReasoningEffort(Option<String>),
    ToggleStyle,
    OpenFile(String),
    FocusChanged(bool),
}

impl EventEmitter<AgentPaneEvent> for AgentPane {}

/// 独立旋转指示器子视图：动画重绘只在此小视图内发生，不触发整条 Markdown 时间线重排。
struct RunningSpinner;

impl Render for RunningSpinner {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let color = Ui::global(cx).tint_strong;
        div()
            .flex()
            .items_center()
            .text_color(color)
            .child(spinner("agent-view-spin", IconName::LoaderCircle, zpx(13.)))
    }
}

/// 独立输入框子视图：光标闪烁与按键上屏只重绘输入框自身。
struct ComposerInput {
    input: Entity<TextareaState>,
    slash_prefix: Option<String>,
    enabled: bool,
    can_submit: bool,
    terminal_style: bool,
    _subs: Vec<Subscription>,
}

enum ComposerEvent {
    Submit(String),
    SlashPrefixChanged(Option<String>),
}

impl EventEmitter<ComposerEvent> for ComposerInput {}

impl ComposerInput {
    fn new(agent_label: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = t!("agentView.placeholder", agent = agent_label).to_string();
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .placeholder(placeholder)
                .submit_on_enter(true)
        });
        let sub = cx.subscribe_in(&input, window, |this, state, ev: &InputEvent, window, cx| {
            match ev {
                InputEvent::PressEnter {
                    secondary: false, ..
                } => {
                    if !this.can_submit {
                        return;
                    }
                    let val = state.read(cx).value().to_string();
                    let trimmed = val.trim().to_string();
                    if !trimmed.is_empty() {
                        state.update(cx, |s, cx| s.set_value("", window, cx));
                        if this.slash_prefix.take().is_some() {
                            cx.emit(ComposerEvent::SlashPrefixChanged(None));
                        }
                        cx.emit(ComposerEvent::Submit(trimmed));
                    }
                }
                InputEvent::Change => {
                    let val = state.read(cx).value().to_string();
                    let next = if val.starts_with('/') && !val.contains('\n') && !val.contains(' ') {
                        Some(val)
                    } else {
                        None
                    };
                    if next != this.slash_prefix {
                        this.slash_prefix = next.clone();
                        cx.emit(ComposerEvent::SlashPrefixChanged(next));
                    }
                    cx.notify();
                }
                _ => {}
            }
        });
        Self {
            input,
            slash_prefix: None,
            enabled: false,
            can_submit: false,
            terminal_style: true,
            _subs: vec![sub],
        }
    }

    fn configure(&mut self, enabled: bool, can_submit: bool, terminal_style: bool, cx: &mut Context<Self>) {
        if (self.enabled, self.can_submit, self.terminal_style) != (enabled, can_submit, terminal_style) {
            self.enabled = enabled;
            self.can_submit = can_submit;
            self.terminal_style = terminal_style;
            cx.notify();
        }
    }

    fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.enabled { return; }
        self.input.update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        self.slash_prefix = None;
        cx.emit(ComposerEvent::SlashPrefixChanged(None));
        cx.notify();
    }

    fn take_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        if !self.can_submit { return None; }
        let val = self.input.read(cx).value().trim().to_string();
        if val.is_empty() {
            return None;
        }
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        if self.slash_prefix.take().is_some() {
            cx.emit(ComposerEvent::SlashPrefixChanged(None));
        }
        cx.notify();
        Some(val)
    }
}

impl Render for ComposerInput {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_w_0()
            .when(self.terminal_style, |d| d.font_family(crate::fonts::BERKELEY))
            .child(Textarea::new(&self.input).appearance(false).small().disabled(!self.enabled))
    }
}

pub struct AgentPane {
    pub session_id: String,
    pub state: AgentViewState,
    terminal_style: bool,
    available: bool,
    composer: Entity<ComposerInput>,
    spinner: Entity<RunningSpinner>,
    scroll: ScrollHandle,
    folded: HashSet<String>,
    slash_prefix: Option<String>,
    style_gen: usize,
    _subs: Vec<Subscription>,
}

fn effort_label(effort: &str) -> String {
    match effort {
        "off" => t!("agentView.effortOff").to_string(),
        "minimal" => t!("agentView.effortMinimal").to_string(),
        "low" => t!("agentView.effortLow").to_string(),
        "medium" => t!("agentView.effortMedium").to_string(),
        "high" => t!("agentView.effortHigh").to_string(),
        "xhigh" => t!("agentView.effortXhigh").to_string(),
        "max" => t!("agentView.effortMax").to_string(),
        other => other.to_string(),
    }
}

fn agent_display_name(agent: falcon_proto::SessionAgent) -> String {
    match agent {
        falcon_proto::SessionAgent::Claude => t!("agent.claude").to_string(),
        falcon_proto::SessionAgent::Codex => t!("agent.codex").to_string(),
        other => other.as_str().to_string(),
    }
}

impl AgentPane {
    pub fn new(
        session_id: String,
        state: AgentViewState,
        terminal_style: bool,
        available: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let label = agent_display_name(state.agent);
        let composer = cx.new(|cx| ComposerInput::new(&label, window, cx));
        composer.update(cx, |c, cx| c.configure(
            available && state.active && !state.terminal_backed && state.status != AgentTurnStatus::Unknown,
            available && state.active && !state.terminal_backed && state.status != AgentTurnStatus::Unknown && !matches!(state.status, AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission),
            terminal_style,
            cx,
        ));
        let spinner = cx.new(|_| RunningSpinner);
        let sub = cx.subscribe(&composer, |this, _, ev: &ComposerEvent, cx| match ev {
            ComposerEvent::Submit(text) => {
                cx.emit(AgentPaneEvent::Prompt(text.clone()));
            }
            ComposerEvent::SlashPrefixChanged(prefix) => {
                this.slash_prefix = prefix.clone();
                cx.notify();
            }
        });
        let focus = composer.read(cx).input.read(cx).focus_handle(cx);
        let focus_in = cx.on_focus_in(&focus, window, |_, _, cx| {
            cx.emit(AgentPaneEvent::FocusChanged(true));
        });
        let focus_out = cx.on_focus_out(&focus, window, |_, _, _, cx| {
            cx.emit(AgentPaneEvent::FocusChanged(false));
        });

        Self {
            session_id,
            state,
            terminal_style,
            available,
            composer,
            spinner,
            scroll: ScrollHandle::new(),
            folded: HashSet::new(),
            slash_prefix: None,
            style_gen: 0,
            _subs: vec![sub, focus_in, focus_out],
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).input.read(cx).focus_handle(cx)
    }

    fn sync_composer(&self, cx: &mut Context<Self>) {
        let enabled = self.available && self.state.active && !self.state.terminal_backed
            && self.state.status != AgentTurnStatus::Unknown;
        let can_submit = enabled && !matches!(self.state.status, AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission);
        self.composer.update(cx, |c, cx| c.configure(enabled, can_submit, self.terminal_style, cx));
    }

    pub fn set_terminal_style(&mut self, terminal_style: bool, cx: &mut Context<Self>) {
        if self.terminal_style != terminal_style {
            self.terminal_style = terminal_style;
            self.style_gen = self.style_gen.wrapping_add(1);
            self.sync_composer(cx);
            cx.notify();
        }
    }

    pub fn set_available(&mut self, available: bool, cx: &mut Context<Self>) {
        if self.available != available {
            self.available = available;
            self.sync_composer(cx);
            cx.notify();
        }
    }

    /// 更新前判断位置：用户上滚查看历史时，流式输出不能把视口拉回末尾。
    fn following_output(&self) -> bool {
        self.scroll.max_offset().y + self.scroll.offset().y <= zpx(32.)
    }

    pub fn apply_snapshot(&mut self, state: AgentViewState, cx: &mut Context<Self>) {
        let follow = self.state.entries.is_empty() || self.following_output();
        self.state = state;
        self.sync_composer(cx);
        if follow {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    pub fn apply_entry(&mut self, entry: AgentEntry, cx: &mut Context<Self>) {
        let follow = matches!(entry, AgentEntry::User { .. }) || self.following_output();
        self.state.upsert_entry(entry);
        if follow {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    pub fn apply_delta(
        &mut self,
        id: &str,
        kind: AgentDeltaKind,
        delta: &str,
        cx: &mut Context<Self>,
    ) {
        let follow = self.following_output();
        self.state.apply_delta(id, kind, delta);
        if follow {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_meta(
        &mut self,
        active: bool,
        status: AgentTurnStatus,
        model: Option<String>,
        permission_mode: Option<String>,
        cwd: Option<String>,
        commands: Option<Vec<AgentSlashCommand>>,
        models: Option<Vec<String>>,
        model_options: Option<Vec<AgentModelOption>>,
        reasoning_effort: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.state.active = active;
        self.state.status = status;
        if model.is_some() {
            self.state.model = model;
        }
        if permission_mode.is_some() {
            self.state.permission_mode = permission_mode;
        }
        if cwd.is_some() {
            self.state.cwd = cwd;
        }
        if let Some(cmds) = commands {
            self.state.commands = cmds;
        }
        if let Some(ms) = models {
            self.state.models = ms;
        }
        if let Some(options) = model_options {
            self.state.model_options = options;
        }
        self.state.reasoning_effort = reasoning_effort;
        self.sync_composer(cx);
        cx.notify();
    }

    fn pending_permission_call_id(&self) -> Option<String> {
        self.state.entries.iter().rev().find_map(|e| match e {
            AgentEntry::ToolCall(tc) if tc.permission_pending => Some(tc.id.clone()),
            _ => None,
        })
    }

    fn render_empty_state(&self, ui: &Ui) -> AnyElement {
        let label = agent_display_name(self.state.agent);
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(zpx(8.))
            .p(zpx(24.))
            .text_color(ui.muted_foreground)
            .child(
                session_icon(Some(self.state.agent))
                    .size(zpx(28.))
                    .text_color(ui.muted_foreground.opacity(0.6)),
            )
            .child(
                div()
                    .text_size(zpx(14.))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(ui.foreground)
                    .child(format!("{label} · {}", t!(if self.state.terminal_backed {
                        "agentView.transcriptEmptyTitle"
                    } else { "agentView.emptyTitle" }))),
            )
            .child(
                div()
                    .text_size(zpx(12.))
                    .text_color(ui.muted_foreground)
                    .child(t!(if self.state.terminal_backed {
                        "agentView.transcriptEmptyHint"
                    } else { "agentView.emptyHint" }).to_string()),
            )
            .into_any_element()
    }

    fn render_slash_popup(&self, ui: &Ui, cx: &mut Context<Self>) -> Option<AnyElement> {
        let prefix = self.slash_prefix.as_deref()?;
        let query = prefix.trim_start_matches('/').to_lowercase();
        let matches: Vec<&AgentSlashCommand> = self
            .state
            .commands
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&query))
            .take(8)
            .collect();
        if matches.is_empty() {
            return None;
        }

        let mut list = div()
            .w_full()
            .mb(zpx(6.))
            .p(zpx(4.))
            .rounded(radius::MD)
            .border_1()
            .border_color(ui.app_border)
            .bg(ui.popover)
            .shadow_md()
            .flex()
            .flex_col()
            .gap(zpx(2.));

        for (i, cmd) in matches.into_iter().enumerate() {
            let cmd_text = format!("/{} ", cmd.name);
            let composer = self.composer.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("agent-slash-{i}-{}", cmd.name)))
                    .px(zpx(8.))
                    .py(zpx(4.5))
                    .rounded(radius::SM)
                    .cursor_pointer()
                    .hover(|s| s.bg(ui.secondary))
                    .flex()
                    .items_center()
                    .gap(zpx(8.))
                    .on_click(cx.listener(move |_, _, w, cx| {
                        composer.update(cx, |c, cx| c.set_text(&cmd_text, w, cx));
                    }))
                    .child(
                        div()
                            .font_family(crate::fonts::BERKELEY)
                            .text_size(zpx(12.))
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(ui.tint_strong)
                            .child(format!("/{}", cmd.name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(zpx(11.5))
                            .text_color(ui.muted_foreground)
                            .child(cmd.description.clone()),
                    ),
            );
        }
        use gpui_kit::base::animation::ease_out_cubic;
        use gpui_kit::{Animation, AnimationExt};
        use std::time::Duration;
        Some(
            list.with_animation(
                SharedString::from(format!("agent-slash-popup-{}", self.session_id)),
                Animation::new(Duration::from_millis(130)).with_easing(ease_out_cubic),
                |el, delta| {
                    el.opacity(delta)
                        .relative()
                        .top(zpx(4. * (1. - delta)))
                },
            )
            .into_any_element(),
        )
    }
}

impl Render for AgentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let is_running = matches!(
            self.state.status,
            AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission
        );

        let enabled = self.available && self.state.active && !self.state.terminal_backed
            && self.state.status != AgentTurnStatus::Unknown;
        let can_configure = enabled && !is_running;
        let entity_approve = cx.entity().downgrade();
        let entity_fold = cx.entity().downgrade();
        let entity_open = cx.entity().downgrade();

        let cb = EntryRenderCallbacks {
            can_approve: enabled,
            spinner: self.spinner.clone().into(),
            on_approve: Rc::new(move |call_id, decision, _, cx| {
                if let Some(this) = entity_approve.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.available && this.state.active && !this.state.terminal_backed && this.state.status != AgentTurnStatus::Unknown {
                            cx.emit(AgentPaneEvent::Permission {
                                call_id,
                                decision,
                                updated_command: None,
                            });
                        }
                    });
                }
            }),
            on_toggle_fold: Rc::new(move |id, _, cx| {
                if let Some(this) = entity_fold.upgrade() {
                    this.update(cx, |this, cx| {
                        if !this.folded.remove(&id) {
                            this.folded.insert(id);
                        }
                        cx.notify();
                    });
                }
            }),
            on_open_file: Rc::new(move |path, _, cx| {
                if let Some(this) = entity_open.upgrade() {
                    this.update(cx, |_, cx| {
                        cx.emit(AgentPaneEvent::OpenFile(path));
                    });
                }
            }),
        };

        let timeline_body: AnyElement = if self.state.entries.is_empty() {
            self.render_empty_state(&ui)
        } else {
            let mut items = Vec::new();
            for item in compact_timeline(&self.state.entries) {
                let element = match item {
                    TimelineItem::Entry(idx) => render_entry(idx, &self.state.entries[idx], self.terminal_style, &self.folded, &cb, window, cx),
                    TimelineItem::Activity(range) => {
                        use gpui_kit::base::animation::ease_out_cubic;
                        use gpui_kit::{Animation, AnimationExt};
                        use std::time::Duration;
                        let first_id = self.state.entries[range.start].id().unwrap_or_default();
                        let fold_id = format!("activity:{first_id}");
                        let is_open = self.folded.contains(&fold_id);
                        let toggle = cb.on_toggle_fold.clone();
                        let mut group = div().w_full().flex_none().flex().flex_col().gap(zpx(6.))
                            .child(div()
                                .id(SharedString::from(format!("agent-activity-{first_id}")))
                                .flex().items_center().gap(zpx(6.)).py(zpx(3.))
                                .cursor_pointer().text_size(zpx(12.)).text_color(ui.muted_foreground)
                                .hover(|s| s.text_color(ui.foreground))
                                .on_click(move |_, w, cx| toggle(fold_id.clone(), w, cx))
                                .child(crate::ui::disclosure_chevron(
                                    SharedString::from(format!("agent-activity-chev-{first_id}")),
                                    is_open,
                                    zpx(12.),
                                ))
                                .child(t!("agentView.activity", count = range.len()).to_string()));
                        if is_open {
                            let mut expanded_items = div().w_full().flex_none().flex().flex_col().gap(zpx(6.));
                            for idx in range {
                                expanded_items = expanded_items.child(div().w_full().flex_none().min_w_0().child(
                                    render_entry(idx, &self.state.entries[idx], self.terminal_style, &self.folded, &cb, window, cx),
                                ));
                            }
                            group = group.child(
                                expanded_items.with_animation(
                                    SharedString::from(format!("agent-activity-body-{first_id}")),
                                    Animation::new(Duration::from_millis(140)).with_easing(ease_out_cubic),
                                    |el, delta| el.opacity(delta),
                                ),
                            );
                        }
                        group.into_any_element()
                    }
                };
                items.push(div().w_full().flex_none().min_w_0().child(element));
            }
            // 视口负责限制高度，内层与每条记录按内容自然增长；否则 flex-shrink 会把长消息压在一起。
            let scroller = div()
                .id(SharedString::from(format!("agent-timeline-{}", self.session_id)))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .when(self.terminal_style, |d| d.font_family(crate::fonts::BERKELEY))
                .child(div()
                    .w_full().flex_none().min_w_0()
                    .px(zpx(if self.terminal_style { 12. } else { 16. }))
                    .py(zpx(12.))
                    .flex().flex_col()
                    .gap(zpx(if self.terminal_style { 8. } else { 10. }))
                    .children(items));
            if self.style_gen > 0 {
                use gpui_kit::base::animation::ease_out_cubic;
                use gpui_kit::{Animation, AnimationExt};
                use std::time::Duration;
                scroller
                    .with_animation(
                        SharedString::from(format!("agent-style-fade-{}-{}", self.session_id, self.style_gen)),
                        Animation::new(Duration::from_millis(140)).with_easing(ease_out_cubic),
                        |el, delta| el.opacity(0.35 + 0.65 * delta),
                    )
                    .into_any_element()
            } else {
                scroller.into_any_element()
            }
        };

        // 两种样式共享真实能力目录；运行中不允许修改模型或思考程度。
        let mut status_bar = div()
            .w_full()
            .flex()
            .items_center()
            .justify_between()
            .gap(zpx(8.))
            .text_size(zpx(11.5))
            .text_color(ui.muted_foreground);

        let mut left_tools = div().flex_1().min_w_0().flex().flex_wrap().items_center().gap(zpx(6.));
        let selected = self.state.selected_model_option();
        let current_id = selected.map(|m| m.id.clone()).or_else(|| self.state.model.clone());
        let model_label = selected.map(|m| m.label.clone()).unwrap_or_else(|| {
            self.state.model.clone().unwrap_or_else(|| t!("agentView.modelLoading").to_string())
        });
        // 旧服务端已给出模型名称时仍允许选择；只有真实能力目录能决定思考程度。
        let models = self.state.model_choices();
        let legacy_catalog = self.state.model_options.is_empty() && !models.is_empty();
        let entity_model = cx.entity().downgrade();
        let model_btn = Button::new(SharedString::from(format!("agent-model-{}", self.session_id)))
            .ghost()
            .xsmall()
            .icon(icon(IconName::Cpu))
            .label(model_label)
            .tooltip(if is_running {
                t!("agentView.settingsWhileRunning").to_string()
            } else if models.is_empty() {
                t!("agentView.modelUnavailable").to_string()
            } else if legacy_catalog {
                t!("agentView.legacyCatalogHint").to_string()
            } else {
                t!("agentView.model").to_string()
            })
            .disabled(!can_configure || models.is_empty());
        left_tools = left_tools.child(model_btn.dropdown_menu(move |mut menu, _, _| {
            for model in &models {
                let model_id = model.id.clone();
                let ent = entity_model.clone();
                menu = menu.item(PopupMenuItem::new(model.label.clone())
                    .checked(current_id.as_deref() == Some(model.id.as_str()))
                    .on_click(move |_, _, cx| {
                        if let Some(this) = ent.upgrade() {
                            this.update(cx, |this, cx| {
                                if this.available && this.state.active && !this.state.terminal_backed && this.state.status != AgentTurnStatus::Unknown && !matches!(this.state.status, AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission) {
                                    cx.emit(AgentPaneEvent::SetModel(model_id.clone()));
                                }
                            });
                        }
                    }));
            }
            menu
        }));

        let efforts = selected.map(|m| m.reasoning_efforts.clone()).unwrap_or_default();
        let default_label = selected.and_then(|m| m.default_reasoning_effort.as_deref())
            .map(|effort| t!("agentView.defaultEffort", effort = effort_label(effort)).to_string())
            .unwrap_or_else(|| t!("agentView.default").to_string());
        let effort_value = self.state.reasoning_effort.clone();
        let effort_text = if legacy_catalog {
            t!("agentView.effortNotReported").to_string()
        } else if efforts.is_empty() {
            t!("agentView.effortUnavailable").to_string()
        } else {
            t!("agentView.reasoningValue", effort = effort_value.as_deref().map(effort_label).unwrap_or_else(|| default_label.clone())).to_string()
        };
        let entity_effort = cx.entity().downgrade();
        left_tools = left_tools.child(
            Button::new(SharedString::from(format!("agent-effort-{}", self.session_id)))
                .ghost()
                .xsmall()
                .icon(icon(IconName::Sparkles))
                .label(effort_text)
                .tooltip(if is_running {
                    t!("agentView.settingsWhileRunning").to_string()
                } else if legacy_catalog {
                    t!("agentView.legacyCatalogHint").to_string()
                } else {
                    t!("agentView.reasoning").to_string()
                })
                .disabled(!can_configure || efforts.is_empty())
                .dropdown_menu(move |mut menu, _, _| {
                    for effort in std::iter::once(None).chain(efforts.iter().cloned().map(Some)) {
                        let ent = entity_effort.clone();
                        let label = effort.as_deref().map(effort_label).unwrap_or_else(|| default_label.clone());
                        let checked = effort == effort_value;
                        menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(move |_, _, cx| {
                            if let Some(this) = ent.upgrade() {
                                this.update(cx, |this, cx| {
                                    if this.available && this.state.active && !this.state.terminal_backed && this.state.status != AgentTurnStatus::Unknown && !matches!(this.state.status, AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission) {
                                        cx.emit(AgentPaneEvent::SetReasoningEffort(effort.clone()));
                                    }
                                });
                            }
                        }));
                    }
                    menu
                }),
        );

        if is_running {
            let status_text = if self.state.status == AgentTurnStatus::AwaitingPermission {
                t!("agentView.awaitingPermission").to_string()
            } else {
                t!("agentView.running").to_string()
            };
            left_tools = left_tools.child(
                div()
                    .flex()
                    .items_center()
                    .gap(zpx(5.))
                    .text_color(ui.tint_strong)
                    .child(self.spinner.clone())
                    .child(status_text),
            );
        }

        let right_tools = div().flex().items_center().gap(zpx(6.)).child(
            Button::new(SharedString::from(format!(
                "agent-switch-shell-{}",
                self.session_id
            )))
            .ghost()
            .xsmall()
            .icon(icon(if self.terminal_style { IconName::Sparkles } else { IconName::SquareTerminal }))
            .label(if self.terminal_style {
                t!("agentView.toggleNative").to_string()
            } else {
                t!("agentView.toggleTerminal").to_string()
            })
            .on_click(cx.listener(|_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(AgentPaneEvent::ToggleStyle);
            })),
        );

        status_bar = status_bar.child(left_tools).child(right_tools);

        // 输入框与发送/终止按钮
        let action_btn = if is_running {
            Button::new(SharedString::from(format!("agent-stop-{}", self.session_id)))
                .danger()
                .small()
                .icon(icon(IconName::Square))
                .label(t!("agentView.stop").to_string())
                .disabled(!enabled)
                .on_click(cx.listener(|this, _, _, cx| {
                    if this.available && this.state.active && !this.state.terminal_backed && this.state.status != AgentTurnStatus::Unknown {
                        cx.emit(AgentPaneEvent::Interrupt);
                    }
                }))
        } else {
            let composer = self.composer.clone();
            Button::new(SharedString::from(format!("agent-send-{}", self.session_id)))
                .primary()
                .small()
                .icon(icon(IconName::ArrowUp))
                .tooltip(t!("agentView.send").to_string())
                .disabled(!enabled)
                .on_click(cx.listener(move |_, _, w, cx| {
                    if let Some(text) = composer.update(cx, |c, cx| c.take_submit(w, cx)) {
                        cx.emit(AgentPaneEvent::Prompt(text));
                    }
                }))
        };

        let slash_popup = if enabled { self.render_slash_popup(&ui, cx) } else { None };
        let composer_focused = self.focus_handle(cx).is_focused(window);
        let composer_shadows = crate::ui::island_shadows(&ui);

        let composer_box = div()
            .w_full()
            .flex_none()
            .px(zpx(12.))
            .py(zpx(8.))
            .bg(ui.background)
            .flex()
            .flex_col()
            .gap(zpx(6.))
            .children(slash_popup)
            .child(status_bar)
            .child(
                div()
                    .w_full()
                    .px(zpx(11.))
                    .py(zpx(7.))
                    .when(!self.terminal_style, |d| {
                        d.rounded(radius::LG)
                            .bg(if ui.is_dark { ui.app } else { ui.card })
                            .border_1()
                            .border_color(if composer_focused { ui.ring } else { ui.border })
                            .when(!composer_shadows.is_empty(), |d| d.shadow(composer_shadows))
                    })
                    .flex()
                    .items_end()
                    .gap(zpx(8.))
                    .when(self.terminal_style, |d| d.child(div().text_color(ui.tint_strong).child("›")))
                    .child(self.composer.clone())
                    .child(action_btn),
            );

        let pending_id = self.pending_permission_call_id();
        div()
            .id(SharedString::from(format!("agent-pane-{}", self.session_id)))
            .size_full()
            .bg(ui.background)
            .flex()
            .flex_col()
            .overflow_hidden()
            .capture_key_down(cx.listener(move |_, e: &KeyDownEvent, _, cx| {
                let cmd_or_ctrl = if crate::ui::mac_keys() {
                    e.keystroke.modifiers.platform
                } else {
                    e.keystroke.modifiers.control
                };
                if enabled && cmd_or_ctrl && e.keystroke.key == "enter"
                    && let Some(call_id) = pending_id.clone()
                {
                    cx.stop_propagation();
                    cx.emit(AgentPaneEvent::Permission {
                        call_id,
                        decision: AgentPermissionDecision::Allow,
                        updated_command: None,
                    });
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(timeline_body)
                    .child(div().absolute().inset_0().child(Scrollbar::vertical(&self.scroll).mode(ScrollbarMode::Always))),
            )
            .when(!self.state.terminal_backed, |d| d.child(composer_box))
    }
}

use gpui_kit::component::scroll::{Scrollbar, ScrollbarMode};
