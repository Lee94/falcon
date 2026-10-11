//! 主题选择器（旧 React 版的 `components/ThemePicker.tsx`，ADR 0006）：Falcon 两套 + Ghostty
//! 全部内置主题，搜索、按深浅筛选，高亮到哪项就实时预览哪项（`crate::theme::preview`），也可以贴一段
//! Ghostty 主题文本当自定义主题（`falcon_theme::resolve_custom_theme`）。
//!
//! 结构照 React 版：设置页里每个槽位一个触发按钮（色块 + 主题名），点开是弹层——搜索框、
//! 分组列表（当前自定义 / Falcon / 同明暗的 Ghostty 主题）、底部"自定义…"与"复制为 Ghostty
//! 主题"。选定才落盘；弹层关掉（选定、Esc、点外面、整个设置被关掉）一律复原到真正的槽位主题。
//!
//! 与 React 版的差别只在实现层：
//! - 目录是编译期嵌进来的（falcon-theme 的 `load_catalog`），第一次打开时同步解析，没有
//!   "正在加载 / 加载失败"两种状态；
//! - 463 套进一个 `uniform_list`，只排可见的那几十行——整份列表每帧重排在 debug 构建里
//!   看得见卡顿，而键盘连按预览时整个窗口本来就在每帧重画；
//! - 筛选不是 cmdk 的 command-score，而是"子串命中排前、子序列命中排后"：同样认 `cm` →
//!   Catppuccin Mocha 这种缩写，组内顺序稳定。

use std::ops::Range;
use std::time::Duration;

use falcon_theme::{
    Appearance, CustomThemeParse, ThemeChoice, ThemeColors, ThemeKind, ThemeMode, choice_of,
    derive_theme, load_catalog, resolve_custom_theme, serialize_ghostty_theme,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Disableable, Sizable, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, ClipboardItem, Context, Div, Entity, Focusable, IntoElement, KeyDownEvent,
    Render, ScrollStrategy, SharedString, Task, UniformListScrollHandle, Window, div,
    uniform_list,
};
use rust_i18n::t;

use crate::theme::{ThemeState, Ui, hsla, radius};
use crate::toasts::ToastExt;
use crate::ui::icon;
use crate::zoom::zpx;

/// 高亮 / 编辑到应用整套主题之间的延迟：键盘连按时别每一下都重排整个窗口（沿用 React 版的值）
const PREVIEW_DELAY: Duration = Duration::from_millis(120);
/// 列表行高：标题行与主题行同高，`uniform_list` 要求每行一样高
const ROW_H: f32 = 30.;
/// 列表最高（React 版的 max-h-80）
const LIST_MAX_H: f32 = 320.;

/// 主题缩略：底色、字色、红、蓝四格，一眼能分出深浅与调性（React 版的 ThemeSwatch）
pub fn swatch(colors: &ThemeColors, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let cell = |c| div().flex_1().bg(hsla(c));
    div()
        .flex_none()
        .size(zpx(16.))
        .rounded(zpx(4.))
        .overflow_hidden()
        .border_1()
        .border_color(ui.foreground.opacity(0.2))
        .flex()
        .flex_col()
        .child(div().flex_1().flex().child(cell(colors.background)).child(cell(colors.foreground)))
        .child(div().flex_1().flex().child(cell(colors.palette[1])).child(cell(colors.palette[4])))
}

/// "复制为 Ghostty 主题"：颜色原样写回 Ghostty 格式（解析 → 序列化恒等，ADR 0006）
fn copy_button(id: &'static str, colors: ThemeColors) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::ClipboardCopy)
        .tooltip(t!("theme.copyGhostty").to_string())
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(serialize_ghostty_theme(&colors)));
            window.push_toast(Notification::success(t!("theme.copied").to_string()), cx);
        })
}

fn slot_appearance(slot: ThemeMode) -> Appearance {
    match slot {
        ThemeMode::Light => Appearance::Light,
        ThemeMode::Dark => Appearance::Dark,
    }
}

fn slot_choice(slot: ThemeMode, cx: &App) -> ThemeChoice {
    ThemeState::global(cx).settings.slot(slot).clone()
}

/// 筛选：子串命中 0、子序列命中 1、不命中 None（都不分大小写）
fn match_rank(needle: &str, name: &str) -> Option<u8> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay = name.to_lowercase();
    if hay.contains(needle) {
        return Some(0);
    }
    let mut chars = hay.chars();
    needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|n| chars.any(|h| h == n))
        .then_some(1)
}

// ---------------- 触发按钮 + 弹层 ----------------

/// 一个槽位的主题选择器：设置页"浅色主题 / 深色主题"那一行右边的按钮。
pub struct ThemePicker {
    slot: ThemeMode,
    list: Entity<ThemeList>,
}

impl ThemePicker {
    pub fn new(slot: ThemeMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let list = cx.new(|cx| ThemeList::new(slot, window, cx));
        cx.observe(&list, |_, _, cx| cx.notify()).detach();
        Self { slot, list }
    }
}

impl Render for ThemePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let choice = slot_choice(self.slot, cx);
        let list = self.list.clone();
        let open = list.read(cx).open;
        let focus = list.read(cx).input.read(cx).focus_handle(cx);
        let id = match self.slot {
            ThemeMode::Light => "theme-picker-light",
            ThemeMode::Dark => "theme-picker-dark",
        };
        let trigger = Button::new(id)
            .outline()
            .small()
            .w(zpx(256.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(swatch(&choice.colors, cx))
                    .child(div().flex_1().min_w_0().truncate().text_left().child(choice.name.clone()))
                    .child(icon(IconName::ChevronsUpDown).size(zpx(14.)).text_color(ui.muted_foreground)),
            );
        let on_change = {
            let list = list.clone();
            move |open: &bool, window: &mut Window, cx: &mut App| {
                let open = *open;
                list.update(cx, |l, cx| l.set_open(open, window, cx));
            }
        };
        Popover::new(SharedString::from(format!("{id}-popover")))
            .anchor(Anchor::TopRight)
            .open(open)
            .on_open_change(on_change)
            .track_focus(&focus)
            .trigger(trigger)
            .p_0()
            .w(zpx(320.))
            .content(move |_, _, _| list.clone())
    }
}

enum Target {
    /// 目录里的一套：选定即落盘
    Builtin(ThemeChoice),
    /// 槽位里现在那套自定义主题：选它 = 进编辑器改
    EditCurrent,
}

struct ItemRow {
    name: String,
    colors: ThemeColors,
    target: Target,
    /// 槽位当前就是这套内置主题（行尾标"当前"）
    current: bool,
}

enum Row {
    Heading(String),
    Item(ItemRow),
}

/// 弹层内容：搜索框 + 分组列表 + 底栏。开合状态也在这里，触发按钮只是读它。
pub struct ThemeList {
    slot: ThemeMode,
    pub open: bool,
    input: Entity<InputState>,
    query: String,
    rows: Vec<Row>,
    /// rows 里可选中（非标题）的下标，键盘上下在它们之间走
    items: Vec<usize>,
    /// 高亮的是 items 的第几个
    highlighted: Option<usize>,
    /// 正在预览的那一行的名字：高亮没变就不重复预览
    previewed: Option<String>,
    scroll: UniformListScrollHandle,
    preview_task: Option<Task<()>>,
}

impl ThemeList {
    fn new(slot: ThemeMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        cx.subscribe_in(&input, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::Change => {
                let q = this.input.read(cx).value().to_string();
                // set_value("") 复位时也会发 Change：查询没变就别把高亮挪回第一行
                if q != this.query {
                    this.query = q;
                    this.rebuild(cx);
                    this.highlight(if this.items.is_empty() { None } else { Some(0) }, cx);
                }
            }
            InputEvent::PressEnter { .. } => this.confirm(window, cx),
            _ => {}
        })
        .detach();
        // 整个设置被关掉时弹层可能还开着：复原到真正的槽位主题
        cx.on_release(|this, cx| {
            if this.open {
                crate::theme::preview(cx, None);
            }
        })
        .detach();
        Self {
            slot,
            open: false,
            input,
            query: String::new(),
            rows: Vec::new(),
            items: Vec::new(),
            highlighted: None,
            previewed: None,
            scroll: UniformListScrollHandle::new(),
            preview_task: None,
        }
    }

    fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.open == open {
            return;
        }
        self.open = open;
        if open {
            let count = load_catalog().iter().filter(|e| e.appearance == slot_appearance(self.slot)).count();
            self.query.clear();
            self.input.update(cx, |i, cx| {
                i.set_value("", window, cx);
                i.set_placeholder(t!("theme.search", n = count).to_string(), window, cx);
            });
            self.rebuild(cx);
            // 打开时高亮停在槽位现在那一项（React 版：setHighlighted(currentValue)）
            let current = slot_choice(self.slot, cx);
            let at = self.items.iter().position(|&ix| match &self.rows[ix] {
                Row::Item(item) => match (&item.target, current.kind) {
                    (Target::EditCurrent, ThemeKind::Custom) => true,
                    (Target::Builtin(c), ThemeKind::Builtin) => c.name == current.name,
                    _ => false,
                },
                Row::Heading(_) => false,
            });
            self.previewed = None;
            self.highlight(at.or(if self.items.is_empty() { None } else { Some(0) }), cx);
            if let Some(i) = self.highlighted {
                self.scroll.scroll_to_item(self.items[i], ScrollStrategy::Center);
            }
        } else {
            self.preview_task = None;
            self.previewed = None;
            crate::theme::preview(cx, None);
        }
        cx.notify();
    }

    /// 按查询重排分组：当前（槽位是自定义时）/ Falcon / 与槽位同明暗的 Ghostty 主题。
    /// 只列同明暗的：`.dark` 按底色亮度切，深色槽位选进浅色主题整站就翻成浅色（ADR 0006）
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let needle = self.query.trim().to_lowercase();
        let current = slot_choice(self.slot, cx);
        let want = slot_appearance(self.slot);
        let candidates = load_catalog().iter().filter(|e| e.appearance == want);

        let mut rows = Vec::new();
        if current.kind == ThemeKind::Custom
            && match_rank(&needle, &format!("custom:{}", current.name)).is_some()
        {
            rows.push(Row::Heading(t!("theme.groupCurrent").to_string()));
            rows.push(Row::Item(ItemRow {
                name: current.name.clone(),
                colors: current.colors.clone(),
                target: Target::EditCurrent,
                current: false,
            }));
        }
        let mut falcon: Vec<(u8, ItemRow)> = Vec::new();
        let mut ghostty: Vec<(u8, ItemRow)> = Vec::new();
        for entry in candidates {
            let Some(rank) = match_rank(&needle, &entry.name) else { continue };
            let item = ItemRow {
                name: entry.name.to_string(),
                colors: entry.colors.clone(),
                current: current.kind == ThemeKind::Builtin && current.name == entry.name,
                target: Target::Builtin(choice_of(entry)),
            };
            if entry.name.starts_with("Falcon ") {
                falcon.push((rank, item));
            } else {
                ghostty.push((rank, item));
            }
        }
        let group_label = match self.slot {
            ThemeMode::Light => t!("theme.groupLight"),
            ThemeMode::Dark => t!("theme.groupDark"),
        };
        for (heading, mut group) in [(t!("theme.groupFalcon").to_string(), falcon), (group_label.to_string(), ghostty)] {
            if group.is_empty() {
                continue;
            }
            group.sort_by_key(|(rank, _)| *rank);
            rows.push(Row::Heading(heading));
            rows.extend(group.into_iter().map(|(_, item)| Row::Item(item)));
        }
        self.items = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| matches!(r, Row::Item(_)))
            .map(|(i, _)| i)
            .collect();
        self.rows = rows;
        cx.notify();
    }

    /// 高亮到哪项就（延迟 120ms）预览哪项；自定义那行预览的是槽位里那套自己
    fn highlight(&mut self, at: Option<usize>, cx: &mut Context<Self>) {
        self.highlighted = at;
        cx.notify();
        let Some(Row::Item(item)) = at.and_then(|i| self.items.get(i)).map(|&ix| &self.rows[ix]) else {
            return;
        };
        if self.previewed.as_deref() == Some(item.name.as_str()) {
            return;
        }
        self.previewed = Some(item.name.clone());
        let colors = item.colors.clone();
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DELAY).await;
            this.update(cx, |this, cx| {
                if this.open {
                    crate::theme::preview(cx, Some(derive_theme(&colors)));
                }
            })
            .ok();
        }));
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.items.len() as isize;
        if n == 0 {
            return;
        }
        // cmdk 的 loop：到头绕回
        let next = match self.highlighted {
            Some(i) => (i as isize + delta).rem_euclid(n),
            None if delta > 0 => 0,
            None => n - 1,
        } as usize;
        self.highlight(Some(next), cx);
        self.scroll.scroll_to_item(self.items[next], ScrollStrategy::Nearest);
    }

    fn on_key(&mut self, e: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match e.keystroke.key.as_str() {
            "down" => {
                self.step(1, cx);
                cx.stop_propagation();
            }
            "up" => {
                self.step(-1, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.highlighted {
            self.activate(self.items[i], window, cx);
        }
    }

    fn activate(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Item(item)) = self.rows.get(row) else { return };
        let slot = self.slot;
        match &item.target {
            Target::Builtin(choice) => {
                let choice = choice.clone();
                self.set_open(false, window, cx);
                crate::theme::update_settings(cx, |s| *s.slot_mut(slot) = choice);
            }
            Target::EditCurrent => {
                self.set_open(false, window, cx);
                open_editor(slot, slot_choice(slot, cx), window, cx);
            }
        }
    }

    fn render_row(&self, row: usize, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        match &self.rows[row] {
            Row::Heading(label) => div()
                .h(zpx(ROW_H))
                .px_2()
                .pb_1()
                .flex()
                .items_end()
                .text_size(zpx(11.))
                .text_color(ui.muted_foreground)
                .child(label.clone())
                .into_any_element(),
            Row::Item(item) => {
                let highlighted = self.highlighted.map(|i| self.items[i]) == Some(row);
                let at = self.items.iter().position(|&ix| ix == row);
                let trailing = match item.target {
                    Target::EditCurrent => Some(icon(IconName::PencilLine).size(zpx(14.)).into_any_element()),
                    Target::Builtin(_) if item.current => Some(
                        div()
                            .text_size(zpx(11.))
                            .text_color(ui.muted_foreground)
                            .child(t!("theme.groupCurrent").to_string())
                            .into_any_element(),
                    ),
                    Target::Builtin(_) => None,
                };
                div()
                    .id(("theme-row", row))
                    .w_full()
                    .h(zpx(ROW_H))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(radius::SM)
                    .cursor_pointer()
                    .text_sm()
                    .when(highlighted, |d| d.bg(ui.tint).text_color(ui.tint_foreground))
                    // cmdk 在指针移动时高亮（不是 hover：列表滚动经过指针下面不算）
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if at.is_some() && this.highlighted != at {
                            this.highlight(at, cx);
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(row, window, cx)))
                    .child(swatch(&item.colors, cx))
                    .child(div().flex_1().min_w_0().truncate().child(item.name.clone()))
                    .children(trailing)
                    .into_any_element()
            }
        }
    }
}

impl Render for ThemeList {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let choice = slot_choice(self.slot, cx);
        let slot = self.slot;
        let list_h = (self.rows.len() as f32 * ROW_H).min(LIST_MAX_H);
        let list: AnyElement = if self.items.is_empty() {
            div()
                .py_6()
                .flex()
                .justify_center()
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(t!("theme.empty").to_string())
                .into_any_element()
        } else {
            uniform_list(
                "theme-list",
                self.rows.len(),
                cx.processor(|this, range: Range<usize>, _window, cx| {
                    range.map(|row| this.render_row(row, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .h(zpx(list_h))
            .into_any_element()
        };
        let edit = cx.listener(move |this, _, window, cx| {
            this.set_open(false, window, cx);
            open_editor(slot, slot_choice(slot, cx), window, cx);
        });
        div()
            .w(zpx(320.))
            .flex()
            .flex_col()
            .capture_key_down(cx.listener(Self::on_key))
            .child(
                div()
                    .px_1()
                    .pt_1()
                    .pb_1()
                    .border_b_1()
                    .border_color(ui.border)
                    .child(
                        Input::new(&self.input)
                            .appearance(false)
                            .prefix(icon(IconName::Search).size(zpx(14.)).text_color(ui.muted_foreground)),
                    ),
            )
            .child(div().p_1().child(list))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .p_1()
                    .border_t_1()
                    .border_color(ui.border)
                    .child(
                        // 左对齐的幽灵按钮（React 版：flex-1 justify-start）；组件库的 Button 内容恒居中
                        div()
                            .id("theme-custom-entry")
                            .flex_1()
                            .h(zpx(24.))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap(zpx(6.))
                            .rounded(radius::SM)
                            .text_xs()
                            .cursor_pointer()
                            .hover(|s| s.bg(ui.muted))
                            .on_click(edit)
                            .child(icon(IconName::PencilLine).size(zpx(14.)))
                            .child(t!("theme.customEntry").to_string()),
                    )
                    .child(copy_button("theme-copy", choice.colors.clone())),
            )
    }
}

// ---------------- 自定义主题编辑器 ----------------

/// 粘贴 / 修改 Ghostty 主题文本。编辑过程中整个应用实时按文本预览，关掉复原。
/// `theme = 内置名` 以目录里那套为底再覆盖，与 Ghostty 读配置的顺序一致。
pub fn open_editor(slot: ThemeMode, initial: ThemeChoice, window: &mut Window, cx: &mut App) {
    let editor = cx.new(|cx| ThemeEditor::new(slot, &initial, window, cx));
    let text = editor.read(cx).text.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let colors = editor.read(cx).parsed.colors.clone();
        let editor_ok = editor.clone();
        let footer = DialogFooter::new()
            .children(colors.clone().map(|c| copy_button("theme-editor-copy", c)))
            .child(DialogClose::new().trigger(|b| b.label(t!("common.cancel").to_string())))
            .child(
                DialogAction::new().child(
                    Button::new("theme-editor-apply")
                        .primary()
                        .disabled(colors.is_none())
                        .label(t!("theme.editorApply").to_string()),
                ),
            );
        dialog
            .title(t!("theme.editorTitle").to_string())
            .w(zpx(672.))
            // 编辑到一半点到外面就丢了——只认取消 / Esc / 应用（React 版的 lockOverlay）
            .overlay_closable(false)
            .child(editor.clone())
            .footer(footer)
            .on_ok(move |_, _, cx| editor_ok.update(cx, |e, cx| e.apply(cx)))
    });
    // 对话框打开时会把焦点收到自己身上：等它收完再交给文本框（React 版的 data-autofocus）
    window.defer(cx, move |window, cx| text.update(cx, |t, cx| t.focus(window, cx)));
}

struct ThemeEditor {
    slot: ThemeMode,
    name: Entity<InputState>,
    text: Entity<TextareaState>,
    parsed: CustomThemeParse,
    preview_task: Option<Task<()>>,
}

impl ThemeEditor {
    fn new(slot: ThemeMode, initial: &ThemeChoice, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (name, text) = match initial.kind {
            ThemeKind::Custom => (initial.name.clone(), serialize_ghostty_theme(&initial.colors)),
            ThemeKind::Builtin => (String::new(), format!("theme = {}\n", initial.name)),
        };
        let name = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let text = cx.new(|cx| TextareaState::new(window, cx).rows(14).default_value(text));
        cx.subscribe_in(&text, window, |this, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::Change) {
                this.reparse(window, cx);
            }
        })
        .detach();
        cx.on_release(|_, cx| crate::theme::preview(cx, None)).detach();
        let mut this = Self {
            slot,
            name,
            text,
            parsed: CustomThemeParse { colors: None, recognized: 0, unknown_base: None, base_name: None },
            preview_task: None,
        };
        this.reparse(window, cx);
        this
    }

    fn reparse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text.read(cx).value().to_string();
        let parsed = resolve_custom_theme(&text, self.slot, load_catalog());
        if parsed.base_name != self.parsed.base_name || self.parsed.recognized == 0 {
            let placeholder = parsed.base_name.clone().unwrap_or_else(|| t!("theme.editorNamePlaceholder").to_string());
            self.name.update(cx, |i, cx| i.set_placeholder(placeholder, window, cx));
        }
        let changed = parsed.colors != self.parsed.colors;
        self.parsed = parsed;
        if changed && let Some(colors) = self.parsed.colors.clone() {
            self.preview_task = Some(cx.spawn(async move |_, cx| {
                cx.background_executor().timer(PREVIEW_DELAY).await;
                cx.update(|cx| crate::theme::preview(cx, Some(derive_theme(&colors))));
            }));
        }
        cx.notify();
    }

    /// 应用：名字没填就用底的内置主题名，再没有就叫"自定义主题"；截到 80 个字
    fn apply(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(colors) = self.parsed.colors.clone() else {
            return false;
        };
        let typed = self.name.read(cx).value().trim().to_string();
        let label = if !typed.is_empty() {
            typed
        } else {
            self.parsed.base_name.clone().unwrap_or_else(|| t!("theme.editorTitle").to_string())
        };
        let name: String = label.chars().take(80).collect();
        let slot = self.slot;
        self.preview_task = None;
        crate::theme::update_settings(cx, |s| *s.slot_mut(slot) = ThemeChoice { name, kind: ThemeKind::Custom, colors });
        true
    }
}

impl Render for ThemeEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = Ui::global(cx).clone();
        let p = &self.parsed;
        let (hint, tone) = match (&p.unknown_base, &p.colors) {
            (Some(base), _) => (t!("theme.editorUnknownBase", name = base.clone()).to_string(), ui.muted_foreground),
            (None, Some(_)) => (t!("theme.editorRecognized", n = p.recognized).to_string(), ui.success),
            (None, None) => (t!("theme.editorNothing").to_string(), ui.destructive),
        };
        let label = |text: String| div().text_xs().text_color(ui.muted_foreground).child(text);
        let mut body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xs().text_color(ui.muted_foreground).child(t!("theme.editorHint").to_string()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(6.))
                    .child(label(t!("theme.editorName").to_string()))
                    .child(Input::new(&self.name)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(zpx(6.))
                    .child(label(t!("theme.editorText").to_string()))
                    // React 版：rows=14、leading-5（20px）+ 上下各 8px
                    .child(div().font_family(crate::fonts::BERKELEY).text_xs().child(Textarea::new(&self.text).h(zpx(296.))))
                    .child(div().text_xs().text_color(tone).child(hint)),
            );
        if let Some(colors) = &p.colors {
            let mut chips = div().flex().gap_1();
            for c in colors.palette {
                chips = chips.child(
                    div()
                        .size(zpx(12.))
                        .rounded(zpx(3.))
                        .border_1()
                        .border_color(ui.foreground.opacity(0.2))
                        .bg(hsla(c)),
                );
            }
            body = body.child(div().flex().items_center().gap_2().child(swatch(colors, cx)).child(chips));
        }
        body
    }
}
