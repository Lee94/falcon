//! falcon-theme：主题系统，不依赖 GPUI。原是 React 前端 `packages/web/src/lib/theme/*` 的移植，
//! React 前端删除后（TS 原文在提交 9c9d045）这里就是真相来源。
//!
//! 权威决策在 `docs/adr/0006-ghostty-themes.md`（Ghostty 主题文件即数据模型、浅色 /
//! 深色双槽位、整站颜色由一套主题派生）与 `docs/adr/0011-floating-island-shell.md`
//! （窗口底 `app`、`tint`、只压亮度不洗色度、色域收缩）。`tests/derive_golden.rs` 拿当年
//! web 的 deriveTheme 对全部内置主题算出的结果逐字对拍——金标准在删 React 时冻结，
//! 是派生规则的回归基线。
//!
//! | 模块 | 对应的 TS | 内容 |
//! |---|---|---|
//! | [`color`] | `color.ts` | `Rgb` / `Rgba`、hex 解析与输出、OKLab、对比度、混色 |
//! | [`appearance`] | `@falcon/shared` 的 `appearanceFromHex` | 按底色亮度判深浅 |
//! | [`ghostty`] | `ghostty.ts` | Ghostty 主题文本的解析 / 补默认 / 序列化 |
//! | [`derive`] | `derive.ts` + `apply.ts` 写的变量表 | 界面 token、语法高亮、终端配色 |
//! | [`catalog`] | `catalog.ts`（+ ThemePicker 的自定义主题解析） | Falcon 两套 + Ghostty 463 套，懒解析 |
//! | [`pref`] | `pref.ts` | 槽位与明暗模式偏好、持久化 JSON 的形状与清洗 |
//!
//! 不在这里的：把颜色落到界面上（web 的 apply.ts 写 DOM；原生由 app 层把强类型字段
//! 转成 gpui 的 Hsla 喂给自己的组件与 gpui-component 的 Theme）、跟随系统明暗
//! （GPUI 的 `window.appearance()`）、按槽位缓存派生结果——都归 falcon-ui。
//!
//! 典型用法：
//!
//! ```
//! use falcon_theme::{load_theme_settings, resolve_theme_mode, derive_theme};
//!
//! let loaded = load_theme_settings(None); // 实际传偏好文件的 StorageLike
//! let mode = resolve_theme_mode(loaded.settings.mode, /* 系统是深色 */ true);
//! let theme = derive_theme(&loaded.settings.slot(mode).colors);
//! assert!(theme.is_dark);
//! let _panel = theme.ui.background; // 岛的底
//! let _gap = theme.ui.app;          // 窗口底
//! ```

pub mod appearance;
pub mod catalog;
pub mod color;
pub mod derive;
pub mod ghostty;
mod js;
pub mod pref;

pub use appearance::{Appearance, appearance_from_hex, appearance_from_rgb};
pub use catalog::{
    CatalogEntry, CustomThemeParse, FALCON_DARK, FALCON_LIGHT, FALCON_THEMES, GHOSTTY_THEMES_COUNT,
    GHOSTTY_THEMES_ORIGIN, cached_catalog, find_builtin, find_theme, load_catalog, resolve_custom_theme,
};
pub use color::{Oklab, Rgb, RgbF, Rgba};
pub use derive::{ResolvedTheme, SyntaxColors, TermHint, TerminalColors, UiTokens, derive_theme};
pub use ghostty::{
    ColorSpec, GhosttyThemeSource, ThemeColors, ThemeSetting, parse_ghostty_color, parse_ghostty_theme,
    parse_theme_setting, resolve_theme_colors, serialize_ghostty_theme, source_from_colors,
};
pub use pref::{
    LegacyTermTheme, LoadedThemeSettings, StorageLike, THEMES_KEY, ThemeChoice, ThemeKind, ThemeMode, ThemePref,
    ThemeSettings, choice_of, is_default_theme_settings, load_theme_settings, resolve_theme_mode,
    save_theme_settings,
};
