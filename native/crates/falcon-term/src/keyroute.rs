//! 终端里一次按键归谁。`packages/web/src/lib/rio/keyRoute.ts` 的移植，去掉了 IME 分支——
//! GPUI 的组字走 `InputHandler`，组字期间的按键根本不会作为普通 keydown 到终端视图。
//!
//! - 命中全局快捷键的按键放过，让 app 的 keymap 处理；
//! - ⌘C（mac）/ Ctrl+Shift+C 只在有选区时复制，否则照常交给终端（⌘ 组合本来就不编码，
//!   Ctrl+Shift+C 在 legacy 编码下也没有对应字节）；
//! - Ctrl+Shift+V 粘贴（⌘V 走 app 的粘贴动作，不经过这里）。

use crate::keystroke::Keystroke;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRoute {
    Global,
    Copy,
    Paste,
    Terminal,
}

pub fn route_key(ks: &Keystroke, is_global_key: bool, has_selection: bool) -> KeyRoute {
    if is_global_key {
        return KeyRoute::Global;
    }
    let key = ks.key.to_lowercase();
    let m = ks.modifiers;
    let mac_copy = m.platform && !m.control && key == "c";
    let ctrl_shift = m.control && m.shift && !m.platform;
    if (mac_copy || (ctrl_shift && key == "c")) && has_selection {
        return KeyRoute::Copy;
    }
    if ctrl_shift && key == "v" {
        return KeyRoute::Paste;
    }
    KeyRoute::Terminal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn copy_only_with_selection() {
        assert_eq!(route_key(&k("cmd-c"), false, true), KeyRoute::Copy);
        assert_eq!(route_key(&k("cmd-c"), false, false), KeyRoute::Terminal);
        assert_eq!(route_key(&k("ctrl-shift-c"), false, true), KeyRoute::Copy);
        assert_eq!(route_key(&k("ctrl-c"), false, true), KeyRoute::Terminal);
    }

    #[test]
    fn paste_and_global() {
        assert_eq!(route_key(&k("ctrl-shift-v"), false, false), KeyRoute::Paste);
        assert_eq!(route_key(&k("cmd-k"), true, true), KeyRoute::Global);
    }
}
