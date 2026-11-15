//! Structured Agent timeline; rich/compact share the same authoritative protocol.
mod blocks;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

use crate::{
    i18n::t,
    shell::{
        popups::{PopupAction, ServerUrlGuard, overflow_menu, themed_tooltip},
        widgets::{Glyph, icon_button},
    },
    store::{AgentItemRole, AgentSessionView, use_workspace},
    terminal::{falcon_terminal, session::AgentCommand},
    theme::{rgb_to_hsla, rgba_to_hsla, use_theme},
    zoom::zpx,
};
use falcon_core::agent_mode::{
    AgentDisplayStyle, agent_slash_matches,
};
use falcon_core::agent_output::{TimelineItem, compact_timeline};
use falcon_proto::{AgentEntry, AgentPermissionDecision, AgentTurnStatus, AgentViewState};
use falcon_theme::ResolvedTheme;
use nsh_core::{element::input, prelude::*};
use std::{collections::HashSet, rc::Rc};

#[derive(Clone, Debug)]
pub(super) enum AgentPaneEvent {
    Command(AgentCommand),
    ToggleStyle,
    OpenFile(String),
}

#[derive(Clone)]
pub(super) struct EntryControls {
    folded: Signal<HashSet<String>>,
    dispatch: Rc<dyn Fn(AgentPaneEvent)>,
    can_approve: bool,
}
impl EntryControls {
    fn toggle(&self, id: String) {
        self.folded.update(|folded| {
            if !folded.remove(&id) {
                folded.insert(id);
            }
        });
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct AgentPaneViewProps {
    pub session_id: String,
    pub focused: bool,
}

#[component]
pub fn AgentPaneView(props: AgentPaneViewProps) -> impl IntoElement {
    let store = use_workspace();
    let theme_ctrl = use_theme();
    let sid = props.session_id.clone();
    let focused = props.focused;
    let draft = signal(String::new());
    let folded = signal(HashSet::<String>::new());
    let composer_focused = signal(false);
    let composer_ref = element_ref();
    let timeline_ref = element_ref();
    let following = signal(true);
    let terminal = store.ensure_terminal_session(&sid);
    let server = ServerUrlGuard::capture(&store);
    let dispatch: Rc<dyn Fn(AgentPaneEvent)> = {
        let store = store.clone();
        let sid = sid.clone();
        let server = server.clone();
        let terminal = terminal.clone();
        Rc::new(move |event| {
            if !server.is_current() {
                return;
            }
            match event {
                AgentPaneEvent::Command(command) => {
                    terminal.send_agent_command(command);
                }
                AgentPaneEvent::ToggleStyle => store.agent_views.update(|views| {
                    if let Some(agent) = views.get_mut(&sid) {
                        agent.style = agent.style.toggle();
                    }
                }),
                AgentPaneEvent::OpenFile(path) => {
                    if let Some(session) = store
                        .sessions
                        .get()
                        .iter()
                        .find(|session| session.id == sid)
                    {
                        store.open_file_pane(&session.project_id, &path);
                    }
                }
            }
        })
    };
    {
        let store = store.clone();
        let sid = sid.clone();
        let dispatch = dispatch.clone();
        effect(move || {
            if !focused
                || !composer_focused.get()
                || store.active_dialog.get() != crate::store::ActiveDialog::None
            {
                return;
            }
            let Some(agent) = store.agent_views.get().get(&sid).cloned() else {
                return;
            };
            let Some(state) = agent.snapshot.as_ref() else {
                return;
            };
            if let Some(call_id) = state.entries.iter().rev().find_map(|entry| match entry {
                AgentEntry::ToolCall(call) if call.permission_pending => Some(call.id.clone()),
                _ => None,
            }) {
                let dispatch = dispatch.clone();
                crate::terminal::bind_safe_key(
                    if apple_keys() {
                        "cmd+enter"
                    } else {
                        "ctrl+enter"
                    },
                    false,
                    move || {
                        dispatch(AgentPaneEvent::Command(AgentCommand::Permission {
                            call_id: call_id.clone(),
                            decision: AgentPermissionDecision::Allow,
                        }));
                    },
                );
            }
        });
    }
    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let Some(agent) = store.agent_views.get().get(&sid).cloned() else {
                return falcon_terminal()
                    .session_id(sid.clone())
                    .focused(focused)
                    .tui_passthrough(store.tui_passthrough.get())
                    .into_element();
            };
            let state = display_state(&agent);
            let compact = agent.style == AgentDisplayStyle::Compact;
            let enabled = agent.available
                && state.active
                && !state.terminal_backed
                && state.status != AgentTurnStatus::Unknown;
            let running = matches!(
                state.status,
                AgentTurnStatus::Running | AgentTurnStatus::AwaitingPermission
            );
            let can_configure = enabled && !running;
            let controls = EntryControls {
                folded,
                dispatch: dispatch.clone(),
                can_approve: enabled,
            };
            let style_dispatch = dispatch.clone();
            let topbar = view()
                .flex_row()
                .items_center()
                .justify_between()
                .px(zpx(10.0))
                .py(zpx(6.0))
                .bg(rgb_to_hsla(ui.card))
                .child(
                    text(agent.agent_name.clone())
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb_to_hsla(ui.foreground)),
                )
                .child(icon_button(
                    format!("agent-style-{sid}"),
                    t!(if compact {
                        "native.agentView.toggleNative"
                    } else {
                        "native.agentView.toggleTerminal"
                    }),
                    if compact {
                        Glyph::Activity
                    } else {
                        Glyph::Terminal
                    },
                    compact,
                    &resolved,
                    move || style_dispatch(AgentPaneEvent::ToggleStyle),
                ));
            if !state.active {
                return view()
                    .size_full()
                    .flex_col()
                    .child(topbar)
                    .child(
                        falcon_terminal()
                            .session_id(sid.clone())
                            .focused(focused)
                            .tui_passthrough(store.tui_passthrough.get()),
                    )
                    .into_element();
            }
            let mut timeline = view()
                .flex_col()
                .flex_none()
                .p(zpx(if compact { 8.0 } else { 12.0 }))
                .gap(zpx(if compact { 6.0 } else { 10.0 }));
            if state.entries.is_empty() {
                timeline = timeline.child(
                    view()
                        .p(zpx(24.0))
                        .flex_col()
                        .gap(zpx(8.0))
                        .child(
                            text(t!(if state.terminal_backed {
                                "native.agentView.transcriptEmptyTitle"
                            } else {
                                "native.agentView.emptyTitle"
                            }))
                            .text_sm()
                            .text_color(rgb_to_hsla(ui.foreground)),
                        )
                        .child(
                            text(t!(if state.terminal_backed {
                                "native.agentView.transcriptEmptyHint"
                            } else {
                                "native.agentView.emptyHint"
                            }))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.muted_foreground)),
                        ),
                );
            }
            for item in compact_timeline(&state.entries) {
                match item {
                    TimelineItem::Entry(index) => {
                        timeline = timeline.child(blocks::render_entry(
                            &state.entries[index],
                            compact,
                            &controls,
                            &resolved,
                        ))
                    }
                    TimelineItem::Activity(range) => {
                        let first = state.entries[range.start].id().unwrap_or_default();
                        let id = format!("activity:{first}");
                        let open = folded.get().contains(&id);
                        let ctrl = controls.clone();
                        let mut group =
                            view()
                                .flex_col()
                                .flex_none()
                                .gap(zpx(6.0))
                                .child(fold_header(
                                    format!("agent-activity-{first}"),
                                    t!("native.agentView.activity", count = range.len()),
                                    open,
                                    &resolved,
                                    move || ctrl.toggle(id.clone()),
                                ));
                        if open {
                            for index in range {
                                group = group.child(blocks::render_entry(
                                    &state.entries[index],
                                    compact,
                                    &controls,
                                    &resolved,
                                ));
                            }
                        }
                        timeline = timeline.child(group);
                    }
                }
            }
            let scroller = scroll()
                .test_id(&format!("agent-timeline-{sid}"))
                .element_ref(&timeline_ref)
                .flex_1()
                .min_h_0()
                .on_scroll(move |event: &nsh_core::element::ScrollEvent| {
                    following.set(
                        event.content_size.height.0 - event.size.height.0 - event.offset.y.0
                            <= zpx(32.0).0,
                    );
                })
                .child(timeline.on_layout(move |_: &Bounds<Points>| {
                    if following.get_untracked() {
                        timeline_ref.scroll_to_end(false);
                    }
                }));
            let page = view()
                .size_full()
                .flex_col()
                .bg(rgb_to_hsla(ui.background))
                .child(topbar)
                .child(scroller);
            if state.terminal_backed {
                return page
                    .child(
                        view().flex_1().min_h_0().child(
                            falcon_terminal()
                                .session_id(sid.clone())
                                .focused(focused)
                                .tui_passthrough(store.tui_passthrough.get()),
                        ),
                    )
                    .into_element();
            }
            let mut footer = view()
                .flex_col()
                .gap(zpx(6.0))
                .p(zpx(10.0))
                .bg(rgb_to_hsla(ui.card));
            let draft_value = draft.get();
            let matches = agent_slash_matches(&state, &draft_value);
            if enabled && !matches.is_empty() {
                let mut popup = view()
                    .flex_col()
                    .p(zpx(4.0))
                    .rounded(zpx(6.0))
                    .bg(rgb_to_hsla(ui.popover));
                for command in matches {
                    let value = format!("/{} ", command.name);
                    popup = popup.child(
                        view()
                            .test_id(&format!("agent-slash-{}", command.name))
                            .focusable(true)
                            .cursor_pointer()
                            .px(zpx(8.0))
                            .py(zpx(5.0))
                            .hover({
                                let bg = rgb_to_hsla(ui.secondary);
                                move |style| style.bg(bg)
                            })
                            .on_press(move || {
                                draft.set(value.clone());
                                composer_ref.focus();
                            })
                            .child(
                                text(format!("/{}  {}", command.name, command.description))
                                    .text_xs()
                                    .text_color(rgb_to_hsla(ui.popover_foreground)),
                            ),
                    );
                }
                footer = footer.child(popup);
            }
            let mut settings = view().flex_row().flex_wrap().items_center().gap(zpx(6.0));
            let choices = state.model_choices();
            let selected = state.selected_model_option();
            let model_label = selected
                .map(|model| model.label.clone())
                .or_else(|| state.model.clone())
                .unwrap_or_else(|| t!("native.agentView.modelLoading"));
            let model_actions = choices
                .iter()
                .map(|model| {
                    let value = model.id.clone();
                    let emit = dispatch.clone();
                    let label = if state.model.as_deref() == Some(&model.id)
                        || selected.is_some_and(|s| s.id == model.id)
                    {
                        format!("✓ {}", model.label)
                    } else {
                        model.label.clone()
                    };
                    PopupAction::new(server.clone(), label, !can_configure, false, move || {
                        emit(AgentPaneEvent::Command(AgentCommand::SetModel(
                            value.clone(),
                        )))
                    })
                })
                .collect();
            let model_hint = if running {
                t!("native.agentView.settingsWhileRunning")
            } else if choices.is_empty() {
                t!("native.agentView.modelUnavailable")
            } else if state.model_options.is_empty() {
                t!("native.agentView.legacyCatalogHint")
            } else {
                t!("native.agentView.model")
            };
            settings = settings.child(overflow_menu(
                selector(
                    "agent-model",
                    model_label,
                    model_hint,
                    !can_configure || choices.is_empty(),
                    &resolved,
                ),
                model_actions,
                &resolved,
            ));
            let efforts = selected
                .map(|model| model.reasoning_efforts.clone())
                .unwrap_or_default();
            let default_label = selected
                .and_then(|model| model.default_reasoning_effort.as_deref())
                .map(|effort| {
                    t!(
                        "native.agentView.defaultEffort",
                        effort = effort_label(effort)
                    )
                })
                .unwrap_or_else(|| t!("native.agentView.default"));
            let effort_text = if state.model_options.is_empty() && !choices.is_empty() {
                t!("native.agentView.effortNotReported")
            } else if efforts.is_empty() {
                t!("native.agentView.effortUnavailable")
            } else {
                t!(
                    "native.agentView.reasoningValue",
                    effort = state
                        .reasoning_effort
                        .as_deref()
                        .map(effort_label)
                        .unwrap_or_else(|| default_label.clone())
                )
            };
            let effort_actions = std::iter::once(None)
                .chain(efforts.into_iter().map(Some))
                .map(|effort| {
                    let emit = dispatch.clone();
                    let label = effort
                        .as_deref()
                        .map(effort_label)
                        .unwrap_or_else(|| default_label.clone());
                    let label = if effort == state.reasoning_effort {
                        format!("✓ {label}")
                    } else {
                        label
                    };
                    PopupAction::new(
                        server.clone(),
                        label,
                        !can_configure
                            || selected.is_none_or(|model| model.reasoning_efforts.is_empty()),
                        false,
                        move || {
                            emit(AgentPaneEvent::Command(AgentCommand::SetReasoningEffort(
                                effort.clone(),
                            )))
                        },
                    )
                })
                .collect();
            settings = settings.child(overflow_menu(
                selector(
                    "agent-effort",
                    effort_text,
                    if running {
                        t!("native.agentView.settingsWhileRunning")
                    } else {
                        t!("native.agentView.reasoning")
                    },
                    !can_configure
                        || selected.is_none_or(|model| model.reasoning_efforts.is_empty()),
                    &resolved,
                ),
                effort_actions,
                &resolved,
            ));
            if running {
                settings = settings.child(
                    text(t!(if state.status == AgentTurnStatus::AwaitingPermission {
                        "native.agentView.awaitingPermission"
                    } else {
                        "native.agentView.running"
                    }))
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.tint_strong)),
                );
            }
            footer = footer.child(settings);
            let send_terminal = terminal.clone();
            let send_server = server.clone();
            let send = Rc::new(move || {
                if send_server.is_current()
                    && send_terminal
                        .send_agent_command(AgentCommand::Prompt(draft.get().trim().to_owned()))
                {
                    draft.set(String::new());
                    following.set(true);
                    timeline_ref.scroll_to_end(false);
                }
            });
            let submit = send.clone();
            let send_click = send.clone();
            let stop = dispatch.clone();
            let action = action_button(
                "agent-send-stop",
                t!(if running {
                    "native.agentView.stop"
                } else {
                    "native.agentView.send"
                }),
                enabled,
                running,
                &resolved,
                move || {
                    if running {
                        stop(AgentPaneEvent::Command(AgentCommand::Interrupt));
                    } else {
                        send_click();
                    }
                },
            );
            footer = footer.child(
                view()
                    .flex_row()
                    .items_end()
                    .gap(zpx(8.0))
                    .child(
                        input()
                            .test_id(&format!("agent-composer-{sid}"))
                            .element_ref(&composer_ref)
                            .flex_1()
                            .multiline(true)
                            .submit_behavior(nsh_core::SubmitBehavior::Submit)
                            .readonly(!enabled)
                            .px(zpx(8.0))
                            .py(zpx(6.0))
                            .rounded(zpx(6.0))
                            .bg(rgba_to_hsla(ui.input))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground))
                            .placeholder(t!(
                                "native.agentView.placeholder",
                                agent = agent.agent_name
                            ))
                            .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                            .value(draft.get())
                            .on_change(move |value: &str| draft.set(value.to_owned()))
                            .on_focus(move || composer_focused.set(true))
                            .on_blur(move || composer_focused.set(false))
                            .on_submit(move |_: &str| submit()),
                    )
                    .child(action),
            );
            page.child(footer).into_element()
        }))
}

fn selector(
    id: &str,
    label: String,
    hint: String,
    disabled: bool,
    theme: &ResolvedTheme,
) -> impl Element + Interactive {
    view()
        .test_id(id)
        .focusable(!disabled)
        .disabled(disabled)
        .px(zpx(6.0))
        .py(zpx(3.0))
        .rounded(zpx(4.0))
        .cursor_pointer()
        .tooltip(themed_tooltip(text(hint), None, theme))
        .bg(rgb_to_hsla(theme.ui.secondary))
        .child(text(label).text_xs().text_color(rgb_to_hsla(if disabled {
            theme.ui.muted_foreground
        } else {
            theme.ui.secondary_foreground
        })))
}
pub(super) fn action_button(
    id: &str,
    label: String,
    enabled: bool,
    danger: bool,
    theme: &ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .test_id(id)
        .focusable(enabled)
        .disabled(!enabled)
        .px(zpx(8.0))
        .py(zpx(5.0))
        .rounded(zpx(5.0))
        .cursor_pointer()
        .bg(rgb_to_hsla(if !enabled {
            theme.ui.muted
        } else if danger {
            theme.ui.destructive
        } else {
            theme.ui.tint
        }))
        .on_press(move || {
            if enabled {
                action();
            }
        })
        .child(text(label).text_xs().text_color(rgb_to_hsla(if !enabled {
            theme.ui.muted_foreground
        } else if danger {
            theme.ui.destructive_foreground
        } else {
            theme.ui.tint_foreground
        })))
}
pub(super) fn fold_header(
    id: String,
    title: String,
    open: bool,
    theme: &ResolvedTheme,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    view()
        .test_id(&id)
        .focusable(true)
        .flex_row()
        .items_center()
        .gap(zpx(6.0))
        .py(zpx(4.0))
        .cursor_pointer()
        .on_press(action)
        .text_color(rgb_to_hsla(theme.ui.muted_foreground))
        .child(crate::shell::widgets::glyph(if open {
            Glyph::ChevronDown
        } else {
            Glyph::ChevronRight
        }))
        .child(text(title).text_xs())
}
fn effort_label(effort: &str) -> String {
    t!(match effort {
        "off" => "native.agentView.effortOff",
        "minimal" => "native.agentView.effortMinimal",
        "low" => "native.agentView.effortLow",
        "medium" => "native.agentView.effortMedium",
        "high" => "native.agentView.effortHigh",
        "xhigh" => "native.agentView.effortXhigh",
        "max" => "native.agentView.effortMax",
        _ => return effort.to_owned(),
    })
}
fn display_state(agent: &AgentSessionView) -> AgentViewState {
    if let Some(state) = &agent.snapshot {
        return state.clone();
    }
    // Preserve the isolated demo without fabricating online capabilities or replies.
    let mut state = AgentViewState::new(falcon_proto::SessionAgent::Claude, agent.graphical);
    state.model = Some(agent.model.clone());
    state.models = agent.available_models.clone();
    state.entries = agent
        .items
        .iter()
        .map(|item| match item.role {
            AgentItemRole::User => AgentEntry::User {
                id: item.id.clone(),
                text: item.body.clone(),
            },
            AgentItemRole::Tool => AgentEntry::ToolCall(falcon_proto::AgentToolCall {
                id: item.id.clone(),
                title: item.title.clone(),
                description: None,
                file_path: None,
                kind: falcon_proto::AgentToolKind::Edit,
                status: falcon_proto::AgentToolStatus::Completed,
                content: if let Some((old, new)) = &item.diff {
                    vec![falcon_proto::AgentToolContent::Diff {
                        path: String::new(),
                        old_text: Some(old.clone()),
                        new_text: Some(new.clone()),
                        unified_diff: None,
                    }]
                } else {
                    vec![falcon_proto::AgentToolContent::Text {
                        text: item.body.clone(),
                    }]
                },
                permission_pending: false,
                permission_reason: None,
            }),
            _ => AgentEntry::Message {
                id: item.id.clone(),
                text: item.body.clone(),
            },
        })
        .collect();
    state
}
