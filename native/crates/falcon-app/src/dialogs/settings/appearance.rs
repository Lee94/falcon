//! 外观：明暗模式、浅 / 深两个主题槽位（ADR 0006）、终端字体 / 字号 / 行高 / 光标 / 闪烁
//! 与预览。web 的 AppearancePane。
//!
//! web 还有一节"实验性 · 终端渲染引擎（xterm.js / Rio）"：原生只有一套终端渲染，这一节不画；
//! 偏好 JSON 里的 `engine` 字段原样保留（falcon-core 的 TermPref 读写不丢值），换回浏览器
//! 用的还是用户选过的那个引擎。

use falcon_core::term::{
    TERM_FONT_SIZE_MAX, TERM_FONT_SIZE_MIN, TERM_LINE_HEIGHT_MAX, TERM_LINE_HEIGHT_MIN, TERM_PREF_KEY,
    TERM_PREF_KEY_LEGACY, TermCursorStyle, TermFontId, TermPref, clamp_font_size, clamp_line_height,
    load_term_pref, sanitize_term_pref, serialize_term_pref,
};
use falcon_theme::{
    FALCON_DARK, FALCON_LIGHT, GHOSTTY_THEMES_COUNT, GHOSTTY_THEMES_ORIGIN, ThemeMode, ThemePref, choice_of,
    is_default_theme_settings,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::{Disableable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    Anchor, AnyElement, App, Context, Entity, IntoElement, Render, SharedString, Window, div, relative,
};
use rust_i18n::t;

use super::widgets::{SegOption, rows, section, segmented};
use crate::dialogs::theme_picker::ThemePicker;
use crate::fonts;
use crate::menus::{MenuItemSpec, to_popup};
use crate::prefs::Prefs;
use crate::theme::{TermPrefs, TerminalLook, ThemeState, Ui, radius};
use crate::ui::icon;
use crate::zoom::zpx;

/// 读偏好文件里的 `falcon.term`（没有再看旧名 `mojito.term`），过一遍 falcon-core 的清洗
fn load_term(cx: &App) -> TermPref {
    let prefs = Prefs::global(cx);
    load_term_pref(prefs.get(TERM_PREF_KEY).or_else(|| prefs.get(TERM_PREF_KEY_LEGACY)))
}

/// TermPref（与 web 同形的偏好）→ 终端渲染用的 TermPrefs（映射在 theme.rs，启动时读偏好也走它）
fn term_look(pref: &TermPref) -> TermPrefs {
    TermPrefs::from_pref(pref)
}

fn font_label(id: TermFontId) -> String {
    t!(format!("term.font_{}", id.as_str().replace('-', "_"))).to_string()
}

pub struct AppearancePane {
    light: Entity<ThemePicker>,
    dark: Entity<ThemePicker>,
    term: TermPref,
    custom_font: Entity<InputState>,
    font_size: Entity<SliderState>,
    line_height: Entity<SliderState>,
}

impl AppearancePane {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let term = load_term(cx);
        let light = cx.new(|cx| ThemePicker::new(ThemeMode::Light, window, cx));
        let dark = cx.new(|cx| ThemePicker::new(ThemeMode::Dark, window, cx));
        let custom_font = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("settings.termCustomFontPlaceholder").to_string())
                .default_value(term.custom_family.clone())
        });
        cx.subscribe(&custom_font, |this, input, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                let v = input.read(cx).value().to_string();
                this.set_term(|t| t.custom_family = v, cx);
            }
        })
        .detach();
        let font_size = cx.new(|_| {
            SliderState::new()
                .min(TERM_FONT_SIZE_MIN as f32)
                .max(TERM_FONT_SIZE_MAX as f32)
                .step(1.)
                .default_value(term.font_size as f32)
        });
        cx.subscribe(&font_size, |this, _, ev: &SliderEvent, cx| {
            if let SliderEvent::Change(v) = ev {
                let n = clamp_font_size(v.start() as f64);
                this.set_term(|t| t.font_size = n, cx);
            }
        })
        .detach();
        let line_height = cx.new(|_| {
            SliderState::new()
                .min(TERM_LINE_HEIGHT_MIN as f32)
                .max(TERM_LINE_HEIGHT_MAX as f32)
                .step(0.05)
                .default_value(term.line_height as f32)
        });
        cx.subscribe(&line_height, |this, _, ev: &SliderEvent, cx| {
            if let SliderEvent::Change(v) = ev {
                let n = clamp_line_height(v.start() as f64);
                this.set_term(|t| t.line_height = n, cx);
            }
        })
        .detach();
        Self { light, dark, term, custom_font, font_size, line_height }
    }

    /// web 的 `setTerm(patch)`：合并 → 清洗 → 落盘，终端立即换上
    fn set_term(&mut self, patch: impl FnOnce(&mut TermPref), cx: &mut Context<Self>) {
        let mut next = self.term.clone();
        patch(&mut next);
        let next = sanitize_term_pref(&serde_json::to_value(&next).unwrap_or_default());
        if next == self.term {
            return;
        }
        crate::theme::update_term(cx, term_look(&next), serialize_term_pref(&next));
        self.term = next;
        cx.notify();
    }

    fn reset_term(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let d = TermPref::default();
        self.custom_font.update(cx, |i, cx| i.set_value(d.custom_family.clone(), window, cx));
        self.font_size.update(cx, |s, cx| s.set_value(d.font_size as f32, window, cx));
        self.line_height.update(cx, |s, cx| s.set_value(d.line_height as f32, window, cx));
        crate::theme::update_term(cx, term_look(&d), serialize_term_pref(&d));
        self.term = d;
        cx.notify();
    }

    fn render_theme_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let settings = ThemeState::global(cx).settings.clone();
        let mode = segmented(
            "theme-mode",
            settings.mode,
            [
                (ThemePref::System, "theme.system", IconName::Monitor),
                (ThemePref::Light, "theme.light", IconName::Sun),
                (ThemePref::Dark, "theme.dark", IconName::Moon),
            ]
            .into_iter()
            .map(|(value, key, ic)| SegOption { value, label: t!(key).to_string(), icon: Some(ic) })
            .collect(),
            true,
            |pref, _, cx| crate::theme::update_settings(cx, |s| s.mode = pref),
            cx,
        );
        let mut children = rows(
            vec![
                (t!("theme.label").to_string(), Some(t!("settings.themeHint").to_string()), mode.into_any_element()),
                (
                    t!("theme.lightTheme").to_string(),
                    Some(t!("theme.lightThemeHint").to_string()),
                    self.light.clone().into_any_element(),
                ),
                (
                    t!("theme.darkTheme").to_string(),
                    Some(t!("theme.darkThemeHint").to_string()),
                    self.dark.clone().into_any_element(),
                ),
                (
                    t!("native.settings.uiZoom").to_string(),
                    Some(t!("native.settings.uiZoomHint").to_string()),
                    zoom_control(cx),
                ),
            ],
            true,
            cx,
        );
        children.push(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .pt_3()
                .child(
                    div().text_xs().text_color(ui.muted_foreground).child(
                        t!("theme.ghosttyOrigin", n = GHOSTTY_THEMES_COUNT, origin = GHOSTTY_THEMES_ORIGIN).to_string(),
                    ),
                )
                .child(
                    Button::new("theme-reset")
                        .outline()
                        .xsmall()
                        .disabled(is_default_theme_settings(&settings))
                        .label(t!("theme.resetThemes").to_string())
                        // web 的 resetThemes：两个槽位回 Falcon 默认，明暗模式不动
                        .on_click(|_, _, cx| {
                            crate::theme::update_settings(cx, |s| {
                                s.light = choice_of(&FALCON_LIGHT);
                                s.dark = choice_of(&FALCON_DARK);
                            })
                        }),
                )
                .into_any_element(),
        );
        section(t!("settings.appearanceTitle").to_string(), None, children, cx).into_any_element()
    }

    fn render_term_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = Ui::global(cx).clone();
        let term = self.term.clone();
        let this = cx.entity().downgrade();

        let items: Vec<MenuItemSpec> = TermFontId::ALL
            .into_iter()
            .map(|id| {
                let this = this.clone();
                MenuItemSpec::new(font_label(id), move |_, cx| {
                    this.update(cx, |p, cx| p.set_term(|t| t.font_id = id, cx)).ok();
                })
                .checked(term.font_id == id)
            })
            .collect();
        let font = Button::new("term-font")
            .outline()
            .small()
            .w(zpx(256.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().text_left().child(font_label(term.font_id)))
                    .child(icon(IconName::ChevronDown).size(zpx(14.)).text_color(ui.muted_foreground)),
            )
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| to_popup(menu, items.clone()));

        let value = |text: String| {
            div()
                .w(zpx(36.))
                .text_right()
                .font_family(fonts::BERKELEY)
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(text)
        };
        let size = div()
            .flex()
            .items_center()
            .gap_2()
            .child(Slider::new(&self.font_size).w(zpx(112.)))
            .child(value(format!("{}", term.font_size)));
        let line = div()
            .flex()
            .items_center()
            .gap_2()
            .child(Slider::new(&self.line_height).w(zpx(112.)))
            .child(value(format!("{:.2}", term.line_height)));
        let cursor = segmented(
            "term-cursor",
            term.cursor_style,
            [TermCursorStyle::Block, TermCursorStyle::Bar, TermCursorStyle::Underline]
                .into_iter()
                .map(|value| {
                    let key = match value {
                        TermCursorStyle::Block => "term.cursor_block",
                        TermCursorStyle::Bar => "term.cursor_bar",
                        TermCursorStyle::Underline => "term.cursor_underline",
                    };
                    SegOption { value, label: t!(key).to_string(), icon: None }
                })
                .collect(),
            false,
            {
                let this = this.clone();
                move |style, _, cx| {
                    this.update(cx, |p, cx| p.set_term(|t| t.cursor_style = style, cx)).ok();
                }
            },
            cx,
        );
        let blink = Checkbox::new("term-blink")
            .checked(term.cursor_blink)
            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                let v = *checked;
                this.set_term(|t| t.cursor_blink = v, cx);
            }));

        let mut list = vec![(t!("settings.termFont").to_string(), Some(t!("settings.termFontHint").to_string()), font.into_any_element())];
        if term.font_id == TermFontId::Custom {
            list.push((
                t!("settings.termCustomFont").to_string(),
                None,
                div().w(zpx(256.)).child(Input::new(&self.custom_font).small()).into_any_element(),
            ));
        }
        list.push((t!("settings.termFontSize").to_string(), None, size.into_any_element()));
        list.push((t!("settings.termLineHeight").to_string(), None, line.into_any_element()));
        list.push((t!("settings.termCursor").to_string(), None, cursor.into_any_element()));
        list.push((t!("settings.termCursorBlink").to_string(), None, blink.into_any_element()));
        let mut children = rows(list, true, cx);
        children.push(
            div()
                .pt_4()
                .child(
                    div()
                        .mb_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(div().text_sm().child(t!("settings.termPreview").to_string()))
                        .child(
                            Button::new("term-reset")
                                .outline()
                                .xsmall()
                                .disabled(term == TermPref::default())
                                .label(t!("settings.termReset").to_string())
                                .on_click(cx.listener(|this, _, window, cx| this.reset_term(window, cx))),
                        ),
                )
                .child(term_preview(cx))
                .into_any_element(),
        );
        section(t!("settings.terminalTitle").to_string(), Some(t!("settings.terminalHint").to_string()), children, cx)
            .into_any_element()
    }
}

/// 界面缩放：− 百分比 +，不在 100% 时多一个复原（⌘+ / ⌘− 与「视图」菜单走同一组函数）
fn zoom_control(cx: &App) -> AnyElement {
    let ui = Ui::global(cx);
    let z = crate::zoom::zoom();
    let steps = crate::zoom::STEPS;
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new("ui-zoom-out")
                .outline()
                .xsmall()
                .icon(IconName::Minus)
                .tooltip(t!("native.settings.uiZoomOut").to_string())
                .disabled(z <= steps[0])
                .on_click(|_, _, cx| crate::zoom::step(-1, cx)),
        )
        .child(
            div()
                .w(zpx(44.))
                .text_center()
                .font_family(fonts::BERKELEY)
                .text_xs()
                .text_color(ui.muted_foreground)
                .child(crate::zoom::percent(z)),
        )
        .child(
            Button::new("ui-zoom-in")
                .outline()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip(t!("native.settings.uiZoomIn").to_string())
                .disabled(z >= steps[steps.len() - 1])
                .on_click(|_, _, cx| crate::zoom::step(1, cx)),
        )
        .child(
            Button::new("ui-zoom-reset")
                .outline()
                .xsmall()
                .disabled(z == 1.)
                .label(t!("native.settings.uiZoomReset").to_string())
                .on_click(|_, _, cx| crate::zoom::reset(cx)),
        )
        .into_any_element()
}

/// 终端预览：当前主题的终端配色 + 当前字体 / 字号 / 行高画几行典型输出（web 的 TermPreview），
/// 含一个 CJK 字与三个 Nerd 图标，换字体时一眼看出回退链是否接得上。
fn term_preview(cx: &App) -> AnyElement {
    let look = TerminalLook::global(cx);
    let ui = Ui::global(cx);
    let pal = &look.palette;
    let a = &pal.ansi;
    let span = |text: &str, color| div().text_color(color).child(SharedString::from(text.to_string()));
    let plain = |text: &str| div().child(SharedString::from(text.to_string()));
    let line = |parts: Vec<gpui_kit::Div>| div().flex().whitespace_nowrap().children(parts);
    let mut font = look.text.font.clone();
    font.weight = gpui_kit::FontWeight::NORMAL;
    div()
        .overflow_hidden()
        .rounded(radius::SM)
        .border_1()
        .border_color(ui.border)
        .bg(pal.background)
        .text_color(pal.foreground)
        .child(
            div()
                .p_3()
                .font(font)
                .text_size(look.text.font_size)
                .line_height(relative(look.text.line_height_factor))
                .flex()
                .flex_col()
                .child(line(vec![
                    span("fay", a[2]),
                    span("@", a[8]),
                    span("host", a[4]),
                    plain(" "),
                    span("~/falcon", a[6]),
                ]))
                .child(line(vec![span("$", a[5]), plain(" git status")]))
                .child(line(vec![span("On branch", a[3]), plain(" main")]))
                .child(line(vec![plain("nothing to commit, working tree clean")]))
                .child(line(vec![span("$", a[5]), plain(" echo 你好 · "), span("\u{e718}", a[2]), plain(" node")]))
                .child(line(vec![
                    span("$", a[5]),
                    plain(" "),
                    span("\u{f111}", a[1]),
                    plain(" "),
                    span("\u{f00c}", a[2]),
                ])),
        )
        .into_any_element()
}

impl Render for AppearancePane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .child(self.render_theme_section(cx))
            .child(self.render_term_section(cx))
    }
}
