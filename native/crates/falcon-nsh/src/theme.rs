//! 主题桥接：`falcon-theme`（Ghostty 主题文件 + 浅深双槽位）→ NSH `Tokens` 与 `Appearance`。
//!
//! 权威规则见 ADR 0006 与 ADR 0011：
//! 1. **深浅只按当前激活主题底色亮度（`ResolvedTheme::is_dark`）判定**，绝不看主题放在浅色还是深色槽位，
//!    也不让操作系统的明暗外观去挑 `AdaptiveColor` 的另一半；
//! 2. 为彻底杜绝宿主系统（例如系统处于浅色模式，而用户在浅色槽位放了 Dracula 深底主题）把组件库画成反色，
//!    生成的 `AdaptiveColor::new(c, c)` 在浅色与深色两栏填入同一个当前主题派生色，同时把根表面的
//!    `Appearance` 显式设为 `Appearance::Dark`（当 `resolved.is_dark`）或 `Appearance::Light`；
//! 3. 界面组件不写死任何颜色常量，一律从 [`ThemeController`] 暴露的 `ResolvedTheme` 语义 token 取色。

use std::sync::Arc;

use falcon_proto::PluginTone;
use falcon_theme::{
    CatalogEntry, ResolvedTheme, Rgb as ThemeRgb, Rgba as ThemeRgba, StorageLike, ThemeChoice,
    ThemeMode, ThemePref, ThemeSettings, choice_of, derive_theme, find_theme, load_catalog,
    load_theme_settings, resolve_theme_mode, save_theme_settings,
};
use nsh_core::element::{FONT_MONO, FONT_SANS};
use nsh_core::prelude::{
    AdaptiveColor, Appearance, Hsla, Memo, Rgba, Signal, Tokens, computed, dark, provide_context,
    set_color_scheme, set_tokens, signal, use_context,
};

use crate::fonts::FONT_FAMILY_MONO;

/// 将 `falcon_theme::Rgb` 转为 NSH `Hsla`。
#[inline]
pub fn rgb_to_hsla(c: ThemeRgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

/// 将 `falcon_theme::Rgba`（alpha 0..=255 u8）转为 NSH `Hsla`。
#[inline]
pub fn rgba_to_hsla(c: ThemeRgba) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.0,
        g: c.g as f32 / 255.0,
        b: c.b as f32 / 255.0,
        a: c.a as f32 / 255.0,
    }
    .into()
}

/// 单值锁定 `AdaptiveColor`：浅深两栏同值，确保无论宿主系统明暗如何，均严格呈现当前 Ghostty 主题派生色。
#[inline]
pub fn locked_adaptive(c: Hsla) -> AdaptiveColor {
    AdaptiveColor::new(c, c)
}

/// ADR 0006：根表面的明暗外观仅取决于当前派生主题的底色亮度，与槽位无关。
#[inline]
pub fn nsh_appearance_of(resolved: &ResolvedTheme) -> Appearance {
    if resolved.is_dark {
        Appearance::Dark
    } else {
        Appearance::Light
    }
}

/// 由 `ResolvedTheme` 生成全套 NSH `Tokens`（覆盖 `nsh-base` 全部 shadcn token 与 Falcon 浮动岛扩展 token）。
pub fn tokens_from_resolved(resolved: &ResolvedTheme) -> Tokens {
    let ui = &resolved.ui;
    let mut tokens = Tokens::default();

    fn put_rgb(tokens: &mut Tokens, key: &str, c: ThemeRgb) {
        tokens.set_color(key, locked_adaptive(rgb_to_hsla(c)));
    }
    fn put_rgba(tokens: &mut Tokens, key: &str, c: ThemeRgba) {
        tokens.set_color(key, locked_adaptive(rgba_to_hsla(c)));
    }

    // 标准 shadcn / nsh-base 语义面
    put_rgb(&mut tokens, "background", ui.background);
    put_rgb(&mut tokens, "foreground", ui.foreground);
    put_rgb(&mut tokens, "card", ui.card);
    put_rgb(&mut tokens, "card-foreground", ui.card_foreground);
    put_rgb(&mut tokens, "popover", ui.popover);
    put_rgb(&mut tokens, "popover-foreground", ui.popover_foreground);
    put_rgb(&mut tokens, "primary", ui.primary);
    put_rgb(&mut tokens, "primary-foreground", ui.primary_foreground);
    put_rgb(&mut tokens, "secondary", ui.secondary);
    put_rgb(&mut tokens, "secondary-foreground", ui.secondary_foreground);
    put_rgb(&mut tokens, "muted", ui.muted);
    put_rgb(&mut tokens, "muted-foreground", ui.muted_foreground);
    put_rgb(&mut tokens, "accent", ui.accent);
    put_rgb(&mut tokens, "accent-foreground", ui.accent_foreground);
    put_rgb(&mut tokens, "destructive", ui.destructive);
    put_rgb(&mut tokens, "destructive-foreground", ui.destructive_foreground);
    put_rgba(&mut tokens, "border", ui.border);
    put_rgba(&mut tokens, "input", ui.input);
    put_rgba(&mut tokens, "ring", ui.ring);

    // 侧栏组
    put_rgb(&mut tokens, "sidebar", ui.sidebar);
    put_rgb(&mut tokens, "sidebar-foreground", ui.sidebar_foreground);
    put_rgb(&mut tokens, "sidebar-primary", ui.sidebar_primary);
    put_rgb(&mut tokens, "sidebar-primary-foreground", ui.sidebar_primary_foreground);
    put_rgb(&mut tokens, "sidebar-accent", ui.sidebar_accent);
    put_rgb(&mut tokens, "sidebar-accent-foreground", ui.sidebar_accent_foreground);
    put_rgba(&mut tokens, "sidebar-border", ui.sidebar_border);
    put_rgba(&mut tokens, "sidebar-ring", ui.sidebar_ring);

    // 图表与提交图泳道（chart-1..5 对应 graph[0..5]）
    put_rgb(&mut tokens, "chart-1", ui.graph[0]);
    put_rgb(&mut tokens, "chart-2", ui.graph[1]);
    put_rgb(&mut tokens, "chart-3", ui.graph[2]);
    put_rgb(&mut tokens, "chart-4", ui.graph[3]);
    put_rgb(&mut tokens, "chart-5", ui.graph[4]);

    // Falcon 浮动岛与状态扩展语义色（ADR 0011）
    put_rgb(&mut tokens, "app", ui.app);
    put_rgba(&mut tokens, "app-border", ui.app_border);
    put_rgb(&mut tokens, "tint", ui.tint);
    put_rgb(&mut tokens, "tint-foreground", ui.tint_foreground);
    put_rgb(&mut tokens, "tint-strong", ui.tint_strong);
    put_rgb(&mut tokens, "success", ui.success);
    put_rgb(&mut tokens, "success-foreground", ui.success_foreground);
    put_rgb(&mut tokens, "warning", ui.warning);
    put_rgb(&mut tokens, "warning-foreground", ui.warning_foreground);
    put_rgb(&mut tokens, "selection", ui.selection);

    tokens.set_number("radius", 10.0);
    tokens.set_font(FONT_SANS, FONT_FAMILY_MONO);
    tokens.set_font(FONT_MONO, FONT_FAMILY_MONO);
    tokens
}

/// 右侧栏插件语义色调 → `falcon-theme` 语义字色（ADR 0025：插件只报色调，客户端映射到当前主题 token）。
pub fn plugin_tone_color(resolved: &ResolvedTheme, tone: Option<&PluginTone>) -> Hsla {
    let ui = &resolved.ui;
    match tone {
        None | Some(PluginTone::Default) | Some(PluginTone::Unknown) => {
            rgb_to_hsla(ui.foreground)
        }
        Some(PluginTone::Muted) => rgb_to_hsla(ui.muted_foreground),
        Some(PluginTone::Accent) => rgb_to_hsla(ui.tint_strong),
        Some(PluginTone::Success) => rgb_to_hsla(ui.success),
        Some(PluginTone::Warning) => rgb_to_hsla(ui.warning),
        Some(PluginTone::Danger) => rgb_to_hsla(ui.destructive),
    }
}

pub(crate) fn load_pref_item(key: &str) -> Option<String> {
    falcon_platform::shared().load_prefs().remove(key)
}

pub(crate) fn save_pref_item(key: &str, value: Option<&str>) {
    let plat = falcon_platform::shared();
    let mut all = plat.load_prefs();
    match value {
        Some(v) => {
            all.insert(key.to_string(), v.to_string());
            plat.store_pref(key, v, &all);
        }
        None => {
            all.remove(key);
            plat.store_pref(key, "", &all);
        }
    }
}

/// 基于 `falcon_platform::shared()` 的偏好存取适配器。
struct PlatformStorage;

impl StorageLike for PlatformStorage {
    fn get_item(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(load_pref_item(key))
    }

    fn set_item(&mut self, key: &str, value: &str) -> anyhow::Result<()> {
        save_pref_item(key, Some(value));
        Ok(())
    }
}

/// 加载持久化的主题设置并按给定的系统明暗解析首帧主题。
pub fn load_initial_theme(system_is_dark: bool) -> (ThemeSettings, Arc<ResolvedTheme>) {
    let settings = load_theme_settings(Some(&PlatformStorage));
    let slot = resolve_theme_mode(settings.mode, system_is_dark);
    let resolved = Arc::new(derive_theme(&settings.slot(slot).colors));
    (settings, resolved)
}

/// 挂在根组件上下文里的响应式主题控制器。
#[derive(Clone, Copy)]
pub struct ThemeController {
    settings: Signal<ThemeSettings>,
    resolved: Memo<Arc<ResolvedTheme>>,
}

impl ThemeController {
    pub fn settings(self) -> ThemeSettings {
        self.settings.get()
    }

    pub fn resolved(self) -> Arc<ResolvedTheme> {
        self.resolved.get()
    }

    pub fn active_slot(self) -> ThemeMode {
        resolve_theme_mode(self.settings.get().mode, dark())
    }

    pub fn set_mode(self, mode: ThemePref) {
        let mut next = self.settings.get();
        if next.mode == mode {
            return;
        }
        next.mode = mode;
        self.apply_settings(next);
    }

    pub fn set_slot_choice(self, slot: ThemeMode, choice: ThemeChoice) {
        let mut next = self.settings.get();
        *next.slot_mut(slot) = choice;
        self.apply_settings(next);
    }

    pub fn select_builtin_for_slot(self, slot: ThemeMode, name: &str) -> bool {
        let Some(entry) = find_theme(load_catalog(), name) else {
            return false;
        };
        self.set_slot_choice(slot, choice_of(entry));
        true
    }

    pub fn apply_settings(self, next: ThemeSettings) {
        save_theme_settings(Some(&mut PlatformStorage), &next);
        self.settings.set(next);
        self.sync_surface();
    }

    /// 将当前派生出的 `Tokens` 与底色亮度判定的 `Appearance` 推送到 NSH 表面。
    pub fn sync_surface(self) {
        let resolved = self.resolved.get();
        set_tokens(tokens_from_resolved(&resolved));
        set_color_scheme(nsh_appearance_of(&resolved));
    }

    pub fn catalog(self) -> &'static [CatalogEntry] {
        load_catalog()
    }
}

/// 在根组件内初始化主题控制器并同步首帧 `Tokens` + `Appearance`。
pub fn install_theme(override_settings: Option<ThemeSettings>) -> ThemeController {
    let initial = override_settings.unwrap_or_else(|| load_theme_settings(Some(&PlatformStorage)));
    let settings = signal(initial);
    let resolved = computed(move || {
        let cur = settings.get();
        let slot = resolve_theme_mode(cur.mode, dark());
        Arc::new(derive_theme(&cur.slot(slot).colors))
    });
    let ctrl = ThemeController { settings, resolved };
    ctrl.sync_surface();
    provide_context(ctrl);
    ctrl
}

/// 在任意子组件中读取当前响应式主题控制器。
pub fn use_theme() -> ThemeController {
    use_context::<ThemeController>().expect("ThemeController must be installed by FalconApp root")
}
