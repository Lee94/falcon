//! 面板文档里各种块的画法。颜色一律取语义 token：插件只给色调（`PluginTone`），这里映射。
//!
//! 窄栏优先：数字卡片两个一行、表格第一列占一半其余平分、选项少就分段控件多了换下拉。
//! 元素 id 用"第几块-第几项"拼：文档每次重画整份换掉，同一位置的按钮仍是同一个 id，
//! hover / 按下的状态不会因为两秒一次的重画闪掉。

use falcon_proto::{
    PluginAction, PluginAlign, PluginBlock, PluginButton, PluginColumn, PluginKv, PluginListItem, PluginOption,
    PluginSeries, PluginStat, PluginTone, PluginView,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Div, ElementId, FontWeight, Hsla, IntoElement, SharedString, div, relative};
use rust_i18n::t;

use super::chart::chart;
use super::{PluginPanel, plugin_icon};
use crate::menus::{MenuItemSpec, to_popup};
use crate::panels::meegle::widgets::{pill, segmented};
use crate::theme::{TerminalLook, Ui, radius};
use crate::ui::{icon, sunken};
use crate::zoom::zpx;

/// 色调 → 语义色；`None` / 认不出的用 `fallback`
pub(super) fn tone_color(tone: Option<PluginTone>, fallback: Hsla, ui: &Ui) -> Hsla {
    match tone {
        Some(PluginTone::Default) => ui.foreground,
        Some(PluginTone::Muted) => ui.muted_foreground,
        Some(PluginTone::Accent) => ui.tint_strong,
        Some(PluginTone::Success) => ui.success,
        Some(PluginTone::Warning) => ui.warning,
        Some(PluginTone::Danger) => ui.destructive,
        _ => fallback,
    }
}

fn eid(prefix: &str, i: usize, j: usize) -> ElementId {
    ElementId::Name(format!("pl-{prefix}-{i}-{j}").into())
}

/// 选项少（≤ 3）且都短时用分段控件，否则下拉
const SEGMENTED_MAX: usize = 3;
const SEGMENTED_LABEL: usize = 8;

impl PluginPanel {
    pub(super) fn render_view(&self, view: PluginView, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let mut col = div().flex().flex_col().gap_3().px_3().pt_2().pb_4();
        if let Some(title) = view.title.clone().filter(|t| !t.is_empty()) {
            col = col.child(div().text_size(zpx(11.)).text_color(ui.muted_foreground).child(title));
        }
        // 这一次没画成：上一份文档留着，顶上说一声
        if let Some(err) = &self.error {
            col = col.child(notice(&format!("{}\n{err}", t!("plugin.staleFailed")), Some(PluginTone::Danger), cx));
        }
        if view.blocks.is_empty() {
            col = col.child(div().text_size(zpx(12.)).text_color(ui.muted_foreground).child(t!("plugin.empty").to_string()));
        }
        for (i, block) in view.blocks.into_iter().enumerate() {
            if let Some(el) = self.block(i, block, cx) {
                col = col.child(el);
            }
        }
        div().id("plugin-body").flex_1().min_h_0().overflow_y_scroll().child(col).into_any_element()
    }

    fn block(&self, i: usize, block: PluginBlock, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = Ui::global(cx).clone();
        Some(match block {
            PluginBlock::Heading { text, detail } => div()
                .flex()
                .items_baseline()
                .gap_2()
                .pt_1()
                .pb_1()
                .border_b_1()
                .border_color(ui.border)
                .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).font_weight(FontWeight::MEDIUM).child(text))
                .when_some(detail, |d, detail| {
                    d.child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(detail))
                })
                .into_any_element(),
            PluginBlock::Text { text, tone, mono } => {
                let mut d = div().text_size(zpx(12.)).line_height(relative(1.5)).text_color(tone_color(tone, ui.foreground, &ui));
                if mono {
                    d = d.font_family(TerminalLook::global(cx).text.font.family.clone()).text_size(zpx(11.5));
                }
                d.child(text).into_any_element()
            }
            PluginBlock::Notice { text, tone } => notice(&text, tone, cx).into_any_element(),
            PluginBlock::Stats { items } => stats(items, cx).into_any_element(),
            PluginBlock::Meter { label, value, max, text, tone } => {
                let ratio = falcon_core::plugin::meter_ratio(value, max);
                let text = text.unwrap_or_else(|| falcon_core::plugin::meter_text(value, max));
                let fill = tone_color(tone, ui.tint_strong, &ui);
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).child(label))
                            .child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(text)),
                    )
                    .child(
                        div()
                            .h(zpx(6.))
                            .w_full()
                            .rounded_full()
                            .bg(ui.app)
                            .overflow_hidden()
                            .child(div().h_full().w(relative(ratio)).rounded_full().bg(fill)),
                    )
                    .into_any_element()
            }
            PluginBlock::Chart { label, text, series, max, slots } => {
                chart_block(label, text, series, max, slots, cx).into_any_element()
            }
            PluginBlock::Kv { items } => kv(items, cx).into_any_element(),
            PluginBlock::Table { columns, rows } => table(columns, rows, cx).into_any_element(),
            PluginBlock::List { items } => self.list(i, items, cx).into_any_element(),
            PluginBlock::Actions { items } => self.actions(i, items, cx).into_any_element(),
            PluginBlock::Select { param, label, value, options } => self.select(i, param, label, value, options, cx),
            PluginBlock::Divider => div().h(zpx(1.)).bg(ui.border).into_any_element(),
            PluginBlock::Unknown => return None,
        })
    }

    fn list(&self, i: usize, items: Vec<PluginListItem>, cx: &mut Context<Self>) -> Div {
        let ui = Ui::global(cx).clone();
        let mut col = div().flex().flex_col();
        for (j, item) in items.into_iter().enumerate() {
            let mut row = div()
                .id(eid("li", i, j))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(zpx(5.))
                .mx(zpx(-8.))
                .rounded(radius::SM)
                .when_some(item.tone, |d, tone| {
                    d.child(div().flex_none().size(zpx(6.)).rounded_full().bg(tone_color(Some(tone), ui.muted_foreground, &ui)))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().text_size(zpx(12.)).child(item.title))
                        .when_some(item.detail, |d, detail| {
                            d.child(div().truncate().text_size(zpx(11.)).text_color(ui.muted_foreground).child(detail))
                        }),
                )
                .when_some(item.badge, |d, badge| d.child(pill(badge, true, cx)));
            if !item.menu.is_empty() {
                // 按钮自己拦住点击，不会连带点了这一行
                let this = cx.weak_entity();
                let busy = self.busy;
                let items: Vec<MenuItemSpec> = item
                    .menu
                    .into_iter()
                    .map(|b| {
                        let (this, action) = (this.clone(), b.action);
                        let dead = action == PluginAction::Unknown;
                        let spec = MenuItemSpec::new(b.label, move |window, cx| {
                            let action = action.clone();
                            let _ = this.update(cx, |p, cx| p.run_action(action, window, cx));
                        })
                        .disabled(dead || busy);
                        if b.tone == Some(PluginTone::Danger) { spec.danger() } else { spec }
                    })
                    .collect();
                row = row.child(
                    Button::new(eid("lm", i, j))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Ellipsis)
                        .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone())),
                );
            }
            if let Some(action) = item.action.filter(|a| *a != PluginAction::Unknown) {
                row = row
                    .cursor_pointer()
                    .hover(|s| s.bg(ui.muted))
                    .on_click(cx.listener(move |this, _, window, cx| this.run_action(action.clone(), window, cx)));
            }
            col = col.child(row);
        }
        col
    }

    fn actions(&self, i: usize, items: Vec<PluginButton>, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().flex_wrap().gap(zpx(6.));
        for (j, b) in items.into_iter().enumerate() {
            let mut btn = Button::new(eid("btn", i, j)).small().label(b.label);
            btn = match b.tone {
                Some(PluginTone::Accent) => btn.primary(),
                Some(PluginTone::Danger) => btn.danger(),
                _ => btn.outline(),
            };
            if let Some(name) = b.icon.as_deref() {
                btn = btn.icon(plugin_icon(name));
            }
            let action = b.action;
            let dead = action == PluginAction::Unknown;
            row = row.child(
                btn.disabled(dead || self.busy)
                    .on_click(cx.listener(move |this, _, window, cx| this.run_action(action.clone(), window, cx))),
            );
        }
        row
    }

    fn select(
        &self,
        i: usize,
        param: String,
        label: Option<String>,
        value: Option<String>,
        options: Vec<PluginOption>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let current = value.or_else(|| self.params.get(&param).cloned());
        let this = cx.weak_entity();
        let short = options.len() <= SEGMENTED_MAX && options.iter().all(|o| o.label.chars().count() <= SEGMENTED_LABEL);
        let control: AnyElement = if short && !options.is_empty() {
            let selected = options.iter().position(|o| Some(&o.value) == current.as_ref()).unwrap_or(usize::MAX);
            let values: Vec<String> = options.iter().map(|o| o.value.clone()).collect();
            segmented(
                &format!("pl-seg-{i}"),
                selected,
                options.into_iter().enumerate().map(|(k, o)| (k, o.label)).collect(),
                move |k, _, cx| {
                    let (param, value) = (param.clone(), values[k].clone());
                    let _ = this.update(cx, |p, cx| p.set_param(param, value, cx));
                },
                cx,
            )
            .flex_1()
            .into_any_element()
        } else {
            let shown: SharedString = options
                .iter()
                .find(|o| Some(&o.value) == current.as_ref())
                .map(|o| o.label.clone())
                .or(current.clone())
                .unwrap_or_default()
                .into();
            let items: Vec<MenuItemSpec> = options
                .into_iter()
                .map(|o| {
                    let (this, param, value) = (this.clone(), param.clone(), o.value.clone());
                    MenuItemSpec::new(o.label, move |_, cx| {
                        let (param, value) = (param.clone(), value.clone());
                        let _ = this.update(cx, |p, cx| p.set_param(param, value, cx));
                    })
                    .checked(Some(&o.value) == current.as_ref())
                })
                .collect();
            Button::new(eid("sel", i, 0))
                .outline()
                .small()
                .w_full()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w_full()
                        .gap_1()
                        .child(div().flex_1().min_w_0().truncate().text_left().child(shown))
                        .child(icon(IconName::ChevronDown).size(zpx(12.)).text_color(ui.muted_foreground)),
                )
                .dropdown_menu(move |menu, _, _| to_popup(menu, items.clone()))
                .into_any_element()
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .when_some(label, |d, label| {
                d.child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(label))
            })
            .child(div().flex_1().min_w_0().flex().child(control))
            .into_any_element()
    }
}

fn notice(text: &str, tone: Option<PluginTone>, cx: &App) -> Div {
    let ui = Ui::global(cx);
    let color = tone_color(tone, ui.tint_strong, ui);
    let glyph = match tone {
        Some(PluginTone::Danger) => IconName::CircleAlert,
        Some(PluginTone::Warning) => IconName::TriangleAlert,
        Some(PluginTone::Success) => IconName::CircleCheck,
        _ => IconName::Info,
    };
    div()
        .flex()
        .items_start()
        .gap_2()
        .px_2()
        .py(zpx(6.))
        .rounded(radius::SM)
        .bg(color.opacity(0.12))
        .child(div().flex_none().pt(zpx(2.)).child(icon(glyph).size(zpx(13.)).text_color(color)))
        .child(div().flex_1().min_w_0().text_size(zpx(12.)).line_height(relative(1.5)).child(text.to_string()))
}

/// 数字卡片：两个一行，单数的最后一张也只占半行（宽度一致才好对比）
fn stats(items: Vec<PluginStat>, cx: &App) -> Div {
    let ui = Ui::global(cx).clone();
    let mut col = div().flex().flex_col().gap_2();
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        let mut row = div().flex().gap_2();
        for k in 0..2 {
            let cell = match items.next() {
                Some(s) => sunken(cx)
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py(zpx(6.))
                    .flex()
                    .flex_col()
                    .child(div().truncate().text_size(zpx(11.)).text_color(ui.muted_foreground).child(s.label))
                    .child(
                        div()
                            .truncate()
                            .text_size(zpx(18.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tone_color(s.tone, ui.foreground, &ui))
                            .child(s.value),
                    )
                    .when_some(s.detail, |d, detail| {
                        d.child(div().truncate().text_size(zpx(11.)).text_color(ui.muted_foreground).child(detail))
                    }),
                None if k == 1 => div().flex_1(),
                None => break,
            };
            row = row.child(cell);
        }
        col = col.child(row);
    }
    col
}

fn chart_block(
    label: Option<String>,
    text: Option<String>,
    series: Vec<PluginSeries>,
    max: Option<f64>,
    slots: Option<u32>,
    cx: &App,
) -> Div {
    let ui = Ui::global(cx).clone();
    let named: Vec<(String, Hsla)> = series
        .iter()
        .filter_map(|s| Some((s.name.clone()?, tone_color(s.tone, ui.tint_strong, &ui))))
        .collect();
    let lines: Vec<(Vec<f64>, Hsla)> = series.iter().map(|s| (s.values.clone(), tone_color(s.tone, ui.tint_strong, &ui))).collect();
    let top = falcon_core::plugin::chart_max(&series, max);
    let slots = slots.map(|s| s as usize).unwrap_or(0);
    let legend = named.len() > 1;
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_baseline()
                .gap_2()
                .child(div().flex_1().min_w_0().truncate().text_size(zpx(12.)).child(label.unwrap_or_default()))
                .when_some(text, |d, text| d.child(div().flex_none().text_size(zpx(11.)).text_color(ui.muted_foreground).child(text))),
        )
        .child(sunken(cx).h(zpx(52.)).p(zpx(4.)).child(chart(lines, top, slots)))
        .when(legend, |d| {
            let mut row = div().flex().gap_3();
            for (name, color) in named {
                row = row.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(zpx(11.))
                        .text_color(ui.muted_foreground)
                        .child(div().size(zpx(6.)).rounded_full().bg(color))
                        .child(name),
                );
            }
            d.child(row)
        })
}

fn kv(items: Vec<PluginKv>, cx: &App) -> Div {
    let ui = Ui::global(cx).clone();
    let mono = TerminalLook::global(cx).text.font.family.clone();
    let mut col = div().flex().flex_col().gap(zpx(3.));
    for item in items {
        let mut value = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_right()
            .text_size(zpx(12.))
            .text_color(tone_color(item.tone, ui.foreground, &ui));
        if item.mono {
            value = value.font_family(mono.clone());
        }
        col = col.child(
            div()
                .flex()
                .items_baseline()
                .gap_3()
                .child(div().flex_none().max_w(relative(0.5)).truncate().text_size(zpx(12.)).text_color(ui.muted_foreground).child(item.key))
                .child(value.child(item.value)),
        );
    }
    col
}

/// 表格：第一列占 2 份、其余各 1 份（窄栏里名字列最需要地方），右对齐的列按表头说
fn table(columns: Vec<PluginColumn>, rows: Vec<Vec<String>>, cx: &App) -> Div {
    let ui = Ui::global(cx).clone();
    let n = columns.len().max(1);
    let share = |k: usize| relative(if k == 0 { 2.0 } else { 1.0 } / (n as f32 + 1.0));
    let right: Vec<bool> = columns.iter().map(|c| c.align == Some(PluginAlign::Right)).collect();
    let cell = |k: usize, text: String| {
        let d = div().w(share(k)).min_w_0().truncate().px(zpx(2.)).child(text);
        if right.get(k).copied().unwrap_or(false) { d.text_right() } else { d }
    };
    let mut col = div().flex().flex_col();
    let mut head = div().flex().pb_1().text_size(zpx(11.)).text_color(ui.muted_foreground).border_b_1().border_color(ui.border);
    for (k, c) in columns.into_iter().enumerate() {
        head = head.child(cell(k, c.title));
    }
    col = col.child(head);
    for row in rows {
        let mut r = div().flex().py(zpx(3.)).text_size(zpx(12.));
        for (k, text) in row.into_iter().enumerate() {
            r = r.child(cell(k, text));
        }
        col = col.child(r);
    }
    col
}
