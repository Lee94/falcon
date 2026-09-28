//! 全局动作与快捷键。键位照 web 的 `lib/shortcuts.ts`（与 VS Code 对齐：⌘⇧P 命令面板、⌘B
//! 侧栏、⌘⇧E 文件……）。web 为绕开浏览器保留键而加的 Alt 别名这里不要——原生菜单与 keymap
//! 没有那层限制。
//!
//! 约束照旧：终端聚焦时 Ctrl+字母全是 shell 语义，全局键不能用裸 Ctrl+字母；mac 上用 ⌘ 系列，
//! 其它平台用 Ctrl+Shift 系列。Esc 永远归终端，只有浮层打开时由浮层自己接。

use gpui_kit::{App, KeyBinding, Menu, MenuItem, NoAction, actions};

actions!(
    falcon,
    [
        Palette,
        QuickOpen,
        NewTerminal,
        CloseTab,
        DetachTab,
        ToggleSidebar,
        ToggleGitPanel,
        ToggleChangesPanel,
        OpenRelays,
        ToggleFilesPanel,
        ToggleMeeglePanel,
        ToggleZoom,
        Reattach,
        Overview,
        NextTab,
        PrevTab,
        Tab1,
        Tab2,
        Tab3,
        Tab4,
        Tab5,
        Tab6,
        Tab7,
        Tab8,
        Tab9,
        OpenSettings,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ConnectServer,
        NewProject,
        Quit,
    ]
);

/// 终端窗口里的动作（key context "Terminal"）
pub mod term {
    use gpui_kit::actions;
    actions!(terminal, [Copy, Paste, SelectAll, Clear]);
}

#[cfg(target_os = "macos")]
const MOD: &str = "cmd";
#[cfg(not(target_os = "macos"))]
const MOD: &str = "ctrl-shift";

pub fn init(cx: &mut App) {
    let m = MOD;
    let mut bindings = vec![
        KeyBinding::new(&format!("{m}-shift-p"), Palette, None),
        KeyBinding::new("f1", Palette, None),
        KeyBinding::new(&format!("{m}-shift-g"), ToggleGitPanel, None),
        KeyBinding::new(&format!("{m}-shift-u"), ToggleChangesPanel, None),
        // 原来转发面板的键位：中转挪进设置后（ADR 0016）打开设置的「中转」页
        KeyBinding::new(&format!("{m}-shift-f"), OpenRelays, None),
        KeyBinding::new(&format!("{m}-shift-e"), ToggleFilesPanel, None),
        KeyBinding::new(&format!("{m}-shift-m"), ToggleMeeglePanel, None),
        // VS Code「新建终端」：两平台都是 Ctrl+Shift+`
        KeyBinding::new("ctrl-shift-`", NewTerminal, None),
        KeyBinding::new(&format!("{m}-,"), OpenSettings, None),
        // 界面缩放（浏览器的 ⌘+ / ⌘−）：= 与 + 同一个键，带不带 Shift 都认。⌘0 已经是会话总览，
        // 「实际大小」只放在菜单与设置里
        KeyBinding::new(&format!("{m}-="), ZoomIn, None),
        KeyBinding::new(&format!("{m}-+"), ZoomIn, None),
        KeyBinding::new(&format!("{m}--"), ZoomOut, None),
        KeyBinding::new("cmd-q", Quit, None),
        // 终端里的剪贴板 / 全选（只在终端聚焦时生效）
        KeyBinding::new(&format!("{m}-c"), term::Copy, Some("Terminal")),
        KeyBinding::new(&format!("{m}-v"), term::Paste, Some("Terminal")),
        KeyBinding::new(&format!("{m}-a"), term::SelectAll, Some("Terminal")),
        // gpui-component 的 Root 把 Tab / Shift+Tab 绑成了焦点轮转（context "Root"），终端在它
        // 下面，keymap 先于 key_down 匹配，不压掉的话 Tab 永远到不了 shell（补全失灵）。
        // NoAction 在更深的 context 上屏蔽掉它，按键落回终端自己的 key_down 编码。
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
    ];
    #[cfg(target_os = "macos")]
    bindings.extend([
        // 旧主键位 ⌘K 留着当命令面板的额外入口（web 同）
        KeyBinding::new("cmd-k", Palette, None),
        KeyBinding::new("cmd-p", QuickOpen, None),
        KeyBinding::new("cmd-t", NewTerminal, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-shift-w", DetachTab, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-r", Reattach, None),
        KeyBinding::new("cmd-0", Overview, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PrevTab, None),
        KeyBinding::new("cmd-shift-enter", ToggleZoom, None),
        KeyBinding::new("cmd-1", Tab1, None),
        KeyBinding::new("cmd-2", Tab2, None),
        KeyBinding::new("cmd-3", Tab3, None),
        KeyBinding::new("cmd-4", Tab4, None),
        KeyBinding::new("cmd-5", Tab5, None),
        KeyBinding::new("cmd-6", Tab6, None),
        KeyBinding::new("cmd-7", Tab7, None),
        KeyBinding::new("cmd-8", Tab8, None),
        KeyBinding::new("cmd-9", Tab9, None),
    ]);
    #[cfg(not(target_os = "macos"))]
    bindings.extend([
        KeyBinding::new("ctrl-k", Palette, None),
        KeyBinding::new("alt-p", QuickOpen, None),
        KeyBinding::new("ctrl-shift-t", NewTerminal, None),
        KeyBinding::new("ctrl-shift-w", CloseTab, None),
        KeyBinding::new("ctrl-shift-b", ToggleSidebar, None),
        KeyBinding::new("ctrl-shift-r", Reattach, None),
        KeyBinding::new("alt-0", Overview, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PrevTab, None),
        KeyBinding::new("alt-1", Tab1, None),
        KeyBinding::new("alt-2", Tab2, None),
        KeyBinding::new("alt-3", Tab3, None),
        KeyBinding::new("alt-4", Tab4, None),
        KeyBinding::new("alt-5", Tab5, None),
        KeyBinding::new("alt-6", Tab6, None),
        KeyBinding::new("alt-7", Tab7, None),
        KeyBinding::new("alt-8", Tab8, None),
        KeyBinding::new("alt-9", Tab9, None),
    ]);
    cx.bind_keys(bindings);

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &ZoomIn, cx| crate::zoom::step(1, cx));
    cx.on_action(|_: &ZoomOut, cx| crate::zoom::step(-1, cx));
    cx.on_action(|_: &ZoomReset, cx| crate::zoom::reset(cx));
    cx.on_action(|_: &ConnectServer, cx| crate::dialogs::connect::open_connect_window(cx));

    set_menus(cx);
}

fn set_menus(cx: &mut App) {
    use rust_i18n::t;
    cx.set_menus(vec![
        Menu {
            name: "Falcon".into(),
            items: vec![
                MenuItem::action(t!("native.menu.settings").to_string(), OpenSettings),
                MenuItem::action(t!("native.menu.relays").to_string(), OpenRelays),
                MenuItem::action(t!("native.menu.connect").to_string(), ConnectServer),
                MenuItem::separator(),
                MenuItem::action(t!("native.menu.quit").to_string(), Quit),
            ],
            disabled: false,
        },
        Menu {
            name: t!("native.menu.file").to_string().into(),
            items: vec![
                MenuItem::action(t!("sidebar.newTerminal").to_string(), NewTerminal),
                MenuItem::action(t!("sidebar.newProject").to_string(), NewProject),
                MenuItem::separator(),
                MenuItem::action(t!("native.menu.closeTab").to_string(), CloseTab),
                MenuItem::action(t!("native.menu.detachTab").to_string(), DetachTab),
            ],
            disabled: false,
        },
        Menu {
            name: t!("native.menu.edit").to_string().into(),
            items: vec![
                MenuItem::action(t!("native.menu.copy").to_string(), term::Copy),
                MenuItem::action(t!("native.menu.paste").to_string(), term::Paste),
                MenuItem::action(t!("native.menu.selectAll").to_string(), term::SelectAll),
            ],
            disabled: false,
        },
        Menu {
            name: t!("native.menu.view").to_string().into(),
            items: vec![
                MenuItem::action(t!("native.menu.palette").to_string(), Palette),
                MenuItem::action(t!("native.menu.quickOpen").to_string(), QuickOpen),
                MenuItem::separator(),
                MenuItem::action(t!("sidebar.collapse").to_string(), ToggleSidebar),
                MenuItem::action(t!("native.menu.files").to_string(), ToggleFilesPanel),
                MenuItem::action(t!("native.menu.changes").to_string(), ToggleChangesPanel),
                MenuItem::action(t!("native.menu.git").to_string(), ToggleGitPanel),
                MenuItem::action(t!("native.menu.meegle").to_string(), ToggleMeeglePanel),
                MenuItem::separator(),
                MenuItem::action(t!("native.menu.zoomIn").to_string(), ZoomIn),
                MenuItem::action(t!("native.menu.zoomOut").to_string(), ZoomOut),
                MenuItem::action(t!("native.menu.zoomReset").to_string(), ZoomReset),
                MenuItem::separator(),
                MenuItem::action(t!("native.menu.zoom").to_string(), ToggleZoom),
                MenuItem::action(t!("native.menu.overview").to_string(), Overview),
                MenuItem::action(t!("native.menu.nextTab").to_string(), NextTab),
                MenuItem::action(t!("native.menu.prevTab").to_string(), PrevTab),
            ],
            disabled: false,
        },
    ]);
}
