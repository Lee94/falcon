//! 原生 Coding Agent 时间线条目的渲染组件：Markdown 回复、可折叠思考过程、工具调用卡片（含行级 Diff、
//! ANSI 着色终端输出、内联权限审批按钮）与任务清单。

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use falcon_core::agent_output::{AnsiColor, DiffKind, DiffLine, line_diff, styled_output, text_preview, unified_diff_lines};
use falcon_proto::{
    AgentEntry, AgentNoticeLevel, AgentPermissionDecision, AgentPlanItem, AgentPlanStatus,
    AgentToolCall, AgentToolContent, AgentToolKind, AgentToolStatus,
};
use gpui_kit::assets::IconName;
use gpui_kit::base::animation::ease_out_cubic;
use gpui_kit::component::Sizable;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::TextView;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, AnyView, App, Div, FontWeight, HighlightStyle,
    InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement,
    Styled, StyledText, Window, div,
};
use rust_i18n::t;

use crate::theme::{Ui, radius};
use crate::ui::{disclosure_chevron, icon, island_shadows};
use crate::zoom::zpx;

pub type OnApproveFn = Rc<dyn Fn(String, AgentPermissionDecision, &mut Window, &mut App)>;
pub type OnToggleFoldFn = Rc<dyn Fn(String, &mut Window, &mut App)>;
pub type OnOpenFileFn = Rc<dyn Fn(String, &mut Window, &mut App)>;

pub struct EntryRenderCallbacks {
    pub can_approve: bool,
    pub spinner: AnyView,
    pub on_approve: OnApproveFn,
    pub on_toggle_fold: OnToggleFoldFn,
    pub on_open_file: OnOpenFileFn,
}

/// 渲染单条 [`AgentEntry`]。
pub fn render_entry(
    idx: usize,
    entry: &AgentEntry,
    terminal_style: bool,
    folded: &HashSet<String>,
    cb: &EntryRenderCallbacks,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let ui = Ui::global(cx).clone();
    // 紧凑排版仍使用原生条目与审批控件，输入协议和图形排版一致。
    if terminal_style {
        match entry {
            AgentEntry::User { id, text } => return terminal_text(idx, id, "›", text, ui.tint_strong),
            AgentEntry::Message { id, text } => return terminal_text(idx, id, "•", text, ui.foreground),
            AgentEntry::Notice { id, text, level } => {
                let color = match level {
                    AgentNoticeLevel::Error => ui.destructive,
                    AgentNoticeLevel::Warning => ui.warning,
                    _ => ui.muted_foreground,
                };
                return terminal_text(idx, id, "·", text, color);
            }
            AgentEntry::Plan { id, items } => {
                let text = items.iter().map(|item| format!("{} {}", match item.status {
                    AgentPlanStatus::Completed => "[x]",
                    AgentPlanStatus::InProgress => "[>]",
                    _ => "[ ]",
                }, item.content)).collect::<Vec<_>>().join("\n");
                return terminal_text(idx, id, "≡", &text, ui.foreground);
            }
            _ => {}
        }
    }
    match entry {
        AgentEntry::User { id, text } => render_user(idx, id, text, &ui),
        AgentEntry::Thought { id, text } => {
            let is_open = folded.contains(id);
            render_thought(idx, id, text, is_open, &cb.on_toggle_fold, &ui)
        }
        AgentEntry::Message { id, text } => render_message(idx, id, text, &ui, window, cx),
        AgentEntry::ToolCall(tc) => {
            let default_open = tc.permission_pending || tc.status == AgentToolStatus::Failed;
            let toggled = folded.contains(&tc.id);
            let is_open = if default_open { !toggled } else { toggled };
            render_tool_call(idx, tc, is_open, terminal_style, folded, cb, &ui)
        }
        AgentEntry::Plan { id, items } => render_plan(idx, id, items, &cb.spinner, &ui),
        AgentEntry::Notice { id, level, text } => render_notice(idx, id, *level, text, &ui),
        AgentEntry::Unknown => div().into_any_element(),
    }
}

fn terminal_text(idx: usize, id: &str, prefix: &str, text: &str, color: gpui_kit::Hsla) -> AnyElement {
    div()
        .id(SharedString::from(format!("agent-terminal-{idx}-{id}")))
        .w_full()
        .flex()
        .items_start()
        .gap(zpx(8.))
        .font_family(crate::fonts::BERKELEY)
        .text_size(zpx(12.))
        .line_height(zpx(19.))
        .text_color(color)
        .child(div().flex_none().child(prefix.to_string()))
        .child(div().flex_1().min_w_0().child(text.to_string()))
        .into_any_element()
}

fn render_user(idx: usize, id: &str, text: &str, ui: &Ui) -> AnyElement {
    let shadows = island_shadows(ui);
    div()
        .id(SharedString::from(format!("agent-user-{idx}-{id}")))
        .w_full()
        .flex()
        .justify_end()
        .child(
            div()
                .max_w(zpx(640.))
                .px(zpx(14.))
                .py(zpx(8.5))
                .rounded(radius::LG)
                .bg(ui.tint)
                .border_1()
                .border_color(ui.tint_strong.opacity(0.24))
                .when(!shadows.is_empty(), |d| d.shadow(shadows))
                .text_size(zpx(13.))
                .line_height(zpx(20.))
                .text_color(ui.foreground)
                .child(text.to_string()),
        )
        .into_any_element()
}

fn render_thought(
    idx: usize,
    id: &str,
    text: &str,
    is_open: bool,
    on_toggle: &OnToggleFoldFn,
    ui: &Ui,
) -> AnyElement {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return div().into_any_element();
    }
    let last_line = trimmed
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let preview: String = last_line.chars().take(72).collect();
    let toggle = on_toggle.clone();
    let id_str = id.to_string();

    let header = div()
        .id(SharedString::from(format!("agent-thought-hdr-{idx}-{id}")))
        .flex()
        .items_center()
        .gap(zpx(6.))
        .py(zpx(2.))
        .cursor_pointer()
        .text_size(zpx(12.))
        .text_color(ui.muted_foreground)
        .hover(|s| s.text_color(ui.foreground))
        .on_click(move |_, w, cx| toggle(id_str.clone(), w, cx))
        .child(disclosure_chevron(
            SharedString::from(format!("agent-thought-chev-{idx}-{id}")),
            is_open,
            zpx(12.),
        ))
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .child(t!("agentView.thinking").to_string()),
        )
        .when(!is_open && !preview.is_empty(), |d| {
            d.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(ui.muted_foreground.opacity(0.75))
                    .child(format!("· {preview}")),
            )
        });

    let mut col = div().w_full().flex().flex_col().gap(zpx(4.)).child(header);
    if is_open {
        col = col.child(
            div()
                .ml(zpx(6.))
                .pl(zpx(10.))
                .border_l_2()
                .border_color(ui.border)
                .text_size(zpx(12.))
                .line_height(zpx(18.))
                .text_color(ui.muted_foreground)
                .child(trimmed.to_string())
                .with_animation(
                    SharedString::from(format!("agent-thought-body-{idx}-{id}")),
                    Animation::new(Duration::from_millis(140)).with_easing(ease_out_cubic),
                    |el, delta| el.opacity(delta),
                ),
        );
    }
    col.into_any_element()
}

fn render_message(
    idx: usize,
    id: &str,
    text: &str,
    ui: &Ui,
    _window: &mut Window,
    _cx: &mut App,
) -> AnyElement {
    if text.trim().is_empty() {
        return div().into_any_element();
    }
    let elem_id = SharedString::from(format!("agent-md-{idx}-{id}"));
    div()
        .w_full()
        .text_size(zpx(13.))
        .line_height(zpx(20.))
        .text_color(ui.foreground)
        .child(TextView::markdown(elem_id, text.to_string()).selectable(true))
        .into_any_element()
}

fn render_plan(idx: usize, id: &str, items: &[AgentPlanItem], spinner: &AnyView, ui: &Ui) -> AnyElement {
    if items.is_empty() {
        return div().into_any_element();
    }
    let mut list = div().flex().flex_col().gap(zpx(4.));
    for item in items {
        let indicator: AnyElement = match item.status {
            AgentPlanStatus::Completed => icon(IconName::CircleCheck).size(zpx(13.)).text_color(ui.success).into_any_element(),
            AgentPlanStatus::InProgress => spinner.clone().into_any_element(),
            _ => icon(IconName::Circle).size(zpx(13.)).text_color(ui.muted_foreground).into_any_element(),
        };
        list = list.child(
            div()
                .flex()
                .items_center()
                .gap(zpx(8.))
                .text_size(zpx(12.))
                .child(indicator)
                .child(
                    div()
                        .flex_1()
                        .text_color(if item.status == AgentPlanStatus::Completed {
                            ui.muted_foreground
                        } else {
                            ui.foreground
                        })
                        .child(item.content.clone()),
                ),
        );
    }
    div()
        .id(SharedString::from(format!("agent-plan-{idx}-{id}")))
        .w_full()
        .p(zpx(11.))
        .rounded(radius::MD)
        .border_1()
        .border_color(ui.border)
        .bg(if ui.is_dark { ui.card } else { ui.app.opacity(0.6) })
        .flex()
        .flex_col()
        .gap(zpx(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(zpx(6.))
                .text_size(zpx(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ui.foreground)
                .child(icon(IconName::ListTodo).size(zpx(13.)).text_color(ui.tint_strong))
                .child(t!("agentView.plan").to_string()),
        )
        .child(list)
        .into_any_element()
}

fn render_notice(
    idx: usize,
    id: &str,
    level: AgentNoticeLevel,
    text: &str,
    ui: &Ui,
) -> AnyElement {
    let (tone, ic) = match level {
        AgentNoticeLevel::Error => (ui.destructive, IconName::CircleX),
        AgentNoticeLevel::Warning => (ui.warning, IconName::TriangleAlert),
        _ => (ui.muted_foreground, IconName::Info),
    };
    div()
        .id(SharedString::from(format!("agent-notice-{idx}-{id}")))
        .w_full()
        .px(zpx(10.))
        .py(zpx(8.))
        .rounded(radius::MD)
        .border_1()
        .border_color(tone.opacity(0.4))
        .bg(tone.opacity(0.08))
        .flex()
        .items_start()
        .gap(zpx(8.))
        .text_size(zpx(12.))
        .text_color(tone)
        .child(icon(ic).size(zpx(14.)).mt(zpx(2.)).text_color(tone))
        .child(div().flex_1().child(text.to_string()))
        .into_any_element()
}

fn tool_kind_icon(kind: AgentToolKind) -> IconName {
    match kind {
        AgentToolKind::Execute => IconName::SquareTerminal,
        AgentToolKind::Edit | AgentToolKind::Write => IconName::FileDiff,
        AgentToolKind::Read => IconName::FileText,
        AgentToolKind::Search => IconName::Search,
        AgentToolKind::Fetch => IconName::Globe,
        AgentToolKind::Mcp => IconName::Cpu,
        _ => IconName::Wrench,
    }
}

fn render_tool_call(
    idx: usize,
    tc: &AgentToolCall,
    is_open: bool,
    terminal_style: bool,
    folded: &HashSet<String>,
    cb: &EntryRenderCallbacks,
    ui: &Ui,
) -> AnyElement {
    let status_color = if tc.permission_pending {
        ui.warning
    } else {
        match tc.status {
            AgentToolStatus::Completed => ui.success,
            AgentToolStatus::Failed => ui.destructive,
            AgentToolStatus::InProgress => ui.tint_strong,
            _ => ui.muted_foreground,
        }
    };
    let status_icon = if tc.permission_pending {
        IconName::ShieldAlert
    } else {
        match tc.status {
            AgentToolStatus::Completed => IconName::Check,
            AgentToolStatus::Failed => IconName::X,
            AgentToolStatus::InProgress => IconName::Loader,
            _ => IconName::Circle,
        }
    };

    let has_body = !tc.content.is_empty() || tc.permission_pending;
    let toggle = cb.on_toggle_fold.clone();
    let call_id = tc.id.clone();

    let mut header_left = div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(zpx(6.))
        .child(
            icon(tool_kind_icon(tc.kind))
                .size(zpx(13.))
                .text_color(ui.muted_foreground),
        )
        .child(
            div()
                .font_family(crate::fonts::BERKELEY)
                .text_size(zpx(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ui.foreground)
                .truncate()
                .child(tc.title.clone()),
        );

    if let Some(desc) = tc.description.as_deref().filter(|s| !s.is_empty()) {
        header_left = header_left.child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(zpx(11.))
                .text_color(ui.muted_foreground)
                .child(format!("— {desc}")),
        );
    }

    let mut header_right = div().flex().items_center().gap(zpx(6.));
    if let Some(fp) = tc.file_path.clone() {
        let on_open = cb.on_open_file.clone();
        let btn_id = SharedString::from(format!("agent-open-file-{idx}-{}", tc.id));
        header_right = header_right.child(
            Button::new(btn_id)
                .ghost()
                .xsmall()
                .icon(icon(IconName::ExternalLink))
                .tooltip(t!("agentView.openFile").to_string())
                .on_click(move |_, w, cx| {
                    cx.stop_propagation();
                    on_open(fp.clone(), w, cx);
                }),
        );
    }
    let status_el: AnyElement = if !tc.permission_pending && tc.status == AgentToolStatus::InProgress {
        cb.spinner.clone().into_any_element()
    } else {
        icon(status_icon).size(zpx(13.)).text_color(status_color).into_any_element()
    };
    header_right = header_right.child(status_el);
    if has_body {
        header_right = header_right.child(
            disclosure_chevron(
                SharedString::from(format!("agent-tc-chev-{idx}-{}", tc.id)),
                is_open,
                zpx(12.),
            )
            .text_color(ui.muted_foreground),
        );
    }

    let header = div()
        .id(SharedString::from(format!("agent-tc-hdr-{idx}-{}", tc.id)))
        .w_full()
        .px(zpx(10.))
        .py(zpx(6.5))
        .flex()
        .items_center()
        .justify_between()
        .gap(zpx(8.))
        .when(has_body, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(ui.secondary.opacity(0.6)))
                .on_click(move |_, w, cx| toggle(call_id.clone(), w, cx))
        })
        .child(header_left)
        .child(header_right);

    let border_col = if tc.permission_pending {
        ui.warning.opacity(0.65)
    } else if tc.status == AgentToolStatus::Failed {
        ui.destructive.opacity(0.45)
    } else {
        ui.border
    };

    let card_bg = if ui.is_dark { ui.card } else { ui.app.opacity(0.55) };
    let mut card = div()
        .id(SharedString::from(format!("agent-tc-{idx}-{}", tc.id)))
        .w_full()
        .when(!terminal_style, |d| d.rounded(radius::MD).border_1().border_color(border_col).bg(card_bg))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(header);

    if is_open && has_body {
        let mut body = div()
            .w_full()
            .when(!terminal_style, |d| d.border_t_1().border_color(ui.border))
            .flex()
            .flex_col();

        for (content_idx, content) in tc.content.iter().enumerate() {
            let content_id = format!("content:{}:{content_idx}", tc.id);
            body = body.child(render_tool_content(content, &content_id, folded.contains(&content_id), &cb.on_toggle_fold, ui));
        }

        if tc.permission_pending {
            body = body.child(render_permission_bar(idx, tc, cb.can_approve, &cb.on_approve, ui));
        }

        let body: AnyElement = if folded.contains(&tc.id) {
            body.with_animation(
                SharedString::from(format!("agent-tc-body-{idx}-{}", tc.id)),
                Animation::new(Duration::from_millis(140)).with_easing(ease_out_cubic),
                |el, delta| el.opacity(delta),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        };
        card = card.child(body);
    }

    card.into_any_element()
}

fn render_permission_bar(
    idx: usize,
    tc: &AgentToolCall,
    enabled: bool,
    on_approve: &OnApproveFn,
    ui: &Ui,
) -> Div {
    let reason = tc
        .permission_reason
        .clone()
        .unwrap_or_else(|| t!("agentView.awaitingPermission").to_string());
    let (id1, id2, id3) = (tc.id.clone(), tc.id.clone(), tc.id.clone());
    let (cb1, cb2, cb3) = (on_approve.clone(), on_approve.clone(), on_approve.clone());

    div()
        .w_full()
        .px(zpx(10.))
        .py(zpx(8.))
        .bg(ui.warning.opacity(0.1))
        .border_t_1()
        .border_color(ui.warning.opacity(0.35))
        .flex()
        .items_center()
        .justify_between()
        .gap(zpx(8.))
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(zpx(6.))
                .text_size(zpx(12.))
                .text_color(ui.warning)
                .child(icon(IconName::ShieldAlert).size(zpx(14.)).text_color(ui.warning))
                .child(div().flex_1().truncate().child(reason)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(zpx(6.))
                .child(
                    Button::new(SharedString::from(format!("agent-perm-allow-{idx}-{}", tc.id)))
                        .primary()
                        .small()
                        .label(t!("agentView.allowOnce").to_string())
                        .disabled(!enabled)
                        .on_click(move |_, w, cx| {
                            cb1(id1.clone(), AgentPermissionDecision::Allow, w, cx);
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("agent-perm-session-{idx}-{}", tc.id)))
                        .outline()
                        .small()
                        .label(t!("agentView.allowSession").to_string())
                        .disabled(!enabled)
                        .on_click(move |_, w, cx| {
                            cb2(id2.clone(), AgentPermissionDecision::AllowSession, w, cx);
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("agent-perm-deny-{idx}-{}", tc.id)))
                        .danger()
                        .small()
                        .label(t!("agentView.deny").to_string())
                        .disabled(!enabled)
                        .on_click(move |_, w, cx| {
                            cb3(id3.clone(), AgentPermissionDecision::Deny, w, cx);
                        }),
                ),
        )
}

fn render_tool_content(
    content: &AgentToolContent,
    id: &str,
    expanded: bool,
    on_toggle: &OnToggleFoldFn,
    ui: &Ui,
) -> AnyElement {
    let (body, has_more) = match content {
        AgentToolContent::Text { text } => {
            if text.trim().is_empty() {
                return div().into_any_element();
            }
            let preview = text_preview(text, 12, 1600);
            (render_ansi_block(if expanded { text } else { preview }, None, ui), preview.len() < text.len())
        }
        AgentToolContent::Terminal { output, exit_code, .. } => {
            if output.trim().is_empty() && exit_code.is_none() {
                return div().into_any_element();
            }
            let preview = text_preview(output, 12, 1600);
            (render_ansi_block(if expanded { output } else { preview }, *exit_code, ui), preview.len() < output.len())
        }
        AgentToolContent::Diff { old_text, new_text, unified_diff, .. } => {
            let lines = if let Some(patch) = unified_diff.as_deref().filter(|s| !s.is_empty()) {
                unified_diff_lines(patch)
            } else {
                line_diff(old_text.as_deref().unwrap_or_default(), new_text.as_deref().unwrap_or_default())
            };
            // 不再静默丢弃第 160 行之后的差异；完整内容通过展开操作可达。
            let preview: Vec<DiffLine> = lines.iter().take(12).map(|line| DiffLine {
                kind: line.kind,
                text: text_preview(&line.text, 1, 160).to_string(),
            }).collect();
            let has_more = preview != lines;
            (render_diff_lines(if expanded { &lines } else { &preview }, ui), has_more)
        }
        AgentToolContent::Unknown => return div().into_any_element(),
    };
    let mut container = div().w_full().flex_none().flex().flex_col().child(body);
    if has_more {
        let toggle = on_toggle.clone();
        let fold_id = id.to_string();
        container = container.child(
            div().id(SharedString::from(format!("agent-expand-{id}")))
                .flex_none().px(zpx(8.)).py(zpx(5.)).cursor_pointer()
                .text_size(zpx(11.)).text_color(ui.muted_foreground)
                .hover(|s| s.text_color(ui.foreground))
                .on_click(move |_, w, cx| toggle(fold_id.clone(), w, cx))
                .child(t!(if expanded { "agentView.collapseOutput" } else { "agentView.expandOutput" }).to_string()),
        );
    }
    container.into_any_element()
}

fn render_ansi_block(raw: &str, exit_code: Option<i32>, ui: &Ui) -> AnyElement {
    let parsed = styled_output(raw);
    let mut highlights = Vec::with_capacity(parsed.spans.len());
    for span in parsed.spans {
        let color = span.fg.map(|c| match c {
            AnsiColor::Red => ui.destructive,
            AnsiColor::Green => ui.success,
            AnsiColor::Yellow => ui.warning,
            AnsiColor::Blue | AnsiColor::Cyan => ui.tint_strong,
            AnsiColor::Magenta => ui.primary,
            AnsiColor::Dim => ui.muted_foreground,
        });
        let style = HighlightStyle {
            color,
            font_weight: span.bold.then_some(FontWeight::BOLD),
            ..Default::default()
        };
        highlights.push((span.range, style));
    }

    let mut box_div = div()
        .w_full()
        .p(zpx(8.))
        .bg(ui.background.opacity(0.65))
        .font_family(crate::fonts::BERKELEY)
        .text_size(zpx(11.5))
        .line_height(zpx(17.))
        .text_color(ui.foreground)
        .flex()
        .flex_col()
        .gap(zpx(4.));

    if !parsed.text.is_empty() {
        let styled = StyledText::new(parsed.text).with_highlights(highlights);
        box_div = box_div.child(styled);
    }
    if let Some(code) = exit_code.filter(|&c| c != 0) {
        box_div = box_div.child(
            div()
                .text_size(zpx(11.))
                .text_color(ui.destructive)
                .child(t!("agentView.exitCode", code = code).to_string()),
        );
    }
    box_div.into_any_element()
}

fn render_diff_lines(lines: &[DiffLine], ui: &Ui) -> AnyElement {
    if lines.is_empty() {
        return div().into_any_element();
    }
    let mut container = div()
        .w_full()
        .py(zpx(4.))
        .bg(ui.background.opacity(0.65))
        .font_family(crate::fonts::BERKELEY)
        .text_size(zpx(11.5))
        .line_height(zpx(17.))
        .flex()
        .flex_col();

    for line in lines {
        let (prefix, fg, bg) = match line.kind {
            DiffKind::Insert => ("+ ", ui.success, Some(ui.success.opacity(0.12))),
            DiffKind::Delete => ("- ", ui.destructive, Some(ui.destructive.opacity(0.12))),
            DiffKind::Context => ("  ", ui.muted_foreground, None),
        };
        container = container.child(
            div()
                .w_full()
                .px(zpx(8.))
                .when_some(bg, |d, b| d.bg(b))
                .text_color(fg)
                .child(format!("{prefix}{}", line.text)),
        );
    }
    container.into_any_element()
}
