//! Ghostty 主题文件格式：解析、补默认值、序列化（`lib/theme/ghostty.ts`）。纯函数。
//!
//! 主题就是一段 Ghostty 配置，只认这几个键（其余键原样忽略，所以整份
//! `~/.config/ghostty/config` 贴进来也能用）：
//!
//! ```text
//! background / foreground                 = #rrggbb | rrggbb | X11 颜色名
//! cursor-color / cursor-text              = 同上 | cell-foreground | cell-background
//! selection-background / -foreground      = 同上 | cell-foreground | cell-background
//! palette                                 = N=颜色   （N 0–255，可重复出现）
//! theme                                   = 名字 | light:名字,dark:名字（先以它为底再覆盖）
//! ```
//!
//! 缺省语义照 Ghostty 文档：cursor-color 缺省用前景色、cursor-text 缺省用背景色；
//! selection-* 缺省是"窗口前景 / 背景互换"（不是格子的）；selection-foreground 写
//! cell-foreground 表示选中文字保留原色，这是 xterm.js 不给 selectionForeground 的
//! 那种效果，所以 [`ThemeColors`] 里用 `None` 表示。
//!
//! 颜色名按 X11 rgb.txt（Ghostty 认的是这张表），与 CSS 名字有四处不一样：
//! green / gray / maroon / purple，这里取 X11 的值；gray0–gray100 按公式生成。

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use serde::{Deserialize, Deserializer, Serialize};

use crate::color::Rgb;
use crate::js::{is_js_line_terminator, is_js_whitespace, js_trim};

/// 一套完整的主题颜色。偏好里存的"颜色副本"就是它，JSON 形状与 TS 的 `ThemeColors`
/// 逐键相同（camelCase，颜色是小写 `#rrggbb`）。
///
/// 反序列化走 `pref::sanitize_theme_colors` 的清洗规则：缺键 / 非法色 / 调色板不是
/// 16 个都整份不认。
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor_color: Rgb,
    pub cursor_text: Rgb,
    pub selection_background: Rgb,
    /// `None` = 选中文字保留原字色（Ghostty 的 cell-foreground）；JSON 里是 `null`
    pub selection_foreground: Option<Rgb>,
    /// ANSI 0–15
    pub palette: [Rgb; 16],
    /// 16–255 的覆盖。Ghostty 允许，内置主题从来不写，用户主题偶尔有。
    ///
    /// TS 里是可选字段；这边**空表 ≡ 没有**（序列化时空表不写，与 TS 省掉该键一致）。
    /// TS 的解析 / 清洗从不产出空的 `extended: {}`，所以两边没有歧义。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub extended: BTreeMap<u8, Rgb>,
}

impl<'de> Deserialize<'de> for ThemeColors {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        crate::pref::sanitize_theme_colors(&v)
            .ok_or_else(|| serde::de::Error::custom("主题颜色不完整或不合法"))
    }
}

/// 颜色值，或 `cell-foreground` / `cell-background` 特殊值（TS: `string | CellRef`）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorSpec {
    Color(Rgb),
    CellForeground,
    CellBackground,
}

/// 一段 Ghostty 文本里写了什么。没写的键就是 `None`，交给 [`resolve_theme_colors`] 补。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct GhosttyThemeSource {
    pub theme: Option<String>,
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor_color: Option<ColorSpec>,
    pub cursor_text: Option<ColorSpec>,
    pub selection_background: Option<ColorSpec>,
    pub selection_foreground: Option<ColorSpec>,
    /// 下标 0–255
    pub palette: BTreeMap<u8, Rgb>,
    /// 识别出的颜色赋值条数（palette 每条算一条）。0 = 这段文字里根本没有主题
    pub recognized: usize,
}

/// Ghostty 自己的默认配色（`ghostty +show-config --default`，1.3.1），
/// 只在用户贴的主题缺键时兜底。
pub const GHOSTTY_DEFAULT_BACKGROUND: Rgb = Rgb::from_u32(0x282c34);
pub const GHOSTTY_DEFAULT_FOREGROUND: Rgb = Rgb::from_u32(0xffffff);
pub const GHOSTTY_DEFAULT_PALETTE: [Rgb; 16] = [
    Rgb::from_u32(0x1d1f21),
    Rgb::from_u32(0xcc6666),
    Rgb::from_u32(0xb5bd68),
    Rgb::from_u32(0xf0c674),
    Rgb::from_u32(0x81a2be),
    Rgb::from_u32(0xb294bb),
    Rgb::from_u32(0x8abeb7),
    Rgb::from_u32(0xc5c8c6),
    Rgb::from_u32(0x666666),
    Rgb::from_u32(0xd54e53),
    Rgb::from_u32(0xb9ca4a),
    Rgb::from_u32(0xe7c547),
    Rgb::from_u32(0x7aa6da),
    Rgb::from_u32(0xc397d8),
    Rgb::from_u32(0x70c0b1),
    Rgb::from_u32(0xeaeaea),
];

const X11_NAMES: &str = concat!(
    "aliceblue f0f8ff antiquewhite faebd7 aqua 00ffff aquamarine 7fffd4 azure f0ffff beige f5f5dc ",
    "bisque ffe4c4 black 000000 blanchedalmond ffebcd blue 0000ff blueviolet 8a2be2 brown a52a2a ",
    "burlywood deb887 cadetblue 5f9ea0 chartreuse 7fff00 chocolate d2691e coral ff7f50 ",
    "cornflowerblue 6495ed cornsilk fff8dc crimson dc143c cyan 00ffff darkblue 00008b darkcyan 008b8b ",
    "darkgoldenrod b8860b darkgray a9a9a9 darkgreen 006400 darkgrey a9a9a9 darkkhaki bdb76b ",
    "darkmagenta 8b008b darkolivegreen 556b2f darkorange ff8c00 darkorchid 9932cc darkred 8b0000 ",
    "darksalmon e9967a darkseagreen 8fbc8f darkslateblue 483d8b darkslategray 2f4f4f ",
    "darkslategrey 2f4f4f darkturquoise 00ced1 darkviolet 9400d3 deeppink ff1493 deepskyblue 00bfff ",
    "dimgray 696969 dimgrey 696969 dodgerblue 1e90ff firebrick b22222 floralwhite fffaf0 ",
    "forestgreen 228b22 fuchsia ff00ff gainsboro dcdcdc ghostwhite f8f8ff gold ffd700 goldenrod daa520 ",
    "gray bebebe green 00ff00 greenyellow adff2f grey bebebe honeydew f0fff0 hotpink ff69b4 ",
    "indianred cd5c5c indigo 4b0082 ivory fffff0 khaki f0e68c lavender e6e6fa lavenderblush fff0f5 ",
    "lawngreen 7cfc00 lemonchiffon fffacd lightblue add8e6 lightcoral f08080 lightcyan e0ffff ",
    "lightgoldenrodyellow fafad2 lightgray d3d3d3 lightgreen 90ee90 lightgrey d3d3d3 lightpink ffb6c1 ",
    "lightsalmon ffa07a lightseagreen 20b2aa lightskyblue 87cefa lightslategray 778899 ",
    "lightslategrey 778899 lightsteelblue b0c4de lightyellow ffffe0 lime 00ff00 limegreen 32cd32 ",
    "linen faf0e6 magenta ff00ff maroon b03060 mediumaquamarine 66cdaa mediumblue 0000cd ",
    "mediumorchid ba55d3 mediumpurple 9370db mediumseagreen 3cb371 mediumslateblue 7b68ee ",
    "mediumspringgreen 00fa9a mediumturquoise 48d1cc mediumvioletred c71585 midnightblue 191970 ",
    "mintcream f5fffa mistyrose ffe4e1 moccasin ffe4b5 navajowhite ffdead navy 000080 navyblue 000080 ",
    "oldlace fdf5e6 olive 808000 olivedrab 6b8e23 orange ffa500 orangered ff4500 orchid da70d6 ",
    "palegoldenrod eee8aa palegreen 98fb98 paleturquoise afeeee palevioletred db7093 papayawhip ffefd5 ",
    "peachpuff ffdab9 peru cd853f pink ffc0cb plum dda0dd powderblue b0e0e6 purple a020f0 ",
    "rebeccapurple 663399 red ff0000 rosybrown bc8f8f royalblue 4169e1 saddlebrown 8b4513 ",
    "salmon fa8072 sandybrown f4a460 seagreen 2e8b57 seashell fff5ee sienna a0522d silver c0c0c0 ",
    "skyblue 87ceeb slateblue 6a5acd slategray 708090 slategrey 708090 snow fffafa springgreen 00ff7f ",
    "steelblue 4682b4 tan d2b48c teal 008080 thistle d8bfd8 tomato ff6347 turquoise 40e0d0 ",
    "violet ee82ee webgray 808080 webgreen 008000 webgrey 808080 webmaroon 800000 webpurple 800080 ",
    "wheat f5deb3 white ffffff whitesmoke f5f5f5 yellow ffff00 yellowgreen 9acd32",
);

/// 颜色名表只在第一次遇到颜色名时建（多数主题全是 #rrggbb，一辈子用不到）
fn named_color(name: &str) -> Option<Rgb> {
    static NAMED: OnceLock<HashMap<String, Rgb>> = OnceLock::new();
    let map = NAMED.get_or_init(|| {
        let mut map = HashMap::new();
        let parts: Vec<&str> = X11_NAMES.split(' ').collect();
        for pair in parts.chunks_exact(2) {
            let v = u32::from_str_radix(pair[1], 16).expect("X11 颜色表写错了");
            map.insert(pair[0].to_string(), Rgb::from_u32(v));
        }
        for n in 0..=100u32 {
            // 与 TS 同一公式：减 1e-9 让 .5 的格子往下取（gray50 = #7f7f7f，不是 #808080）
            let v = ((f64::from(n) * 255.0) / 100.0 - 1e-9).round() as u8;
            map.insert(format!("gray{n}"), Rgb::new(v, v, v));
            map.insert(format!("grey{n}"), Rgb::new(v, v, v));
        }
        map
    });
    let key: String = name
        .to_lowercase()
        .chars()
        .filter(|&c| !(is_js_whitespace(c) || c == '_' || c == '-'))
        .collect();
    map.get(&key).copied()
}

/// `/^#?([\da-f]{n})$/i`
fn hex_digits(v: &str, n: usize) -> Option<&str> {
    let body = v.strip_prefix('#').unwrap_or(v);
    (body.len() == n && body.bytes().all(|c| c.is_ascii_hexdigit())).then_some(body)
}

/// 一个 Ghostty 颜色值 → 颜色；认不出返回 `None`。
pub fn parse_ghostty_color(value: &str) -> Option<Rgb> {
    let v = unquote(value);
    if let Some(h) = hex_digits(v, 6) {
        return u32::from_str_radix(h, 16).ok().map(Rgb::from_u32);
    }
    // Ghostty 不认 3 位缩写，但用户从 CSS 那边搬来的主题偶尔这么写，宽容一点
    if let Some(h) = hex_digits(v, 3) {
        let d = |i: usize| u8::from_str_radix(&h[i..=i], 16).map(|x| x * 17).ok();
        return Some(Rgb::new(d(0)?, d(1)?, d(2)?));
    }
    named_color(v)
}

fn cell_ref(lowered: &str) -> Option<ColorSpec> {
    match lowered {
        "cell-foreground" => Some(ColorSpec::CellForeground),
        "cell-background" => Some(ColorSpec::CellBackground),
        _ => None,
    }
}

/// 去首尾空白；两头都是双引号就剥掉再去一次空白
fn unquote(s: &str) -> &str {
    let t = js_trim(s);
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') { js_trim(&t[1..t.len() - 1]) } else { t }
}

#[derive(Clone, Copy)]
enum Field {
    Background,
    Foreground,
    CursorColor,
    CursorText,
    SelectionBackground,
    SelectionForeground,
}

/// TS 的 `COLOR_KEYS`。
///
/// TS 用普通对象查表，`constructor = red`、`toString = #fff` 这类键会查到
/// Object.prototype 上的继承属性，被当成"认出一条"（recognized +1，颜色写进一个
/// 没人读的属性）。那是 TS 的偶然行为，这边不复刻：只认下面六个键。
fn color_field(key: &str) -> Option<Field> {
    Some(match key {
        "background" => Field::Background,
        "foreground" => Field::Foreground,
        "cursor-color" => Field::CursorColor,
        "cursor-text" => Field::CursorText,
        "selection-background" => Field::SelectionBackground,
        "selection-foreground" => Field::SelectionForeground,
        _ => return None,
    })
}

/// `/^(\d{1,3})\s*=\s*(.+)$/`：palette 的值 `N=颜色` → (N, 颜色部分)
fn split_palette(value: &str) -> Option<(u32, &str)> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    // 四位数字时 \d{1,3} 怎么回溯后面都接不上 \s*=，整行不匹配
    if digits == 0 || digits > 3 {
        return None;
    }
    let idx: u32 = value[..digits].parse().ok()?;
    let rest = value[digits..].trim_start_matches(is_js_whitespace).strip_prefix('=')?;
    // value 已经去过首尾空白，`\s*(.+)$` 等价于：去掉前导空白后非空，且整段没有行终止符
    let color = rest.trim_start_matches(is_js_whitespace);
    if color.is_empty() || color.contains(is_js_line_terminator) {
        return None;
    }
    Some((idx, color))
}

/// 解析一段 Ghostty 配置。认不出的行静默跳过（这是"贴整份 config 也能用"的代价），
/// 空值（`key =`）按 Ghostty 语义视为恢复默认，即从结果里去掉。
pub fn parse_ghostty_theme(text: &str) -> GhosttyThemeSource {
    let mut src = GhosttyThemeSource::default();
    // TS 按 /\r?\n/ 切行；这里按 \n 切，行尾的 \r 被下面的 trim 吃掉，结果相同
    for raw in text.split('\n') {
        let line = js_trim(raw);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let key = js_trim(&line[..eq]).to_lowercase();
        let value = unquote(&line[eq + 1..]);
        if key == "theme" {
            src.theme = (!value.is_empty()).then(|| value.to_string());
            continue;
        }
        if key == "palette" {
            let Some((idx, color)) = split_palette(value) else { continue };
            let Ok(idx) = u8::try_from(idx) else { continue };
            let Some(color) = parse_ghostty_color(color) else { continue };
            src.palette.insert(idx, color);
            src.recognized += 1;
            continue;
        }
        let Some(field) = color_field(&key) else { continue };
        if value.is_empty() {
            // 恢复默认：去掉之前写的。recognized 不回退——与 TS 一致
            match field {
                Field::Background => src.background = None,
                Field::Foreground => src.foreground = None,
                Field::CursorColor => src.cursor_color = None,
                Field::CursorText => src.cursor_text = None,
                Field::SelectionBackground => src.selection_background = None,
                Field::SelectionForeground => src.selection_foreground = None,
            }
            continue;
        }
        // background / foreground 不接受 cell-* 特殊值（那一行会被当成认不出的颜色跳过）
        let spec = match field {
            Field::Background | Field::Foreground => None,
            _ => cell_ref(&value.to_lowercase()),
        };
        let spec = match spec {
            Some(s) => s,
            None => match parse_ghostty_color(value) {
                Some(c) => ColorSpec::Color(c),
                None => continue,
            },
        };
        let color = match spec {
            ColorSpec::Color(c) => Some(c),
            _ => None,
        };
        match field {
            Field::Background => src.background = color,
            Field::Foreground => src.foreground = color,
            Field::CursorColor => src.cursor_color = Some(spec),
            Field::CursorText => src.cursor_text = Some(spec),
            Field::SelectionBackground => src.selection_background = Some(spec),
            Field::SelectionForeground => src.selection_foreground = Some(spec),
        }
        src.recognized += 1;
    }
    src
}

/// `theme = X` 的值：单个名字，或 `light:X,dark:Y`（顺序、空格、只写一边都允许）
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ThemeSetting {
    pub light: Option<String>,
    pub dark: Option<String>,
    pub single: Option<String>,
}

pub fn parse_theme_setting(value: &str) -> ThemeSetting {
    let v = unquote(value);
    let mut out = ThemeSetting::default();
    if !v.contains(':') {
        if !v.is_empty() {
            out.single = Some(v.to_string());
        }
        return out;
    }
    for part in v.split(',') {
        let Some(colon) = part.find(':') else { continue };
        let k = js_trim(&part[..colon]).to_lowercase();
        let name = js_trim(&part[colon + 1..]);
        if name.is_empty() {
            continue;
        }
        match k.as_str() {
            "light" => out.light = Some(name.to_string()),
            "dark" => out.dark = Some(name.to_string()),
            _ => {}
        }
    }
    out
}

/// 把一套完整颜色当作"底"：所有键都算显式写了，供 `theme = X` 再覆盖
pub fn source_from_colors(colors: &ThemeColors) -> GhosttyThemeSource {
    let mut palette: BTreeMap<u8, Rgb> = (0u8..).zip(colors.palette).collect();
    palette.extend(colors.extended.iter().map(|(&k, &v)| (k, v)));
    GhosttyThemeSource {
        theme: None,
        background: Some(colors.background),
        foreground: Some(colors.foreground),
        cursor_color: Some(ColorSpec::Color(colors.cursor_color)),
        cursor_text: Some(ColorSpec::Color(colors.cursor_text)),
        selection_background: Some(ColorSpec::Color(colors.selection_background)),
        selection_foreground: Some(colors.selection_foreground.map_or(ColorSpec::CellForeground, ColorSpec::Color)),
        recognized: 6 + palette.len(),
        palette,
    }
}

fn resolve_ref(v: Option<ColorSpec>, fg: Rgb, bg: Rgb, fallback: Rgb) -> Rgb {
    match v {
        None => fallback,
        Some(ColorSpec::CellForeground) => fg,
        Some(ColorSpec::CellBackground) => bg,
        Some(ColorSpec::Color(c)) => c,
    }
}

/// 补齐成一套完整颜色。src 覆盖 base，base 缺省是 Ghostty 自己的默认值。
/// 缺省推导用的是**最终**的前景 / 背景：只写了 background / foreground 的主题，
/// 光标与选区要跟着新的字底走，而不是继承 base 的。
pub fn resolve_theme_colors(src: &GhosttyThemeSource, base: Option<&GhosttyThemeSource>) -> ThemeColors {
    let pick = |s: Option<ColorSpec>, b: Option<ColorSpec>| s.or(b);
    let bg = src.background.or(base.and_then(|b| b.background)).unwrap_or(GHOSTTY_DEFAULT_BACKGROUND);
    let fg = src.foreground.or(base.and_then(|b| b.foreground)).unwrap_or(GHOSTTY_DEFAULT_FOREGROUND);
    let cursor_color = pick(src.cursor_color, base.and_then(|b| b.cursor_color));
    let cursor_text = pick(src.cursor_text, base.and_then(|b| b.cursor_text));
    let selection_background = pick(src.selection_background, base.and_then(|b| b.selection_background));
    let selection_foreground = pick(src.selection_foreground, base.and_then(|b| b.selection_foreground));

    let mut merged = base.map(|b| b.palette.clone()).unwrap_or_default();
    merged.extend(src.palette.iter().map(|(&k, &v)| (k, v)));
    let palette = std::array::from_fn(|i| merged.get(&(i as u8)).copied().unwrap_or(GHOSTTY_DEFAULT_PALETTE[i]));
    let extended = merged.range(16..).map(|(&k, &v)| (k, v)).collect();

    ThemeColors {
        background: bg,
        foreground: fg,
        cursor_color: resolve_ref(cursor_color, fg, bg, fg),
        cursor_text: resolve_ref(cursor_text, fg, bg, bg),
        selection_background: resolve_ref(selection_background, fg, bg, fg),
        selection_foreground: match selection_foreground {
            Some(ColorSpec::CellForeground) => None,
            other => Some(resolve_ref(other, fg, bg, bg)),
        },
        palette,
        extended,
    }
}

/// 回写成 Ghostty 主题文件，键序与 Ghostty 内置主题一致（palette 在前）。
/// 解析 → 序列化 → 解析是恒等的，测试钉住这一点。
pub fn serialize_ghostty_theme(colors: &ThemeColors) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(22 + colors.extended.len());
    for (i, c) in colors.palette.iter().enumerate() {
        lines.push(format!("palette = {i}={c}"));
    }
    for (idx, c) in &colors.extended {
        lines.push(format!("palette = {idx}={c}"));
    }
    lines.push(format!("background = {}", colors.background));
    lines.push(format!("foreground = {}", colors.foreground));
    lines.push(format!("cursor-color = {}", colors.cursor_color));
    lines.push(format!("cursor-text = {}", colors.cursor_text));
    lines.push(format!("selection-background = {}", colors.selection_background));
    match colors.selection_foreground {
        Some(c) => lines.push(format!("selection-foreground = {c}")),
        None => lines.push("selection-foreground = cell-foreground".to_string()),
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// 移植自 `ghostty.test.ts`，用例与断言值一一对应。
#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const MOCHA: &str = "palette = 0=#45475a
palette = 1=#f38ba8
palette = 2=#a6e3a1
palette = 3=#f9e2af
palette = 4=#89b4fa
palette = 5=#f5c2e7
palette = 6=#94e2d5
palette = 7=#a6adc8
palette = 8=#585b70
palette = 9=#f37799
palette = 10=#89d88b
palette = 11=#ebd391
palette = 12=#74a8fc
palette = 13=#f2aede
palette = 14=#6bd7ca
palette = 15=#bac2de
background = #1e1e2e
foreground = #cdd6f4
cursor-color = #f5e0dc
cursor-text = #1e1e2e
selection-background = #585b70
selection-foreground = #cdd6f4
";

    fn hex(s: &str) -> Rgb {
        Rgb::parse_hex(s).unwrap()
    }

    fn color_hex(v: &str) -> Option<String> {
        parse_ghostty_color(v).map(|c| c.to_hex())
    }

    // describe("parseGhosttyColor")

    /// 带不带 # 都认，统一小写
    #[test]
    fn color_with_or_without_hash() {
        assert_eq!(color_hex("#1E1E2E").as_deref(), Some("#1e1e2e"));
        assert_eq!(color_hex("1e1e2e").as_deref(), Some("#1e1e2e"));
        assert_eq!(color_hex("\"#1e1e2e\"").as_deref(), Some("#1e1e2e"));
        assert_eq!(color_hex("#abc").as_deref(), Some("#aabbcc"));
    }

    /// X11 颜色名（含与 CSS 不同的四个）与 grayN
    #[test]
    fn color_x11_names() {
        assert_eq!(color_hex("red").as_deref(), Some("#ff0000"));
        assert_eq!(color_hex("Light Blue").as_deref(), Some("#add8e6"));
        assert_eq!(color_hex("green").as_deref(), Some("#00ff00"));
        assert_eq!(color_hex("gray").as_deref(), Some("#bebebe"));
        assert_eq!(color_hex("gray50").as_deref(), Some("#7f7f7f"));
        assert_eq!(color_hex("grey100").as_deref(), Some("#ffffff"));
    }

    /// 认不出 undefined
    #[test]
    fn color_unrecognized() {
        assert_eq!(color_hex("cell-foreground"), None);
        assert_eq!(color_hex("#12345"), None);
        assert_eq!(color_hex(""), None);
    }

    // describe("parseGhosttyTheme")

    /// 完整内置主题：22 条全认
    #[test]
    fn theme_full_builtin() {
        let src = parse_ghostty_theme(MOCHA);
        assert_eq!(src.recognized, 22);
        assert_eq!(src.background, Some(hex("#1e1e2e")));
        assert_eq!(src.selection_foreground, Some(ColorSpec::Color(hex("#cdd6f4"))));
        assert_eq!(src.palette.get(&0), Some(&hex("#45475a")));
        assert_eq!(src.palette.get(&15), Some(&hex("#bac2de")));
    }

    /// 注释、空行、无关键、CRLF、大小写键、无 # 的值
    #[test]
    fn theme_comments_blank_crlf_case() {
        let src = parse_ghostty_theme(
            "# 主题\r\n\r\nfont-size = 14\r\nBackground = 282c34\r\nkeybind = cmd+t=new_tab\r\npalette=1 = ff0000\r\n",
        );
        assert_eq!(src.recognized, 2);
        assert_eq!(src.background, Some(hex("#282c34")));
        assert_eq!(src.palette.get(&1), Some(&hex("#ff0000")));
    }

    /// cell-foreground / cell-background 特殊值
    #[test]
    fn theme_cell_refs() {
        let src = parse_ghostty_theme("cursor-color = cell-foreground\nselection-foreground = Cell-Background\n");
        assert_eq!(src.cursor_color, Some(ColorSpec::CellForeground));
        assert_eq!(src.selection_foreground, Some(ColorSpec::CellBackground));
        assert_eq!(src.recognized, 2);
        // background 不接受特殊值
        assert_eq!(parse_ghostty_theme("background = cell-foreground").recognized, 0);
    }

    /// 空值 = 恢复默认（去掉之前写的）
    #[test]
    fn theme_empty_value_resets() {
        let src = parse_ghostty_theme("background = #000000\nbackground =\n");
        assert_eq!(src.background, None);
    }

    /// theme 行与 256 色调色板
    #[test]
    fn theme_line_and_256_palette() {
        let src = parse_ghostty_theme("theme = Dracula\npalette = 232=#080808\npalette = 300=#ffffff\n");
        assert_eq!(src.theme.as_deref(), Some("Dracula"));
        assert_eq!(src.palette.get(&232), Some(&hex("#080808")));
        assert_eq!(src.palette.len(), 1, "300 超出 0–255，不收");
    }

    /// 什么都没有 recognized = 0
    #[test]
    fn theme_nothing_recognized() {
        assert_eq!(parse_ghostty_theme("hello\n").recognized, 0);
        assert_eq!(parse_ghostty_theme("").recognized, 0);
    }

    // describe("parseThemeSetting")

    /// 单名 / 双槽
    #[test]
    fn theme_setting_single_and_pair() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(parse_theme_setting("Dracula"), ThemeSetting { single: s("Dracula"), ..Default::default() });
        assert_eq!(
            parse_theme_setting("light:Catppuccin Latte,dark:Catppuccin Mocha"),
            ThemeSetting { light: s("Catppuccin Latte"), dark: s("Catppuccin Mocha"), single: None }
        );
        assert_eq!(
            parse_theme_setting(" dark: Nord , light:Nord Light "),
            ThemeSetting { light: s("Nord Light"), dark: s("Nord"), single: None }
        );
        assert_eq!(parse_theme_setting(""), ThemeSetting::default());
    }

    // describe("resolveThemeColors")

    /// 完整主题原样
    #[test]
    fn resolve_full_theme_as_is() {
        let c = resolve_theme_colors(&parse_ghostty_theme(MOCHA), None);
        assert_eq!(c.background, hex("#1e1e2e"));
        assert_eq!(c.cursor_color, hex("#f5e0dc"));
        assert_eq!(c.selection_foreground, Some(hex("#cdd6f4")));
        assert_eq!(c.palette.len(), 16);
        assert!(c.extended.is_empty());
    }

    /// 缺省照 Ghostty：光标 = 前景、光标下字 = 背景、选区 = 前后景互换
    #[test]
    fn resolve_defaults_follow_ghostty() {
        let c = resolve_theme_colors(&parse_ghostty_theme("background = #101010\nforeground = #e0e0e0\n"), None);
        assert_eq!(c.cursor_color, hex("#e0e0e0"));
        assert_eq!(c.cursor_text, hex("#101010"));
        assert_eq!(c.selection_background, hex("#e0e0e0"));
        assert_eq!(c.selection_foreground, Some(hex("#101010")));
        assert_eq!(c.palette, GHOSTTY_DEFAULT_PALETTE);
    }

    /// 什么都没写就是 Ghostty 默认
    #[test]
    fn resolve_empty_is_ghostty_default() {
        let c = resolve_theme_colors(&parse_ghostty_theme(""), None);
        assert_eq!(c.background, GHOSTTY_DEFAULT_BACKGROUND);
        assert_eq!(c.foreground, GHOSTTY_DEFAULT_FOREGROUND);
    }

    /// 特殊值按最终前后景解析；selection-foreground = cell-foreground → null
    #[test]
    fn resolve_cell_refs_against_final_colors() {
        let c = resolve_theme_colors(
            &parse_ghostty_theme(
                "background = #000000\nforeground = #ffffff\ncursor-color = cell-background\ncursor-text = cell-foreground\nselection-background = cell-foreground\nselection-foreground = cell-foreground\n",
            ),
            None,
        );
        assert_eq!(c.cursor_color, hex("#000000"));
        assert_eq!(c.cursor_text, hex("#ffffff"));
        assert_eq!(c.selection_background, hex("#ffffff"));
        assert_eq!(c.selection_foreground, None);
    }

    /// 以内置主题为底再覆盖：只换 background，光标沿用底的显式值
    #[test]
    fn resolve_over_base_theme() {
        let base = source_from_colors(&resolve_theme_colors(&parse_ghostty_theme(MOCHA), None));
        let c = resolve_theme_colors(&parse_ghostty_theme("background = #000000\npalette = 1=#ff0000\n"), Some(&base));
        assert_eq!(c.background, hex("#000000"));
        assert_eq!(c.foreground, hex("#cdd6f4"));
        assert_eq!(c.cursor_color, hex("#f5e0dc"));
        assert_eq!(c.palette[1], hex("#ff0000"));
        assert_eq!(c.palette[2], hex("#a6e3a1"));
    }

    /// 16 以上进 extended
    #[test]
    fn resolve_extended_palette() {
        let c = resolve_theme_colors(&parse_ghostty_theme("palette = 16=#000000\npalette = 255=#eeeeee\n"), None);
        assert_eq!(c.extended, BTreeMap::from([(16, hex("#000000")), (255, hex("#eeeeee"))]));
    }

    // describe("serializeGhosttyTheme")

    /// 解析 → 序列化 → 解析恒等，键序与内置文件一致
    #[test]
    fn serialize_roundtrip_matches_builtin_file() {
        let colors = resolve_theme_colors(&parse_ghostty_theme(MOCHA), None);
        let text = serialize_ghostty_theme(&colors);
        assert_eq!(text, MOCHA);
        assert_eq!(resolve_theme_colors(&parse_ghostty_theme(&text), None), colors);
    }

    /// selectionForeground null 写成 cell-foreground，extended 跟在 15 后面
    #[test]
    fn serialize_null_selection_and_extended() {
        let colors = ThemeColors {
            selection_foreground: None,
            extended: BTreeMap::from([(232, hex("#080808")), (16, hex("#000000"))]),
            ..resolve_theme_colors(&parse_ghostty_theme(MOCHA), None)
        };
        let text = serialize_ghostty_theme(&colors);
        assert!(
            text.contains("palette = 15=#bac2de\npalette = 16=#000000\npalette = 232=#080808\nbackground"),
            "{text}"
        );
        assert!(text.ends_with("selection-foreground = cell-foreground\n"), "{text}");
        assert_eq!(resolve_theme_colors(&parse_ghostty_theme(&text), None), colors);
    }

    // 以下不是从 TS 移植的：钉住几处照抄 JS 语义的边角

    /// 开头带 BOM（Windows 记事本存的主题）照样认：JS 的 trim 会去掉 U+FEFF
    #[test]
    fn bom_is_whitespace() {
        let src = parse_ghostty_theme("\u{feff}background = #101010\n");
        assert_eq!(src.background, Some(hex("#101010")));
    }

    /// palette 的下标最多三位，前导零照 Number() 解析
    #[test]
    fn palette_index_forms() {
        assert_eq!(parse_ghostty_theme("palette = 007=#010203").palette.get(&7), Some(&hex("#010203")));
        assert_eq!(parse_ghostty_theme("palette = 0007=#010203").recognized, 0);
        assert_eq!(parse_ghostty_theme("palette = 7 =  \"#010203\"").palette.get(&7), Some(&hex("#010203")));
    }
}
