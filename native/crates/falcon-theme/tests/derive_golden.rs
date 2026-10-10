//! 派生规则的回归基线：`tests/fixtures/*` 是当年直接跑 web 的 TS 源码（deriveTheme /
//! JSON.stringify）生成的（导出脚本随 React 前端一起删了：
//! `git show 9c9d045:native/scripts/export-ghostty-themes.mjs`），这里要求 Rust 的输出逐字相同。
//!
//! 红了的意思是派生规则变了。是有意改规则，就连同这份 fixture 一起改并在提交里写清楚；
//! 不是，就是改错了。别为了变绿去改 fixture。

use falcon_theme::{FALCON_THEMES, ThemeSettings, derive_theme, load_catalog};

const GOLDEN: &str = include_str!("fixtures/derive-golden.tsv");
const PREF_DEFAULT: &str = include_str!("fixtures/pref-default.json");

/// Falcon 两套 + 全部 Ghostty 主题：每个 CSS 变量的值、深浅都与 web 的 deriveTheme 相同
#[test]
fn every_builtin_theme_derives_exactly_like_web() {
    let mut lines = GOLDEN.lines().filter(|l| !l.starts_with('#'));
    let keys: Vec<&str> = lines
        .next()
        .and_then(|l| l.strip_prefix("@keys\t"))
        .expect("金标准缺 @keys 行")
        .split(' ')
        .collect();
    let catalog = load_catalog();
    let mut seen = 0;
    let mut mismatches: Vec<String> = Vec::new();
    for line in lines {
        let mut cols = line.split('\t');
        let (Some(name), Some(appearance), Some(values)) = (cols.next(), cols.next(), cols.next()) else {
            panic!("金标准坏行：{line}");
        };
        let want: Vec<&str> = values.split(' ').collect();
        assert_eq!(want.len(), keys.len(), "{name} 的值个数不对");
        let entry = catalog.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("目录里没有 {name}"));
        let theme = derive_theme(&entry.colors);
        if theme.appearance.as_str() != appearance {
            mismatches.push(format!("{name} appearance: rust={} web={appearance}", theme.appearance.as_str()));
        }
        let got = theme.css_vars();
        let got_keys: Vec<&str> = got.iter().map(|(k, _)| *k).collect();
        assert_eq!(got_keys, keys, "css_vars 的键 / 顺序与 web 的 vars 不同");
        for ((key, value), want) in got.iter().zip(&want) {
            let value = value.strip_prefix('#').unwrap_or(value);
            if value != *want {
                mismatches.push(format!("{name} {key}: rust={value} web={want}"));
            }
        }
        seen += 1;
    }
    assert_eq!(seen, catalog.len(), "金标准条数与目录不同——重跑导出脚本？");
    assert!(mismatches.is_empty(), "{} 处与 web 不同：\n{}", mismatches.len(), mismatches.join("\n"));
}

/// 偏好的 JSON 形状：默认设置序列化出来与 web 的 `JSON.stringify(DEFAULT_THEME_SETTINGS)` 逐字节相同
#[test]
fn default_settings_json_matches_web() {
    let json = serde_json::to_string(&ThemeSettings::default()).unwrap();
    assert_eq!(json, PREF_DEFAULT.trim_end());
    // web 写的偏好读进来就是默认值
    let back: ThemeSettings = serde_json::from_str(PREF_DEFAULT).unwrap();
    assert_eq!(back, ThemeSettings::default());
    assert_eq!(FALCON_THEMES.len(), 2);
}
