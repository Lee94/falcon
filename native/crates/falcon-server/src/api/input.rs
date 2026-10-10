//! 请求体的 JS 式取值。Node 版的路由直接拿 `req.body as X` 用，靠可选链与真值判断兜住
//! 缺省；这里照同一口径从 `serde_json::Value` 里取，不先反序列化成强类型（那会把
//! "字段类型不对" 变成整条请求 400，而 Node 版是按字段各自处理的）。
//!
//! 与 Node 版的出入：字段是非字符串的真值时，`x?.trim()` 在 Node 里抛 TypeError（Fastify
//! 回 500）；这里当作缺省处理（走后续的 400 校验）。客户端只会发字符串，碰不到。

use serde_json::Value;

use crate::term_env::js;

/// `obj?.[key]`，不是对象时为 None
pub fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key),
        _ => None,
    }
}

/// `typeof v[key] === "string" ? v[key] : undefined`
pub fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    field(v, key).and_then(Value::as_str)
}

/// `v[key]?.trim()`（JS 的空白定义）
pub fn trimmed<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    str_field(v, key).map(js::trim)
}

/// `v[key]?.trim() || null`
pub fn trimmed_non_empty(v: &Value, key: &str) -> Option<String> {
    trimmed(v, key).filter(|s| !s.is_empty()).map(str::to_string)
}

/// JS 的真值判断
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}

/// `v != null`（undefined 与 null 都算没有）
pub fn present(v: Option<&Value>) -> bool {
    !matches!(v, None | Some(Value::Null))
}

/// `Number.isInteger(v)` 时的整数值
pub fn integer(v: Option<&Value>) -> Option<i64> {
    let f = v?.as_f64()?;
    (f.is_finite() && f.trunc() == f).then_some(f as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn js_semantics() {
        let v = json!({ "a": "  x ", "b": "", "n": 0, "f": 2.0, "g": 2.5, "s": "3", "z": null });
        assert_eq!(trimmed(&v, "a"), Some("x"));
        assert_eq!(trimmed_non_empty(&v, "b"), None);
        assert_eq!(trimmed(&v, "n"), None);
        assert!(!truthy(field(&v, "n")));
        assert!(!truthy(field(&v, "missing")));
        assert!(truthy(field(&v, "a")));
        assert_eq!(integer(field(&v, "f")), Some(2));
        assert_eq!(integer(field(&v, "g")), None);
        assert_eq!(integer(field(&v, "s")), None);
        assert!(!present(field(&v, "z")));
        assert!(present(field(&v, "n")));
        assert_eq!(str_field(&json!([1]), "a"), None);
    }
}
