//! 时间线上各种条目的画法（docs/design/ai-assistant.md 2.2）。纯画法：状态（哪块展开了、在改哪条
//! 命令、按钮按下去做什么）由调用方传进来。
//!
//! 时间线要像一个终端：命令块与你对 agent 说的话都画成「提示符 + 一行」，字体字号与底下的实时
//! 终端一样、左边对齐终端网格的留白（`element.rs` 的 `PADDING_X`）；AI 的一轮（思考、回复、
//! 每一步的说明与卡片）收在左边一道竖线里，和终端内容分得开。
//!
//! - 命令块：输出是服务端存的原始字节，这里按 SGR 上色画成静态文字（`styled_output`），不为每块
//!   开一个终端模拟器；颜色取主题的终端调色板，与终端画面一致。超过 [`FOLD_LINES`] 行时默认只露
//!   最后几行，点开看全部（最多 [`MAX_LINES`] 行）；
//! - 思考直接画出来（淡色），长了折起：用户要看得见 AI 为什么这么做；
//! - 工具卡片：头一行是状态（等批准时是一句问话 + 按钮），下面是命令 / 路径 / diff。内置 agent 的
//!   命令输出不在卡片上，在卡片后面的命令块里（与终端里看到的一样）。

use std::rc::Rc;

use falcon_core::assistant::{DiffLine, StyledSpan, TermColor, line_diff, styled_output, tilde};
use falcon_proto::assistant::{NoticeLevel, PermissionKind, PlanItem, ToolCall, ToolContent, ToolKind, ToolStatus};
use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Div, Entity, FontStyle, FontWeight, HighlightStyle, Hsla, IntoElement, Pixels, SharedString,
    StyledText, UnderlineStyle, Window, div, px,
};
use rust_i18n::t;

use crate::terminal::element::{PADDING_X, TermPalette, indexed_color, rgb_color};
use crate::theme::{TerminalLook, Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

/// 命令块的输出超过这么多行就折起来，只露最后几行
pub const FOLD_LINES: usize = 20;
/// 展开时最多画这么多行（服务端已经掐过头尾，这里再兜一道）
const MAX_LINES: usize = 600;
/// diff 最多画多少行
const DIFF_MAX: usize = 60;
/// 思考超过这么多字就折起来，只露开头
const THOUGHT_FOLD_CHARS: usize = 600;

pub type Click = Rc<dyn Fn(&mut Window, &mut App)>;

fn mono(cx: &App) -> SharedString {
    TerminalLook::global(cx).text.font.family.clone()
}

/// 终端的字号：命令块、提示符行、Agent 输入与实时终端一样大
pub fn term_font_size(cx: &App) -> Pixels {
    TerminalLook::global(cx).text.font_size
}

/// 左边对齐终端网格（实时终端在元素里有这么宽的留白）
fn gutter() -> Pixels {
    px(PADDING_X)
}

/// 终端颜色 → 主题里的颜色
fn color(palette: &TermPalette, c: TermColor) -> Hsla {
    match c {
        TermColor::Indexed(i) => indexed_color(palette, i),
        TermColor::Rgb(r, g, b) => rgb_color(r, g, b),
    }
}

/// 带样式的行拼成一段 StyledText（一个元素，长输出也不会生出几百个子元素）
pub fn styled_lines(lines: &[Vec<StyledSpan>], cx: &App) -> StyledText {
    let palette = &TerminalLook::global(cx).palette;
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        for span in line {
            let start = text.len();
            text.push_str(&span.text);
            let style = HighlightStyle {
                color: span.fg.map(|c| color(palette, c)),
                background_color: span.bg.map(|c| color(palette, c)),
                font_weight: span.bold.then_some(FontWeight::BOLD),
                font_style: span.italic.then_some(FontStyle::Italic),
                underline: span.underline.then(|| UnderlineStyle { thickness: px(1.), ..Default::default() }),
                fade_out: span.dim.then_some(0.4),
                ..Default::default()
            };
            if style != HighlightStyle::default() {
                highlights.push((start..text.len(), style));
            }
        }
    }
    StyledText::new(text).with_highlights(highlights)
}

/// 提示符那一格：当时的目录（`~` 缩写），没有就一个 `$`
fn prompt_mark(cwd: Option<&str>, home: Option<&str>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    div().flex_none().text_color(ui.tint_strong).child(cwd.map_or_else(|| "$".to_string(), |c| tilde(c, home)))
}

/// 终端里跑完的一条命令（你敲的，或 agent 打进去的）：`目录 命令` + 输出，画得和终端里一样
#[allow(clippy::too_many_arguments)]
pub fn command_block(
    id: &str,
    command: &str,
    cwd: Option<&str>,
    home: Option<&str>,
    output: &str,
    exit_code: Option<i32>,
    expanded: bool,
    on_toggle: Click,
    cx: &App,
) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let all = styled_output(output, MAX_LINES);
    let hidden = if expanded { 0 } else { all.len().saturating_sub(FOLD_LINES) };
    let shown = &all[hidden..];
    let failed = exit_code.is_some_and(|c| c != 0);
    let mut block = div()
        .flex()
        .flex_col()
        .pl(gutter())
        .font_family(mono(cx))
        .text_size(term_font_size(cx))
        .child(
            div()
                .flex()
                .items_start()
                .gap_2()
                .child(prompt_mark(cwd, home, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_normal()
                        .child(command.to_string()),
                )
                .children(failed.then(|| {
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(ui.destructive)
                        .child(t!("assistant.exitCode", code = exit_code.unwrap_or_default()).to_string())
                })),
        );
    if all.len() > FOLD_LINES {
        let label = if expanded {
            t!("assistant.foldOutput").to_string()
        } else {
            t!("assistant.foldedBefore", n = hidden).to_string()
        };
        block = block.child(
            div()
                .id(SharedString::from(format!("fold-{id}")))
                .text_xs()
                .text_color(ui.muted_foreground)
                .cursor_pointer()
                .hover(|s| s.text_color(ui.foreground))
                .on_click(move |_, window, cx| on_toggle(window, cx))
                .child(label),
        );
    }
    if !shown.is_empty() {
        block = block.child(div().text_color(TerminalLook::global(cx).palette.foreground).child(styled_lines(shown, cx)));
    }
    block.into_any_element()
}

/// 你对 agent 说的话：画成命令行上敲的一行，`目录 ✦ 这句话`
pub fn user_prompt(text: &str, cwd: Option<&str>, home: Option<&str>, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let size = term_font_size(cx);
    div()
        .flex()
        .items_start()
        .gap_2()
        .pl(gutter())
        .font_family(mono(cx))
        .text_size(size)
        .child(prompt_mark(cwd, home, cx))
        .child(div().flex_none().h(size * 1.3).flex().items_center().child(icon(IconName::Sparkles).size(size * 0.85).text_color(ui.tint_strong)))
        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).whitespace_normal().child(text.to_string()))
        .into_any_element()
}

/// AI 的一轮：左边一道竖线，右边一列（思考、回复、每一步）
pub fn ai_group(items: Vec<AnyElement>, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    div()
        .flex()
        .pl(gutter())
        .gap_3()
        .child(div().flex_none().w(zpx(2.)).rounded_full().bg(ui.border))
        .child(div().flex_1().min_w_0().flex().flex_col().gap_2().py_1().children(items))
        .into_any_element()
}

/// 一轮里打头的那一项前面的 ✦（像在说「AI：」）
fn lead_mark(lead: bool, cx: &App) -> Option<AnyElement> {
    let ui = Ui::global(cx);
    lead.then(|| {
        div()
            .flex_none()
            .h(zpx(20.))
            .flex()
            .items_center()
            .child(icon(IconName::Sparkles).size(zpx(14.)).text_color(ui.tint_strong))
            .into_any_element()
    })
}

/// AI 的话（markdown，可选中，链接用系统浏览器开）
pub fn agent_text(id: &str, text: &str, lead: bool, cx: &App) -> Option<AnyElement> {
    if text.trim().is_empty() {
        return None;
    }
    let style = TextViewStyle {
        highlight_theme: gpui_kit::component::theme::Theme::global(cx).highlight_theme.clone(),
        ..Default::default()
    };
    Some(
        div()
            .flex()
            .gap_2()
            .children(lead_mark(lead, cx))
            .child(
                div().flex_1().min_w_0().text_size(zpx(13.)).child(
                    TextView::markdown(SharedString::from(format!("a-{id}")), text.to_string())
                        .style(style)
                        .selectable(true)
                        .on_link_click(|href, _, _, cx| cx.open_url(href)),
                ),
            )
            .into_any_element(),
    )
}

/// 思考：直接画出来（淡色），太长时只露开头，点开看全部
pub fn thought(id: &str, text: &str, lead: bool, open: bool, on_toggle: Click, cx: &App) -> Option<AnyElement> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let ui = Ui::global(cx).clone();
    let long = text.chars().count() > THOUGHT_FOLD_CHARS;
    let shown = if long && !open {
        format!("{}…", text.chars().take(THOUGHT_FOLD_CHARS).collect::<String>())
    } else {
        text.to_string()
    };
    Some(
        div()
            .flex()
            .gap_2()
            .children(lead_mark(lead, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_size(zpx(13.))
                    .text_color(ui.muted_foreground)
                    .child(div().whitespace_normal().child(shown))
                    .when(long, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("t-{id}")))
                                .text_xs()
                                .cursor_pointer()
                                .hover(|s| s.text_color(ui.foreground))
                                .on_click(move |_, window, cx| on_toggle(window, cx))
                                .child(if open { t!("assistant.foldOutput") } else { t!("assistant.expandThought") }.to_string()),
                        )
                    }),
            )
            .into_any_element(),
    )
}

pub fn notice(text: &str, level: NoticeLevel, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let color = match level {
        NoticeLevel::Error => ui.destructive,
        NoticeLevel::Warning => ui.warning,
        _ => ui.muted_foreground,
    };
    div().text_xs().text_color(color).whitespace_normal().child(text.to_string()).into_any_element()
}

pub fn plan(items: &[PlanItem], cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .children(items.iter().map(|it| {
            let done = it.status == "completed";
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(if done { ui.muted_foreground } else { ui.foreground })
                .child(icon(if done { IconName::CircleCheck } else { IconName::Circle }).size(zpx(12.)))
                .child(it.content.clone())
        }))
        .into_any_element()
}

/// 卡片按钮按下去做什么
pub struct CardActions {
    /// (卡片 id, 选项 id, 改过的命令)
    pub answer: Rc<dyn Fn(String, String, Option<String>, &mut App)>,
    /// 「修改」：卡片上的命令变成输入框（只给内置 agent 的单行命令）
    pub modify: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    pub cancel_edit: Click,
}

/// 卡片在改命令时：输入框（读里面的字）与画它的小视图
pub struct CardEdit<'a> {
    pub input: &'a Entity<TextareaState>,
    pub view: &'a Entity<InputView>,
}

/// 一步：上面一行「这一步要做什么」，下面是卡片
pub fn tool_step(
    call: &ToolCall,
    builtin: bool,
    edit: Option<CardEdit<'_>>,
    actions: CardActions,
    cx: &App,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(call.description.as_ref().filter(|d| !d.trim().is_empty()).map(|d| {
            div().text_size(zpx(13.)).whitespace_normal().child(d.trim().to_string())
        }))
        .child(tool_card(call, builtin, edit, actions, cx))
        .into_any_element()
}

/// 工具卡片：头一行是状态（等批准时是问话 + 按钮），下面是命令 / 路径 / diff
fn tool_card(call: &ToolCall, builtin: bool, edit: Option<CardEdit<'_>>, actions: CardActions, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    let mono = mono(cx);
    let size = term_font_size(cx);
    let execute = call.kind == ToolKind::Execute;

    // ---- 头一行 ----
    let mut head = div().flex().items_center().gap_2().px_3().py(zpx(6.)).bg(ui.muted).text_xs();
    match &call.permission {
        Some(options) => {
            let ask = match call.kind {
                ToolKind::Execute => t!("assistant.ask.execute"),
                ToolKind::Edit | ToolKind::Delete | ToolKind::Move => t!("assistant.ask.edit"),
                _ => t!("assistant.ask.other"),
            };
            head = head.child(div().flex_1().min_w_0().whitespace_normal().text_color(ui.foreground).child(ask.to_string()));
            let mac = crate::ui::mac_keys();
            if let Some(edit) = &edit {
                // 改命令时只剩两个：按改过的跑、取消
                let (answer, input, call_id) = (actions.answer.clone(), edit.input.clone(), call.id.clone());
                let allow = options.iter().find(|o| o.kind == PermissionKind::AllowOnce).map(|o| o.id.clone());
                let run_label = format!("{} {}", t!("assistant.runEdited"), if mac { "⌘↵" } else { "Ctrl+↵" });
                head = head
                    .child(
                        Button::new(SharedString::from(format!("perm-{}-edited", call.id)))
                            .xsmall()
                            .primary()
                            .label(run_label)
                            .on_click(move |_, _, cx| {
                                let cmd = input.read(cx).value().trim().to_string();
                                if let Some(allow) = allow.clone() {
                                    answer(call_id.clone(), allow, Some(cmd), cx);
                                }
                            }),
                    )
                    .child({
                        let cancel = actions.cancel_edit.clone();
                        Button::new(SharedString::from(format!("perm-{}-cancel", call.id)))
                            .xsmall()
                            .ghost()
                            .label(t!("assistant.cancelEdit").to_string())
                            .on_click(move |_, window, cx| cancel(window, cx))
                    });
            } else {
                for opt in options {
                    let (call_id, opt_id, answer) = (call.id.clone(), opt.id.clone(), actions.answer.clone());
                    // 内置 agent 的命令卡片上「允许」就叫「执行」（ACP 的选项名原样用，它们可能带范围）
                    let name = match opt.kind {
                        PermissionKind::AllowOnce if builtin && execute => t!("assistant.run").to_string(),
                        _ => opt.name.clone(),
                    };
                    let label = match opt.kind {
                        PermissionKind::AllowOnce => format!("{name} {}", if mac { "⌘↵" } else { "Ctrl+↵" }),
                        _ => name,
                    };
                    let b = Button::new(SharedString::from(format!("perm-{}-{}", call.id, opt.id)))
                        .xsmall()
                        .label(label)
                        .on_click(move |_, _, cx| answer(call_id.clone(), opt_id.clone(), None, cx));
                    let b = match opt.kind {
                        PermissionKind::AllowOnce => b.primary(),
                        _ => b.ghost(),
                    };
                    head = head.child(b);
                    // 「修改」排在「执行 / 总是允许」后面、「拒绝」前面
                    if opt.kind == PermissionKind::AllowAlways
                        && let Some(modify) = actions.modify.clone()
                    {
                        head = head.child(
                            Button::new(SharedString::from(format!("perm-{}-modify", call.id)))
                                .xsmall()
                                .ghost()
                                .label(t!("assistant.modify").to_string())
                                .on_click(move |_, window, cx| modify(window, cx)),
                        );
                    }
                }
            }
        }
        None => {
            // 在跑的卡片不转圈：时间线末尾那一行已经在转了（转圈是持续动画，画在这里每帧要重画
            // 整条时间线，见 [`Spinner`]）
            let (glyph, label, tone) = card_status(call, &ui);
            head = head
                .text_color(tone)
                .child(icon(glyph).size(zpx(13.)))
                .child(div().flex_none().font_weight(FontWeight::MEDIUM).child(label));
        }
    }

    // ---- 下面：命令 / 路径 ----
    let body = match (&edit, execute) {
        (Some(edit), _) => div()
            .flex()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .font_family(mono.clone())
            .text_size(size)
            .child(div().flex_none().h(input_line()).flex().items_center().text_color(ui.muted_foreground).child("$"))
            .child(div().flex_1().min_w_0().child(edit.view.clone())),
        (None, true) => div()
            .flex()
            .items_start()
            .gap_2()
            .px_3()
            .py_2()
            .font_family(mono.clone())
            .text_size(size)
            .child(div().flex_none().text_color(ui.muted_foreground).child("$"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .text_color(ui.tint_strong)
                    .child(if call.title.is_empty() { "…".to_string() } else { call.title.clone() }),
            ),
        (None, false) => {
            let verb = match call.kind {
                ToolKind::Read | ToolKind::Search => t!("assistant.tool.read"),
                ToolKind::Edit | ToolKind::Delete | ToolKind::Move => t!("assistant.tool.edit"),
                _ => t!("assistant.tool.other"),
            };
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .text_xs()
                .child(div().flex_none().text_color(ui.muted_foreground).child(verb.to_string()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_family(mono.clone())
                        .child(if call.title.is_empty() { "…".to_string() } else { call.title.clone() }),
                )
        }
    };
    let mut card = div()
        .flex()
        .flex_col()
        .rounded(radius::MD)
        .bg(ui.app)
        .overflow_hidden()
        .text_xs()
        .child(head)
        .child(body);

    for (i, content) in call.content.iter().enumerate() {
        match content {
            // ACP agent 的命令在它自己的进程里跑，输出只在卡片上；内置 agent 的在卡片后面的命令块里
            // （旧对话的卡片上可能还有，照样画）
            ToolContent::Terminal { output, exit_code } => {
                let all = styled_output(output, MAX_LINES);
                let hidden = all.len().saturating_sub(FOLD_LINES);
                if all.is_empty() && exit_code.is_none() {
                    continue;
                }
                card = card.child(
                    div()
                        .px_3()
                        .pb_2()
                        .font_family(mono.clone())
                        .text_size(size)
                        .text_color(TerminalLook::global(cx).palette.foreground)
                        .flex()
                        .flex_col()
                        .when(hidden > 0, |d| {
                            d.child(div().text_xs().text_color(ui.muted_foreground).child(t!("assistant.hiddenBefore", n = hidden).to_string()))
                        })
                        .child(styled_lines(&all[hidden..], cx))
                        .children(exit_code.filter(|c| *c != 0).map(|c| {
                            div().text_xs().text_color(ui.destructive).child(t!("assistant.exitCode", code = c).to_string())
                        })),
                );
            }
            ToolContent::Diff { old_text, new_text, .. } => {
                let diff = line_diff(old_text.as_deref().unwrap_or(""), new_text, 3);
                let total = diff.len();
                let mut body = div().px_3().pb_2().font_family(mono.clone()).flex().flex_col();
                for line in diff.into_iter().take(DIFF_MAX) {
                    body = body.child(match line {
                        DiffLine::Same(l) => div().text_color(ui.muted_foreground).child(format!("  {l}")),
                        DiffLine::Removed(l) => div().text_color(ui.destructive).child(format!("- {l}")),
                        DiffLine::Added(l) => div().text_color(ui.success).child(format!("+ {l}")),
                        DiffLine::Gap(n) => div().text_color(ui.muted_foreground).child(t!("assistant.unchanged", n = n).to_string()),
                    });
                }
                if total > DIFF_MAX {
                    body = body
                        .child(div().text_color(ui.muted_foreground).child(t!("assistant.hiddenAfter", n = total - DIFF_MAX).to_string()));
                }
                card = card.child(body);
            }
            ToolContent::Text { text } => {
                card = card.child(
                    div()
                        .id(SharedString::from(format!("card-text-{}-{i}", call.id)))
                        .px_3()
                        .pb_2()
                        .text_color(ui.muted_foreground)
                        .whitespace_normal()
                        .child(text.clone()),
                );
            }
            ToolContent::Unknown => {}
        }
    }
    card.into_any_element()
}

/// 卡片头一行的状态：图标、字、颜色。命令说「已执行」，读文件说「已读取」……
fn card_status(call: &ToolCall, ui: &Ui) -> (IconName, String, Hsla) {
    let done = match call.kind {
        ToolKind::Execute => t!("assistant.card.executed"),
        ToolKind::Read | ToolKind::Search => t!("assistant.card.read"),
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move => t!("assistant.card.edited"),
        _ => t!("assistant.card.done"),
    };
    match call.status {
        ToolStatus::Pending | ToolStatus::Unknown => (IconName::Clock, t!("assistant.card.pending").to_string(), ui.muted_foreground),
        ToolStatus::InProgress => (
            IconName::LoaderCircle,
            if call.kind == ToolKind::Execute { t!("assistant.card.executing") } else { t!("assistant.card.working") }.to_string(),
            ui.tint_strong,
        ),
        ToolStatus::Completed => (IconName::Check, done.to_string(), ui.success),
        ToolStatus::Failed => (IconName::CircleX, t!("assistant.card.failed").to_string(), ui.destructive),
    }
}

/// 「在跑」那一行的转圈。自己一个小视图：持续动画重画的是发起它的那个视图，画在对话里的话每一帧
/// 都是整条时间线（markdown、命令块、卡片）重排重画；单独一个视图每帧只重画这个图标
pub struct Spinner;

impl gpui_kit::Render for Spinner {
    fn render(&mut self, _: &mut Window, _: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        crate::ui::spinner("assistant-busy", IconName::LoaderCircle, zpx(13.))
    }
}

/// 这一轮还在跑：竖线 + 转圈 + 在干什么 + 「终止」
pub fn status_row(id: &str, spinner: AnyElement, label: String, on_stop: Click, cx: &App) -> AnyElement {
    let ui = Ui::global(cx).clone();
    div()
        .flex()
        .pl(gutter())
        .gap_3()
        .child(div().flex_none().w(zpx(2.)).rounded_full().bg(ui.border))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .py_1()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(spinner)
                .child(label)
                .child(div().w(zpx(1.)).h(zpx(12.)).bg(ui.border))
                .child(
                    div()
                        .id(SharedString::from(format!("stop-{id}")))
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_color(ui.destructive)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.8))
                        .on_click(move |_, window, cx| on_stop(window, cx))
                        .child(icon(IconName::CircleStop).size(zpx(13.)))
                        .child(t!("assistant.stop").to_string()),
                ),
        )
        .into_any_element()
}

/// 输入框的一行有多高：组件里写死的行高（gpui-component `input.rs` 的 `LINE_HEIGHT`）。提示符
/// 那几格按它对齐第一行
fn input_line() -> gpui_kit::Rems {
    gpui_kit::rems(1.25)
}

/// 没有边框、没有内边距的输入框（xsmall 的上下内边距是 0），字号同终端。
///
/// 单独一个视图：光标一闪输入框的状态就变一次，GPUI 会重画读过这个状态的视图——画在对话里的话，
/// 每闪一下整条时间线（markdown、命令块、卡片）跟着重排；自己一个视图只重画这一行
pub struct InputView {
    input: Entity<TextareaState>,
}

impl InputView {
    pub fn new(input: Entity<TextareaState>) -> Self {
        Self { input }
    }
}

impl gpui_kit::Render for InputView {
    fn render(&mut self, _: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        Textarea::new(&self.input).appearance(false).xsmall().text_size(term_font_size(cx))
    }
}

/// Agent 输入：画成命令行上的一行，`✦ 目录 ▌`，字体字号与终端一样
pub fn agent_input_line(input: &Entity<InputView>, cwd: Option<&str>, home: Option<&str>, cx: &App) -> Div {
    let ui = Ui::global(cx).clone();
    let size = term_font_size(cx);
    div()
        .flex()
        .items_start()
        .gap_2()
        .pl(gutter())
        .font_family(mono(cx))
        .text_size(size)
        .child(
            div()
                .flex_none()
                .h(input_line())
                .flex()
                .items_center()
                .gap_2()
                .child(icon(IconName::Sparkles).size(size * 0.85).text_color(ui.tint_strong))
                .child(prompt_mark(cwd, home, cx)),
        )
        .child(div().flex_1().min_w_0().child(input.clone()))
}

/// 时间线里一行小字（空状态、分隔说明）
pub fn hint(text: String, cx: &App) -> Div {
    div().text_xs().text_color(Ui::global(cx).muted_foreground).child(text)
}
