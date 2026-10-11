//! 一套 Ghostty 主题颜色 → 整个应用要的全部东西：界面的语义色（shadcn token）、
//! 语法高亮色、终端配色、给 PTY 的深浅线索（`lib/theme/derive.ts`）。纯函数。
//!
//! 界面色全部从主题的底 / 字 / 16 色推出来，规则照 shadcn neutral 两套值反推：
//! "从底色往字色掺 t"就是 shadcn 那些 0.97 / 0.269 之类的灰阶（掺色在 OKLab 里做，
//! 见 color.rs）。深浅两套比例不对称是刻意的——shadcn 自己就是这样：深底上
//! muted-foreground 比浅底上离字色更近，因为同样的 WCAG 对比度在黑底上看着更暗。
//!
//! 语义色（destructive / success / warning）与高亮色取自 ANSI 16 色：先在普通色与
//! 亮色里挑对比度够的，都不够就往字色掺到 3:1——文字级可读性的底线；主题的红绿黄
//! 本来就是给终端里的文字用的，多数主题不需要掺。
//!
//! 界面是「浮动岛」骨架（ADR 0011）：`app` 是窗口底，侧栏 / 主区 / 右面板都是
//! 浮在它上面的圆角面板，面板底一律 `background`（终端要的就是主题原底色，面板与
//! 终端同色才不会在圆角边缘露出色差）。所以唯一能拉开层次的是 `app`，它必须跟
//! `background` 差得看得见。
//!
//! **界面色只准用这里派生出来的语义 token**，不许在组件里写死颜色；新增语义色就在
//! 这里加规则（当初是先在旧 React 版的 derive.ts 加、再照抄过来，React 版删掉后这里
//! 就是真相来源）。`tests/derive_golden.rs` 冻结着既有规则的输出，有意改规则要连
//! fixture 一起改；[`ResolvedTheme::css_vars`] 的键与 fixture 一一对应，往里加键也一样。
//!
//! apply.ts 的对应：React 版把 [`ResolvedTheme::css_vars`] 那张表写到 `<html>` 的内联
//! style 上、按 `appearance` 切 `.dark`、把 `<meta name="theme-color">` 设成底色。
//! GPUI 界面不经 DOM，app 层直接读强类型字段（转 gpui 的 Hsla 在 app 层做）；
//! `systemPrefersDark` / `watchSystemTheme` 对应 GPUI 的 `window.appearance()` 与
//! 其变化回调，也在 app 层。React 版 store 里按槽位对象缓存派生结果那一层（`WeakMap`，
//! 保证同一套主题拿到同一个终端配色对象）同样归 app 层。

use std::collections::BTreeMap;

use crate::appearance::{Appearance, appearance_from_rgb};
use crate::color::{
    Rgb, Rgba, ensure_contrast, mix, more_readable, perceptual_lightness, pick_readable, shift_lightness,
};
use crate::ghostty::ThemeColors;

/// 文字级最低对比度：13px 正文按 WCAG 该 4.5，主题自带色达不到时退到 3
const TEXT_MIN: f64 = 3.0;
const TEXT_WANT: f64 = 4.5;
/// 次要文字（muted-foreground）的底线，比语义色略高：它是成段的说明文字
const MUTED_MIN: f64 = 3.5;

/// 派生结果（TS 的 `ResolvedTheme`）。
#[derive(Clone, PartialEq, Debug)]
pub struct ResolvedTheme {
    pub colors: ThemeColors,
    /// 按底色亮度判的深浅，决定界面按深色还是浅色画（React 版的 `.dark`、color-scheme）、
    /// 给 PTY 的 COLORFGBG。
    /// **不是明暗模式**：浅色槽位里放一套深底主题，这里就是 Dark。
    pub appearance: Appearance,
    /// `appearance == Dark`，即 React 版里 `<html>` 有没有 `.dark`
    pub is_dark: bool,
    /// 界面语义 token
    pub ui: UiTokens,
    /// 语法高亮（shiki css-variables 主题那一组）
    pub syntax: SyntaxColors,
    /// 终端配色（xterm ITheme 那一组）
    pub terminal: TerminalColors,
    /// 建会话 / 换主题时发给服务端，供 OSC 10/11 应答与 COLORFGBG
    pub hint: TermHint,
}

/// 界面语义 token。每个字段对应 React 版写在 `<html>` 上的一个 CSS 变量（注释里的名字）。
///
/// 不透明的是 [`Rgb`]，带 alpha 的（边框、焦点环）是 [`Rgba`]。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UiTokens {
    /// `--background`：主题底色。岛（面板）、终端、卡片（浅色）的底都是它
    pub background: Rgb,
    /// `--foreground`：主题字色
    pub foreground: Rgb,
    /// `--app`：窗口底（浮动岛之间那道缝的颜色）。只动亮度不动色度
    pub app: Rgb,
    /// `--app-border`：岛的描边。深色靠提亮（字色 10%），浅色靠压暗（黑 6%）
    pub app_border: Rgba,
    /// `--tint`：「当前选中」的着色面（主题 ANSI 蓝往底色里掺）
    pub tint: Rgb,
    /// `--tint-foreground`
    pub tint_foreground: Rgb,
    /// `--tint-strong`：ANSI 蓝本身（语义蓝，活动列边框等）
    pub tint_strong: Rgb,
    /// `--card`
    pub card: Rgb,
    /// `--card-foreground`
    pub card_foreground: Rgb,
    /// `--popover`
    pub popover: Rgb,
    /// `--popover-foreground`
    pub popover_foreground: Rgb,
    /// `--primary`：浅色就是字色本身，深色比字色略暗一档
    pub primary: Rgb,
    /// `--primary-foreground`
    pub primary_foreground: Rgb,
    /// `--secondary`
    pub secondary: Rgb,
    /// `--secondary-foreground`
    pub secondary_foreground: Rgb,
    /// `--muted`（与 secondary 同值）
    pub muted: Rgb,
    /// `--muted-foreground`：次要文字，保底 3.5:1
    pub muted_foreground: Rgb,
    /// `--accent`：hover 等
    pub accent: Rgb,
    /// `--accent-foreground`
    pub accent_foreground: Rgb,
    /// `--destructive`：取自 ANSI 红
    pub destructive: Rgb,
    /// `--destructive-foreground`：按钮上的字。深色按钮是 `bg-destructive/60` 叠在底上，
    /// 字色对着实际填色挑
    pub destructive_foreground: Rgb,
    /// `--success`：取自 ANSI 绿
    pub success: Rgb,
    /// `--success-foreground`
    pub success_foreground: Rgb,
    /// `--warning`：取自 ANSI 黄
    pub warning: Rgb,
    /// `--warning-foreground`
    pub warning_foreground: Rgb,
    /// `--border`
    pub border: Rgba,
    /// `--input`
    pub input: Rgba,
    /// `--ring`
    pub ring: Rgba,
    /// `--sidebar`
    pub sidebar: Rgb,
    /// `--sidebar-foreground`
    pub sidebar_foreground: Rgb,
    /// `--sidebar-primary`
    pub sidebar_primary: Rgb,
    /// `--sidebar-primary-foreground`
    pub sidebar_primary_foreground: Rgb,
    /// `--sidebar-accent`
    pub sidebar_accent: Rgb,
    /// `--sidebar-accent-foreground`
    pub sidebar_accent_foreground: Rgb,
    /// `--sidebar-border`
    pub sidebar_border: Rgba,
    /// `--sidebar-ring`
    pub sidebar_ring: Rgba,
    /// `--host-s`：主机身份色的饱和度，百分数（34 = "34%"）。色相由 hostColor 哈希
    pub host_s: u8,
    /// `--host-l`：主机身份色的亮度，百分数（56 = "56%"）
    pub host_l: u8,
    /// `--graph-1` … `--graph-6`：提交图泳道色，色相尽量分开：蓝 黄 绿 紫 青 红
    pub graph: [Rgb; 6],
    /// `--selection`：界面文字选区，跟终端选区同色
    pub selection: Rgb,
    /// `--selection-foreground`：`None` 时 CSS 写 `currentcolor`（选中文字保留原色）
    pub selection_foreground: Option<Rgb>,
}

/// 语法高亮色：shiki 的 css-variables 主题读的那组变量（React 版的 `lib/highlight.ts`）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SyntaxColors {
    /// `--shiki-foreground`
    pub foreground: Rgb,
    /// `--shiki-background`
    pub background: Rgb,
    /// `--shiki-token-comment`：ANSI 亮黑往字色掺到 3:1
    pub comment: Rgb,
    /// `--shiki-token-keyword`：紫
    pub keyword: Rgb,
    /// `--shiki-token-string`：绿
    pub string: Rgb,
    /// `--shiki-token-string-expression`
    pub string_expression: Rgb,
    /// `--shiki-token-constant`：黄
    pub constant: Rgb,
    /// `--shiki-token-function`：蓝
    pub function: Rgb,
    /// `--shiki-token-parameter`
    pub parameter: Rgb,
    /// `--shiki-token-punctuation`
    pub punctuation: Rgb,
    /// `--shiki-token-link`
    pub link: Rgb,
    /// `--shiki-token-inserted`
    pub inserted: Rgb,
    /// `--shiki-token-deleted`
    pub deleted: Rgb,
    /// `--shiki-token-changed`
    pub changed: Rgb,
    /// `--shiki-ansi-black` … `--shiki-ansi-bright-white`（下标即 ANSI 0–15，名字见 [`SHIKI_ANSI`]）
    pub ansi: [Rgb; 16],
}

/// 终端配色。字段沿用 xterm.js `ITheme` 的形状（旧 React 版直接把它交给 xterm.js），字段注释是 ITheme 的键名。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TerminalColors {
    /// `background`
    pub background: Rgb,
    /// `foreground`
    pub foreground: Rgb,
    /// `cursor`（Ghostty 的 cursor-color）
    pub cursor: Rgb,
    /// `cursorAccent`：光标下的字色（Ghostty 的 cursor-text）
    pub cursor_accent: Rgb,
    /// `selectionBackground`
    pub selection_background: Rgb,
    /// `selectionForeground`：`None` = 不给，选中文字保留原色（Ghostty 的 cell-foreground）
    pub selection_foreground: Option<Rgb>,
    /// `black` `red` `green` `yellow` `blue` `magenta` `cyan` `white` `brightBlack` …
    /// `brightWhite`（ANSI 0–15，名字见 [`ANSI_NAMES`]）
    pub ansi: [Rgb; 16],
    /// `extendedAnsi`：只有主题写了 16–255 的覆盖时才给，给就是整份 240 色（16–255），
    /// 缺省按 xterm 256 色标准补齐（[`extended_ansi`]）
    pub extended_ansi: Option<Vec<Rgb>>,
}

/// 给服务端的深浅线索（OSC 10/11 应答与 COLORFGBG），对应 falcon-proto 的 `OscColorHint`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TermHint {
    pub appearance: Appearance,
    pub background: Rgb,
    pub foreground: Rgb,
}

/// xterm ITheme 里 ANSI 0–15 的键名
pub const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];

/// shiki css-variables 主题里 ANSI 0–15 的变量名后缀（`--shiki-ansi-<name>`）
pub const SHIKI_ANSI: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];

pub fn derive_theme(colors: &ThemeColors) -> ResolvedTheme {
    let bg = colors.background;
    let fg = colors.foreground;
    let appearance = appearance_from_rgb(bg);
    let dark = appearance.is_dark();
    let p = &colors.palette;

    // 底色往字色掺
    let tone = |t: f64| mix(bg, fg, t);
    // ANSI 语义色：普通色优先，亮色备选，都不够就往字色掺
    let sem = |i: usize| ensure_contrast(pick_readable(&[p[i], p[i + 8]], bg, TEXT_WANT), bg, TEXT_MIN, fg);

    // shadcn 浅色的 primary 就是字色本身，深色比字色略暗一档
    let primary = if dark { mix(fg, bg, 0.075) } else { fg };
    let secondary = tone(if dark { 0.15 } else { 0.035 });
    // 字色本身不够深（3024 Day 这类灰字）时按 shadcn 比例掺出来的 muted 会淡到读不出，保个底
    let muted_foreground = ensure_contrast(mix(fg, bg, if dark { 0.33 } else { 0.44 }), bg, MUTED_MIN, fg);
    let border = fg.with_alpha(0.1);
    let ring = fg.with_alpha(if dark { 0.45 } else { 0.3 });
    let destructive = sem(1);
    let success = sem(2);
    let warning = sem(3);
    let blue = sem(4);
    let magenta = sem(5);
    let cyan = sem(6);

    // 窗口底。方向是「内容亮、外壳暗」（Nova / macOS 都是这个方向，凹陷感来自它）：
    // 底色还压得动就压。压不动的（#000 那批，OKLab 亮度 ≤ 0.16）反过来提亮，凹陷感
    // 换成浮起感，圆角一样看得见——提亮量要保证落到 L≈0.21，纯黑上 +0.05 还是黑。
    // 只动亮度不动色度（shift_lightness）：Solarized 的暖米、Catppuccin 的紫灰、Nord
    // 的蓝灰都要留在外壳上，那是整屏最大的一块颜色，洗成中性灰就没有主题了。
    let bg_l = perceptual_lightness(bg);
    let app = if bg_l > 0.16 {
        shift_lightness(bg, if dark { -0.07 } else { -0.05 })
    } else {
        shift_lightness(bg, f64::max(0.065, 0.21 - bg_l))
    };
    // 岛的描边。深色靠提亮、浅色靠压暗，都只要一丝——真正分隔靠的是 app 那道缝。
    // 投影只对浅色有用（深色里投影落在更黑的窗口底上等于没画），深色的边缘全靠它。
    let app_border = if dark { fg.with_alpha(0.1) } else { Rgb::new(0, 0, 0).with_alpha(0.06) };

    // 「当前选中」的着色面。取主题自己的 ANSI 蓝——界面不引入主题以外的色相，
    // 单色主题里它自动退化成灰，不会有哪套主题被染坏。
    let tint = mix(bg, blue, if dark { 0.22 } else { 0.12 });

    let ui = UiTokens {
        background: bg,
        foreground: fg,
        app,
        app_border,
        tint,
        tint_foreground: fg,
        tint_strong: blue,
        card: if dark { tone(0.07) } else { bg },
        card_foreground: fg,
        popover: if dark { tone(0.15) } else { bg },
        popover_foreground: fg,
        primary,
        primary_foreground: bg,
        secondary,
        secondary_foreground: fg,
        muted: secondary,
        muted_foreground,
        accent: tone(if dark { 0.27 } else { 0.035 }),
        accent_foreground: fg,
        destructive,
        // button / badge 深色是 `bg-destructive/60`（叠在底上），浅色是实心。字色要对
        // 实际填色挑，否则深底上半透明红会把给实心粉红挑的深字铺上去（黑字压暗红）。
        destructive_foreground: more_readable(bg, fg, if dark { mix(bg, destructive, 0.6) } else { destructive }),
        success,
        success_foreground: more_readable(bg, fg, success),
        warning,
        warning_foreground: more_readable(bg, fg, warning),
        border,
        input: fg.with_alpha(if dark { 0.15 } else { 0.1 }),
        ring,
        sidebar: tone(if dark { 0.07 } else { 0.017 }),
        sidebar_foreground: fg,
        sidebar_primary: primary,
        sidebar_primary_foreground: bg,
        sidebar_accent: secondary,
        sidebar_accent_foreground: fg,
        sidebar_border: border,
        sidebar_ring: ring,
        // 主机身份色的饱和度 / 亮度：色相由 hostColor 哈希，深浅在这里按明暗定
        host_s: if dark { 34 } else { 48 },
        host_l: if dark { 56 } else { 38 },
        // 提交图泳道色，色相尽量分开：蓝 黄 绿 紫 青 红
        graph: [blue, warning, success, magenta, cyan, destructive],
        // 界面文字选区跟终端选区同色；selection-foreground 是 cell-foreground 时保留原色
        selection: colors.selection_background,
        selection_foreground: colors.selection_foreground,
    };

    let syntax = SyntaxColors {
        foreground: fg,
        background: bg,
        comment: ensure_contrast(p[8], bg, TEXT_MIN, fg),
        keyword: magenta,
        string: success,
        string_expression: success,
        constant: warning,
        function: blue,
        parameter: fg,
        punctuation: muted_foreground,
        link: blue,
        inserted: success,
        deleted: destructive,
        changed: warning,
        ansi: *p,
    };

    let terminal = TerminalColors {
        background: bg,
        foreground: fg,
        cursor: colors.cursor_color,
        cursor_accent: colors.cursor_text,
        selection_background: colors.selection_background,
        selection_foreground: colors.selection_foreground,
        ansi: *p,
        // TS 是 `if (colors.extended)`：手搓的空对象 `{}` 在那边也会给整份 240 色，
        // 这边空表 ≡ 没有。解析 / 清洗从不产出空对象，实际没有差别。
        extended_ansi: (!colors.extended.is_empty()).then(|| extended_ansi(&colors.extended)),
    };

    ResolvedTheme {
        colors: colors.clone(),
        appearance,
        is_dark: dark,
        ui,
        syntax,
        terminal,
        hint: TermHint { appearance, background: bg, foreground: fg },
    }
}

/// xterm 的 extendedAnsi 要整份 240 色（16–255）。缺省值按 xterm 256 色标准生成
/// （6×6×6 色立方 + 24 级灰），与 Ghostty `+show-config --default` 打出来的一致。
/// 覆盖表里 16 以下的下标忽略。
pub fn extended_ansi(overrides: &BTreeMap<u8, Rgb>) -> Vec<Rgb> {
    let step = |n: u8| if n == 0 { 0 } else { 55 + n * 40 };
    let mut out = Vec::with_capacity(240);
    for i in 0..216u8 {
        out.push(Rgb::new(step(i / 36), step((i / 6) % 6), step(i % 6)));
    }
    for i in 0..24u8 {
        let v = 8 + i * 10;
        out.push(Rgb::new(v, v, v));
    }
    for (&k, &v) in overrides {
        if k >= 16 {
            out[usize::from(k) - 16] = v;
        }
    }
    out
}

impl ResolvedTheme {
    /// React 版写到 `<html>` 上的整张 CSS 变量表，键与顺序都与 derive.ts 的 `vars` 相同，
    /// 值的文本形式也相同（`#rrggbb` / `#rrggbbaa` / `34%` / `currentcolor`）。
    ///
    /// 界面不需要它——字段直接读；它留着是为了与 React 版冻结下来的金标准 fixture
    /// 逐字对拍（`tests/derive_golden.rs`），排查时也能把输出与 fixture 摆在一起看。
    pub fn css_vars(&self) -> Vec<(&'static str, String)> {
        let u = &self.ui;
        let s = &self.syntax;
        let mut v: Vec<(&'static str, String)> = vec![
            ("--background", u.background.to_hex()),
            ("--foreground", u.foreground.to_hex()),
            ("--app", u.app.to_hex()),
            ("--app-border", u.app_border.to_hex()),
            ("--tint", u.tint.to_hex()),
            ("--tint-foreground", u.tint_foreground.to_hex()),
            ("--tint-strong", u.tint_strong.to_hex()),
            ("--card", u.card.to_hex()),
            ("--card-foreground", u.card_foreground.to_hex()),
            ("--popover", u.popover.to_hex()),
            ("--popover-foreground", u.popover_foreground.to_hex()),
            ("--primary", u.primary.to_hex()),
            ("--primary-foreground", u.primary_foreground.to_hex()),
            ("--secondary", u.secondary.to_hex()),
            ("--secondary-foreground", u.secondary_foreground.to_hex()),
            ("--muted", u.muted.to_hex()),
            ("--muted-foreground", u.muted_foreground.to_hex()),
            ("--accent", u.accent.to_hex()),
            ("--accent-foreground", u.accent_foreground.to_hex()),
            ("--destructive", u.destructive.to_hex()),
            ("--destructive-foreground", u.destructive_foreground.to_hex()),
            ("--success", u.success.to_hex()),
            ("--success-foreground", u.success_foreground.to_hex()),
            ("--warning", u.warning.to_hex()),
            ("--warning-foreground", u.warning_foreground.to_hex()),
            ("--border", u.border.to_hex()),
            ("--input", u.input.to_hex()),
            ("--ring", u.ring.to_hex()),
            ("--sidebar", u.sidebar.to_hex()),
            ("--sidebar-foreground", u.sidebar_foreground.to_hex()),
            ("--sidebar-primary", u.sidebar_primary.to_hex()),
            ("--sidebar-primary-foreground", u.sidebar_primary_foreground.to_hex()),
            ("--sidebar-accent", u.sidebar_accent.to_hex()),
            ("--sidebar-accent-foreground", u.sidebar_accent_foreground.to_hex()),
            ("--sidebar-border", u.sidebar_border.to_hex()),
            ("--sidebar-ring", u.sidebar_ring.to_hex()),
            ("--host-s", format!("{}%", u.host_s)),
            ("--host-l", format!("{}%", u.host_l)),
            ("--graph-1", u.graph[0].to_hex()),
            ("--graph-2", u.graph[1].to_hex()),
            ("--graph-3", u.graph[2].to_hex()),
            ("--graph-4", u.graph[3].to_hex()),
            ("--graph-5", u.graph[4].to_hex()),
            ("--graph-6", u.graph[5].to_hex()),
            ("--selection", u.selection.to_hex()),
            ("--selection-foreground", u.selection_foreground.map_or_else(|| "currentcolor".to_string(), Rgb::to_hex)),
            ("--shiki-foreground", s.foreground.to_hex()),
            ("--shiki-background", s.background.to_hex()),
            ("--shiki-token-comment", s.comment.to_hex()),
            ("--shiki-token-keyword", s.keyword.to_hex()),
            ("--shiki-token-string", s.string.to_hex()),
            ("--shiki-token-string-expression", s.string_expression.to_hex()),
            ("--shiki-token-constant", s.constant.to_hex()),
            ("--shiki-token-function", s.function.to_hex()),
            ("--shiki-token-parameter", s.parameter.to_hex()),
            ("--shiki-token-punctuation", s.punctuation.to_hex()),
            ("--shiki-token-link", s.link.to_hex()),
            ("--shiki-token-inserted", s.inserted.to_hex()),
            ("--shiki-token-deleted", s.deleted.to_hex()),
            ("--shiki-token-changed", s.changed.to_hex()),
        ];
        const SHIKI_ANSI_VARS: [&str; 16] = [
            "--shiki-ansi-black",
            "--shiki-ansi-red",
            "--shiki-ansi-green",
            "--shiki-ansi-yellow",
            "--shiki-ansi-blue",
            "--shiki-ansi-magenta",
            "--shiki-ansi-cyan",
            "--shiki-ansi-white",
            "--shiki-ansi-bright-black",
            "--shiki-ansi-bright-red",
            "--shiki-ansi-bright-green",
            "--shiki-ansi-bright-yellow",
            "--shiki-ansi-bright-blue",
            "--shiki-ansi-bright-magenta",
            "--shiki-ansi-bright-cyan",
            "--shiki-ansi-bright-white",
        ];
        v.extend(SHIKI_ANSI_VARS.iter().zip(s.ansi).map(|(&k, c)| (k, c.to_hex())));
        v
    }
}

/// 移植自 `derive.test.ts`，用例与断言值一一对应。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{FALCON_DARK, FALCON_LIGHT, find_theme, load_catalog, parse_catalog_data, GHOSTTY_THEMES_DATA};
    use crate::color::{contrast, parse_hex, rgb_to_oklab};
    use crate::ghostty::{parse_ghostty_theme, resolve_theme_colors};

    fn noctis_lux() -> ThemeColors {
        resolve_theme_colors(
            &parse_ghostty_theme(
                "background = #f6edda
foreground = #005661
selection-background = #d4e8e2
selection-foreground = #005661
cursor-color = #005661
cursor-text = #f6edda
palette = 0=#003b42
palette = 1=#e34e1c
palette = 2=#00b368
palette = 3=#f49725
palette = 4=#0094f0
palette = 5=#ff5792
palette = 6=#00bdd6
palette = 7=#8ca6a6
palette = 8=#004d57
palette = 9=#ff4000
palette = 10=#00d17a
palette = 11=#ff8c00
palette = 12=#0fa3ff
palette = 13=#ff6b9f
palette = 14=#00cbe6
palette = 15=#bbc3c4
",
            ),
            None,
        )
    }

    fn hex(s: &str) -> Rgb {
        Rgb::parse_hex(s).unwrap()
    }

    /// TS 的 `UI_TEXT_VARS`：--muted-foreground --destructive --success --warning
    fn ui_text(t: &ResolvedTheme) -> [(&'static str, Rgb); 4] {
        [
            ("--muted-foreground", t.ui.muted_foreground),
            ("--destructive", t.ui.destructive),
            ("--success", t.ui.success),
            ("--warning", t.ui.warning),
        ]
    }

    /// 语义色的可读性底线：3:1，但主题自己的字色都不到 3:1（C64 这类复古主题）时只要求不比字色差
    fn text_floor(colors: &ThemeColors) -> f64 {
        f64::min(3.0, contrast(colors.foreground, colors.background)) - 1e-9
    }

    fn all_entries() -> Vec<crate::catalog::CatalogEntry> {
        let mut v = vec![FALCON_LIGHT, FALCON_DARK];
        v.extend(parse_catalog_data(GHOSTTY_THEMES_DATA).unwrap());
        v
    }

    // describe("deriveTheme")

    /// 深浅按底色判，不按槽位
    #[test]
    fn appearance_by_background() {
        assert_eq!(derive_theme(&FALCON_DARK.colors).appearance, Appearance::Dark);
        assert_eq!(derive_theme(&FALCON_LIGHT.colors).appearance, Appearance::Light);
        assert_eq!(derive_theme(&noctis_lux()).appearance, Appearance::Light);
        assert!(derive_theme(&FALCON_DARK.colors).is_dark);
        assert!(!derive_theme(&noctis_lux()).is_dark);
    }

    /// Falcon 默认复现 shadcn neutral 的灰阶
    #[test]
    fn falcon_defaults_reproduce_shadcn_neutral() {
        let near = |c: Rgb, l: f64| (perceptual_lightness(c) - l).abs() < 0.012;
        let light = derive_theme(&FALCON_LIGHT.colors).ui;
        assert!(near(light.muted, 0.97), "{}", light.muted);
        assert!(near(light.muted_foreground, 0.556), "{}", light.muted_foreground);
        assert!(near(light.primary, 0.205), "{}", light.primary);
        assert_eq!(light.card.to_hex(), "#ffffff");
        let dark = derive_theme(&FALCON_DARK.colors).ui;
        assert!(near(dark.card, 0.205), "{}", dark.card);
        assert!(near(dark.popover, 0.269), "{}", dark.popover);
        assert!(near(dark.muted_foreground, 0.708), "{}", dark.muted_foreground);
        assert!(near(dark.accent, 0.371), "{}", dark.accent);
        assert_eq!(dark.border.to_hex(), "#fafafa1a");
    }

    /// 语义色来自 ANSI，且在底色上至少 3:1
    #[test]
    fn semantic_colors_from_ansi_readable() {
        let t = derive_theme(&FALCON_DARK.colors);
        // Tango 的普通红 #cc0000 在 #0a0a0a 上不到 4.5，亮红 #ef2929 够
        assert_eq!(t.ui.destructive.to_hex(), "#ef2929");
        for (key, c) in ui_text(&t) {
            assert!(contrast(c, t.colors.background) >= 3.0, "{key}={c}");
        }
        let lux = derive_theme(&noctis_lux());
        for (key, c) in ui_text(&lux) {
            assert!(contrast(c, lux.colors.background) >= 3.0, "{key}={c}");
        }
    }

    /// 语义色的字色在按钮实际填色上选底 / 字里对比更高的
    #[test]
    fn semantic_foreground_against_actual_fill() {
        let light = derive_theme(&FALCON_LIGHT.colors).ui;
        assert_eq!(light.destructive_foreground.to_hex(), "#ffffff");
        let dark = derive_theme(&FALCON_DARK.colors).ui;
        // 深色是 bg-destructive/60，#ef2929 叠到 #0a0a0a 上变暗红，浅字才读得出
        assert_eq!(dark.destructive_foreground.to_hex(), "#fafafa");
        let catalog = parse_catalog_data(GHOSTTY_THEMES_DATA).unwrap();
        let mocha = derive_theme(&find_theme(&catalog, "Catppuccin Mocha").unwrap().colors).ui;
        // 实心粉红 #f38ba8 上深底更清楚，但按钮叠 60% 之后浅字对比更高
        assert_eq!(mocha.destructive_foreground.to_hex(), "#cdd6f4");
    }

    /// 每个 var 都是合法颜色或百分比，全部主题无一例外
    #[test]
    fn every_var_valid_for_every_theme() {
        for e in all_entries() {
            let t = derive_theme(&e.colors);
            for (k, v) in t.css_vars() {
                let is_percent = v.strip_suffix('%').is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
                let ok = parse_hex(&v).is_some() || is_percent || v == "currentcolor";
                assert!(ok, "{} {k}={v}", e.name);
            }
            for (key, c) in ui_text(&t) {
                assert!(contrast(c, e.colors.background) >= text_floor(&e.colors), "{} {key}={c}", e.name);
            }
            assert_eq!(t.appearance, e.appearance);
        }
    }

    /// 窗口底与面板底在每套主题上都分得出，且保留主题的色调
    #[test]
    fn app_background_separates_and_keeps_hue() {
        for e in all_entries() {
            let app = derive_theme(&e.colors).ui.app;
            let gap = (perceptual_lightness(app) - perceptual_lightness(e.colors.background)).abs();
            // 差得比这少就看不出圆角了；近纯黑的主题是反过来提亮，绝对值一样管用
            assert!(gap >= 0.045, "{} app={app} bg={} gap={gap}", e.name, e.colors.background);
        }
        // 暖底主题的窗口底还是暖的（只压了亮度）
        let lux = derive_theme(&noctis_lux()).ui.app;
        let warm = rgb_to_oklab(lux);
        assert!(warm.b > 0.02, "{lux} b={}", warm.b);
    }

    /// xterm ITheme 逐字段映射；选区保留原字色时 selectionForeground 缺省
    #[test]
    fn terminal_colors_map_fields() {
        let t = derive_theme(&FALCON_DARK.colors).terminal;
        assert_eq!(t.background.to_hex(), "#0a0a0a");
        assert_eq!(t.cursor.to_hex(), "#fafafa");
        assert_eq!(t.cursor_accent.to_hex(), "#0a0a0a");
        assert_eq!(t.selection_background.to_hex(), "#3b3b3b");
        assert_eq!(t.selection_foreground, None);
        assert_eq!(t.ansi[0].to_hex(), "#2e3436"); // black
        assert_eq!(t.ansi[15].to_hex(), "#eeeeec"); // brightWhite
        assert_eq!(t.extended_ansi, None);
        let lux = derive_theme(&noctis_lux()).terminal;
        assert_eq!(lux.selection_foreground, Some(hex("#005661")));
    }

    /// hint 就是深浅 + 底字
    #[test]
    fn hint_is_appearance_and_colors() {
        assert_eq!(
            derive_theme(&FALCON_LIGHT.colors).hint,
            TermHint { appearance: Appearance::Light, background: hex("#ffffff"), foreground: hex("#171717") }
        );
    }

    /// shiki 变量齐全
    #[test]
    fn shiki_vars_complete() {
        let vars = derive_theme(&FALCON_DARK.colors).css_vars();
        let get = |k: &str| vars.iter().find(|(name, _)| *name == k).map(|(_, v)| v.clone());
        for name in [
            "comment",
            "keyword",
            "string",
            "string-expression",
            "constant",
            "function",
            "parameter",
            "punctuation",
            "link",
            "inserted",
            "deleted",
            "changed",
        ] {
            assert!(get(&format!("--shiki-token-{name}")).is_some(), "{name}");
        }
        assert_eq!(get("--shiki-ansi-bright-white").as_deref(), Some("#eeeeec"));
    }

    // describe("extendedAnsi")

    /// xterm 256 色标准立方与灰阶，与 Ghostty 默认一致
    #[test]
    fn extended_ansi_defaults() {
        let x = extended_ansi(&BTreeMap::new());
        assert_eq!(x.len(), 240);
        assert_eq!(x[0].to_hex(), "#000000");
        assert_eq!(x[1].to_hex(), "#00005f");
        assert_eq!(x[21 - 16].to_hex(), "#0000ff");
        assert_eq!(x[196 - 16].to_hex(), "#ff0000");
        assert_eq!(x[231 - 16].to_hex(), "#ffffff");
        assert_eq!(x[232 - 16].to_hex(), "#080808");
        assert_eq!(x[255 - 16].to_hex(), "#eeeeee");
    }

    /// 覆盖落到对应下标
    #[test]
    fn extended_ansi_overrides() {
        let x = extended_ansi(&BTreeMap::from([(16, hex("#123456")), (255, hex("#abcdef")), (3, hex("#000000"))]));
        assert_eq!(x[0].to_hex(), "#123456");
        assert_eq!(x[239].to_hex(), "#abcdef");
    }

    /// 带 extended 的主题给出整份 extendedAnsi
    #[test]
    fn theme_with_extended_gets_full_table() {
        let colors = ThemeColors { extended: BTreeMap::from([(16, hex("#123456"))]), ..FALCON_DARK.colors };
        let t = derive_theme(&colors);
        let ext = t.terminal.extended_ansi.expect("应当给整份 extendedAnsi");
        assert_eq!(ext.len(), 240);
        assert_eq!(ext[0].to_hex(), "#123456");
    }

    // 以下不是从 TS 移植的

    /// css_vars 与 React 版的 vars 同样 76 个键、无重复（写 CSS 变量的那张表一个不漏）
    #[test]
    fn css_vars_keys_unique() {
        let vars = derive_theme(&load_catalog()[0].colors).css_vars();
        assert_eq!(vars.len(), 76);
        let mut keys: Vec<&str> = vars.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 76);
    }
}
