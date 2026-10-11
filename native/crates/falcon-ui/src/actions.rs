//! 全局动作与快捷键。全局命令的键位表在 falcon-core 的 `shortcuts`（旧 React 版
//! `lib/shortcuts.ts` 的移植，与 VS Code 对齐：⌘⇧P 命令面板、⌘B 侧栏、⌘⇧E 文件……），按平台
//! 在运行时取：mac 习惯还是 Ctrl+Shift 看操作系统（浏览器里看浏览器所在的那台），为绕开浏览器
//! 保留键（⌘T / ⌘W / Ctrl+Shift+T……，preventDefault 无效）而加的 Alt 别名只在浏览器里注册——
//! 原生菜单与 keymap 没有那层限制。表外的几个（设置、缩放、退出、终端剪贴板）在这里写。
//!
//! 约束照旧：终端聚焦时 Ctrl+字母全是 shell 语义，全局键不能用裸 Ctrl+字母；mac 上用 ⌘ 系列，
//! 其它平台用 Ctrl+Shift 系列。Esc 永远归终端，只有浮层打开时由浮层自己接。

use falcon_core::shortcuts::{self, BindingRole, Command};
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
        NextCanvas,
        PrevCanvas,
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

/// 全局命令 → 它的 GPUI 动作
fn bind(cmd: Command, keys: &str) -> KeyBinding {
    match cmd {
        Command::Palette => KeyBinding::new(keys, Palette, None),
        Command::QuickOpen => KeyBinding::new(keys, QuickOpen, None),
        Command::NewTerminal => KeyBinding::new(keys, NewTerminal, None),
        Command::CloseTab => KeyBinding::new(keys, CloseTab, None),
        Command::ToggleSidebar => KeyBinding::new(keys, ToggleSidebar, None),
        Command::ToggleGitPanel => KeyBinding::new(keys, ToggleGitPanel, None),
        Command::ToggleChangesPanel => KeyBinding::new(keys, ToggleChangesPanel, None),
        // 原来转发面板的键位：中转挪进设置后（ADR 0016）打开设置的「中转」页
        Command::OpenRelays => KeyBinding::new(keys, OpenRelays, None),
        Command::ToggleFilesPanel => KeyBinding::new(keys, ToggleFilesPanel, None),
        Command::ToggleMeeglePanel => KeyBinding::new(keys, ToggleMeeglePanel, None),
        Command::Reattach => KeyBinding::new(keys, Reattach, None),
        Command::Overview => KeyBinding::new(keys, Overview, None),
        Command::NextTab => KeyBinding::new(keys, NextTab, None),
        Command::PrevTab => KeyBinding::new(keys, PrevTab, None),
        Command::NextCanvas => KeyBinding::new(keys, NextCanvas, None),
        Command::PrevCanvas => KeyBinding::new(keys, PrevCanvas, None),
        Command::Tab(1) => KeyBinding::new(keys, Tab1, None),
        Command::Tab(2) => KeyBinding::new(keys, Tab2, None),
        Command::Tab(3) => KeyBinding::new(keys, Tab3, None),
        Command::Tab(4) => KeyBinding::new(keys, Tab4, None),
        Command::Tab(5) => KeyBinding::new(keys, Tab5, None),
        Command::Tab(6) => KeyBinding::new(keys, Tab6, None),
        Command::Tab(7) => KeyBinding::new(keys, Tab7, None),
        Command::Tab(8) => KeyBinding::new(keys, Tab8, None),
        Command::Tab(_) => KeyBinding::new(keys, Tab9, None),
    }
}

pub fn init(cx: &mut App) {
    let info = falcon_platform::get(cx).info();
    crate::ui::set_mac_keys(info.mac);
    let mut bindings: Vec<KeyBinding> = Command::all()
        .into_iter()
        .flat_map(|cmd| {
            shortcuts::bindings(cmd, info.mac)
                .into_iter()
                .filter(|b| b.role != BindingRole::BrowserAlias || info.browser)
                .map(move |b| bind(cmd, &b.keystroke.gpui()))
        })
        .collect();

    // 表外的：设置、界面缩放、退出、终端里的剪贴板。mac 是 ⌘，其余 Ctrl+Shift
    let m = if info.mac { "cmd" } else { "ctrl-shift" };
    bindings.extend([
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
    ]);
    if info.mac {
        bindings.extend([
            KeyBinding::new("cmd-shift-w", DetachTab, None),
            KeyBinding::new("cmd-shift-enter", ToggleZoom, None),
        ]);
    }
    cx.bind_keys(bindings);

    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &ZoomIn, cx| crate::zoom::step(1, cx));
    cx.on_action(|_: &ZoomOut, cx| crate::zoom::step(-1, cx));
    cx.on_action(|_: &ZoomReset, cx| crate::zoom::reset(cx));
    // 连接到别的服务端 = 另开一个窗口；浏览器版只有一个窗口、连的就是页面的源
    if !info.browser {
        cx.on_action(|_: &ConnectServer, cx| crate::dialogs::connect::open_connect_window(cx));
    }

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
                MenuItem::action(t!("native.menu.nextCanvas").to_string(), NextCanvas),
                MenuItem::action(t!("native.menu.prevCanvas").to_string(), PrevCanvas),
            ],
            disabled: false,
        },
    ]);
}
