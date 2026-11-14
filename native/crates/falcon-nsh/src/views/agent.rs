//! Coding Agent 结构化图形界面与 `TUI ↔ GUI` 双向模式切换窗口（ADR 0013 + ADR 0027）。
//!
//! - 复用 `falcon_core::agent_mode::{AgentDisplayStyle, AgentModeTransition}` 状态机；
//! - 支持 `图形`（`Rich`）与 `紧凑`（`Compact`）两种本地排版切换；
//! - 支持在可交互 GUI 与底层完整 PTY（[`FalconTerminal`]）之间即时无损切换；
//! - 内建模型选择、思考程度切换、工具 Diff 折叠卡片与权限审批条。

use falcon_core::agent_mode::AgentDisplayStyle;
use nsh_core::element::input;
use nsh_core::prelude::*;

use crate::fonts::FONT_FAMILY_MONO;
use crate::i18n::t;
use crate::store::{AgentItemRole, AgentTimelineItem, use_workspace};
use crate::terminal::falcon_terminal;
use crate::theme::{rgb_to_hsla, rgba_to_hsla, use_theme};
use crate::zoom::zpx;

/// Agent 会话窗口属性。
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
    let prompt_draft = signal(String::new());

    view()
        .size_full()
        .flex_col()
        .overflow_hidden()
        .child(dynamic(move || {
            let resolved = theme_ctrl.resolved();
            let ui = &resolved.ui;
            let views = store.agent_views.get();
            let Some(ag) = views.get(&sid).cloned() else {
                return falcon_terminal()
                    .session_id(sid.clone())
                    .focused(focused)
                    .tui_passthrough(store.tui_passthrough.get())
                    .into_element();
            };

            let compact = ag.style == AgentDisplayStyle::Compact;

            // 顶部模式控制条：Agent 标识 + 模型/思考程度 + 图形/紧凑切换 + TUI/GUI 双向切换
            let topbar = {
                let st_style = store.clone();
                let sid_style = sid.clone();
                let st_mode = store.clone();
                let sid_mode = sid.clone();
                let st_model = store.clone();
                let sid_model = sid.clone();
                let cur_graphical = ag.graphical;
                let models = ag.available_models.clone();
                let cur_model = ag.model.clone();

                view()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .px(zpx(10.0))
                    .py(zpx(6.0))
                    .border_b_1()
                    .border_color(rgba_to_hsla(ui.border))
                    .bg(rgb_to_hsla(ui.card))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.0))
                            .child(
                                text(ag.agent_name.clone())
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb_to_hsla(ui.foreground)),
                            )
                            .child(
                                view()
                                    .px(zpx(6.0))
                                    .py(zpx(2.0))
                                    .rounded(zpx(4.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.secondary))
                                    .on_press(move || {
                                        if !models.is_empty() {
                                            let pos = models
                                                .iter()
                                                .position(|m| m == &cur_model)
                                                .unwrap_or(0);
                                            let next = models[(pos + 1) % models.len()].clone();
                                            st_model.agent_views.update(|m| {
                                                if let Some(entry) = m.get_mut(&sid_model) {
                                                    entry.model = next;
                                                }
                                            });
                                        }
                                    })
                                    .child(
                                        text(ag.model.clone())
                                            .text_xs()
                                            .font_family(FONT_FAMILY_MONO)
                                            .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                    ),
                            )
                            .child(
                                text(t!(
                                    "agentView.reasoningValue",
                                    effort = ag.reasoning_effort.as_str()
                                ))
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.muted_foreground)),
                            ),
                    )
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap(zpx(6.0))
                            .when(cur_graphical, |el| {
                                el.child(
                                    view()
                                        .px(zpx(7.0))
                                        .py(zpx(2.5))
                                        .rounded(zpx(4.0))
                                        .cursor_pointer()
                                        .bg(rgb_to_hsla(ui.secondary))
                                        .on_press(move || {
                                            st_style.agent_views.update(|m| {
                                                if let Some(entry) = m.get_mut(&sid_style) {
                                                    entry.style = entry.style.toggle();
                                                }
                                            });
                                        })
                                        .child(
                                            text(if compact {
                                                t!("agentView.styleTerminal")
                                            } else {
                                                t!("agentView.styleGui")
                                            })
                                            .text_xs()
                                            .text_color(rgb_to_hsla(ui.secondary_foreground)),
                                        ),
                                )
                            })
                            .child(
                                view()
                                    .px(zpx(7.0))
                                    .py(zpx(2.5))
                                    .rounded(zpx(4.0))
                                    .cursor_pointer()
                                    .bg(rgb_to_hsla(ui.tint))
                                    .on_press(move || {
                                        st_mode.agent_views.update(|m| {
                                            if let Some(entry) = m.get_mut(&sid_mode) {
                                                let next_gui = !entry.graphical;
                                                entry.transition = entry
                                                    .transition
                                                    .start(next_gui)
                                                    .snapshot(next_gui, true)
                                                    .finish(true);
                                                entry.graphical = next_gui;
                                            }
                                        });
                                    })
                                    .child(
                                        text(if cur_graphical {
                                            t!("agentView.modeTerminal")
                                        } else {
                                            t!("native.nsh.agentGuiBadge")
                                        })
                                        .text_xs()
                                        .text_color(rgb_to_hsla(ui.tint_foreground)),
                                    ),
                            ),
                    )
            };

            // 若切换到底层终端控制模式，直接嵌入统一 `FalconTerminal`
            if !ag.graphical {
                return view()
                    .size_full()
                    .flex_col()
                    .child(topbar)
                    .child(
                        view().flex_1().child(
                            falcon_terminal()
                                .session_id(sid.clone())
                                .focused(focused)
                                .tui_passthrough(store.tui_passthrough.get()),
                        ),
                    )
                    .into_element();
            }

            let timeline = view()
                .flex_col()
                .p(zpx(if compact { 8.0 } else { 12.0 }))
                .gap(zpx(if compact { 6.0 } else { 10.0 }))
                .children(ag.items.iter().map(|item| {
                    let role_color = match item.role {
                        AgentItemRole::User => rgb_to_hsla(ui.tint_strong),
                        AgentItemRole::Assistant => rgb_to_hsla(ui.foreground),
                        AgentItemRole::Tool => rgb_to_hsla(ui.muted_foreground),
                        AgentItemRole::Permission => rgb_to_hsla(ui.warning),
                    };

                    let mut card = view()
                        .flex_col()
                        .gap(zpx(4.0))
                        .p(zpx(if compact { 6.0 } else { 10.0 }))
                        .rounded(zpx(if compact { 4.0 } else { 8.0 }))
                        .bg(rgb_to_hsla(ui.card))
                        .border_1()
                        .border_color(rgba_to_hsla(ui.border))
                        .child(
                            text(item.title.clone())
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(role_color),
                        )
                        .child(
                            text(item.body.clone())
                                .text_xs()
                                .font_family(if compact { FONT_FAMILY_MONO } else { "" })
                                .text_color(rgb_to_hsla(ui.foreground)),
                        );

                    if let Some((old_s, new_s)) = &item.diff {
                        let mut diff_box = view()
                            .flex_col()
                            .p(zpx(6.0))
                            .rounded(zpx(5.0))
                            .bg(rgb_to_hsla(ui.muted));
                        for l in old_s.lines() {
                            diff_box = diff_box.child(
                                text(format!("- {l}"))
                                    .text_xs()
                                    .font_family(FONT_FAMILY_MONO)
                                    .text_color(rgb_to_hsla(ui.destructive)),
                            );
                        }
                        for l in new_s.lines() {
                            diff_box = diff_box.child(
                                text(format!("+ {l}"))
                                    .text_xs()
                                    .font_family(FONT_FAMILY_MONO)
                                    .text_color(rgb_to_hsla(ui.success)),
                            );
                        }
                        card = card.child(diff_box);
                    }
                    card
                }));

            let mut footer = view()
                .flex_col()
                .gap(zpx(6.0))
                .p(zpx(10.0))
                .border_t_1()
                .border_color(rgba_to_hsla(ui.border))
                .bg(rgb_to_hsla(ui.card));

            if let Some(perm_prompt) = ag.awaiting_permission.clone() {
                let st_ok = store.clone();
                let sid_ok = sid.clone();
                let st_no = store.clone();
                let sid_no = sid.clone();
                footer = footer.child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .p(zpx(8.0))
                        .rounded(zpx(6.0))
                        .bg(rgb_to_hsla(ui.muted))
                        .child(
                            text(perm_prompt)
                                .text_xs()
                                .text_color(rgb_to_hsla(ui.warning)),
                        )
                        .child(
                            view()
                                .flex_row()
                                .gap(zpx(6.0))
                                .child(
                                    view()
                                        .px(zpx(8.0))
                                        .py(zpx(3.0))
                                        .rounded(zpx(5.0))
                                        .cursor_pointer()
                                        .bg(rgb_to_hsla(ui.success))
                                        .on_press(move || {
                                            st_ok.agent_views.update(|m| {
                                                if let Some(e) = m.get_mut(&sid_ok) {
                                                    e.awaiting_permission = None;
                                                }
                                            });
                                        })
                                        .child(
                                            text(t!("native.nsh.permissionApprove"))
                                                .text_xs()
                                                .text_color(rgb_to_hsla(ui.success_foreground)),
                                        ),
                                )
                                .child(
                                    view()
                                        .px(zpx(8.0))
                                        .py(zpx(3.0))
                                        .rounded(zpx(5.0))
                                        .cursor_pointer()
                                        .bg(rgb_to_hsla(ui.destructive))
                                        .on_press(move || {
                                            st_no.agent_views.update(|m| {
                                                if let Some(e) = m.get_mut(&sid_no) {
                                                    e.awaiting_permission = None;
                                                }
                                            });
                                        })
                                        .child(
                                            text(t!("native.nsh.permissionDeny"))
                                                .text_xs()
                                                .text_color(rgb_to_hsla(ui.destructive_foreground)),
                                        ),
                                ),
                        ),
                );
            }

            let st_send = store.clone();
            let sid_send = sid.clone();
            let agent_label = ag.agent_name.clone();
            footer = footer.child(
                view()
                    .flex_row()
                    .items_center()
                    .gap(zpx(8.0))
                    .child(
                        input()
                            .flex_1()
                            .px(zpx(8.0))
                            .py(zpx(6.0))
                            .rounded(zpx(6.0))
                            .bg(rgba_to_hsla(ui.input))
                            .text_xs()
                            .text_color(rgb_to_hsla(ui.foreground))
                            .placeholder(t!(
                                "agentView.placeholder",
                                agent = agent_label.as_str()
                            ))
                            .placeholder_color(rgb_to_hsla(ui.muted_foreground))
                            .value(prompt_draft.get())
                            .on_change(move |v: &str| prompt_draft.set(v.to_string())),
                    )
                    .child(
                        view()
                            .px(zpx(10.0))
                            .py(zpx(6.0))
                            .rounded(zpx(6.0))
                            .cursor_pointer()
                            .bg(rgb_to_hsla(ui.tint_strong))
                            .on_press(move || {
                                let msg = prompt_draft.get();
                                if !msg.trim().is_empty() {
                                    st_send.agent_views.update(|m| {
                                        if let Some(entry) = m.get_mut(&sid_send) {
                                            let next_id = format!("item-{}", entry.items.len() + 1);
                                            entry.items.push(AgentTimelineItem {
                                                id: next_id,
                                                role: AgentItemRole::User,
                                                title: "User".into(),
                                                body: msg.trim().to_string(),
                                                diff: None,
                                            });
                                        }
                                    });
                                    prompt_draft.set(String::new());
                                }
                            })
                            .child(
                                text(t!("agentView.send"))
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb_to_hsla(ui.primary_foreground)),
                            ),
                    ),
            );

            view()
                .size_full()
                .flex_col()
                .bg(rgb_to_hsla(ui.background))
                .child(topbar)
                .child(scroll().flex_1().child(timeline))
                .child(footer)
                .into_element()
        }))
}
