//! 内置主题目录：Falcon 自己的两套默认 + Ghostty 1.3.1 内置的 463 套（`lib/theme/catalog.ts`）。
//!
//! Ghostty 那份数据由 `scripts/vendor-ghostty-themes.mjs` 从 Ghostty.app（或 GitHub）
//! 生成到 `data/ghostty-themes.tsv`，编译期 `include_str!` 嵌进来（77KB 文本），
//! **启动时一行都不解析**：
//!
//! - 偏好里存的是选中主题的完整颜色副本（pref.rs），启动不需要目录就能把界面画对；
//! - 按名字取一套（[`find_builtin`]）只扫行首的名字，命中那一行才解析；
//! - 整份目录（[`load_catalog`]）在第一次要列表时（打开主题选择器）才解析，之后缓存。
//!
//! 名字与 `ghostty +list-themes` 一字不差，Ghostty 配置里 `theme = Catppuccin Mocha`
//! 在这里查同一个名字就能命中。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use anyhow::{Context, bail};

use crate::appearance::{Appearance, appearance_from_rgb};
use crate::color::Rgb;
use crate::ghostty::{ThemeColors, parse_ghostty_theme, parse_theme_setting, resolve_theme_colors, source_from_colors};
use crate::js::js_trim;
use crate::pref::ThemeMode;

include!("../data/ghostty-themes.meta.rs");

/// 内置 Ghostty 主题数据：每行 `名字 \t 22 个不带 # 的 rrggbb`，顺序是
/// bg fg cursor cursorText selBg selFg palette0..15（与 scripts/vendor-ghostty-themes.mjs
/// 的 KEYS 一致）。与 web 的 `GHOSTTY_THEMES_DATA` 逐字节相同。
pub const GHOSTTY_THEMES_DATA: &str = include_str!("../data/ghostty-themes.tsv");

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CatalogEntry {
    pub name: Cow<'static, str>,
    pub colors: ThemeColors,
    /// 按底色亮度判的深浅（选择器只列与槽位同深浅的主题）
    pub appearance: Appearance,
}

pub const FALCON_LIGHT_NAME: &str = "Falcon Light";
pub const FALCON_DARK_NAME: &str = "Falcon Dark";

const fn palette(hex: [u32; 16]) -> [Rgb; 16] {
    let mut out = [Rgb::new(0, 0, 0); 16];
    let mut i = 0;
    while i < 16 {
        out[i] = Rgb::from_u32(hex[i]);
        i += 1;
    }
    out
}

/// 默认深色：shadcn neutral 的底 / 字，ANSI 用 xterm.js 默认（Tango，本来就是给
/// 深底配的）。选区是以前的 #ffffff33 预混到底色上——Ghostty 格式没有 alpha。
pub const FALCON_DARK_COLORS: ThemeColors = ThemeColors {
    background: Rgb::from_u32(0x0a0a0a),
    foreground: Rgb::from_u32(0xfafafa),
    cursor_color: Rgb::from_u32(0xfafafa),
    cursor_text: Rgb::from_u32(0x0a0a0a),
    selection_background: Rgb::from_u32(0x3b3b3b),
    selection_foreground: None,
    palette: palette([
        0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, //
        0x555753, 0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ]),
    extended: BTreeMap::new(),
};

/// 默认浅色：白底近黑字，ANSI 取 VS Code Light+——白底上公认可读的一套
pub const FALCON_LIGHT_COLORS: ThemeColors = ThemeColors {
    background: Rgb::from_u32(0xffffff),
    foreground: Rgb::from_u32(0x171717),
    cursor_color: Rgb::from_u32(0x171717),
    cursor_text: Rgb::from_u32(0xffffff),
    selection_background: Rgb::from_u32(0xd9d9d9),
    selection_foreground: None,
    palette: palette([
        0x000000, 0xcd3131, 0x00bc00, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, //
        0x666666, 0xcd3131, 0x14ce14, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
    ]),
    extended: BTreeMap::new(),
};

/// TS 里 appearance 是 `appearanceFromHex(background)` 算出来的；这里为了能写成 const
/// 直接填，测试核对它与公式一致。
pub const FALCON_LIGHT: CatalogEntry = CatalogEntry {
    name: Cow::Borrowed(FALCON_LIGHT_NAME),
    colors: FALCON_LIGHT_COLORS,
    appearance: Appearance::Light,
};

pub const FALCON_DARK: CatalogEntry = CatalogEntry {
    name: Cow::Borrowed(FALCON_DARK_NAME),
    colors: FALCON_DARK_COLORS,
    appearance: Appearance::Dark,
};

/// Falcon 自己的两套，目录里排在最前
pub const FALCON_THEMES: [CatalogEntry; 2] = [FALCON_LIGHT, FALCON_DARK];

fn entry(name: Cow<'static, str>, colors: ThemeColors) -> CatalogEntry {
    let appearance = appearance_from_rgb(colors.background);
    CatalogEntry { name, colors, appearance }
}

/// 一行数据 → (名字, 22 个颜色)。坏行报错：数据是构建期生成的，
/// 运行时遇到坏行只可能是产物被手改过。
fn parse_line(line: &str) -> anyhow::Result<(&str, ThemeColors)> {
    let Some(tab) = line.find('\t') else {
        let head: String = line.chars().take(40).collect();
        bail!("主题数据坏行：{head}");
    };
    let name = &line[..tab];
    let hexes: Vec<&str> = line[tab + 1..].split(' ').collect();
    // 只认小写（TS 的正则没有 i 标志）
    let valid = |h: &&str| h.len() == 6 && h.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    if hexes.len() != 22 || !hexes.iter().all(valid) {
        bail!("主题「{name}」颜色数不对");
    }
    let c: Vec<Rgb> = hexes
        .iter()
        .map(|h| u32::from_str_radix(h, 16).map(Rgb::from_u32))
        .collect::<Result<_, _>>()
        .with_context(|| format!("主题「{name}」颜色解析失败"))?;
    let colors = ThemeColors {
        background: c[0],
        foreground: c[1],
        cursor_color: c[2],
        cursor_text: c[3],
        selection_background: c[4],
        // 内置主题的 selection-foreground 都是显式颜色，没有 cell-foreground
        selection_foreground: Some(c[5]),
        palette: std::array::from_fn(|i| c[6 + i]),
        extended: BTreeMap::new(),
    };
    Ok((name, colors))
}

/// 数据里的行（空行跳过——与 TS 的 `if (!line) continue` 一致）
fn data_lines(data: &str) -> impl Iterator<Item = &str> {
    data.split('\n').filter(|l| !l.is_empty())
}

/// 行首的名字，不解析颜色（按名字查找时用）
fn line_name(line: &str) -> &str {
    line.find('\t').map_or(line, |tab| &line[..tab])
}

/// 解析 vendor 脚本的产物（任意来源的文本）。
pub fn parse_catalog_data(data: &str) -> anyhow::Result<Vec<CatalogEntry>> {
    data_lines(data)
        .map(|line| parse_line(line).map(|(name, colors)| entry(Cow::Owned(name.to_string()), colors)))
        .collect()
}

/// 解析内置数据：名字直接借用嵌入的静态文本，不分配
fn parse_builtin_line(line: &'static str) -> CatalogEntry {
    let (name, colors) = parse_line(line).expect("内置主题数据坏了：data/ghostty-themes.tsv 被手改过？重跑导出脚本");
    entry(Cow::Borrowed(name), colors)
}

/// 先精确匹配，再忽略大小写（名字先去首尾空白）——Ghostty 自己只认精确文件名，
/// 宽松一档是给手打的
pub fn find_theme<'a>(entries: &'a [CatalogEntry], name: &str) -> Option<&'a CatalogEntry> {
    if let Some(exact) = entries.iter().find(|e| e.name == name) {
        return Some(exact);
    }
    let lower = js_trim(name).to_lowercase();
    entries.iter().find(|e| e.name.to_lowercase() == lower)
}

static CATALOG: OnceLock<Vec<CatalogEntry>> = OnceLock::new();

/// Falcon 两套 + Ghostty 全部，第一次调用时解析，之后给同一份。
///
/// 内置数据有测试逐行核过，这里不会失败（坏了会 panic 并指向导出脚本）。
pub fn load_catalog() -> &'static [CatalogEntry] {
    CATALOG.get_or_init(|| {
        let mut all: Vec<CatalogEntry> = FALCON_THEMES.to_vec();
        all.extend(data_lines(GHOSTTY_THEMES_DATA).map(parse_builtin_line));
        all
    })
}

/// 已经加载过就给，没有就 `None`（选择器首次打开前）
pub fn cached_catalog() -> Option<&'static [CatalogEntry]> {
    CATALOG.get().map(Vec::as_slice)
}

/// 按名字取一套内置主题，语义同 `find_theme(load_catalog(), name)`，但目录还没加载时
/// 不解析整份：只比对每行行首的名字，命中那一行才解析颜色。
///
/// 启动路径上用它（例如旧偏好迁移、`theme = X` 找底），不把 463 套全解析一遍。
pub fn find_builtin(name: &str) -> Option<CatalogEntry> {
    match cached_catalog() {
        Some(all) => find_theme(all, name).cloned(),
        None => find_builtin_by_line(name),
    }
}

fn find_builtin_by_line(name: &str) -> Option<CatalogEntry> {
    // 两轮的顺序与 find_theme 在 [Falcon…, Ghostty…] 上一致：先全体精确，再全体忽略大小写
    if let Some(e) = FALCON_THEMES.iter().find(|e| e.name == name) {
        return Some(e.clone());
    }
    if let Some(line) = data_lines(GHOSTTY_THEMES_DATA).find(|l| line_name(l) == name) {
        return Some(parse_builtin_line(line));
    }
    let lower = js_trim(name).to_lowercase();
    if let Some(e) = FALCON_THEMES.iter().find(|e| e.name.to_lowercase() == lower) {
        return Some(e.clone());
    }
    data_lines(GHOSTTY_THEMES_DATA)
        .find(|l| line_name(l).to_lowercase() == lower)
        .map(parse_builtin_line)
}

/// 自定义主题编辑器的解析结果
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CustomThemeParse {
    /// 认出了任何东西才有；`None` = 这段文字里根本没有主题
    pub colors: Option<ThemeColors>,
    /// 识别出的条数（以内置主题为底时底算 22 条）
    pub recognized: usize,
    /// `theme = X` 写了、但目录里找不到的名字（编辑器里要提示）
    pub unknown_base: Option<String>,
    /// 实际用作底的内置主题名（没起名时拿它当自定义主题的名字）
    pub base_name: Option<String>,
}

/// 用户贴的一段 Ghostty 文本 → 颜色：`theme = X` 以内置主题为底再覆盖（与 Ghostty 读
/// 配置的顺序一致），`light:X,dark:Y` 取当前槽位那一半，只写一边就用那一边。
///
/// 这段逻辑在 web 里写在 `components/ThemePicker.tsx`（编辑器的 `parsed`），不在
/// lib/theme 下；原生的编辑器也要同样的行为，所以挪进主题层。web 那边目录还没加载完时
/// 不找底；这边目录是嵌入的，调用方把 [`load_catalog`] 传进来。
pub fn resolve_custom_theme(text: &str, slot: ThemeMode, catalog: &[CatalogEntry]) -> CustomThemeParse {
    let src = parse_ghostty_theme(text);
    let base_name = src.theme.as_deref().and_then(|t| {
        let s = parse_theme_setting(t);
        let slot_name = match slot {
            ThemeMode::Light => s.light.clone(),
            ThemeMode::Dark => s.dark.clone(),
        };
        s.single.or(slot_name).or(s.light).or(s.dark)
    });
    let base = base_name.as_deref().and_then(|n| find_theme(catalog, n));
    let unknown_base = if base.is_none() { base_name } else { None };
    let recognized = src.recognized + if base.is_some() { 22 } else { 0 };
    let colors = (recognized > 0).then(|| resolve_theme_colors(&src, base.map(|b| source_from_colors(&b.colors)).as_ref()));
    CustomThemeParse { colors, recognized, unknown_base, base_name: base.map(|b| b.name.to_string()) }
}

/// 移植自 `catalog.test.ts`，用例与断言值一一对应。
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghostty::serialize_ghostty_theme;

    fn entries() -> Vec<CatalogEntry> {
        parse_catalog_data(GHOSTTY_THEMES_DATA).unwrap()
    }

    fn hex(s: &str) -> Rgb {
        Rgb::parse_hex(s).unwrap()
    }

    // describe("Ghostty 内置目录")

    /// 条数与生成脚本记录一致，名字唯一且不与 Falcon 撞
    #[test]
    fn count_matches_and_names_unique() {
        let entries = entries();
        assert_eq!(entries.len(), GHOSTTY_THEMES_COUNT);
        let names: std::collections::HashSet<&str> = entries.iter().map(|e| e.name.as_ref()).collect();
        assert_eq!(names.len(), entries.len());
        for f in &FALCON_THEMES {
            assert!(!names.contains(f.name.as_ref()), "{}", f.name);
        }
    }

    /// 用户 Ghostty 配置里常见的名字都在
    #[test]
    fn common_names_present() {
        let entries = entries();
        for name in ["Catppuccin Mocha", "Catppuccin Latte", "Dracula", "Nord", "TokyoNight", "Gruvbox Dark"] {
            assert!(find_theme(&entries, name).is_some(), "{name}");
        }
    }

    /// Catppuccin Mocha 与 Ghostty 自带文件逐色一致
    #[test]
    fn mocha_matches_ghostty_file() {
        let entries = entries();
        let mocha = find_theme(&entries, "Catppuccin Mocha").unwrap();
        assert_eq!(mocha.appearance, Appearance::Dark);
        assert_eq!(mocha.colors.background, hex("#1e1e2e"));
        assert_eq!(mocha.colors.foreground, hex("#cdd6f4"));
        assert_eq!(mocha.colors.cursor_color, hex("#f5e0dc"));
        assert_eq!(mocha.colors.selection_background, hex("#585b70"));
        assert_eq!(mocha.colors.palette[0], hex("#45475a"));
        assert_eq!(mocha.colors.palette[15], hex("#bac2de"));
    }

    /// 每条都能序列化成合法 Ghostty 主题再读回来
    #[test]
    fn every_entry_roundtrips_through_ghostty_text() {
        for e in entries() {
            let back = resolve_theme_colors(&parse_ghostty_theme(&serialize_ghostty_theme(&e.colors)), None);
            assert_eq!(back, e.colors, "{}", e.name);
        }
    }

    /// findTheme 精确优先，其次忽略大小写
    #[test]
    fn find_theme_exact_then_case_insensitive() {
        let entries = entries();
        assert_eq!(find_theme(&entries, "dracula").map(|e| e.name.as_ref()), Some("Dracula"));
        assert_eq!(find_theme(&entries, "  DRACULA ").map(|e| e.name.as_ref()), Some("Dracula"));
        assert_eq!(find_theme(&entries, "nope"), None);
    }

    /// 坏数据直接抛
    #[test]
    fn bad_data_errors() {
        assert!(parse_catalog_data("Bad\t000000").is_err());
        assert!(parse_catalog_data("no tab").is_err());
    }

    // describe("Falcon 默认主题")

    /// 深浅各一套，与旧版跟随界面的底 / 字一致
    #[test]
    fn falcon_defaults() {
        assert_eq!(FALCON_DARK.appearance, Appearance::Dark);
        assert_eq!(FALCON_LIGHT.appearance, Appearance::Light);
        assert_eq!(FALCON_DARK.colors.background, hex("#0a0a0a"));
        assert_eq!(FALCON_DARK.colors.foreground, hex("#fafafa"));
        assert_eq!(FALCON_LIGHT.colors.background, hex("#ffffff"));
        assert_eq!(FALCON_LIGHT.colors.foreground, hex("#171717"));
        assert_eq!(FALCON_LIGHT.colors.palette.len(), 16);
        // const 里直接填的 appearance 必须与公式一致
        for f in &FALCON_THEMES {
            assert_eq!(f.appearance, appearance_from_rgb(f.colors.background), "{}", f.name);
        }
    }

    // 以下不是从 TS 移植的：懒加载与按名字取

    /// 目录还没加载时 find_builtin 按行查，结果与整份目录上的 find_theme 一致。
    /// 直接测按行查的那条路：测试并行跑，别的用例可能已经把目录加载了
    #[test]
    fn find_builtin_matches_full_catalog() {
        let all = {
            let mut v = FALCON_THEMES.to_vec();
            v.extend(entries());
            v
        };
        for name in ["Catppuccin Mocha", "catppuccin mocha", " NORD ", "Falcon Dark", "falcon light", "nope", "0x96f", "Zenwritten Light"] {
            assert_eq!(find_builtin_by_line(name), find_theme(&all, name).cloned(), "{name:?}");
            assert_eq!(find_builtin(name), find_theme(&all, name).cloned(), "{name:?}");
        }
        let dracula = find_builtin_by_line("Dracula").unwrap();
        assert!(matches!(dracula.name, Cow::Borrowed(_)), "内置主题名应借用嵌入数据");
    }

    #[test]
    fn load_catalog_is_falcon_then_ghostty() {
        let all = load_catalog();
        assert_eq!(all.len(), 2 + GHOSTTY_THEMES_COUNT);
        assert_eq!(all[0].name, FALCON_LIGHT_NAME);
        assert_eq!(all[1].name, FALCON_DARK_NAME);
        assert!(std::ptr::eq(all, cached_catalog().unwrap()));
        assert!(!GHOSTTY_THEMES_ORIGIN.is_empty());
    }

    /// 编辑器：`theme = X` 当底、按槽位取一半、找不到的底要报出来
    #[test]
    fn custom_theme_over_builtin_base() {
        let catalog = load_catalog();
        let p = resolve_custom_theme("theme = Catppuccin Mocha\nbackground = #000000\n", ThemeMode::Dark, catalog);
        assert_eq!(p.recognized, 23);
        assert_eq!(p.base_name.as_deref(), Some("Catppuccin Mocha"));
        let c = p.colors.unwrap();
        assert_eq!(c.background, hex("#000000"));
        assert_eq!(c.cursor_color, hex("#f5e0dc"));

        let pair = "theme = light:Catppuccin Latte,dark:Catppuccin Mocha";
        assert_eq!(resolve_custom_theme(pair, ThemeMode::Light, catalog).base_name.as_deref(), Some("Catppuccin Latte"));
        assert_eq!(resolve_custom_theme(pair, ThemeMode::Dark, catalog).base_name.as_deref(), Some("Catppuccin Mocha"));
        // 只写了另一边也用
        let only_dark = resolve_custom_theme("theme = dark:Nord", ThemeMode::Light, catalog);
        assert_eq!(only_dark.base_name.as_deref(), Some("Nord"));

        let unknown = resolve_custom_theme("theme = Nope\n", ThemeMode::Dark, catalog);
        assert_eq!(unknown.unknown_base.as_deref(), Some("Nope"));
        assert_eq!(unknown.colors, None);
        assert_eq!(resolve_custom_theme("hello", ThemeMode::Dark, catalog).colors, None);
    }
}
