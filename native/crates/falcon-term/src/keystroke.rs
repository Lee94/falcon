//! 按键的最小表示。形状照 `gpui::Keystroke`（key / key_char / modifiers），这样 falcon-term
//! 不必依赖 GPUI，app 层逐字段搬过来即可。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    /// macOS 的 ⌘ / Windows 的 Win 键
    pub platform: bool,
    pub function: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keystroke {
    pub modifiers: Modifiers,
    /// GPUI 的键名："a"、"up"、"enter"、"f5"……字符键是小写的基础字符
    pub key: String,
    /// 这次按键实际产出的文本（含 Shift / 输入法的结果），没有就是 None
    pub key_char: Option<String>,
}

impl Keystroke {
    /// 按 GPUI 的写法解析 "ctrl-shift-a" / "alt--" / "up"。只给测试和快捷键表用。
    pub fn parse(source: &str) -> Option<Self> {
        let mut modifiers = Modifiers::default();
        let mut rest = source;
        loop {
            let (m, tail) = if let Some(t) = rest.strip_prefix("ctrl-") {
                (&mut modifiers.control, t)
            } else if let Some(t) = rest.strip_prefix("alt-") {
                (&mut modifiers.alt, t)
            } else if let Some(t) = rest.strip_prefix("shift-") {
                (&mut modifiers.shift, t)
            } else if let Some(t) = rest.strip_prefix("cmd-") {
                (&mut modifiers.platform, t)
            } else if let Some(t) = rest.strip_prefix("fn-") {
                (&mut modifiers.function, t)
            } else {
                break;
            };
            // "alt--" 的键就是 "-"：剩下的只有一个字符时不再当修饰键前缀剥
            if tail.is_empty() {
                break;
            }
            *m = true;
            rest = tail;
        }
        if rest.is_empty() {
            return None;
        }
        Some(Self {
            modifiers,
            key: rest.to_string(),
            key_char: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_key() {
        let k = Keystroke::parse("ctrl-shift-a").unwrap();
        assert!(k.modifiers.control && k.modifiers.shift && !k.modifiers.alt);
        assert_eq!(k.key, "a");
        assert_eq!(Keystroke::parse("up").unwrap().key, "up");
        assert_eq!(Keystroke::parse("alt--").unwrap().key, "-");
        assert_eq!(Keystroke::parse("alt- ").unwrap().key, " ");
        assert!(Keystroke::parse("alt--").unwrap().modifiers.alt);
    }
}
