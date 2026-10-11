//! 照抄 TS 语义时要用到的几处 JavaScript 行为。
//!
//! falcon-core 移植自 React 前端的 `lib/*.ts`，偏好 JSON、会话命名、路径排序这些行为要与
//! 当年的 TS 实现一致（已有的偏好数据与用户习惯都按它来）。Rust 标准库里名字相同的
//! 东西语义并不完全一样，差异都收在这里（与 falcon-theme 的 `js.rs` 同源，两边各留
//! 一份——那边是私有模块，这里也不该为几十行去依赖主题 crate）：
//!
//! - `trim()` / 正则 `\s` 的空白集合：JS 含 U+FEFF、不含 U+0085，Rust 反过来。
//! - `Math.round`：JS 是 .5 往 +∞ 舍（`Math.round(-2.5) === -2`），`f64::round` 是远离零。
//! - `Math.max` / `Math.min` 遇 NaN 给 NaN，`f64::max` 会把 NaN 吞掉。
//! - `Number(x)`：JSON 值转数字的那套规则（`null → 0`、`"" → 0`、`[5] → 5`、`{} → NaN`）。
//! - `toFixed`：恰好落在中点时取大的那个（JS 规范），`format!("{:.1}")` 是银行家舍入。
//! - `encodeURIComponent` / `decodeURIComponent`。
//! - `localeCompare`：见 [`locale_compare`] 的注释，只能近似。
//! - 字符串长度 / 下标按 UTF-16 码元算。

use std::cmp::Ordering;

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

/// `String.prototype.trim`
pub(crate) fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_whitespace)
}

/// 正则 `/\s/.test(s)`
pub(crate) fn has_js_whitespace(s: &str) -> bool {
    s.chars().any(is_js_whitespace)
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

/// `s.length`：UTF-16 码元数
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.slice(0, max)`：按 UTF-16 码元截断。
///
/// 截断点正好落在代理对中间时 JS 会留下半个代理，Rust 的 `str` 表示不了，这里整个
/// 字符不要。
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

/// `Math.round`：.5 往 +∞ 舍。`floor(x + 0.5)` 在 0.49999999999999994 上会错，这里按差值判。
pub(crate) fn js_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    if x - f >= 0.5 { f + 1.0 } else { f }
}

/// `Math.max(a, b)`：任一是 NaN 结果就是 NaN
pub(crate) fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) }
}

/// `Math.min(a, b)`：任一是 NaN 结果就是 NaN
pub(crate) fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) }
}

/// `Number(v)`：JSON 值（`None` = undefined）转数字。
///
/// 数组先 `join(",")` 成字符串再转（`[] → 0`、`[5] → 5`、`[1,2] → NaN`），对象是 NaN。
pub(crate) fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => js_string_to_number(s),
        Some(Value::Array(items)) => js_string_to_number(&array_to_string(items)),
        Some(Value::Object(_)) => f64::NAN,
    }
}

/// `Array.prototype.toString`（= `join(",")`，null / undefined 元素写成空串）
fn array_to_string(items: &[Value]) -> String {
    items
        .iter()
        .map(|v| match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.as_f64().map(js_number_to_string).unwrap_or_default(),
            Value::Array(inner) => array_to_string(inner),
            Value::Object(_) => "[object Object]".to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// `String(n)` 的常见情形：整数不带 `.0`。只用在 [`array_to_string`] 这种边角上。
fn js_number_to_string(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i128)
    } else {
        format!("{n}")
    }
}

/// `Number(string)`（ECMAScript StringToNumber）：首尾空白、空串为 0、十六进制 / 八进制 /
/// 二进制前缀、`Infinity`；`str::parse::<f64>` 这些一样都不认，反而认 `inf` / `nan`。
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

/// `x.toFixed(digits)`，x 有限且 |x| < 1e21（更大的 JS 改走指数表示，这里用不到）。
///
/// 规范要的是"离 x 的**精确值**最近的那个，恰好居中取大的"：先把 x 的精确十进制展开
/// 写出来（f64 的小数位最多一千出头），再按字符串做一次"逢五进一"。
pub(crate) fn js_to_fixed(x: f64, digits: usize) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "NaN".into()
        } else if x > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    let neg = x < 0.0;
    let exact = format!("{:.1100}", x.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((&exact, ""));
    let mut kept: Vec<u8> = int_part.bytes().chain(frac_part.bytes().take(digits)).collect();
    let round_up = frac_part.as_bytes().get(digits).is_some_and(|d| *d >= b'5');
    if round_up {
        let mut i = kept.len();
        loop {
            if i == 0 {
                kept.insert(0, b'1');
                break;
            }
            i -= 1;
            if kept[i] == b'9' {
                kept[i] = b'0';
            } else {
                kept[i] += 1;
                break;
            }
        }
    }
    let split = kept.len() - digits;
    let (i, f) = kept.split_at(split);
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    out.push_str(std::str::from_utf8(i).unwrap_or("0"));
    if digits > 0 {
        out.push('.');
        out.push_str(std::str::from_utf8(f).unwrap_or(""));
    }
    out
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

/// `decodeURIComponent`；JS 抛 URIError 的情形（孤零零的 `%`、不成 UTF-8 的字节序列）给 `None`
pub(crate) fn decode_uri_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let first = hex_byte(b, i)?;
        i += 3;
        if first < 0x80 {
            out.push(first);
            continue;
        }
        // UTF-8 首字节决定后面还要几个 %XX；0b10xxxxxx 开头或 5 字节以上都是 URIError
        let n = first.leading_ones() as usize;
        if n == 1 || n > 4 {
            return None;
        }
        let mut seq = vec![first];
        for _ in 1..n {
            if b.get(i) != Some(&b'%') {
                return None;
            }
            let cont = hex_byte(b, i)?;
            if cont & 0xC0 != 0x80 {
                return None;
            }
            seq.push(cont);
            i += 3;
        }
        // 过长编码、代理区、超过 U+10FFFF 同样是 URIError——std 的 UTF-8 校验正好挡这些
        out.extend_from_slice(std::str::from_utf8(&seq).ok()?.as_bytes());
    }
    String::from_utf8(out).ok()
}

fn hex_byte(b: &[u8], at: usize) -> Option<u8> {
    let hi = (*b.get(at + 1)? as char).to_digit(16)?;
    let lo = (*b.get(at + 2)? as char).to_digit(16)?;
    Some((hi * 16 + lo) as u8)
}

/// `a.localeCompare(b)` 的近似。
///
/// V8 的 localeCompare 走 ICU 排序规则，还随浏览器语言变（zh 下汉字按拼音）；Rust
/// 标准库没有排序规则数据，为了几处列表排序去拖一整套 ICU 数据不值当。这里照 CLDR
/// 根排序做三件最常碰到的事：
///
/// 1. 一级：空白 < 标点符号（按 CLDR 的次序）< 数字 < 拉丁字母（不分大小写）< 其它字符（按码位）；
/// 2. 三级：一级全同时小写排在大写前面（`a.ts` < `A.ts`）；
/// 3. 还分不出来按码位。
///
/// 已知对不上的：汉字（React 版在中文环境下是拼音序，这里是码位序）、带重音的拉丁字母
/// （ICU 把 é 当 e 的二级差异，这里当"其它字符"）。纯 ASCII 的路径与 React 版排出来的一样。
pub(crate) fn locale_compare(a: &str, b: &str) -> Ordering {
    let primary = |s: &str| s.chars().map(primary_weight).collect::<Vec<_>>();
    primary(a)
        .cmp(&primary(b))
        .then_with(|| {
            // 一级全同时逐字比大小写：小写在前
            for (x, y) in a.chars().zip(b.chars()) {
                if x != y {
                    let (xu, yu) = (x.is_uppercase(), y.is_uppercase());
                    if xu != yu {
                        return if xu { Ordering::Greater } else { Ordering::Less };
                    }
                }
            }
            Ordering::Equal
        })
        .then_with(|| a.cmp(b))
}

/// CLDR 根排序里 ASCII 标点符号的次序
const PUNCT_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn primary_weight(c: char) -> (u8, u32) {
    if is_js_whitespace(c) {
        return (0, c as u32);
    }
    if let Some(i) = PUNCT_ORDER.find(c) {
        return (1, i as u32);
    }
    if c.is_ascii_digit() {
        return (2, c as u32);
    }
    if c.is_ascii_alphabetic() {
        return (3, c.to_ascii_lowercase() as u32);
    }
    (4, c as u32)
}

/// f64 按 JS `JSON.stringify` 的样子写：整数不带 `.0`。
///
/// 不是为了能解析（`320` 与 `320.0` 解出来一样），是为了与 React 版落盘的 JSON 逐字
/// 可比——当初排查时直接 diff 原生的偏好文件与浏览器里存的那份。
pub(crate) mod js_num {
    use serde::{Deserialize, Deserializer, Serializer};

    const MAX_SAFE: f64 = 9_007_199_254_740_992.0;

    pub fn serialize<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
        if v.fract() == 0.0 && v.abs() < MAX_SAFE {
            s.serialize_i64(*v as i64)
        } else {
            s.serialize_f64(*v)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        f64::deserialize(d)
    }

    /// `Option<f64>`：`None` 写成 `null`（TS 的 `number | null`）
    pub mod opt {
        use serde::{Deserialize, Deserializer, Serializer};

        pub fn serialize<S: Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
            match v {
                Some(n) => super::serialize(n, s),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
            Option::<f64>::deserialize(d)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_matches_js() {
        assert_eq!(js_trim("\u{feff} a \u{3000}"), "a");
        assert_eq!(js_trim("\u{85}a"), "\u{85}a");
    }

    #[test]
    fn round_is_half_up() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(300.6), 301.0);
        assert_eq!(js_round(0.49999999999999994), 0.0);
    }

    #[test]
    fn number_of_json_values() {
        let n = |s: &str| js_number(Some(&serde_json::from_str::<Value>(s).unwrap()));
        assert_eq!(n("16"), 16.0);
        assert_eq!(n("\"16\""), 16.0);
        assert_eq!(n("null"), 0.0);
        assert_eq!(n("true"), 1.0);
        assert_eq!(n("[]"), 0.0);
        assert_eq!(n("[5]"), 5.0);
        assert!(n("[1,2]").is_nan());
        assert!(n("{}").is_nan());
        assert!(n("\"abc\"").is_nan());
        assert!(js_number(None).is_nan());
        assert_eq!(js_string_to_number("0x10"), 16.0);
        assert!(js_string_to_number("inf").is_nan());
    }

    #[test]
    fn to_fixed_rounds_ties_up() {
        assert_eq!(js_to_fixed(10.42, 1), "10.4");
        // 0.25 是精确的中点：JS 取大的，银行家舍入会给 0.2
        assert_eq!(js_to_fixed(0.25, 1), "0.3");
        assert_eq!(js_to_fixed(10.25, 1), "10.3");
        // 1.005 的精确值比中点小一丁点
        assert_eq!(js_to_fixed(1.005, 2), "1.00");
        assert_eq!(js_to_fixed(9.96, 1), "10.0");
        assert_eq!(js_to_fixed(0.0, 2), "0.00");
    }

    #[test]
    fn uri_component_round_trip() {
        assert_eq!(encode_uri_component("a b/c#1?.png"), "a%20b%2Fc%231%3F.png");
        assert_eq!(encode_uri_component("图"), "%E5%9B%BE");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
        assert_eq!(decode_uri_component("%E5%9B%BE").as_deref(), Some("图"));
        assert_eq!(decode_uri_component("%2E%2E").as_deref(), Some(".."));
        assert_eq!(decode_uri_component("100%"), None);
        assert_eq!(decode_uri_component("%E5%9B"), None);
        assert_eq!(decode_uri_component("%C0%AF"), None);
        assert_eq!(decode_uri_component("%zz"), None);
    }

    #[test]
    fn locale_compare_basics() {
        assert_eq!(locale_compare("a.ts", "z.ts"), Ordering::Less);
        assert_eq!(locale_compare("B.ts", "a.ts"), Ordering::Greater);
        assert_eq!(locale_compare("a.ts", "A.ts"), Ordering::Less);
        assert_eq!(locale_compare("a_b", "a1"), Ordering::Less);
        assert_eq!(locale_compare("a", "ab"), Ordering::Less);
        assert_eq!(locale_compare("same", "same"), Ordering::Equal);
    }

    #[test]
    fn js_num_writes_integers_without_fraction() {
        #[derive(serde::Serialize)]
        struct S {
            #[serde(with = "js_num")]
            a: f64,
            #[serde(with = "js_num::opt")]
            b: Option<f64>,
            #[serde(with = "js_num::opt")]
            c: Option<f64>,
        }
        let s = serde_json::to_string(&S { a: 320.0, b: Some(1.05), c: None }).unwrap();
        assert_eq!(s, r#"{"a":320,"b":1.05,"c":null}"#);
    }
}
