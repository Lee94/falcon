//! GPUI entry semantics expressed with NSH controls and semantic theme tokens.
use super::{AgentPaneEvent, EntryControls, action_button, fold_header};
use crate::{
    fonts::FONT_FAMILY_MONO,
    i18n::t,
    shell::widgets::{Glyph, icon_button},
    terminal::session::AgentCommand,
    theme::{rgb_to_hsla, rgba_to_hsla},
    zoom::zpx,
};
use falcon_core::agent_output::{
    AnsiColor, DiffKind, DiffLine, line_diff, styled_output, text_preview, unified_diff_lines,
};
use falcon_proto::{
    AgentEntry, AgentNoticeLevel, AgentPermissionDecision, AgentPlanStatus, AgentToolCall,
    AgentToolContent, AgentToolStatus,
};
use falcon_theme::ResolvedTheme;
use nsh_core::prelude::*;

pub(super) fn render_entry(
    entry: &AgentEntry,
    compact: bool,
    controls: &EntryControls,
    theme: &ResolvedTheme,
) -> AnyElement {
    let ui = &theme.ui;
    match entry {
        AgentEntry::User { id, text: body } => {
            if compact {
                return compact_text(id, "›", body, rgb_to_hsla(ui.tint_strong));
            }
            view()
                .test_id(&format!("agent-user-{id}"))
                .flex_row()
                .justify_end()
                .flex_none()
                .child(
                    view()
                        .max_w(zpx(640.0))
                        .px(zpx(14.0))
                        .py(zpx(8.0))
                        .rounded(zpx(10.0))
                        .bg(rgb_to_hsla(ui.tint))
                        .child(
                            text(body.clone())
                                .text_sm()
                                .text_color(rgb_to_hsla(ui.foreground)),
                        ),
                )
                .into_element()
        }
        AgentEntry::Message { id, text: body } => {
            if compact {
                return compact_text(id, "•", body, rgb_to_hsla(ui.foreground));
            }
            view()
                .test_id(&format!("agent-message-{id}"))
                .flex_none()
                .child(
                    nsh_base::text_view()
                        .markdown(body.clone())
                        .selectable(true),
                )
                .into_element()
        }
        AgentEntry::Thought { id, text: body } => {
            let open = controls.folded.get().contains(id);
            let preview = body
                .trim()
                .lines()
                .rfind(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim()
                .chars()
                .take(72)
                .collect::<String>();
            let title = if open {
                t!("native.agentView.thinking")
            } else {
                format!("{} · {preview}", t!("native.agentView.thinking"))
            };
            let ctrl = controls.clone();
            let key = id.clone();
            let mut col = view().flex_col().flex_none().child(fold_header(
                format!("agent-thought-{id}"),
                title,
                open,
                theme,
                move || ctrl.toggle(key.clone()),
            ));
            if open {
                col = col.child(
                    text(body.trim().to_owned())
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground))
                        .pl(zpx(12.0)),
                );
            }
            col.into_element()
        }
        AgentEntry::ToolCall(call) => render_tool(call, compact, controls, theme),
        AgentEntry::Plan { id, items } => {
            let mut col = view()
                .test_id(&format!("agent-plan-{id}"))
                .flex_col()
                .flex_none()
                .gap(zpx(4.0));
            if !compact {
                col = col.child(
                    text(t!("native.agentView.plan"))
                        .text_xs()
                        .text_color(rgb_to_hsla(ui.muted_foreground)),
                );
            }
            for item in items {
                let (marker, color) = match item.status {
                    AgentPlanStatus::Completed => ("[✓]", ui.success),
                    AgentPlanStatus::InProgress => ("[>]", ui.tint_strong),
                    _ => ("[ ]", ui.muted_foreground),
                };
                col = col.child(
                    text(format!("{marker} {}", item.content))
                        .text_xs()
                        .font_family(FONT_FAMILY_MONO)
                        .text_color(rgb_to_hsla(color)),
                );
            }
            col.into_element()
        }
        AgentEntry::Notice {
            id,
            text: body,
            level,
        } => compact_text(
            id,
            "·",
            body,
            rgb_to_hsla(match level {
                AgentNoticeLevel::Error => ui.destructive,
                AgentNoticeLevel::Warning => ui.warning,
                _ => ui.muted_foreground,
            }),
        ),
        AgentEntry::Unknown => view().into_element(),
    }
}

fn compact_text(id: &str, prefix: &str, body: &str, color: Hsla) -> AnyElement {
    view()
        .test_id(&format!("agent-text-{id}"))
        .flex_row()
        .flex_none()
        .gap(zpx(8.0))
        .text_color(color)
        .child(
            text(prefix.to_owned())
                .text_xs()
                .font_family(FONT_FAMILY_MONO),
        )
        .child(
            text(body.to_owned())
                .text_xs()
                .font_family(FONT_FAMILY_MONO)
                .flex_1()
                .min_w_0(),
        )
        .into_element()
}

fn render_tool(
    call: &AgentToolCall,
    compact: bool,
    controls: &EntryControls,
    theme: &ResolvedTheme,
) -> AnyElement {
    let ui = &theme.ui;
    // The toggle is an override: failures and approvals start open, other tools closed.
    let default_open = call.permission_pending || call.status == AgentToolStatus::Failed;
    let open = default_open != controls.folded.get().contains(&call.id);
    let color = if call.permission_pending {
        ui.warning
    } else {
        match call.status {
            AgentToolStatus::Completed => ui.success,
            AgentToolStatus::Failed => ui.destructive,
            AgentToolStatus::InProgress => ui.tint_strong,
            _ => ui.muted_foreground,
        }
    };
    let status = if call.permission_pending {
        t!("native.agentView.awaitingPermission")
    } else {
        t!(match call.status {
            AgentToolStatus::Completed => "native.agentView.toolCompleted",
            AgentToolStatus::Failed => "native.agentView.toolFailed",
            AgentToolStatus::InProgress => "native.agentView.running",
            _ => "native.agentView.toolPending",
        })
    };
    let ctrl = controls.clone();
    let id = call.id.clone();
    let mut header = view()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(zpx(6.0))
        .px(zpx(8.0))
        .child(fold_header(
            format!("agent-tool-toggle-{}", call.id),
            call.title.clone(),
            open,
            theme,
            move || ctrl.toggle(id.clone()),
        ))
        .child(text(status).text_xs().text_color(rgb_to_hsla(color)));
    if let Some(path) = &call.file_path {
        let emit = controls.dispatch.clone();
        let path = path.clone();
        header = header.child(icon_button(
            format!("agent-open-file-{}", call.id),
            t!("native.agentView.openFile"),
            Glyph::Link,
            false,
            theme,
            move || emit(AgentPaneEvent::OpenFile(path.clone())),
        ));
    }
    let mut card = view()
        .test_id(&format!("agent-tool-{}", call.id))
        .flex_col()
        .flex_none()
        .overflow_hidden();
    if !compact {
        card = card
            .rounded(zpx(6.0))
            .bg(rgb_to_hsla(ui.card))
            .border_1()
            .border_color(
                if call.permission_pending || call.status == AgentToolStatus::Failed {
                    rgb_to_hsla(color)
                } else {
                    rgba_to_hsla(ui.border)
                },
            );
    }
    card = card.child(header);
    if open {
        if let Some(description) = &call.description {
            card = card.child(
                text(description.clone())
                    .text_xs()
                    .px(zpx(8.0))
                    .text_color(rgb_to_hsla(ui.muted_foreground)),
            );
        }
        for (index, content) in call.content.iter().enumerate() {
            card = card.child(render_content(
                content,
                &format!("content:{}:{index}", call.id),
                controls,
                theme,
            ));
        }
        if call.permission_pending {
            let mut bar = view()
                .flex_col()
                .gap(zpx(6.0))
                .p(zpx(8.0))
                .bg(rgb_to_hsla(ui.muted))
                .child(
                    text(
                        call.permission_reason
                            .clone()
                            .unwrap_or_else(|| t!("native.agentView.awaitingPermission")),
                    )
                    .text_xs()
                    .text_color(rgb_to_hsla(ui.warning)),
                );
            let mut buttons = view().flex_row().flex_wrap().gap(zpx(6.0));
            for (key, decision) in [
                ("allowOnce", AgentPermissionDecision::Allow),
                ("allowSession", AgentPermissionDecision::AllowSession),
                ("deny", AgentPermissionDecision::Deny),
            ] {
                let emit = controls.dispatch.clone();
                let id = call.id.clone();
                buttons = buttons.child(action_button(
                    &format!("agent-permission-{key}-{}", call.id),
                    t!(format!("native.agentView.{key}")),
                    controls.can_approve,
                    decision == AgentPermissionDecision::Deny,
                    theme,
                    move || {
                        emit(AgentPaneEvent::Command(AgentCommand::Permission {
                            call_id: id.clone(),
                            decision,
                        }))
                    },
                ));
            }
            bar = bar.child(buttons);
            card = card.child(bar);
        }
    }
    card.into_element()
}

fn render_content(
    content: &AgentToolContent,
    id: &str,
    controls: &EntryControls,
    theme: &ResolvedTheme,
) -> AnyElement {
    let expanded = controls.folded.get().contains(id);
    let (body, has_more) = match content {
        AgentToolContent::Text { text: raw } => {
            if raw.trim().is_empty() {
                return view().into_element();
            }
            let preview = text_preview(raw, 12, 1600);
            (
                ansi_block(if expanded { raw } else { preview }, None, theme),
                preview.len() < raw.len(),
            )
        }
        AgentToolContent::Terminal {
            output, exit_code, ..
        } => {
            let preview = text_preview(output, 12, 1600);
            (
                ansi_block(if expanded { output } else { preview }, *exit_code, theme),
                preview.len() < output.len(),
            )
        }
        AgentToolContent::Diff {
            old_text,
            new_text,
            unified_diff,
            ..
        } => {
            let lines =
                if let Some(patch) = unified_diff.as_deref().filter(|patch| !patch.is_empty()) {
                    unified_diff_lines(patch)
                } else {
                    line_diff(
                        old_text.as_deref().unwrap_or_default(),
                        new_text.as_deref().unwrap_or_default(),
                    )
                };
            let preview = lines
                .iter()
                .take(12)
                .map(|line| DiffLine {
                    kind: line.kind,
                    text: text_preview(&line.text, 1, 160).to_owned(),
                })
                .collect::<Vec<_>>();
            (
                diff_block(if expanded { &lines } else { &preview }, theme),
                preview != lines,
            )
        }
        AgentToolContent::Unknown => return view().into_element(),
    };
    let mut col = view()
        .test_id(&format!("agent-content-{id}"))
        .flex_col()
        .flex_none()
        .child(body);
    if has_more {
        let ctrl = controls.clone();
        let id_owned = id.to_owned();
        col = col.child(action_button(
            &format!("agent-expand-{id}"),
            t!(if expanded {
                "native.agentView.collapseOutput"
            } else {
                "native.agentView.expandOutput"
            }),
            true,
            false,
            theme,
            move || ctrl.toggle(id_owned.clone()),
        ));
    }
    col.into_element()
}

fn ansi_block(raw: &str, exit_code: Option<i32>, theme: &ResolvedTheme) -> AnyElement {
    let parsed = styled_output(raw);
    let ui = &theme.ui;
    // Nested NSH text spans preserve selection, wrapping, Unicode and ANSI run boundaries.
    let mut output = text("")
        .text_xs()
        .font_family(FONT_FAMILY_MONO)
        .text_color(rgb_to_hsla(ui.foreground));
    let mut end = 0;
    for span in parsed.spans {
        if span.range.start > end {
            output = output.child(text(parsed.text[end..span.range.start].to_owned()));
        }
        let mut part = text(parsed.text[span.range.clone()].to_owned());
        if let Some(color) = span.fg {
            part = part.text_color(rgb_to_hsla(match color {
                AnsiColor::Red => ui.destructive,
                AnsiColor::Green => ui.success,
                AnsiColor::Yellow => ui.warning,
                AnsiColor::Blue | AnsiColor::Cyan => ui.tint_strong,
                AnsiColor::Magenta => ui.primary,
                AnsiColor::Dim => ui.muted_foreground,
            }));
        }
        if span.bold {
            part = part.font_weight(FontWeight::BOLD);
        }
        end = span.range.end;
        output = output.child(part);
    }
    if end < parsed.text.len() {
        output = output.child(text(parsed.text[end..].to_owned()));
    }
    let mut col = view()
        .flex_col()
        .flex_none()
        .p(zpx(8.0))
        .bg(rgb_to_hsla(ui.background))
        .child(output);
    if let Some(code) = exit_code.filter(|code| *code != 0) {
        col = col.child(
            text(t!("native.agentView.exitCode", code = code))
                .text_xs()
                .text_color(rgb_to_hsla(ui.destructive)),
        );
    }
    col.into_element()
}

fn diff_block(lines: &[DiffLine], theme: &ResolvedTheme) -> AnyElement {
    let ui = &theme.ui;
    let mut col = view()
        .flex_col()
        .flex_none()
        .py(zpx(4.0))
        .bg(rgb_to_hsla(ui.background));
    for line in lines {
        let (prefix, color) = match line.kind {
            DiffKind::Insert => ("+ ", ui.success),
            DiffKind::Delete => ("- ", ui.destructive),
            DiffKind::Context => ("  ", ui.muted_foreground),
        };
        col = col.child(
            text(format!("{prefix}{}", line.text))
                .text_xs()
                .font_family(FONT_FAMILY_MONO)
                .px(zpx(8.0))
                .text_color(rgb_to_hsla(color)),
        );
    }
    col.into_element()
}
