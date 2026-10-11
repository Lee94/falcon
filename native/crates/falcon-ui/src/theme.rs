//! 主题桥接：falcon-theme 派生出的一整套颜色 → GPUI。
//!
//! 规则照 ADR 0006：一套 Ghostty 主题推出界面语义色、语法高亮色、终端配色；浅色 / 深色各一个
//! 槽位，明暗模式（跟随系统 / 浅 / 深）决定此刻用哪个槽位；`is_dark` 按主题底色亮度判，不按
//! 明暗模式。界面色只准用这里的语义 token，不许在组件里写死颜色。
//!
//! 三个出口：
//! - [`Ui`]：我们自己的组件用的语义 token（对应旧 React 版的 CSS 变量）；
//! - gpui-component 的 `Theme`：把同一套 token 映射到它的 134 个颜色字段上，组件库画出来的
//!   按钮 / 菜单 / 输入框与我们的界面同色；
//! - [`TerminalLook`]：终端配色 + 字体 + 光标 + 告诉服务端的外观（OscColorGate 代答用）。

use falcon_proto::{OscColorHint, TermAppearance};
use falcon_term::alacritty_terminal::vte::ansi::CursorShape;
use falcon_theme::{
    Appearance, ResolvedTheme, Rgb, Rgba as ThemeRgba, ThemeMode as SlotMode, ThemePref,
    ThemeSettings, derive_theme, load_theme_settings, resolve_theme_mode, save_theme_settings,
};
use gpui_kit::component::theme::{Theme as KitTheme, ThemeMode as KitMode};
use gpui_kit::{App, FontWeight, Global, Hsla, Rgba, Window, WindowAppearance, px};

use crate::fonts;
use crate::prefs::Prefs;
use crate::zoom::zpx;
use crate::terminal::element::{TermPalette, TermTextStyle};

pub fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

pub fn hsla_a(c: ThemeRgba) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: c.a as f32 / 255.0,
    }
    .into()
}

/// 界面语义 token（字段名沿用 React 版的 CSS 变量，去掉 `--`、连字符换下划线）。
#[derive(Clone, Debug)]
pub struct Ui {
    pub is_dark: bool,
    pub background: Hsla,
    pub foreground: Hsla,
    /// 窗口底（岛与岛之间露出来的那层，ADR 0011）
    pub app: Hsla,
    pub app_border: Hsla,
    /// 当前选中的弱着色面
    pub tint: Hsla,
    pub tint_foreground: Hsla,
    /// 语义蓝本身（活动窗口的描边）
    pub tint_strong: Hsla,
    // 与 falcon-theme 的 UiTokens 一一对应，界面暂时没有用到的组件，保留着便于对照
    #[allow(dead_code)]
    pub card: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub destructive: Hsla,
    pub destructive_foreground: Hsla,
    pub success: Hsla,
    pub warning: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub ring: Hsla,
    pub selection: Hsla,
    pub graph: [Hsla; 6],
    /// 主机身份色的饱和度 / 亮度（百分比，hostColor 用）
    pub host_s: u8,
    pub host_l: u8,
    pub syntax: Syntax,
}

#[derive(Clone, Debug)]
pub struct Syntax {
    pub foreground: Hsla,
    #[allow(dead_code)] // React 版的 shiki 变量里有，这里的高亮映射暂不区分
    pub background: Hsla,
    pub comment: Hsla,
    pub keyword: Hsla,
    pub string: Hsla,
    pub string_expression: Hsla,
    pub constant: Hsla,
    pub function: Hsla,
    #[allow(dead_code)] // React 版的 shiki 变量里有，这里的高亮映射暂不区分
    pub parameter: Hsla,
    pub punctuation: Hsla,
    pub link: Hsla,
    #[allow(dead_code)] // React 版的 shiki 变量里有，这里的高亮映射暂不区分
    pub inserted: Hsla,
    #[allow(dead_code)] // React 版的 shiki 变量里有，这里的高亮映射暂不区分
    pub deleted: Hsla,
    #[allow(dead_code)] // React 版的 shiki 变量里有，这里的高亮映射暂不区分
    pub changed: Hsla,
}

impl Global for Ui {}

impl Ui {
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

/// 圆角阶梯（ADR 0011：`--radius` 12px 减出来的 sm 8 / md 10 / lg 12 / xl 16）
pub mod radius {
    use gpui_kit::{Pixels, px};
    pub const SM: Pixels = px(8.);
    pub const MD: Pixels = px(10.);
    pub const LG: Pixels = px(12.);
    pub const XL: Pixels = px(16.);
}

/// 岛之间的缝（ADR 0011）
pub const GAP: gpui_kit::Pixels = px(6.);

/// 终端偏好（偏好键 `falcon.term`，沿用 React 版）。
#[derive(Clone, Debug, PartialEq)]
pub struct TermPrefs {
    pub family: String,
    pub font_size: f32,
    pub line_height: f32,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
}

impl Default for TermPrefs {
    fn default() -> Self {
        Self {
            family: fonts::BERKELEY.to_string(),
            font_size: 13.0,
            line_height: 1.0,
            cursor_shape: CursorShape::Block,
            cursor_blink: true,
        }
    }
}

impl TermPrefs {
    /// 读 `falcon.term`，清洗规则在 falcon-core（照 React 版的口径）
    pub fn load(prefs: &Prefs) -> Self {
        use falcon_core::term::{TERM_PREF_KEY, load_term_pref};
        Self::from_pref(&load_term_pref(prefs.get(TERM_PREF_KEY)))
    }

    /// 偏好（形状沿用 React 版）→ 终端渲染用的这份。`system` 交给 Menlo（React 版是整条交给
    /// 系统回退栈，GPUI 没有"系统等宽"这个名字）；`custom` 空着回落默认字体
    pub fn from_pref(pref: &falcon_core::term::TermPref) -> Self {
        use falcon_core::term::{TermCursorStyle, TermFontId, primary_family};
        let family = match pref.font_id {
            TermFontId::System => "Menlo".to_string(),
            _ => primary_family(pref).unwrap_or_else(|| fonts::BERKELEY.to_string()),
        };
        Self {
            family,
            font_size: pref.font_size as f32,
            line_height: pref.line_height as f32,
            cursor_shape: match pref.cursor_style {
                TermCursorStyle::Block => CursorShape::Block,
                TermCursorStyle::Bar => CursorShape::Beam,
                TermCursorStyle::Underline => CursorShape::Underline,
            },
            cursor_blink: pref.cursor_blink,
        }
    }
}

/// 终端的外观：配色、排版、光标，以及告诉服务端的深浅 + 底字色。
#[derive(Clone, Debug)]
pub struct TerminalLook {
    pub palette: TermPalette,
    pub text: TermTextStyle,
    pub cursor_shape: CursorShape,
    pub cursor_blink: bool,
    pub appearance: OscColorHint,
}

impl Global for TerminalLook {}

impl TerminalLook {
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

/// 主题状态：偏好 + 当前生效的派生结果。
pub struct ThemeState {
    pub settings: ThemeSettings,
    pub resolved: ResolvedTheme,
    pub term: TermPrefs,
    /// 系统此刻是不是深色（跟随系统时用）
    pub system_dark: bool,
}

impl Global for ThemeState {}

impl ThemeState {
    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

pub fn init(cx: &mut App) {
    let settings = load_theme_settings(Some(Prefs::global(cx) as &dyn falcon_theme::StorageLike));
    let term = TermPrefs::load(Prefs::global(cx));
    let system_dark = matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let mode = resolve_theme_mode(settings.mode, system_dark);
    let resolved = derive_theme(&settings.slot(mode).colors);
    cx.set_global(ThemeState {
        settings,
        resolved,
        term,
        system_dark,
    });
    apply(cx);
}

/// 窗口的外观变了（系统切了明暗）：跟随系统时重新派生。
pub fn on_window_appearance(window: &Window, cx: &mut App) {
    let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    if ThemeState::global(cx).system_dark == dark {
        return;
    }
    cx.global_mut::<ThemeState>().system_dark = dark;
    recompute(cx);
}

/// 改偏好（设置 / 主题选择器调用）：落盘并立即生效。
pub fn update_settings(cx: &mut App, f: impl FnOnce(&mut ThemeSettings)) {
    let mut settings = ThemeState::global(cx).settings.clone();
    f(&mut settings);
    save_theme_settings(Some(cx.global_mut::<Prefs>() as &mut dyn falcon_theme::StorageLike), &settings);
    cx.global_mut::<ThemeState>().settings = settings;
    recompute(cx);
}

pub fn update_term(cx: &mut App, term: TermPrefs, raw_json: String) {
    cx.global_mut::<Prefs>().set("falcon.term", raw_json);
    cx.global_mut::<ThemeState>().term = term;
    apply(cx);
}

/// 预览一套主题（主题选择器高亮到哪项就实时预览哪项），不落盘。
pub fn preview(cx: &mut App, resolved: Option<ResolvedTheme>) {
    match resolved {
        Some(r) => {
            cx.global_mut::<ThemeState>().resolved = r;
            apply(cx);
        }
        None => recompute(cx),
    }
}

fn recompute(cx: &mut App) {
    let state = ThemeState::global(cx);
    let mode: SlotMode = resolve_theme_mode(state.settings.mode, state.system_dark);
    let resolved = derive_theme(&state.settings.slot(mode).colors);
    cx.global_mut::<ThemeState>().resolved = resolved;
    apply(cx);
}

pub fn current_mode_pref(cx: &App) -> ThemePref {
    ThemeState::global(cx).settings.mode
}

/// 界面缩放变了：组件库字号（= rem）按新倍数重设，并 refresh_windows
pub fn reapply(cx: &mut App) {
    apply(cx);
}

fn apply(cx: &mut App) {
    let state = ThemeState::global(cx);
    let r = &state.resolved;
    let u = &r.ui;
    let s = &r.syntax;
    let ui = Ui {
        is_dark: r.is_dark,
        background: hsla(u.background),
        foreground: hsla(u.foreground),
        app: hsla(u.app),
        app_border: hsla_a(u.app_border),
        tint: hsla(u.tint),
        tint_foreground: hsla(u.tint_foreground),
        tint_strong: hsla(u.tint_strong),
        card: hsla(u.card),
        popover: hsla(u.popover),
        popover_foreground: hsla(u.popover_foreground),
        primary: hsla(u.primary),
        primary_foreground: hsla(u.primary_foreground),
        secondary: hsla(u.secondary),
        secondary_foreground: hsla(u.secondary_foreground),
        muted: hsla(u.muted),
        muted_foreground: hsla(u.muted_foreground),
        accent: hsla(u.accent),
        accent_foreground: hsla(u.accent_foreground),
        destructive: hsla(u.destructive),
        destructive_foreground: hsla(u.destructive_foreground),
        success: hsla(u.success),
        warning: hsla(u.warning),
        border: hsla_a(u.border),
        input: hsla_a(u.input),
        ring: hsla_a(u.ring),
        selection: hsla(u.selection),
        graph: u.graph.map(hsla),
        host_s: u.host_s,
        host_l: u.host_l,
        syntax: Syntax {
            foreground: hsla(s.foreground),
            background: hsla(s.background),
            comment: hsla(s.comment),
            keyword: hsla(s.keyword),
            string: hsla(s.string),
            string_expression: hsla(s.string_expression),
            constant: hsla(s.constant),
            function: hsla(s.function),
            parameter: hsla(s.parameter),
            punctuation: hsla(s.punctuation),
            link: hsla(s.link),
            inserted: hsla(s.inserted),
            deleted: hsla(s.deleted),
            changed: hsla(s.changed),
        },
    };

    let t = &r.terminal;
    let term = &state.term;
    let look = TerminalLook {
        palette: TermPalette {
            foreground: hsla(t.foreground),
            background: hsla(t.background),
            cursor: hsla(t.cursor),
            cursor_text: hsla(t.cursor_accent),
            selection_background: hsla(t.selection_background),
            selection_foreground: t.selection_foreground.map(hsla),
            ansi: t.ansi.map(hsla),
            extended: t.extended_ansi.as_ref().map(|ext| ext.iter().copied().map(hsla).collect()),
        },
        text: TermTextStyle {
            font: {
                let mut f = fonts::mono(term.family.clone());
                f.weight = FontWeight::NORMAL;
                f
            },
            font_size: px(term.font_size),
            line_height_factor: term.line_height,
        },
        cursor_shape: term.cursor_shape,
        cursor_blink: term.cursor_blink,
        appearance: OscColorHint {
            appearance: Some(match r.hint.appearance {
                Appearance::Light => TermAppearance::Light,
                Appearance::Dark => TermAppearance::Dark,
            }),
            background: Some(r.hint.background.to_hex()),
            foreground: Some(r.hint.foreground.to_hex()),
        },
    };

    apply_kit_theme(&ui, cx);
    cx.set_global(ui);
    cx.set_global(look);
    cx.refresh_windows();
}

/// 把语义 token 映射到 gpui-component 的颜色字段上。映射的原则：组件库的"面"一律落在我们的
/// 语义色上（popover / muted / accent / primary …），hover / active 用 tint 与 muted 推。
fn apply_kit_theme(ui: &Ui, cx: &mut App) {
    // 先让组件库按深浅装上它自己的默认值（字体、圆角、滚动条等非颜色设置），再覆盖颜色
    KitTheme::change(if ui.is_dark { KitMode::Dark } else { KitMode::Light }, None, cx);
    let theme = KitTheme::global_mut(cx);
    theme.font_family = fonts::BERKELEY.into();
    theme.mono_font_family = fonts::BERKELEY.into();
    // font_size 就是窗口的 rem（Root 每帧拿它 set_rem_size），必须与浏览器同为 16px，缩放只乘倍数；
    // 正文 13px 显式挂在窗口根上（见 zoom.rs）
    theme.font_size = zpx(16.);
    theme.mono_font_size = zpx(13.);
    theme.radius = radius::SM;
    theme.radius_lg = radius::LG;
    theme.shadow = true;

    let c = &mut theme.colors;
    c.background = ui.background;
    c.foreground = ui.foreground;
    c.border = ui.border;
    c.input = ui.input;
    c.ring = ui.ring;
    c.caret = ui.foreground;
    c.selection = ui.selection.opacity(0.6);
    c.primary = ui.primary;
    c.primary_foreground = ui.primary_foreground;
    c.primary_hover = ui.primary.opacity(0.9);
    c.primary_active = ui.primary.opacity(0.8);
    c.secondary = ui.secondary;
    c.secondary_foreground = ui.secondary_foreground;
    c.secondary_hover = ui.muted;
    c.secondary_active = ui.muted;
    c.muted = ui.muted;
    c.muted_foreground = ui.muted_foreground;
    c.accent = ui.accent;
    c.accent_foreground = ui.accent_foreground;
    c.popover = ui.popover;
    c.popover_foreground = ui.popover_foreground;
    c.danger = ui.destructive;
    c.danger_foreground = ui.destructive_foreground;
    c.danger_hover = ui.destructive.opacity(0.9);
    c.danger_active = ui.destructive.opacity(0.8);
    c.success = ui.success;
    c.warning = ui.warning;
    c.link = ui.syntax.link;
    c.link_hover = ui.syntax.link;
    c.link_active = ui.syntax.link;
    c.list = ui.background;
    c.list_hover = ui.muted;
    c.list_active = ui.tint;
    c.list_active_border = ui.tint;
    c.list_even = ui.background;
    c.list_head = ui.background;
    c.table = ui.background;
    c.table_hover = ui.muted;
    c.table_active = ui.tint;
    c.table_active_border = ui.tint;
    c.table_even = ui.background;
    c.table_head = ui.background;
    c.table_head_foreground = ui.muted_foreground;
    c.table_row_border = ui.border;
    c.sidebar = ui.background;
    c.sidebar_foreground = ui.foreground;
    c.sidebar_accent = ui.tint;
    c.sidebar_accent_foreground = ui.tint_foreground;
    c.sidebar_border = ui.border;
    c.sidebar_primary = ui.primary;
    c.sidebar_primary_foreground = ui.primary_foreground;
    c.tab_bar = ui.app;
    c.tab = ui.app;
    c.tab_active = ui.background;
    c.tab_active_foreground = ui.foreground;
    c.tab_foreground = ui.muted_foreground;
    c.tab_bar_segmented = ui.app;
    c.title_bar = ui.app;
    c.title_bar_border = ui.app;
    c.overlay = gpui_kit::black().opacity(if ui.is_dark { 0.5 } else { 0.25 });
    c.scrollbar = gpui_kit::transparent_black();
    c.scrollbar_thumb = ui.muted_foreground.opacity(0.35);
    c.scrollbar_thumb_hover = ui.muted_foreground.opacity(0.55);
    c.drop_target = ui.tint.opacity(0.6);
    c.drag_border = ui.primary;
    c.window_border = ui.app_border;
    c.chart_1 = ui.graph[0];
    c.chart_2 = ui.graph[1];
    c.chart_3 = ui.graph[2];
    c.chart_4 = ui.graph[3];
    c.chart_5 = ui.graph[4];
    // 对话框等组件读的是 tokens 而不是 colors：只改 colors 的话，换了主题对话框底还是旧色
    theme.tokens = (&theme.colors).into();
    // 代码查看器、diff、Markdown 代码块都读组件库的高亮主题：跟着界面语法色一起换
    theme.highlight_theme = std::sync::Arc::new(highlight_theme(ui));
    KitTheme::sync_base(cx);
}

/// tree-sitter 的高亮名 → 主题语法色。对照的是 shiki css-variables 主题把 TextMate 作用域
/// 分到哪个 `--shiki-token-*`（React 版的高亮就是它），沿用它给代码上色的口径：
/// 带引号的字符串在 shiki 里落 `string-expression`、类型名与属性名落 `function`、`this` / 数字 /
/// 布尔落 `constant`、运算符算 `keyword`。认不出的名字不上色（用前景色）。
fn highlight_theme(ui: &Ui) -> gpui_kit::component::highlighter::HighlightTheme {
    use gpui_kit::component::highlighter::HighlightTheme;
    let s = &ui.syntax;
    let color = |c: Hsla| serde_json::json!({ "color": c });
    let bold = serde_json::json!({ "font_weight": 700 });
    let syntax: serde_json::Map<String, serde_json::Value> = [
        ("comment", color(s.comment)),
        ("comment.doc", color(s.comment)),
        ("string", color(s.string_expression)),
        ("string.regex", color(s.string_expression)),
        ("string.special", color(s.string_expression)),
        ("string.escape", color(s.constant)),
        ("string.special.symbol", color(s.constant)),
        ("text.literal", color(s.string)),
        ("text.code.span", color(s.string)),
        ("keyword", color(s.keyword)),
        ("operator", color(s.keyword)),
        ("preproc", color(s.keyword)),
        ("label", color(s.keyword)),
        ("tag.doctype", color(s.keyword)),
        ("punctuation.special", color(s.keyword)),
        ("link_text", color(s.keyword)),
        ("constant", color(s.constant)),
        ("boolean", color(s.constant)),
        ("number", color(s.constant)),
        ("variant", color(s.constant)),
        ("variable.special", color(s.constant)),
        ("function", color(s.function)),
        ("constructor", color(s.function)),
        ("type", color(s.function)),
        ("enum", color(s.function)),
        ("attribute", color(s.function)),
        ("tag", color(s.string_expression)),
        ("punctuation", color(s.punctuation)),
        ("punctuation.bracket", color(s.punctuation)),
        ("punctuation.delimiter", color(s.punctuation)),
        ("punctuation.list_marker", color(s.string)),
        ("link_uri", color(s.link)),
        ("title", bold.clone()),
        ("emphasis", serde_json::json!({ "font_style": "italic" })),
        ("emphasis.strong", bold),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let json = serde_json::json!({
        "name": "falcon",
        "appearance": if ui.is_dark { "dark" } else { "light" },
        "style": {
            "editor.background": ui.background,
            "editor.foreground": s.foreground,
            "editor.gutter.background": ui.background,
            "editor.line_number": ui.muted_foreground.opacity(0.6),
            "editor.active_line_number": ui.muted_foreground,
            "syntax": syntax,
        }
    });
    serde_json::from_value(json).unwrap_or_else(|err| {
        log::warn!("语法高亮主题映射失败，退回组件库默认：{err}");
        (*if ui.is_dark { HighlightTheme::default_dark() } else { HighlightTheme::default_light() }).clone()
    })
}

