//! 内置目录整体：嵌入的数据全部能解析、全部能派生，按名字取得到。

use falcon_theme::catalog::{GHOSTTY_THEMES_DATA, parse_catalog_data};
use falcon_theme::{
    Appearance, GHOSTTY_THEMES_COUNT, derive_theme, find_builtin, find_theme, load_catalog, parse_ghostty_theme,
    resolve_theme_colors, serialize_ghostty_theme,
};

/// 463 套全部能解析，派生不 panic，派生结果自洽
#[test]
fn all_builtin_themes_parse_and_derive() {
    let ghostty = parse_catalog_data(GHOSTTY_THEMES_DATA).expect("内置数据有坏行");
    assert_eq!(ghostty.len(), GHOSTTY_THEMES_COUNT);
    let all = load_catalog();
    assert_eq!(all.len(), 2 + GHOSTTY_THEMES_COUNT);
    let (mut light, mut dark) = (0, 0);
    for e in all {
        let t = derive_theme(&e.colors);
        assert_eq!(t.appearance, e.appearance, "{}", e.name);
        assert_eq!(t.is_dark, e.appearance == Appearance::Dark, "{}", e.name);
        assert_eq!(t.ui.background, e.colors.background, "{}", e.name);
        assert_eq!(t.terminal.ansi, e.colors.palette, "{}", e.name);
        assert_eq!(t.css_vars().len(), 76, "{}", e.name);
        // Ghostty 文本往返
        let back = resolve_theme_colors(&parse_ghostty_theme(&serialize_ghostty_theme(&e.colors)), None);
        assert_eq!(back, e.colors, "{}", e.name);
        if t.is_dark { dark += 1 } else { light += 1 }
    }
    // ADR 0006 验证状态里的数：浅色 78 / 深色 387（含 Falcon 两套）
    assert_eq!((light, dark), (78, 387));
}

/// 按名字取：精确、忽略大小写、Falcon 自己的、取不到
#[test]
fn builtin_lookup_by_name() {
    let mocha = find_builtin("Catppuccin Mocha").expect("Catppuccin Mocha");
    assert_eq!(mocha.colors.background.to_hex(), "#1e1e2e");
    assert_eq!(mocha.appearance, Appearance::Dark);
    assert_eq!(find_builtin("catppuccin latte").map(|e| e.name.into_owned()).as_deref(), Some("Catppuccin Latte"));
    assert_eq!(find_builtin("Falcon Dark").map(|e| e.colors.background.to_hex()).as_deref(), Some("#0a0a0a"));
    assert!(find_builtin("No Such Theme").is_none());
    // 旧版终端配色迁移会用到的名字全都在
    for (_, legacy) in falcon_theme::pref::LEGACY_TERM_THEME_NAMES {
        let e = find_theme(load_catalog(), legacy.name).unwrap_or_else(|| panic!("{}", legacy.name));
        assert_eq!(e.name, legacy.name);
    }
}
