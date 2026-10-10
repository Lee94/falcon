//! 与 JS 的 `JSON.parse` / `JSON.stringify` 同一口径的 JSON 值。
//!
//! fixture 原来由 Node 脚本落盘（`JSON.stringify(value, null, 2)`），换成 Rust 之后输出要
//! 逐字节不变：否则重生一次 fixture，git diff 里满屏格式差异，真正要看的字段增减反而被
//! 淹掉。serde_json 有几处对不上，所以这里自己读、自己写：
//!
//! - **键序**：不开 `preserve_order` 时 serde_json 的 Map 是 BTreeMap，按字母排。开它又会
//!   波及 xtask 里所有用 serde_json 的模块（feature 按包合并，管不到单个模块）。
//!   compare-fixtures 还要比键序，必须保持收到的顺序。
//! - **数字**：JS 只有 f64。`1.0` 读进来再写出去是 `1`；整数超过 2^53 会丢精度；
//!   `1e21` 起与 `1e-7` 起才写成指数，指数带符号（`1e+21`）。serde_json 保留 `.0`、
//!   u64 不丢精度、ryu 的指数写法也不同。
//! - **重复键**：`JSON.parse` 取最后一个值，但键留在第一次出现的位置。
//!
//! 字符串转义照 `JSON.stringify`（规范里的 QuoteJSONString）：`"` `\` 与 `\b\f\n\r\t`
//! 用短转义，其余 U+0000–U+001F 写 `\u00xx`（小写十六进制），别的一律原样——非 ASCII、
//! DEL、U+2028/2029 都不转义。

use std::fmt::Write as _;

/// JSON 值。`Undefined` 对应 `api()` 里响应不是 JSON 时的 `json = undefined`：
/// 只会出现在顶层，`save` 遇到它报错（与脚本里 `save` 的检查一致）。
#[derive(Clone, Debug, PartialEq)]
pub enum Js {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Js>),
    /// 键按收到的顺序；不会有重复键（解析时已按 JSON.parse 的规则合并）
    Obj(Vec<(String, Js)>),
}

impl Js {
    /// `JSON.parse`
    pub fn parse(src: &str) -> Result<Js, String> {
        let mut p = Parser { s: src.as_bytes(), i: 0 };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        if p.i != p.s.len() {
            return Err(p.err("JSON 后面还有多余的内容"));
        }
        Ok(v)
    }

    /// 字面量对象，键按给出的顺序
    pub fn obj<const N: usize>(pairs: [(&str, Js); N]) -> Js {
        Js::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    pub fn str(s: &str) -> Js {
        Js::Str(s.to_string())
    }

    /// `value[key]`；不是对象或没有这个键都是 None（JS 里是 undefined）
    pub fn get(&self, key: &str) -> Option<&Js> {
        match self {
            Js::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `value[i]`
    pub fn at(&self, i: usize) -> Option<&Js> {
        match self {
            Js::Arr(items) => items.get(i),
            _ => None,
        }
    }

    /// `obj[key] = value`：已有的键原地替换（JS 对象赋值不改键序），没有就追加
    pub fn set(&mut self, key: &str, value: Js) {
        if let Js::Obj(pairs) = self {
            match pairs.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = value,
                None => pairs.push((key.to_string(), value)),
            }
        }
    }

    /// 模板字符串里的 `${value[key]}`：拼 URL 用的 id。只认字符串与数字，别的报错
    /// （JS 会拼出 "undefined" 之类，再撞一个莫名其妙的 404，不如这里直接说清楚）
    pub fn text(&self, key: &str) -> Result<String, String> {
        match self.get(key) {
            Some(Js::Str(s)) => Ok(s.clone()),
            Some(Js::Num(n)) => Ok(number(*n)),
            Some(other) => Err(format!("字段 {key} 不是字符串：{}", other.compact())),
            None => Err(format!("没有字段 {key}：{}", self.compact())),
        }
    }

    /// JS 的真值：缺字段（None）由调用方当 false
    pub fn truthy(&self) -> bool {
        match self {
            Js::Undefined | Js::Null => false,
            Js::Bool(b) => *b,
            Js::Num(n) => *n != 0.0 && !n.is_nan(),
            Js::Str(s) => !s.is_empty(),
            Js::Arr(_) | Js::Obj(_) => true,
        }
    }

    /// compare-fixtures 用的类型名：`typeof`，但 null 与数组单列
    pub fn kind(&self) -> &'static str {
        match self {
            Js::Undefined => "undefined",
            Js::Null => "null",
            Js::Bool(_) => "boolean",
            Js::Num(_) => "number",
            Js::Str(_) => "string",
            Js::Arr(_) => "array",
            Js::Obj(_) => "object",
        }
    }

    /// `JSON.stringify(value, null, 2)`
    pub fn pretty(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, Some(""));
        out
    }

    /// `JSON.stringify(value)`（报错信息里用）
    pub fn compact(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, None);
        out
    }

    /// `indent` 为 None 是紧凑写法；Some 是当前这一层的缩进（每层两个空格）
    fn write(&self, out: &mut String, indent: Option<&str>) {
        match self {
            // 只在报错信息里出现（save 先拦掉了）；JSON.stringify(undefined) 本来也写不出东西
            Js::Undefined => out.push_str("undefined"),
            Js::Null => out.push_str("null"),
            Js::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Js::Num(n) => out.push_str(&number(*n)),
            Js::Str(s) => quote(s, out),
            Js::Arr(items) if items.is_empty() => out.push_str("[]"),
            Js::Obj(pairs) if pairs.is_empty() => out.push_str("{}"),
            Js::Arr(items) => {
                out.push('[');
                let inner = indent.map(|i| format!("{i}  "));
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    if let Some(inner) = &inner {
                        out.push('\n');
                        out.push_str(inner);
                    }
                    item.write(out, inner.as_deref());
                }
                if let Some(indent) = indent {
                    out.push('\n');
                    out.push_str(indent);
                }
                out.push(']');
            }
            Js::Obj(pairs) => {
                out.push('{');
                let inner = indent.map(|i| format!("{i}  "));
                for (n, (k, v)) in pairs.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    if let Some(inner) = &inner {
                        out.push('\n');
                        out.push_str(inner);
                    }
                    quote(k, out);
                    out.push(':');
                    if inner.is_some() {
                        out.push(' ');
                    }
                    v.write(out, inner.as_deref());
                }
                if let Some(indent) = indent {
                    out.push('\n');
                    out.push_str(indent);
                }
                out.push('}');
            }
        }
    }
}

/// `Number.prototype.toString()`（规范 Number::toString，基数 10）。
///
/// 有效数字取"能读回同一个 f64 的最短十进制串"——Rust 的 `{:e}` 给的正是这个（与 JS
/// 同一个要求：最短，并列时取离真值最近的）。再按 JS 的规则决定写不写指数：
/// 设数字串为 s（k 位）、十进制指数 n（值 = 0.s × 10^n）：
/// k ≤ n ≤ 21 补零写整数；0 < n ≤ 21 中间点小数点；−6 < n ≤ 0 写 0.000ddd；其余写指数。
pub fn number(x: f64) -> String {
    if !x.is_finite() {
        // JSON.stringify 把 NaN / ±Infinity 写成 null（`1e999` 读进来就是 Infinity）
        return "null".into();
    }
    if x == 0.0 {
        // -0 也写 "0"
        return "0".into();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("{:e} 必有指数");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exp: i32 = exp.parse().expect("{:e} 的指数是整数");
    let k = digits.len() as i32;
    let n = exp + 1;
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n('0', (-n) as usize));
        out.push_str(&digits);
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{sign}{}", e.abs());
    }
    out
}

/// QuoteJSONString
fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// 严格按 JSON 语法（JSON.parse 不认的这里也不认）。嵌套层数设个上限防栈溢出，fixture
/// 远到不了。
struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

const MAX_DEPTH: usize = 512;

impl Parser<'_> {
    fn err(&self, what: &str) -> String {
        format!("JSON 解析失败（第 {} 字节）：{what}", self.i)
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    /// JSON 的空白只有这四个
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> Result<(), String> {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(())
        } else {
            Err(self.err(&format!("期望 {lit}")))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Js, String> {
        if depth > MAX_DEPTH {
            return Err(self.err("嵌套太深"));
        }
        match self.peek() {
            Some(b'n') => self.eat("null").map(|_| Js::Null),
            Some(b't') => self.eat("true").map(|_| Js::Bool(true)),
            Some(b'f') => self.eat("false").map(|_| Js::Bool(false)),
            Some(b'"') => self.string().map(Js::Str),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Js::Arr(items));
                }
                loop {
                    self.ws();
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Js::Arr(items));
                        }
                        _ => return Err(self.err("数组里期望 , 或 ]")),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut obj = Js::Obj(Vec::new());
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(obj);
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("对象的键必须是字符串"));
                    }
                    let key = self.string()?;
                    self.ws();
                    self.eat(":")?;
                    self.ws();
                    let value = self.value(depth + 1)?;
                    // 重复键：值取后一个、位置留在前一个，正是 set 的语义
                    obj.set(&key, value);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(obj);
                        }
                        _ => return Err(self.err("对象里期望 , 或 }")),
                    }
                }
            }
            Some(_) => Err(self.err("不认识的字符")),
            None => Err(self.err("内容提前结束")),
        }
    }

    /// `-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?`，再交给 `f64::from_str`（正确舍入，与 JS 一致）
    fn number(&mut self) -> Result<Js, String> {
        let start = self.i;
        let digits = |p: &mut Self| {
            let from = p.i;
            while matches!(p.peek(), Some(b'0'..=b'9')) {
                p.i += 1;
            }
            p.i > from
        };
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err(self.err("数字格式不对")),
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            if !digits(self) {
                return Err(self.err("小数点后面没有数字"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !digits(self) {
                return Err(self.err("指数没有数字"));
            }
        }
        let text = std::str::from_utf8(&self.s[start..self.i]).expect("ASCII");
        text.parse::<f64>().map(Js::Num).map_err(|e| self.err(&format!("数字 {text}：{e}")))
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // 开头的引号
        let mut out = Vec::new();
        loop {
            let Some(b) = self.peek() else {
                return Err(self.err("字符串没有结束"));
            };
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return Err(self.err("字符串没有结束"));
                    };
                    self.i += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) && self.s[self.i..].starts_with(b"\\u") {
                                // 代理对：后面紧跟低位才合成；否则高位单独落下
                                let save = self.i;
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).expect("合法码点")
                                } else {
                                    self.i = save;
                                    '\u{FFFD}'
                                }
                            } else {
                                // 落单的代理项：JS 的字符串装得下，Rust 的 String 装不下，换成 U+FFFD。
                                // falcon 服务端从 Rust String 序列化，产不出这种东西
                                char::from_u32(hi).unwrap_or('\u{FFFD}')
                            }
                        }
                        _ => return Err(self.err("不认识的转义")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                0x00..=0x1F => return Err(self.err("字符串里有未转义的控制字符")),
                _ => out.push(b),
            }
        }
        // 输入是 &str，按字节原样拷过来的部分仍是合法 UTF-8
        Ok(String::from_utf8(out).expect("UTF-8"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let hex = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("\\u 后面不足四位"))?;
        let hex = std::str::from_utf8(hex).map_err(|_| self.err("\\u 后面不是十六进制"))?;
        let v = u32::from_str_radix(hex, 16).map_err(|_| self.err("\\u 后面不是十六进制"))?;
        self.i += 4;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(src: &str) -> String {
        Js::parse(src).unwrap().compact()
    }

    #[test]
    fn numbers_print_like_js() {
        // 期望值都是在 node 里 JSON.stringify(JSON.parse(x)) 的结果
        let cases = [
            ("64", "64"),
            ("64.0", "64"),
            ("-3", "-3"),
            ("-0", "0"),
            ("-0.0", "0"),
            ("1.5", "1.5"),
            ("0.1", "0.1"),
            ("0.30000000000000004", "0.30000000000000004"),
            ("1e20", "100000000000000000000"),
            ("1e21", "1e+21"),
            ("1.5e21", "1.5e+21"),
            ("123456789012345680000", "123456789012345680000"),
            ("0.000001", "0.000001"),
            ("1e-7", "1e-7"),
            ("1.2345e-7", "1.2345e-7"),
            ("-1.25E+2", "-125"),
            ("9007199254740993", "9007199254740992"),
            ("1791234567890", "1791234567890"),
            ("1e999", "null"),
        ];
        for (src, want) in cases {
            assert_eq!(roundtrip(src), want, "{src}");
        }
    }

    #[test]
    fn strings_escape_like_js() {
        let src = r#""a\"b\\c\/d\b\f\n\r\t\u0001\u001f\u007f é 中   😀 \ud83d""#;
        assert_eq!(roundtrip(src), "\"a\\\"b\\\\c/d\\b\\f\\n\\r\\t\\u0001\\u001f\u{7f} é 中 \u{2028} 😀 \u{FFFD}\"");
    }

    #[test]
    fn keeps_key_order_and_merges_duplicates_like_json_parse() {
        assert_eq!(roundtrip(r#"{"b":1,"a":2,"b":3}"#), r#"{"b":3,"a":2}"#);
    }

    #[test]
    fn pretty_matches_json_stringify_with_two_spaces() {
        let v = Js::parse(r#"{"a":[],"b":{},"c":[1,{"d":null,"e":[true,false]}],"f":"x"}"#).unwrap();
        let want = "{\n  \"a\": [],\n  \"b\": {},\n  \"c\": [\n    1,\n    {\n      \"d\": null,\n      \"e\": [\n        true,\n        false\n      ]\n    }\n  ],\n  \"f\": \"x\"\n}";
        assert_eq!(v.pretty(), want);
        assert_eq!(Js::Arr(vec![]).pretty(), "[]");
        assert_eq!(Js::str("x").pretty(), "\"x\"");
    }

    #[test]
    fn rejects_what_json_parse_rejects() {
        for bad in ["", "01", "1.", ".5", "[1,]", "{\"a\":1,}", "'x'", "\"a\nb\"", "nul", "[1] 2", "{a:1}"] {
            assert!(Js::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn set_replaces_in_place() {
        let mut v = Js::parse(r#"{"installed":true,"user":{"name":"x"},"host":"h"}"#).unwrap();
        v.set("user", Js::obj([("key", Js::str("k"))]));
        assert_eq!(v.compact(), r#"{"installed":true,"user":{"key":"k"},"host":"h"}"#);
    }
}
