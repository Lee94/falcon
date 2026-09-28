//! 全局快捷键的定义表。对应 web 的 `lib/shortcuts.ts`（设计文档 §4.3）。
//!
//! 键位跟 VS Code 对齐——命令面板是 ⌘⇧P / Ctrl+Shift+P / F1，其余能对上的也用同一套
//! （⌘B 侧栏、⌘⇧E 文件、⌘⇧] / ⌘⇧[ 切窗口）。
//!
//! 硬约束（两个客户端都一样）：终端聚焦时几乎吞掉所有 Ctrl+* 组合（Ctrl+C/D/R/W 全是
//! shell 语义），所以全局键**不能**用裸 Ctrl+字母。安全区是 ⌘ 系列（mac）、Ctrl+Shift+*
//! （Win/Linux 惯例上留给应用层）和 Alt+*。Esc 永远归终端，只有对话框 / 命令面板 /
//! 菜单打开时被上层拦截——那时终端必定已失焦。
//!
//! 浏览器会吞掉 ⌘T / ⌘W / Ctrl+Shift+T / Ctrl+Shift+W / Ctrl+Tab（保留快捷键，页面
//! preventDefault 无效），所以 web 给每个命令额外注册了一个 Alt 别名。**原生不需要
//! 这些别名**（设计文档 §4.3），但表里照样列出来并标成 [`BindingRole::BrowserAlias`]：
//! 注册与否由 app 决定，两边的键位表仍能逐条对照。
//!
//! 这里是数据：动作 id、各平台的键位（主键位 / 额外入口 / 浏览器别名）、说明文案的
//! i18n key、分组。GPUI 的 keybinding 字符串由 [`Keystroke::gpui`] 给出。另有
//! [`match_command`]：web `matchCommand` 的逐行移植，按物理键码判命令，给不经过
//! GPUI keymap 的场合（终端里判"这是不是全局键"）用。

/// 全局命令
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Palette,
    QuickOpen,
    NewTerminal,
    CloseTab,
    ToggleSidebar,
    ToggleGitPanel,
    ToggleChangesPanel,
    /// 打开设置并停在「中转」页（web 的 `openRelays`）。中转挂机器、在设置里管（ADR 0016），
    /// 沿用原来转发面板的 ⌘⇧F
    OpenRelays,
    ToggleFilesPanel,
    ToggleMeeglePanel,
    Reattach,
    Overview,
    NextTab,
    PrevTab,
    /// 切到第 N 个窗口，N 为 1–9
    Tab(u8),
}

impl Command {
    /// 固定的那些（不含 `Tab(n)`），web 的书写顺序
    pub const FIXED: [Command; 14] = [
        Command::Palette,
        Command::QuickOpen,
        Command::NewTerminal,
        Command::CloseTab,
        Command::ToggleSidebar,
        Command::ToggleGitPanel,
        Command::ToggleChangesPanel,
        Command::OpenRelays,
        Command::ToggleFilesPanel,
        Command::ToggleMeeglePanel,
        Command::Reattach,
        Command::Overview,
        Command::NextTab,
        Command::PrevTab,
    ];

    /// 全部命令：固定的 14 个 + tab1…tab9
    pub fn all() -> Vec<Command> {
        Self::FIXED.into_iter().chain((1..=9).map(Command::Tab)).collect()
    }

    /// web 里的动作 id（`palette`、`quickOpen`、`tab3`……）
    pub fn id(self) -> String {
        match self {
            Command::Palette => "palette".into(),
            Command::QuickOpen => "quickOpen".into(),
            Command::NewTerminal => "newTerminal".into(),
            Command::CloseTab => "closeTab".into(),
            Command::ToggleSidebar => "toggleSidebar".into(),
            Command::ToggleGitPanel => "toggleGitPanel".into(),
            Command::ToggleChangesPanel => "toggleChangesPanel".into(),
            Command::OpenRelays => "openRelays".into(),
            Command::ToggleFilesPanel => "toggleFilesPanel".into(),
            Command::ToggleMeeglePanel => "toggleMeeglePanel".into(),
            Command::Reattach => "reattach".into(),
            Command::Overview => "overview".into(),
            Command::NextTab => "nextTab".into(),
            Command::PrevTab => "prevTab".into(),
            Command::Tab(n) => format!("tab{n}"),
        }
    }

    pub fn from_id(id: &str) -> Option<Command> {
        if let Some(n) = id.strip_prefix("tab").and_then(|d| d.parse::<u8>().ok()) {
            return (1..=9).contains(&n).then_some(Command::Tab(n));
        }
        Self::FIXED.into_iter().find(|c| c.id() == id)
    }
}

/// 快捷键列表里的分组
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutGroup {
    /// 命令面板、转到文件
    General,
    /// 新建 / 关闭 / 接回会话
    Session,
    /// 侧栏与右侧各面板的开合
    Panels,
    /// 总览、在窗口之间切换
    Navigation,
}

/// 一个命令在快捷键表里的说明
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutDef {
    pub command: Command,
    /// 说明文案的 i18n key，与 web 在菜单 / tooltip / 命令面板里配这条快捷键用的那句同名。
    /// web 没有现成文案的（切窗口）是 `None`，需要时由 app 补 key。
    pub label_key: Option<&'static str>,
    pub group: ShortcutGroup,
}

/// 按 web 的书写顺序列出全部命令的说明
pub fn shortcut_defs() -> Vec<ShortcutDef> {
    use Command::*;
    use ShortcutGroup::*;
    let def = |command, label_key, group| ShortcutDef { command, label_key, group };
    let mut out = vec![
        def(Palette, Some("palette.title"), General),
        def(QuickOpen, Some("palette.goToFile"), General),
        def(NewTerminal, Some("sidebar.newTerminal"), Session),
        def(CloseTab, Some("tab.close"), Session),
        def(ToggleSidebar, Some("sidebar.collapse"), Panels),
        def(ToggleGitPanel, Some("git.panel"), Panels),
        def(ToggleChangesPanel, Some("changes.panel"), Panels),
        def(OpenRelays, Some("palette.openRelays"), Panels),
        def(ToggleFilesPanel, Some("files.panel"), Panels),
        def(ToggleMeeglePanel, Some("meegle.panel"), Panels),
        def(Reattach, Some("session.reattach"), Session),
        def(Overview, Some("palette.overview"), Navigation),
        def(NextTab, None, Navigation),
        def(PrevTab, None, Navigation),
    ];
    out.extend((1..=9).map(|n| def(Tab(n), None, Navigation)));
    out
}

/// 一次按键组合。`key` 用 GPUI 的键名：小写字母、数字、`[` `]` `` ` ``、`tab`、`f1`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Keystroke {
    /// mac 的 ⌘（GPUI 的 `cmd`，web 的 metaKey）
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: &'static str,
}

impl Keystroke {
    const fn new(key: &'static str) -> Self {
        Keystroke { cmd: false, ctrl: false, alt: false, shift: false, key }
    }
    const fn cmd(self) -> Self {
        Keystroke { cmd: true, ..self }
    }
    const fn ctrl(self) -> Self {
        Keystroke { ctrl: true, ..self }
    }
    const fn alt(self) -> Self {
        Keystroke { alt: true, ..self }
    }
    const fn shift(self) -> Self {
        Keystroke { shift: true, ..self }
    }

    /// GPUI keymap 的写法：`cmd-shift-p`、`ctrl-tab`、`f1`
    pub fn gpui(&self) -> String {
        let mut s = String::new();
        for (on, name) in [(self.ctrl, "ctrl-"), (self.alt, "alt-"), (self.shift, "shift-"), (self.cmd, "cmd-")] {
            if on {
                s.push_str(name);
            }
        }
        s.push_str(self.key);
        s
    }

    /// 对应的 web 物理键码（`KeyP`、`Digit0`、`BracketLeft`……），给 [`match_command`] 用
    pub fn web_code(&self) -> String {
        match self.key {
            "[" => "BracketLeft".into(),
            "]" => "BracketRight".into(),
            "`" => "Backquote".into(),
            "tab" => "Tab".into(),
            "f1" => "F1".into(),
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => format!("Digit{k}"),
            k => format!("Key{}", k.to_ascii_uppercase()),
        }
    }
}

/// 这条键位在 web 里是什么身份
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingRole {
    /// 菜单、tooltip 里显示的那个（web 的 `chord()`）
    Primary,
    /// VS Code 主键位之外的额外入口（旧的 ⌘K / Ctrl+K、F1、Ctrl+Shift+`），原生照样要
    Secondary,
    /// 为绕开浏览器保留键才加的 Alt 别名（web 的 `altChord()` 里除 ⌘K 以外那些）。
    /// 原生不需要
    BrowserAlias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Binding {
    pub keystroke: Keystroke,
    pub role: BindingRole,
}

const fn letter(c: &'static str) -> Keystroke {
    Keystroke::new(c)
}

/// 命令在某个平台上的全部键位，主键位排第一。与 [`match_command`] 逐条对得上（有单测）。
pub fn bindings(cmd: Command, mac: bool) -> Vec<Binding> {
    use BindingRole::*;
    let b = |keystroke, role| Binding { keystroke, role };
    // 命令的"平台主键位"：mac 是 ⌘（有的带 ⇧），其余是 Ctrl+Shift
    let panel = |key: &'static str| {
        let primary = if mac { letter(key).cmd().shift() } else { letter(key).ctrl().shift() };
        vec![b(primary, Primary), b(letter(key).alt(), BrowserAlias)]
    };
    let plain = |key: &'static str| {
        let primary = if mac { letter(key).cmd() } else { letter(key).ctrl().shift() };
        vec![b(primary, Primary), b(letter(key).alt(), BrowserAlias)]
    };
    match cmd {
        Command::Palette => vec![
            b(if mac { letter("p").cmd().shift() } else { letter("p").ctrl().shift() }, Primary),
            b(if mac { letter("k").cmd() } else { letter("k").ctrl() }, Secondary),
            b(Keystroke::new("f1"), Secondary),
        ],
        Command::QuickOpen => {
            // Win 不能绑 Ctrl+P：那是 shell 的上一条历史，所以非 mac 的主键位就是 Alt+P
            if mac {
                vec![b(letter("p").cmd(), Primary), b(letter("p").alt(), BrowserAlias)]
            } else {
                vec![b(letter("p").alt(), Primary)]
            }
        }
        Command::NewTerminal => {
            let mut v = plain("t");
            // VS Code「新建终端」：两平台都是 Ctrl+Shift+`（Mac 也用 Ctrl 不是 ⌘）
            v.insert(1, b(Keystroke::new("`").ctrl().shift(), Secondary));
            v
        }
        Command::CloseTab => plain("w"),
        Command::ToggleSidebar => plain("b"),
        Command::Reattach => plain("r"),
        Command::ToggleGitPanel => panel("g"),
        Command::ToggleChangesPanel => panel("u"),
        Command::OpenRelays => panel("f"),
        Command::ToggleFilesPanel => panel("e"),
        Command::ToggleMeeglePanel => panel("m"),
        Command::Overview | Command::Tab(_) => {
            let key = match cmd {
                Command::Tab(n) => DIGITS[n as usize % 10],
                _ => "0",
            };
            if mac {
                vec![b(Keystroke::new(key).cmd(), Primary), b(Keystroke::new(key).alt(), BrowserAlias)]
            } else {
                vec![b(Keystroke::new(key).alt(), Primary)]
            }
        }
        Command::NextTab => vec![
            b(if mac { Keystroke::new("]").cmd().shift() } else { Keystroke::new("tab").ctrl() }, Primary),
            b(Keystroke::new("]").alt(), BrowserAlias),
        ],
        Command::PrevTab => vec![
            b(if mac { Keystroke::new("[").cmd().shift() } else { Keystroke::new("tab").ctrl().shift() }, Primary),
            b(Keystroke::new("[").alt(), BrowserAlias),
        ],
    }
}

const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// 菜单、tooltip 里显示的主键位（web 的 `chord()`，字面逐字一致）
pub fn chord(cmd: Command, mac: bool) -> String {
    let pick = |m: &str, o: &str| if mac { m.to_string() } else { o.to_string() };
    match cmd {
        Command::Palette => pick("⌘⇧P", "Ctrl+Shift+P"),
        // Win 不能绑 Ctrl+P：那是 shell 的上一条历史
        Command::QuickOpen => pick("⌘P", "Alt+P"),
        Command::NewTerminal => pick("⌘T", "Ctrl+Shift+T"),
        Command::CloseTab => pick("⌘W", "Ctrl+Shift+W"),
        Command::ToggleSidebar => pick("⌘B", "Ctrl+Shift+B"),
        Command::ToggleGitPanel => pick("⌘⇧G", "Ctrl+Shift+G"),
        Command::ToggleChangesPanel => pick("⌘⇧U", "Ctrl+Shift+U"),
        Command::OpenRelays => pick("⌘⇧F", "Ctrl+Shift+F"),
        Command::ToggleFilesPanel => pick("⌘⇧E", "Ctrl+Shift+E"),
        Command::ToggleMeeglePanel => pick("⌘⇧M", "Ctrl+Shift+M"),
        Command::Reattach => pick("⌘R", "Ctrl+Shift+R"),
        Command::Overview => pick("⌘0", "Alt+0"),
        Command::NextTab => pick("⌘⇧]", "Ctrl+Tab"),
        Command::PrevTab => pick("⌘⇧[", "Ctrl+Shift+Tab"),
        Command::Tab(n) => format!("{}{n}", if mac { "⌘" } else { "Alt+" }),
    }
}

/// web 的 `altChord()`：浏览器保留了主键位时能用的别名（外加命令面板的旧入口 ⌘K / Ctrl+K）
pub fn alt_chord(cmd: Command, mac: bool) -> Option<&'static str> {
    Some(match cmd {
        Command::NewTerminal => "Alt+T",
        Command::CloseTab => "Alt+W",
        Command::ToggleSidebar => "Alt+B",
        Command::ToggleGitPanel => "Alt+G",
        Command::ToggleChangesPanel => "Alt+U",
        Command::OpenRelays => "Alt+F",
        Command::ToggleFilesPanel => "Alt+E",
        Command::ToggleMeeglePanel => "Alt+M",
        Command::Reattach => "Alt+R",
        // VS Code 主键位之外：旧的 ⌘/Ctrl+K，以及 F1
        Command::Palette => {
            if mac {
                "⌘K"
            } else {
                "Ctrl+K"
            }
        }
        Command::QuickOpen => "Alt+P",
        Command::NextTab => "Alt+]",
        Command::PrevTab => "Alt+[",
        Command::Overview | Command::Tab(_) => return None,
    })
}

/// 一次按键事件（web KeyboardEvent 的那几个字段）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyEventLike {
    /// 物理键码（`KeyP`、`Digit1`、`BracketLeft`、`Tab`、`F1`、`Backquote`）；拿不到给空串
    pub code: String,
    /// 逻辑键值（`p`、`[`、`F1`……），`code` 为空时才用
    pub key: String,
    pub alt: bool,
    pub ctrl: bool,
    /// mac 的 ⌘
    pub meta: bool,
    pub shift: bool,
}

fn alt_letter(code: &str) -> Option<Command> {
    Some(match code {
        "KeyT" => Command::NewTerminal,
        "KeyW" => Command::CloseTab,
        "KeyB" => Command::ToggleSidebar,
        "KeyG" => Command::ToggleGitPanel,
        "KeyU" => Command::ToggleChangesPanel,
        "KeyF" => Command::OpenRelays,
        "KeyE" => Command::ToggleFilesPanel,
        "KeyM" => Command::ToggleMeeglePanel,
        "KeyR" => Command::Reattach,
        "KeyP" => Command::QuickOpen,
        _ => return None,
    })
}

fn modshift_letter(code: &str) -> Option<Command> {
    Some(match code {
        "KeyP" => Command::Palette,
        "KeyT" => Command::NewTerminal,
        "KeyW" => Command::CloseTab,
        "KeyB" => Command::ToggleSidebar,
        "KeyG" => Command::ToggleGitPanel,
        "KeyU" => Command::ToggleChangesPanel,
        "KeyF" => Command::OpenRelays,
        "KeyE" => Command::ToggleFilesPanel,
        "KeyM" => Command::ToggleMeeglePanel,
        "KeyR" => Command::Reattach,
        _ => return None,
    })
}

fn mac_mod_letter(code: &str) -> Option<Command> {
    Some(match code {
        // 旧主键位。VS Code 里 ⌘K 是 chord 前缀，这里没有 chord，留着当额外入口
        "KeyK" => Command::Palette,
        "KeyP" => Command::QuickOpen,
        "KeyT" => Command::NewTerminal,
        "KeyW" => Command::CloseTab,
        "KeyB" => Command::ToggleSidebar,
        "KeyR" => Command::Reattach,
        _ => return None,
    })
}

fn mac_modshift_letter(code: &str) -> Option<Command> {
    Some(match code {
        "KeyP" => Command::Palette,
        "KeyG" => Command::ToggleGitPanel,
        "KeyU" => Command::ToggleChangesPanel,
        "KeyF" => Command::OpenRelays,
        "KeyE" => Command::ToggleFilesPanel,
        "KeyM" => Command::ToggleMeeglePanel,
        _ => return None,
    })
}

fn digit(code: &str) -> Option<u8> {
    let d = code.strip_prefix("Digit")?;
    (d.len() == 1).then(|| d.as_bytes()[0]).filter(u8::is_ascii_digit).map(|b| b - b'0')
}

fn digit_command(d: u8) -> Command {
    if d == 0 { Command::Overview } else { Command::Tab(d) }
}

/// 物理键位。优先用 code（Alt+字母在 mac 上会变成特殊字符，key 认不出来），
/// 但有些输入法、远程桌面和合成事件不带 code，退回按 key 推。
fn event_code(e: &KeyEventLike) -> String {
    if !e.code.is_empty() {
        return e.code.clone();
    }
    let key = e.key.as_str();
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphabetic() {
            return format!("Key{}", c.to_ascii_uppercase());
        }
        if c.is_ascii_digit() {
            return format!("Digit{c}");
        }
    }
    match key {
        "[" => "BracketLeft".into(),
        "]" => "BracketRight".into(),
        "Tab" => "Tab".into(),
        "F1" => "F1".into(),
        "`" | "~" => "Backquote".into(),
        _ => String::new(),
    }
}

/// 命中则返回命令。web `matchCommand` 的逐行移植（含它的优先级：F1 → Ctrl+Shift+` →
/// Alt 别名 → 平台修饰键）。注意平台修饰键那一支不看 Alt：mac 上 ⌘⌥P 也是 quickOpen。
pub fn match_command(e: &KeyEventLike, mac: bool) -> Option<Command> {
    let modifier = if mac { e.meta && !e.ctrl } else { e.ctrl && !e.meta };
    let code = event_code(e);
    if code.is_empty() {
        return None;
    }

    // F1：VS Code 命令面板，两平台、无修饰键
    if code == "F1" && !e.alt && !e.ctrl && !e.meta && !e.shift {
        return Some(Command::Palette);
    }

    // VS Code「新建终端」：两平台都是 Ctrl+Shift+`（Mac 也用 Ctrl 不是 ⌘）
    if e.ctrl && e.shift && !e.alt && !e.meta && code == "Backquote" {
        return Some(Command::NewTerminal);
    }

    // Alt 别名（两个平台都有）
    if e.alt && !e.ctrl && !e.meta && !e.shift {
        if let Some(d) = digit(&code) {
            return Some(digit_command(d));
        }
        return match code.as_str() {
            "BracketRight" => Some(Command::NextTab),
            "BracketLeft" => Some(Command::PrevTab),
            c => alt_letter(c),
        };
    }

    if !modifier {
        return None;
    }

    if e.shift {
        if mac {
            return match code.as_str() {
                "BracketRight" => Some(Command::NextTab),
                "BracketLeft" => Some(Command::PrevTab),
                c => mac_modshift_letter(c),
            };
        }
        if code == "Tab" {
            return Some(Command::PrevTab);
        }
        return modshift_letter(&code);
    }

    if !mac {
        return match code.as_str() {
            "Tab" => Some(Command::NextTab),
            "KeyK" => Some(Command::Palette),
            _ => None,
        };
    }

    if let Some(d) = digit(&code) {
        return Some(digit_command(d));
    }
    mac_mod_letter(&code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mods {
        alt: bool,
        ctrl: bool,
        meta: bool,
        shift: bool,
        key: &'static str,
    }

    fn key(code: &str, m: Mods) -> KeyEventLike {
        KeyEventLike { code: code.into(), key: m.key.into(), alt: m.alt, ctrl: m.ctrl, meta: m.meta, shift: m.shift }
    }

    fn hit(mac: bool, code: &str, m: Mods) -> Option<Command> {
        match_command(&key(code, m), mac)
    }

    const META: Mods = Mods { alt: false, ctrl: false, meta: true, shift: false, key: "" };
    const META_SHIFT: Mods = Mods { alt: false, ctrl: false, meta: true, shift: true, key: "" };
    const CTRL: Mods = Mods { alt: false, ctrl: true, meta: false, shift: false, key: "" };
    const CTRL_SHIFT: Mods = Mods { alt: false, ctrl: true, meta: false, shift: true, key: "" };
    const ALT: Mods = Mods { alt: true, ctrl: false, meta: false, shift: false, key: "" };

    // ---- chord ----

    #[test]
    fn chord_palette_follows_vs_code() {
        assert_eq!(chord(Command::Palette, true), "⌘⇧P");
        assert_eq!(chord(Command::Palette, false), "Ctrl+Shift+P");
    }

    #[test]
    fn chord_other_display_keys_do_not_change_with_the_palette() {
        assert_eq!(chord(Command::ToggleSidebar, true), "⌘B");
        assert_eq!(chord(Command::ToggleFilesPanel, true), "⌘⇧E");
        assert_eq!(chord(Command::NextTab, true), "⌘⇧]");
        assert_eq!(chord(Command::NewTerminal, false), "Ctrl+Shift+T");
    }

    // ---- altChord ----

    #[test]
    fn alt_chord_palette_extra_entry_is_the_old_cmd_ctrl_k() {
        assert_eq!(alt_chord(Command::Palette, true), Some("⌘K"));
        assert_eq!(alt_chord(Command::Palette, false), Some("Ctrl+K"));
    }

    // ---- matchCommand: 命令面板 ----

    #[test]
    fn palette_opens_with_cmd_shift_p_and_ctrl_shift_p() {
        assert_eq!(hit(true, "KeyP", META_SHIFT), Some(Command::Palette));
        assert_eq!(hit(false, "KeyP", CTRL_SHIFT), Some(Command::Palette));
    }

    #[test]
    fn f1_opens_the_palette_on_both_platforms() {
        assert_eq!(hit(true, "F1", Mods::default()), Some(Command::Palette));
        assert_eq!(hit(false, "F1", Mods::default()), Some(Command::Palette));
        assert_eq!(hit(true, "", Mods { key: "F1", ..Mods::default() }), Some(Command::Palette));
    }

    #[test]
    fn the_old_cmd_k_ctrl_k_still_works() {
        assert_eq!(hit(true, "KeyK", META), Some(Command::Palette));
        assert_eq!(hit(false, "KeyK", CTRL), Some(Command::Palette));
    }

    // ---- matchCommand: Quick Open ----

    #[test]
    fn quick_open_is_cmd_p_on_mac_and_does_not_steal_ctrl_p_on_win() {
        assert_eq!(hit(true, "KeyP", META), Some(Command::QuickOpen));
        assert_eq!(hit(false, "KeyP", CTRL), None);
        assert_eq!(hit(true, "KeyP", CTRL), None);
        assert_eq!(chord(Command::QuickOpen, true), "⌘P");
        assert_eq!(chord(Command::QuickOpen, false), "Alt+P");
    }

    #[test]
    fn alt_p_opens_quick_open_on_both_platforms() {
        assert_eq!(hit(true, "KeyP", ALT), Some(Command::QuickOpen));
        assert_eq!(hit(false, "KeyP", ALT), Some(Command::QuickOpen));
    }

    #[test]
    fn cmd_shift_p_stays_the_palette() {
        assert_eq!(hit(true, "KeyP", META_SHIFT), Some(Command::Palette));
        assert_eq!(hit(false, "KeyP", CTRL_SHIFT), Some(Command::Palette));
    }

    // ---- matchCommand: VS Code 新建终端 Ctrl+Shift+` ----

    #[test]
    fn ctrl_shift_backquote_is_new_terminal_on_both_platforms() {
        assert_eq!(hit(true, "Backquote", CTRL_SHIFT), Some(Command::NewTerminal));
        assert_eq!(hit(false, "Backquote", CTRL_SHIFT), Some(Command::NewTerminal));
        assert_eq!(hit(true, "", Mods { key: "`", ..CTRL_SHIFT }), Some(Command::NewTerminal));
    }

    #[test]
    fn cmd_shift_backquote_is_not_this_binding() {
        assert_eq!(hit(true, "Backquote", META_SHIFT), None);
    }

    // ---- matchCommand: 其余 VS Code 对齐的键仍在 ----

    #[test]
    fn remaining_vs_code_aligned_keys() {
        let cases: [(bool, &str, Mods, Command); 12] = [
            (true, "KeyB", META, Command::ToggleSidebar),
            (true, "KeyE", META_SHIFT, Command::ToggleFilesPanel),
            (true, "KeyG", META_SHIFT, Command::ToggleGitPanel),
            (true, "BracketRight", META_SHIFT, Command::NextTab),
            (true, "BracketLeft", META_SHIFT, Command::PrevTab),
            (true, "KeyT", META, Command::NewTerminal),
            (true, "KeyW", META, Command::CloseTab),
            (false, "KeyE", CTRL_SHIFT, Command::ToggleFilesPanel),
            (false, "KeyG", CTRL_SHIFT, Command::ToggleGitPanel),
            (false, "KeyB", CTRL_SHIFT, Command::ToggleSidebar),
            (false, "Tab", CTRL, Command::NextTab),
            (false, "Tab", CTRL_SHIFT, Command::PrevTab),
        ];
        for (mac, code, mods, cmd) in cases {
            assert_eq!(hit(mac, code, mods), Some(cmd), "{} {code} → {}", if mac { "Mac" } else { "Win" }, cmd.id());
        }
    }

    #[test]
    fn does_not_steal_bare_ctrl_letters_from_the_terminal() {
        for code in ["KeyC", "KeyR", "KeyW", "KeyB"] {
            assert_eq!(hit(false, code, CTRL), None, "{code}");
        }
    }

    // ---- 以下是 Rust 侧补的：定义表与 match_command / chord 自洽 ----

    #[test]
    fn every_binding_in_the_table_matches_its_command() {
        for mac in [true, false] {
            for cmd in Command::all() {
                let bs = bindings(cmd, mac);
                assert_eq!(bs[0].role, BindingRole::Primary, "{}", cmd.id());
                for b in bs {
                    let k = b.keystroke;
                    let e = KeyEventLike { code: k.web_code(), key: String::new(), alt: k.alt, ctrl: k.ctrl, meta: k.cmd, shift: k.shift };
                    assert_eq!(match_command(&e, mac), Some(cmd), "mac={mac} {} {}", cmd.id(), k.gpui());
                }
            }
        }
    }

    #[test]
    fn browser_aliases_are_exactly_the_alt_chords_that_are_not_primary() {
        for mac in [true, false] {
            for cmd in Command::all() {
                let aliases: Vec<Binding> =
                    bindings(cmd, mac).into_iter().filter(|b| b.role == BindingRole::BrowserAlias).collect();
                for a in &aliases {
                    assert!(a.keystroke.alt && !a.keystroke.cmd && !a.keystroke.ctrl && !a.keystroke.shift);
                }
                // 主键位本身就是 Alt 的（非 mac 的 quickOpen / overview / tabN）不再另标别名
                if !mac && matches!(cmd, Command::QuickOpen | Command::Overview | Command::Tab(_)) {
                    assert!(aliases.is_empty(), "{}", cmd.id());
                }
            }
        }
    }

    #[test]
    fn defs_cover_every_command_once_and_ids_round_trip() {
        let defs = shortcut_defs();
        assert_eq!(defs.iter().map(|d| d.command).collect::<Vec<_>>(), Command::all());
        for cmd in Command::all() {
            assert_eq!(Command::from_id(&cmd.id()), Some(cmd));
        }
        assert_eq!(Command::from_id("tab0"), None);
        assert_eq!(Command::from_id("tab10"), None);
        assert_eq!(chord(Command::Tab(3), true), "⌘3");
        assert_eq!(chord(Command::Tab(3), false), "Alt+3");
        assert_eq!(bindings(Command::Palette, true)[0].keystroke.gpui(), "shift-cmd-p");
        assert_eq!(bindings(Command::NextTab, false)[0].keystroke.gpui(), "ctrl-tab");
        assert_eq!(bindings(Command::NewTerminal, true)[1].keystroke.gpui(), "ctrl-shift-`");
    }
}
