//! 全局动作与快捷键。全局命令的键位表在 falcon-core 的 `shortcuts`（旧 React 版
//! `lib/shortcuts.ts` 的移植，与 VS Code 对齐：⌘⇧P 命令面板、⌘B 侧栏、⌘⇧E 文件……），按平台
//! 在运行时取：mac 习惯还是 Ctrl+Shift 看操作系统（浏览器里看浏览器所在的那台），为绕开浏览器
//! 保留键（⌘T / ⌘W / Ctrl+Shift+T……，preventDefault 无效）而加的 Alt 别名只在浏览器里注册——
//! 原生菜单与 keymap 没有那层限制。表外的几个（设置、缩放、退出、终端剪贴板）在这里写。
//!
//! 约束照旧：终端聚焦时 Ctrl+字母全是 shell 语义，全局键不能用裸 Ctrl+字母；mac 上用 ⌘ 系列，
//! 其它平台用 Ctrl+Shift 系列。Esc 永远归终端，只有浮层打开时由浮层自己接。

use std::rc::Rc;

use falcon_core::shortcuts::{self, BindingRole, Command};
use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate, Menu, MenuItem, NoAction, actions};

actions!(
    falcon,
    [
        Palette,
        QuickOpen,
        NewTerminal,
        SplitRight,
        SplitDown,
        CloseTab,
        DetachTab,
        ToggleSidebar,
        ToggleChangesPanel,
        OpenRelays,
        ToggleFilesPanel,
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

/// 文件窗口里的动作（key context "FileView"）
pub mod file_view {
    use gpui_kit::actions;
    actions!(file_view, [Save]);
}

/// 终端窗口里的动作（key context "Terminal"）
pub mod term {
    use gpui_kit::actions;
    actions!(terminal, [Copy, Paste, SelectAll, Clear]);
}

/// 运行时字符串的键位（gpui 0.3.12 起 `KeyBinding::new` 只接 `&'static`，动态键走 `load`）
fn dyn_binding<A: Action>(keys: &str, action: A, context: Option<&str>, cx: &App) -> KeyBinding {
    let pred = context
        .map(|c| KeyBindingContextPredicate::parse(c).unwrap())
        .map(Rc::new);
    KeyBinding::load(keys, Box::new(action), pred, false, None, cx).unwrap()
}

/// 全局命令 → 它的 GPUI 动作
fn bind(cmd: Command, keys: &str, cx: &App) -> KeyBinding {
    match cmd {
        Command::Palette => dyn_binding(keys, Palette, None, cx),
        Command::QuickOpen => dyn_binding(keys, QuickOpen, None, cx),
        Command::NewTerminal => dyn_binding(keys, NewTerminal, None, cx),
        Command::SplitRight => dyn_binding(keys, SplitRight, None, cx),
        Command::SplitDown => dyn_binding(keys, SplitDown, None, cx),
        Command::CloseTab => dyn_binding(keys, CloseTab, None, cx),
        Command::ToggleSidebar => dyn_binding(keys, ToggleSidebar, None, cx),
        Command::ToggleChangesPanel => dyn_binding(keys, ToggleChangesPanel, None, cx),
        // 原来转发面板的键位：中转挪进设置后（ADR 0016）打开设置的「中转」页
        Command::OpenRelays => dyn_binding(keys, OpenRelays, None, cx),
        Command::ToggleFilesPanel => dyn_binding(keys, ToggleFilesPanel, None, cx),
        Command::Reattach => dyn_binding(keys, Reattach, None, cx),
        Command::Overview => dyn_binding(keys, Overview, None, cx),
        Command::NextTab => dyn_binding(keys, NextTab, None, cx),
        Command::PrevTab => dyn_binding(keys, PrevTab, None, cx),
        Command::NextCanvas => dyn_binding(keys, NextCanvas, None, cx),
        Command::PrevCanvas => dyn_binding(keys, PrevCanvas, None, cx),
        Command::Tab(1) => dyn_binding(keys, Tab1, None, cx),
        Command::Tab(2) => dyn_binding(keys, Tab2, None, cx),
        Command::Tab(3) => dyn_binding(keys, Tab3, None, cx),
        Command::Tab(4) => dyn_binding(keys, Tab4, None, cx),
        Command::Tab(5) => dyn_binding(keys, Tab5, None, cx),
        Command::Tab(6) => dyn_binding(keys, Tab6, None, cx),
        Command::Tab(7) => dyn_binding(keys, Tab7, None, cx),
        Command::Tab(8) => dyn_binding(keys, Tab8, None, cx),
        Command::Tab(_) => dyn_binding(keys, Tab9, None, cx),
    }
}

pub fn init(cx: &mut App) {
    let info = falcon_platform::get(cx).info();
    crate::ui::set_mac_keys(info.mac);
    let app: &App = cx;
    let mut bindings: Vec<KeyBinding> = Command::all()
        .into_iter()
        .flat_map(|cmd| {
            shortcuts::bindings(cmd, info.mac)
                .into_iter()
                .filter(|b| b.role != BindingRole::BrowserAlias)
                .map(move |b| bind(cmd, &b.keystroke.gpui(), app))
        })
        .collect();

    // 表外的：设置、界面缩放、退出、终端里的剪贴板。mac 是 ⌘，其余 Ctrl+Shift
    let m = if info.mac { "cmd" } else { "ctrl-shift" };
    bindings.extend([
        dyn_binding(&format!("{m}-,"), OpenSettings, None, app),
        // 界面缩放（⌘+ / ⌘−）：= 与 + 同一个键，带不带 Shift 都认。⌘0 已经是会话总览，
        // 「实际大小」只放在菜单与设置里
        dyn_binding(&format!("{m}-="), ZoomIn, None, app),
        dyn_binding(&format!("{m}-+"), ZoomIn, None, app),
        dyn_binding(&format!("{m}--"), ZoomOut, None, app),
        KeyBinding::new("cmd-q", Quit, None),
        // 终端里的剪贴板 / 全选（只在终端聚焦时生效）
        dyn_binding(&format!("{m}-c"), term::Copy, Some("Terminal"), app),
        dyn_binding(&format!("{m}-v"), term::Paste, Some("Terminal"), app),
        dyn_binding(&format!("{m}-a"), term::SelectAll, Some("Terminal"), app),
        // gpui-component 的 Root 把 Tab / Shift+Tab 绑成了焦点轮转（context "Root"），终端在它
        // 下面，keymap 先于 key_down 匹配，不压掉的话 Tab 永远到不了 shell（补全失灵）。
        // NoAction 在更深的 context 上屏蔽掉它，按键落回终端自己的 key_down 编码。
        KeyBinding::new("tab", NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", NoAction, Some("Terminal")),
        // 文件窗口里保存。不在终端里，裸 Ctrl+S 没有 shell 语义要让
        KeyBinding::new(if info.mac { "cmd-s" } else { "ctrl-s" }, file_view::Save, Some("FileView")),
    ]);
    if info.mac {
        bindings.extend([
            KeyBinding::new("cmd-shift-w", DetachTab, None),
            KeyBinding::new("cmd-shift-enter", ToggleZoom, None),
        ]);
    }
    cx.bind_keys(bindings);

    cx.on_action(|_: &Quit, cx| crate::window::quit(cx));
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
                MenuItem::action(t!("tab.newToRight").to_string(), SplitRight),
                MenuItem::action(t!("tab.newBelow").to_string(), SplitDown),
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
