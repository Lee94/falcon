//! 终端深浅线索：写进 PTY 环境，并用来应答 OSC 10/11/12 查询。移植自
//! `packages/shared/src/termEnv.ts` 的运行时部分（类型 `TermAppearance` / `OscColorHint`
//! 在 falcon-proto 的 `term_env`）。纯函数与纯状态，零 I/O。
//!
//! 主题画在客户端上，进程只看得到 PTY。grok / vim / less 靠
//! COLORFGBG、GROK_APPEARANCE，或启动时问一声 OSC 11。这边给出统一的值。
//!
//! 环境变量一律是**有序**的 `(名, 值)` 列表（与 `zellij::host` 同一约定）：TS 里是
//! 对象的插入顺序，发到宿主机的命令串要逐字节一致。

use std::sync::LazyLock;

use falcon_proto::{OscColorHint, TermAppearance};
use regex::{Captures, Regex};
use serde_json::Value;

/// `isTermAppearance`：只认字面量 "light" / "dark"（TS 收 `unknown`，这里 None = undefined）
pub fn is_term_appearance(v: Option<&Value>) -> bool {
    term_appearance_of(v).is_some()
}

/// [`is_term_appearance`] 成立时顺手给出值
pub fn term_appearance_of(v: Option<&Value>) -> Option<TermAppearance> {
    match v {
        Some(Value::String(s)) if s == "light" => Some(TermAppearance::Light),
        Some(Value::String(s)) if s == "dark" => Some(TermAppearance::Dark),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// 合法的 #rgb / #rrggbb（忽略 alpha）。
///
/// TS 是 `/^#([\da-f]{3}|[\da-f]{6}|[\da-f]{8})$/i` 匹配 `hex.trim()`；这里手写同一判断
/// （只认 ASCII 十六进制位，首尾空白按 JS 的 trim 去掉）。
pub fn parse_hex_rgb(hex: &str) -> Option<Rgb> {
    let body = js::trim(hex).strip_prefix('#')?;
    if !matches!(body.len(), 3 | 6 | 8) || !body.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let h: String = if body.len() == 3 { body.chars().flat_map(|c| [c, c]).collect() } else { body.to_string() };
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some(Rgb { r: byte(0)?, g: byte(2)?, b: byte(4)? })
}

/// 相对亮度 0–1（sRGB / WCAG）。解析失败当纯黑。
pub fn hex_luminance(hex: &str) -> f64 {
    let Some(n) = parse_hex_rgb(hex) else { return 0.0 };
    let lin = |c: u8| {
        let s = f64::from(c) / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(n.r) + 0.7152 * lin(n.g) + 0.0722 * lin(n.b)
}

pub fn appearance_from_hex(hex: Option<&str>) -> TermAppearance {
    if hex_luminance(hex.unwrap_or("#000000")) > 0.5 { TermAppearance::Light } else { TermAppearance::Dark }
}

/// 深浅线索到了、底字没到时的兜底，与 falcon-theme 内置的 Falcon Dark / Falcon Light（`catalog.rs`）对齐。
/// 返回 (bg, fg)。
fn fallback(appearance: TermAppearance) -> (&'static str, &'static str) {
    match appearance {
        TermAppearance::Light => ("#ffffff", "#171717"),
        TermAppearance::Dark => ("#0a0a0a", "#fafafa"),
    }
}

/// 写入 PTY 的环境。
///
/// TERM_PROGRAM=falcon 是实话，不冒充 vscode——grok 对 vscode 的键盘特例
/// 不该被我们误触发。COLORTERM 与 TERM 分开：TERM 仍是 xterm-256color
/// （terminfo 最稳的名字），truecolor 靠 COLORTERM 声明。
///
/// appearance 缺省时不写 COLORFGBG / GROK_*：调用方必须先清掉宿主进程
/// 带来的同名变量，否则会继承「启动 falcon 的那个 iTerm」的深浅。
pub fn term_pty_env(appearance: Option<TermAppearance>) -> Vec<(&'static str, &'static str)> {
    let mut env = vec![("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TERM_PROGRAM", "falcon")];
    match appearance {
        Some(TermAppearance::Light) => {
            env.extend([("COLORFGBG", "0;15"), ("GROK_APPEARANCE", "light"), ("LC_GROK_APPEARANCE", "light")])
        }
        Some(TermAppearance::Dark) => {
            env.extend([("COLORFGBG", "15;0"), ("GROK_APPEARANCE", "dark"), ("LC_GROK_APPEARANCE", "dark")])
        }
        None => {}
    }
    env
}

pub const TERM_APPEARANCE_KEYS: [&str; 3] = ["COLORFGBG", "GROK_APPEARANCE", "LC_GROK_APPEARANCE"];

/// 有序 env 的赋值：已有同名键就地改值（保持原位置，与 JS 对象一致），没有就追加
fn env_set(env: &mut Vec<(String, String)>, key: String, value: String) {
    match env.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => env.push((key, value)),
    }
}

/// 叠到已有 env 上：先丢掉宿主的深浅线索，再写入我们的。
/// base 里的 undefined（这里是 `None`）会被扔掉，node-pty 不接受。
///
/// base 的值可以是 `String`（普通 env）或 `Option<String>`（带 undefined 的）。
pub fn apply_term_pty_env<I, K, V>(base: I, appearance: Option<TermAppearance>) -> Vec<(String, String)>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<Option<String>>,
{
    let mut env: Vec<(String, String)> = Vec::new();
    for (k, v) in base {
        if let Some(v) = v.into() {
            env_set(&mut env, k.into(), v);
        }
    }
    env.retain(|(k, _)| !TERM_APPEARANCE_KEYS.contains(&k.as_str()));
    for (k, v) in term_pty_env(appearance) {
        env_set(&mut env, k.to_string(), v.to_string());
    }
    env
}

/// #rrggbb → xterm `rgb:rrrr/gggg/bbbb`（每通道 16 bit，重复字节）。
pub fn hex_to_osc_rgb(hex: &str) -> Option<String> {
    let n = parse_hex_rgb(hex)?;
    let c = |v: u8| format!("{v:02x}{v:02x}");
    Some(format!("rgb:{}/{}/{}", c(n.r), c(n.g), c(n.b)))
}

/// REST / WS 入口用：非法值整项丢掉，不当成 4xx——深浅缺失只是退回不注入。
///
/// raw 是请求体 / WS 消息原文；不是对象时（TS 里会直接抛 TypeError，实际到不了）当空对象。
/// 合法的颜色**原样**保留（不去首尾空白），与 TS 一致。
pub fn sanitize_color_hint(raw: &Value) -> OscColorHint {
    let get = |k: &str| match raw {
        Value::Object(m) => m.get(k),
        _ => None,
    };
    let color = |k: &str| match get(k) {
        Some(Value::String(s)) if parse_hex_rgb(s).is_some() => Some(s.clone()),
        _ => None,
    };
    OscColorHint {
        appearance: term_appearance_of(get("appearance")),
        background: color("background"),
        foreground: color("foreground"),
    }
}

fn osc_color(hex: Option<&str>, fallback: &str) -> String {
    hex_to_osc_rgb(hex.unwrap_or("")).or_else(|| hex_to_osc_rgb(fallback)).unwrap_or_default()
}

/// OSC 10/11/12 查询的答复（TS 的 `Record<"10" | "11" | "12", string>`）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscColorReplies {
    pub osc10: String,
    pub osc11: String,
    pub osc12: String,
}

impl OscColorReplies {
    /// 按查询号取答复（TS 的 `table[id]`）
    pub fn get(&self, id: &str) -> Option<&str> {
        match id {
            "10" => Some(&self.osc10),
            "11" => Some(&self.osc11),
            "12" => Some(&self.osc12),
            _ => None,
        }
    }
}

/// OSC 10/11/12 查询的答复（ST 结尾，与 xterm.js 一致）。
pub fn osc_color_replies(hint: &OscColorHint) -> OscColorReplies {
    let appearance = hint.appearance.unwrap_or(TermAppearance::Dark);
    let (fb_bg, fb_fg) = fallback(appearance);
    let fg = osc_color(hint.foreground.as_deref(), fb_fg);
    let bg = osc_color(hint.background.as_deref(), fb_bg);
    let st = "\x1b\\";
    OscColorReplies {
        osc10: format!("\x1b]10;{fg}{st}"),
        osc11: format!("\x1b]11;{bg}{st}"),
        osc12: format!("\x1b]12;{fg}{st}"),
    }
}

static QUERY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\](1[012]);\?(?:\x07|\x1b\\)").expect("QUERY_RE"));

const QUERY_PREFIXES: [&str; 6] =
    ["\x1b]10;?\x07", "\x1b]11;?\x07", "\x1b]12;?\x07", "\x1b]10;?\x1b\\", "\x1b]11;?\x1b\\", "\x1b]12;?\x1b\\"];

fn is_query_prefix(s: &[u8]) -> bool {
    !s.is_empty() && QUERY_PREFIXES.iter().any(|t| t.as_bytes().starts_with(s))
}

/// 把结尾可能是半截查询的那段扣下来，等下一块拼上再说。
///
/// TS 在 UTF-16 码元上取最后 8 个；这里在字节上取最后 8 个。两者等价：查询前缀全是 ASCII、
/// 最长 8 个，能命中的后缀在两种表示里占同样的末尾位置；命中的切点是 ESC，必在字符边界上。
fn hold_back(s: &str) -> (&str, &str) {
    const MAX: usize = 8;
    let bytes = s.as_bytes();
    let start = bytes.len().saturating_sub(MAX);
    for i in start..bytes.len() {
        if is_query_prefix(&bytes[i..]) {
            return (&s[..i], &s[i..]);
        }
    }
    (s, "")
}

/// [`OscColorGate::push`] 的结果
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GateOutput {
    /// 去掉查询后要给 Viewer（也进 RingBuffer）的输出
    pub visible: String,
    /// 要写回 PTY 的答复
    pub replies: Vec<String>,
}

/// 从 PTY 输出里抽出 OSC 10/11/12 查询，自己答、不转给 viewer。
///
/// 交给客户端答有两个坑（旧 React 版就是让 xterm.js 答的）：回放历史会再答一次（键入一串
/// ESC 垃圾），多个 Viewer 会每人答一次。Zellij 0.44 实测会把 pane 里的查询实时
/// 转发到外层并把答复带回，所以这里的答复就是内层程序看到的底色；
/// zellij client 自己 attach 时也会查一次。
///
/// appearance 还没到时原样放过。当初有 xterm.js 兜底；现在的客户端不答颜色查询（falcon-term
/// 的 `Listener`），靠的是客户端先发 appearance 再发 resize（`falcon_proto::ws` 顶部），
/// zellij attach 时的那次查询到这里时 appearance 已经在了。
///
/// 输入是已经做过 UTF-8 跨块拼接的字符串（TS 的 onData 给的也是字符串）；跨块的只有
/// 半截查询，扣在 `pending` 里。TS 构造时传一个取深浅的闭包、每块调一次；这里改成每块把
/// 当前深浅直接传进来——会话条目同时持有 gate 与深浅，闭包捕获它会自引用。
#[derive(Debug, Clone, Default)]
pub struct OscColorGate {
    pending: String,
}

impl OscColorGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, hint: &OscColorHint, data: &str) -> GateOutput {
        let mut combined = std::mem::take(&mut self.pending);
        combined.push_str(data);

        if hint.appearance.is_none() {
            return GateOutput { visible: combined, replies: Vec::new() };
        }

        // 快路径：查询极罕见（Zellij 持久会话里通常到不了外层），而这段跑在
        // PTY 输出的热路径上，不该让每个 chunk 都过一遍正则。所有查询前缀都以
        // \x1b] 开头，唯一不含 \x1b] 的"可能是前缀"的形态是结尾的孤立 ESC
        // （下个 chunk 可能以 ] 续上），这两种都没有时直接原样放行。
        if !combined.contains("\x1b]") && combined.as_bytes().last() != Some(&0x1b) {
            return GateOutput { visible: combined, replies: Vec::new() };
        }
        let (emit, hold) = hold_back(&combined);
        self.pending = hold.to_string();

        let mut replies = Vec::new();
        let table = osc_color_replies(hint);
        let visible = QUERY_RE
            .replace_all(emit, |caps: &Captures| {
                if let Some(reply) = table.get(&caps[1]) {
                    replies.push(reply.to_string());
                }
                ""
            })
            .into_owned();
        GateOutput { visible, replies }
    }
}

/// 照抄 TS 语义时要用到的几处 JavaScript 行为（falcon-core 的 `js` 是私有模块，这里留一份
/// 服务端用的子集）。本 crate 的 forward_spec / share_spec / login_env / service / app_icon 都从
/// 这里取；lib.rs 有了公共的 js 模块后应挪过去。
pub(crate) mod js {
    /// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套
    /// （含 U+FEFF、不含 U+0085，与 Rust 的 `char::is_whitespace` 正好相反）
    pub(crate) fn is_whitespace(c: char) -> bool {
        matches!(
            c,
            '\u{9}' | '\u{a}' | '\u{b}' | '\u{c}' | '\u{d}' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    }

    /// `String.prototype.trim`
    pub(crate) fn trim(s: &str) -> &str {
        s.trim_matches(is_whitespace)
    }

    /// 正则 `/\s/.test(s)`
    /// `s.replace(/\s+$/, "")`
    pub(crate) fn trim_end(s: &str) -> &str {
        s.trim_end_matches(is_whitespace)
    }

    pub(crate) fn has_whitespace(s: &str) -> bool {
        s.chars().any(is_whitespace)
    }

    /// `s.length`：UTF-16 码元数
    pub(crate) fn utf16_len(s: &str) -> usize {
        s.chars().map(char::len_utf16).sum()
    }

    /// `Number(string)`（ECMAScript StringToNumber）：首尾空白、空串为 0、十六进制 / 八进制 /
    /// 二进制前缀、`Infinity`；`str::parse::<f64>` 这些一样都不认，反而认 `inf` / `nan`。
    pub(crate) fn string_to_number(s: &str) -> f64 {
        let t = trim(s);
        if t.is_empty() {
            return 0.0;
        }
        let bytes = t.as_bytes();
        if bytes.len() > 2 && bytes[0] == b'0' {
            let radix = match bytes[1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                b'b' | b'B' => 2,
                _ => 0,
            };
            if radix != 0 {
                let mut v = 0.0f64;
                for c in t[2..].chars() {
                    match c.to_digit(radix) {
                        Some(d) => v = v * f64::from(radix) + f64::from(d),
                        None => return f64::NAN,
                    }
                }
                return v;
            }
        }
        let (neg, body) = match bytes[0] {
            b'-' => (true, &t[1..]),
            b'+' => (false, &t[1..]),
            _ => (false, t),
        };
        let v = if body == "Infinity" {
            f64::INFINITY
        } else if is_decimal_literal(body) {
            body.parse::<f64>().unwrap_or(f64::NAN)
        } else {
            return f64::NAN;
        };
        if neg { -v } else { v }
    }

    /// StrUnsignedDecimalLiteral：`digits [. digits] [e[+-]digits]`，小数点两边至少一边有数字
    fn is_decimal_literal(s: &str) -> bool {
        let b = s.as_bytes();
        let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
        let mut i = int_digits;
        let mut frac_digits = 0;
        if i < b.len() && b[i] == b'.' {
            i += 1;
            frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            i += frac_digits;
        }
        if int_digits == 0 && frac_digits == 0 {
            return false;
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            i += 1;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
            }
            let exp_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
            if exp_digits == 0 {
                return false;
            }
            i += exp_digits;
        }
        i == b.len()
    }

    /// `encodeURIComponent`：只放过 `A-Z a-z 0-9 - _ . ! ~ * ' ( )`，其余按 UTF-8 百分号编码（大写十六进制）
    pub(crate) fn encode_uri_component(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn trim_uses_the_js_whitespace_set() {
            assert_eq!(trim("\u{feff} a \u{3000}"), "a");
            // U+0085 不是 JS 的空白
            assert_eq!(trim("\u{85}a"), "\u{85}a");
        }

        #[test]
        fn string_to_number_follows_js() {
            assert_eq!(string_to_number(" 22 "), 22.0);
            assert_eq!(string_to_number("0x50"), 80.0);
            assert_eq!(string_to_number("1e3"), 1000.0);
            assert_eq!(string_to_number(""), 0.0);
            assert!(string_to_number("22px").is_nan());
            assert!(string_to_number("inf").is_nan());
            assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
        }

        #[test]
        fn encode_uri_component_keeps_the_unreserved_set() {
            assert_eq!(encode_uri_component("a b/中!"), "a%20b%2F%E4%B8%AD!");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    fn get_static<'a>(env: &'a [(&str, &str)], key: &str) -> Option<&'a str> {
        env.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
    }

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    fn hint(appearance: Option<TermAppearance>, background: Option<&str>) -> OscColorHint {
        OscColorHint { appearance, background: background.map(Into::into), foreground: None }
    }

    // describe("isTermAppearance")

    #[test]
    fn is_term_appearance_accepts_light_dark_only() {
        assert!(is_term_appearance(Some(&json!("light"))));
        assert!(is_term_appearance(Some(&json!("dark"))));
        assert!(!is_term_appearance(Some(&json!("auto"))));
        assert!(!is_term_appearance(None));
    }

    // describe("parseHexRgb / luminance")

    #[test]
    fn parse_hex_rgb_parses_short_and_long_forms() {
        assert_eq!(parse_hex_rgb("#fff"), Some(rgb(255, 255, 255)));
        assert_eq!(parse_hex_rgb("#0a0a0a"), Some(rgb(10, 10, 10)));
        assert_eq!(parse_hex_rgb("red"), None);
    }

    #[test]
    fn luminance_treats_white_as_light_and_near_black_as_dark() {
        assert!(hex_luminance("#ffffff") > 0.9);
        assert!(hex_luminance("#0a0a0a") < 0.05);
        assert_eq!(appearance_from_hex(Some("#ffffff")), TermAppearance::Light);
        assert_eq!(appearance_from_hex(Some("#0a0a0a")), TermAppearance::Dark);
        assert_eq!(appearance_from_hex(Some("#fdf6e3")), TermAppearance::Light);
        assert_eq!(appearance_from_hex(Some("#002b36")), TermAppearance::Dark);
    }

    // describe("termPtyEnv")

    #[test]
    fn term_pty_env_always_declares_truecolor_and_stamps_polarity_only_when_known() {
        let bare = term_pty_env(None);
        assert_eq!(get_static(&bare, "COLORTERM"), Some("truecolor"));
        assert_eq!(get_static(&bare, "TERM_PROGRAM"), Some("falcon"));
        assert_eq!(get_static(&bare, "COLORFGBG"), None);
        assert_eq!(get_static(&bare, "GROK_APPEARANCE"), None);

        let dark = term_pty_env(Some(TermAppearance::Dark));
        assert_eq!(get_static(&dark, "COLORFGBG"), Some("15;0"));
        assert_eq!(get_static(&dark, "GROK_APPEARANCE"), Some("dark"));
        assert_eq!(get_static(&dark, "LC_GROK_APPEARANCE"), Some("dark"));
        let light = term_pty_env(Some(TermAppearance::Light));
        assert_eq!(get_static(&light, "COLORFGBG"), Some("0;15"));
        assert_eq!(get_static(&light, "GROK_APPEARANCE"), Some("light"));
    }

    #[test]
    fn term_pty_env_drops_the_host_terminals_colorfgbg() {
        // so iTerm 不污染网页会话
        let env = apply_term_pty_env(
            [
                ("PATH", Some("/bin".to_string())),
                ("COLORFGBG", Some("0;15".to_string())),
                ("GROK_APPEARANCE", Some("light".to_string())),
                ("EMPTY", None),
            ],
            Some(TermAppearance::Dark),
        );
        assert_eq!(get(&env, "PATH"), Some("/bin"));
        assert_eq!(get(&env, "COLORFGBG"), Some("15;0"));
        assert_eq!(get(&env, "GROK_APPEARANCE"), Some("dark"));
        assert_eq!(get(&env, "EMPTY"), None);
    }

    #[test]
    fn term_pty_env_clears_inherited_polarity_when_the_client_did_not_send_one() {
        let env = apply_term_pty_env(
            [("COLORFGBG".to_string(), "0;15".to_string()), ("GROK_APPEARANCE".to_string(), "light".to_string())],
            None,
        );
        assert_eq!(get(&env, "COLORFGBG"), None);
        assert_eq!(get(&env, "GROK_APPEARANCE"), None);
        assert_eq!(get(&env, "COLORTERM"), Some("truecolor"));
    }

    // describe("OscColorGate")

    #[test]
    fn osc_color_gate_answers_osc_11_and_strips_the_query_from_visible_output() {
        let h = hint(Some(TermAppearance::Dark), Some("#0a0a0a"));
        let mut gate = OscColorGate::new();
        let out = gate.push(&h, "hello\x1b]11;?\x07world");
        assert_eq!(out.visible, "helloworld");
        assert_eq!(out.replies, vec![osc_color_replies(&h).osc11]);
        assert_eq!(hex_to_osc_rgb("#0a0a0a").as_deref(), Some("rgb:0a0a/0a0a/0a0a"));
    }

    #[test]
    fn osc_color_gate_holds_a_split_query_across_chunks() {
        let h = hint(Some(TermAppearance::Light), None);
        let mut gate = OscColorGate::new();
        let a = gate.push(&h, "pre\x1b]11;");
        assert_eq!(a.visible, "pre");
        assert!(a.replies.is_empty());
        let b = gate.push(&h, "?\x1b\\post");
        assert_eq!(b.visible, "post");
        assert_eq!(b.replies.len(), 1);
        assert_eq!(b.replies[0], "\x1b]11;rgb:ffff/ffff/ffff\x1b\\");
    }

    #[test]
    fn osc_color_gate_passes_through_when_appearance_is_unknown() {
        let mut gate = OscColorGate::new();
        let out = gate.push(&OscColorHint::default(), "\x1b]11;?\x07");
        assert_eq!(out.visible, "\x1b]11;?\x07");
        assert!(out.replies.is_empty());
    }

    // describe("sanitizeColorHint")

    #[test]
    fn sanitize_color_hint_drops_illegal_values_instead_of_throwing() {
        assert_eq!(sanitize_color_hint(&json!({ "appearance": "auto", "background": "red" })), OscColorHint::default());
        assert_eq!(
            sanitize_color_hint(&json!({ "appearance": "light", "background": "#fff", "foreground": "#171717" })),
            OscColorHint {
                appearance: Some(TermAppearance::Light),
                background: Some("#fff".into()),
                foreground: Some("#171717".into()),
            }
        );
    }

    // 以下不在 TS 测试里：与 TS 实跑（tsx）对出来的边角

    #[test]
    fn osc_color_gate_edge_cases_match_ts() {
        let dark = hint(Some(TermAppearance::Dark), None);
        let bg = "\x1b]11;rgb:0a0a/0a0a/0a0a\x1b\\".to_string();
        let fg10 = "\x1b]10;rgb:fafa/fafa/fafa\x1b\\".to_string();
        let fg12 = "\x1b]12;rgb:fafa/fafa/fafa\x1b\\".to_string();

        // 整条查询恰好落在块尾时也会被扣住（它也是自己的"前缀"），等下一块非查询输出才答
        let mut g = OscColorGate::new();
        assert_eq!(g.push(&dark, "abc\x1b]11;?\x07"), GateOutput { visible: "abc".into(), replies: vec![] });
        assert_eq!(g.push(&dark, ""), GateOutput { visible: "".into(), replies: vec![] });
        assert_eq!(g.push(&dark, "x"), GateOutput { visible: "x".into(), replies: vec![bg.clone()] });

        // 多字节字符紧挨着半截查询；孤立 ESC 结尾
        let mut g = OscColorGate::new();
        assert_eq!(g.push(&dark, "中\x1b]1"), GateOutput { visible: "中".into(), replies: vec![] });
        assert_eq!(g.push(&dark, "0;?\x1b\\尾"), GateOutput { visible: "尾".into(), replies: vec![fg10] });
        assert_eq!(g.push(&dark, "\x1b"), GateOutput { visible: "".into(), replies: vec![] });
        assert_eq!(
            g.push(&dark, "]12;?\x07\x1b]11;?\x1b\\z"),
            GateOutput { visible: "z".into(), replies: vec![fg12, bg] }
        );
    }

    #[test]
    fn hex_forms_and_fallbacks() {
        // #rrggbbaa 也收，忽略 alpha；首尾空白按 JS trim 去掉
        assert_eq!(parse_hex_rgb(" #11223344 "), Some(rgb(0x11, 0x22, 0x33)));
        assert_eq!(parse_hex_rgb("#ABC"), Some(rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex_rgb("#abcd"), None);
        assert_eq!(appearance_from_hex(None), TermAppearance::Dark);
        // 底字色缺失 / 非法时按深浅兜底
        let r = osc_color_replies(&OscColorHint {
            appearance: Some(TermAppearance::Light),
            background: Some("nope".into()),
            foreground: None,
        });
        assert_eq!(r.osc10, "\x1b]10;rgb:1717/1717/1717\x1b\\");
        assert_eq!(r.osc11, "\x1b]11;rgb:ffff/ffff/ffff\x1b\\");
        assert_eq!(r.get("13"), None);
    }

    #[test]
    fn apply_keeps_key_order_like_a_js_object() {
        // 已有的 TERM 就地改值，新键追加在后
        let env = apply_term_pty_env([("TERM", "dumb".to_string()), ("PATH", "/bin".to_string())], None);
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["TERM", "PATH", "COLORTERM", "TERM_PROGRAM"]);
        assert_eq!(get(&env, "TERM"), Some("xterm-256color"));
    }
}
