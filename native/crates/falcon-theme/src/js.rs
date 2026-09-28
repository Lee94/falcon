//! 照抄 TS 语义时要用到的几处 JavaScript 行为。
//!
//! 主题层的真相来源是 `packages/web/src/lib/theme/*.ts`，两边读同一份偏好 JSON、
//! 同一段用户贴的 Ghostty 文本，结果必须一样。Rust 标准库里名字相同的东西语义
//! 并不完全一样，差异都收在这里：
//!
//! - `String.prototype.trim` / 正则 `\s` 的空白集合与 `str::trim` 不同：JS 含
//!   U+FEFF（BOM）、不含 U+0085（NEL），Rust 反过来。从 Windows 记事本复制出来的
//!   主题文本开头就可能带 BOM。
//! - `Number(string)` 认十六进制 / 八进制 / 二进制前缀、首尾空白、空串为 0，
//!   `str::parse::<f64>` 一样都不认，反而认 `inf` / `nan`。
//! - JSON 值的"真假"（`if (current)`）：`0`、`""`、`false`、`null` 是假。
//! - `slice(0, n)` 按 UTF-16 码元数截断。

use serde_json::Value;

/// ECMAScript 的 WhiteSpace + LineTerminator：`trim()` 与正则 `\s` 用的就是这一套。
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{9}'
            | '\u{a}'
            | '\u{b}'
            | '\u{c}'
            | '\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// 正则 `.` 不匹配的字符（非 dotAll 模式）
pub(crate) fn is_js_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `String.prototype.trim`
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// JSON 值在 `if (x)` 里的真假
pub(crate) fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `s.slice(0, max)`：按 UTF-16 码元截断。
///
/// 截断点正好落在代理对中间时 JS 会留下半个代理（孤立的高位代理），Rust 的 `str`
/// 表示不了，这里整个字符不要——名字截到 80 这种场景下差一个 emoji 无所谓。
pub(crate) fn js_slice_utf16(s: &str, max: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &s[..i];
        }
    }
    s
}

/// `Number(string)`（ECMAScript StringToNumber）。
///
/// 只用在偏好 JSON 里 `extended` 的键上（`Object.entries` 给的键都是字符串，TS 用
/// `Number(k)` 转下标），所以 `"16"`、`" 16 "`、`"0x10"`、`"16.0"`、`"1.6e1"` 在 TS
/// 那边都是 16，这里也得是。
pub(crate) fn js_string_to_number(s: &str) -> f64 {
    let t = js_trim(s);
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
        // 语法已经按 JS 核过，剩下交给标准库做正确舍入
        body.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        return f64::NAN;
    };
    if neg { -v } else { v }
}

/// StrUnsignedDecimalLiteral：`digits [. digits] [e[+-]digits]`，小数点两边至少一边有数字
fn is_decimal_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    i += int_digits;
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

/// `Object.entries(v)` 对对象 / 数组给出的键值对，顺序照 JS：
/// 规范的数组下标键（`"0"`、`"16"`，不含前导零）按数值升序排在前面，其余键随后。
///
/// 其余键在 JS 里按插入顺序；serde_json 的 Map 没开 preserve_order 时按字典序，开了就是
/// 插入顺序。这个 crate 自己不开（那个 feature 会波及整个 workspace），但 app 的构建里
/// GPUI 那一侧会把它打开（feature 合并），所以单独编与在 app 里编两种顺序都会遇到。只有
/// "同一个下标写了两种拼法"（`"16"` 与 `"0x10"`）时顺序才影响结果，手改偏好文件才会出现。
pub(crate) fn js_object_entries(v: &Value) -> Vec<(String, &Value)> {
    match v {
        Value::Array(items) => items.iter().enumerate().map(|(i, x)| (i.to_string(), x)).collect(),
        Value::Object(map) => {
            let mut index_keys: Vec<(u32, &String, &Value)> = Vec::new();
            let mut other_keys: Vec<(String, &Value)> = Vec::new();
            for (k, x) in map {
                match array_index(k) {
                    Some(n) => index_keys.push((n, k, x)),
                    None => other_keys.push((k.clone(), x)),
                }
            }
            index_keys.sort_by_key(|(n, _, _)| *n);
            let mut out: Vec<(String, &Value)> =
                index_keys.into_iter().map(|(_, k, x)| (k.clone(), x)).collect();
            out.extend(other_keys);
            out
        }
        _ => Vec::new(),
    }
}

/// 规范的数组下标：`"0"` 或不带前导零的十进制，且小于 2^32 − 1
fn array_index(k: &str) -> Option<u32> {
    if k.is_empty() || !k.bytes().all(|c| c.is_ascii_digit()) || (k.len() > 1 && k.starts_with('0')) {
        return None;
    }
    k.parse::<u32>().ok().filter(|&n| n != u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_matches_js() {
        assert_eq!(js_trim("\u{feff} a \u{3000}"), "a");
        // NEL 不是 JS 空白
        assert_eq!(js_trim("\u{85}a"), "\u{85}a");
    }

    #[test]
    fn string_to_number_matches_js() {
        for (s, want) in [
            ("16", 16.0),
            (" 16 ", 16.0),
            ("016", 16.0),
            ("0x10", 16.0),
            ("0o20", 16.0),
            ("0b10000", 16.0),
            ("16.0", 16.0),
            ("1.6e1", 16.0),
            (".5", 0.5),
            ("5.", 5.0),
            ("", 0.0),
            ("  ", 0.0),
            ("-3", -3.0),
        ] {
            assert_eq!(js_string_to_number(s), want, "{s:?}");
        }
        for s in ["abc", "1e", "0x", "-0x10", "inf", "nan", "1_000", "."] {
            assert!(js_string_to_number(s).is_nan(), "{s:?}");
        }
        assert_eq!(js_string_to_number("-Infinity"), f64::NEG_INFINITY);
    }

    #[test]
    fn slice_utf16_counts_code_units() {
        assert_eq!(js_slice_utf16("abc", 2), "ab");
        assert_eq!(js_slice_utf16("ab", 5), "ab");
        // 😀 占两个码元，截在中间时整个不要
        assert_eq!(js_slice_utf16("a😀", 2), "a");
        assert_eq!(js_slice_utf16("a😀", 3), "a😀");
    }

    #[test]
    fn object_entries_order() {
        let v: Value = serde_json::from_str(r#"{"b":1,"20":2,"3":3,"03":4}"#).unwrap();
        let keys: Vec<String> = js_object_entries(&v).into_iter().map(|(k, _)| k).collect();
        // 下标键按数值升序打头；其余键的先后跟着 Map 的遍历顺序（字典序或插入顺序，看整张
        // 依赖图里有没有人开 preserve_order），这里只断言集合
        assert_eq!(&keys[..2], ["3", "20"]);
        let mut rest = keys[2..].to_vec();
        rest.sort();
        assert_eq!(rest, ["03", "b"]);
    }
}
