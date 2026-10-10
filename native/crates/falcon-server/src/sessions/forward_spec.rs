//! 端口转发规则的入参校验与归一化。移植自 `packages/server/src/sessions/forwardSpec.ts`。
//! 纯函数，零 I/O。
//!
//! 入参是**原样的 JSON**（`Option<&Value>`，None = undefined）而不是 falcon-proto 的
//! `PortForwardInput`：TS 收的是 `Partial<PortForwardInput>`，实际是请求体原文，端口写成
//! 数字串（`"5432"`）也收、名字不是字符串要回「名称无效」——先反序列化成强类型就把这些
//! 宽松语义丢了，错误文案也会变成 serde 的。

use std::fmt::Display;

use falcon_proto::ForwardKind;
use serde_json::Value;

use crate::term_env::js;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedForward {
    pub name: Option<String>,
    pub kind: ForwardKind,
    pub bind_host: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    pub enabled: bool,
}

pub const DEFAULT_HOST: &str = "127.0.0.1";
const NAME_MAX: usize = 80;

/// `Number.isInteger`
fn is_integer(x: f64) -> bool {
    x.is_finite() && x.trunc() == x
}

/// 端口：整数，或去掉首尾空白后非空、按 JS `Number()` 转出来是整数的字符串。
///
/// 返回 `f64`（JS 的 number）：`1e20`、`"0x50"` 这种 TS 也原样收下，交给 [`is_valid_port`] 拒。
pub fn parse_port(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Number(n)) => n.as_f64().filter(|f| is_integer(*f)),
        Some(Value::String(s)) if !js::trim(s).is_empty() => {
            let n = js::string_to_number(s);
            is_integer(n).then_some(n)
        }
        _ => None,
    }
}

pub fn is_valid_port(port: f64) -> bool {
    is_integer(port) && (1.0..=65535.0).contains(&port)
}

/// 监听 / 目标主机。空串回退到 127.0.0.1——端口转发默认只绑本机回环，
/// 避免一不小心把远端服务暴露到局域网。
pub fn parse_host(value: Option<&Value>) -> Option<String> {
    parse_host_or(value, DEFAULT_HOST)
}

/// [`parse_host`]，回退值自定（TS 的 `fallback` 参数）
pub fn parse_host_or(value: Option<&Value>, fallback: &str) -> Option<String> {
    let s = match value {
        None | Some(Value::Null) => return Some(fallback.to_string()),
        Some(Value::String(s)) => s,
        Some(_) => return None,
    };
    if s.is_empty() {
        return Some(fallback.to_string());
    }
    let host = js::trim(s);
    if host.is_empty() {
        return Some(fallback.to_string());
    }
    if js::utf16_len(host) > 253 || js::has_whitespace(host) {
        return None;
    }
    Some(host.to_string())
}

/// 取请求体的一个字段。TS 的 `typeof input === "object"` 对数组也成立，数组上读具名字段
/// 只会得到 undefined，所以这里数组与对象都放行、数组一律读成 None。
pub(crate) fn field<'a>(input: &'a Value, key: &str) -> Option<&'a Value> {
    match input {
        Value::Object(map) => map.get(key),
        _ => None,
    }
}

/// TS 的 `!input || typeof input !== "object"`
pub(crate) fn is_object_like(input: Option<&Value>) -> bool {
    matches!(input, Some(Value::Object(_) | Value::Array(_)))
}

/// 可选的名称：缺省 / null / 空串 → None；不是字符串 → 「名称无效」；去掉首尾空白后为空 → None；
/// 超过 [`NAME_MAX`] 个字符（UTF-16 码元，TS 的 `.length`）→ 报错。转发与发布共用这一段。
pub(crate) fn parse_name(value: Option<&Value>) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => {
            let name = js::trim(s);
            if name.is_empty() {
                Ok(None)
            } else if js::utf16_len(name) > NAME_MAX {
                Err(format!("名称最多 {NAME_MAX} 个字符"))
            } else {
                Ok(Some(name.to_string()))
            }
        }
        Some(_) => Err("名称无效".into()),
    }
}

/// TS 的 `input.enabled !== false`：只有字面量 false 才算关
pub(crate) fn enabled_flag(value: Option<&Value>) -> bool {
    !matches!(value, Some(Value::Bool(false)))
}

/// 已经过 [`is_valid_port`] 的端口落到 u16
fn port_u16(port: f64) -> u16 {
    port as u16
}

pub fn validate_forward_input(input: Option<&Value>) -> Result<NormalizedForward, String> {
    let input = match input {
        Some(v) if is_object_like(Some(v)) => v,
        _ => return Err("缺少转发配置".into()),
    };

    let kind = match field(input, "kind") {
        Some(Value::String(k)) if k == "local" => ForwardKind::Local,
        Some(Value::String(k)) if k == "remote" => ForwardKind::Remote,
        _ => return Err("转发方向必须是 local 或 remote".into()),
    };

    let bind_port = match parse_port(field(input, "bindPort")) {
        Some(p) if is_valid_port(p) => port_u16(p),
        _ => return Err("监听端口无效".into()),
    };
    let dest_port = match parse_port(field(input, "destPort")) {
        Some(p) if is_valid_port(p) => port_u16(p),
        _ => return Err("目标端口无效".into()),
    };

    let Some(bind_host) = parse_host(field(input, "bindHost")) else {
        return Err("监听地址无效".into());
    };
    let Some(dest_host) = parse_host(field(input, "destHost")) else {
        return Err("目标地址无效".into());
    };

    let name = parse_name(field(input, "name"))?;

    Ok(NormalizedForward {
        name,
        kind,
        bind_host,
        bind_port,
        dest_host,
        dest_port,
        enabled: enabled_flag(field(input, "enabled")),
    })
}

/// `host:port`；IPv6（含冒号）加方括号，端口才看得清
pub fn format_forward_endpoint(host: &str, port: impl Display) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn port(v: Value) -> Option<f64> {
        parse_port(Some(&v))
    }

    fn host(v: Value) -> Option<String> {
        parse_host(Some(&v))
    }

    fn validate(v: Value) -> Result<NormalizedForward, String> {
        validate_forward_input(Some(&v))
    }

    // describe("parsePort")

    #[test]
    fn parse_port_accepts_integers_and_numeric_strings() {
        assert_eq!(port(json!(3000)), Some(3000.0));
        assert_eq!(port(json!("5432")), Some(5432.0));
    }

    #[test]
    fn parse_port_rejects_junk_so_we_do_not_listen_on_nan() {
        assert_eq!(parse_port(None), None);
        assert_eq!(port(json!("")), None);
        assert_eq!(port(json!("22.5")), None);
        assert_eq!(port(json!(22.5)), None);
    }

    // describe("isValidPort")

    #[test]
    fn is_valid_port_only_allows_1_to_65535() {
        assert!(is_valid_port(1.0));
        assert!(is_valid_port(65535.0));
        assert!(!is_valid_port(0.0));
        assert!(!is_valid_port(65536.0));
    }

    // describe("parseHost")

    #[test]
    fn parse_host_defaults_empty_to_loopback() {
        // do not bind 0.0.0.0 by accident
        assert_eq!(parse_host(None).as_deref(), Some("127.0.0.1"));
        assert_eq!(host(json!("")).as_deref(), Some("127.0.0.1"));
        assert_eq!(host(json!("   ")).as_deref(), Some("127.0.0.1"));
    }

    #[test]
    fn parse_host_rejects_whitespace_inside_a_host() {
        assert_eq!(host(json!("127.0.0.1 extra")), None);
    }

    #[test]
    fn parse_host_keeps_explicit_addresses() {
        assert_eq!(host(json!("0.0.0.0")).as_deref(), Some("0.0.0.0"));
        assert_eq!(host(json!("db.internal")).as_deref(), Some("db.internal"));
        assert_eq!(host(json!("::1")).as_deref(), Some("::1"));
    }

    // describe("validateForwardInput")

    #[test]
    fn validate_forward_input_fills_defaults_for_a_local_tunnel() {
        let got = validate(json!({ "kind": "local", "bindPort": 3000, "destPort": 3000 }));
        assert_eq!(
            got,
            Ok(NormalizedForward {
                name: None,
                kind: ForwardKind::Local,
                bind_host: "127.0.0.1".into(),
                bind_port: 3000,
                dest_host: "127.0.0.1".into(),
                dest_port: 3000,
                enabled: true,
            })
        );
    }

    #[test]
    fn validate_forward_input_trims_an_optional_name_and_rejects_an_empty_kind() {
        let named = validate(json!({
            "kind": "remote",
            "name": "  vite  ",
            "bindPort": 5173,
            "destPort": 5173,
            "enabled": false,
        }))
        .expect("ok");
        assert_eq!(named.name.as_deref(), Some("vite"));
        assert!(!named.enabled);
        assert!(validate(json!({ "bindPort": 1, "destPort": 1 })).is_err());
        assert!(validate(json!({ "kind": "local", "bindPort": 0, "destPort": 80 })).is_err());
    }

    // describe("formatForwardEndpoint")

    #[test]
    fn format_forward_endpoint_brackets_ipv6_so_the_port_is_readable() {
        assert_eq!(format_forward_endpoint("::1", 80), "[::1]:80");
        assert_eq!(format_forward_endpoint("127.0.0.1", 80), "127.0.0.1:80");
    }

    // 以下不在 TS 测试里：钉住 JSON 宽松语义与错误文案（与 TS 实跑一致）

    #[test]
    fn loose_json_semantics_match_ts() {
        assert_eq!(validate_forward_input(None), Err("缺少转发配置".into()));
        assert_eq!(validate(json!(null)), Err("缺少转发配置".into()));
        assert_eq!(validate(json!("x")), Err("缺少转发配置".into()));
        // 数组过得了 typeof object，然后在 kind 上失败
        assert_eq!(validate(json!([])), Err("转发方向必须是 local 或 remote".into()));
        assert_eq!(
            validate(json!({ "kind": "local", "bindPort": "0x50", "destPort": " 22 " }))
                .map(|f| (f.bind_port, f.dest_port)),
            Ok((80, 22))
        );
        assert_eq!(
            validate(json!({ "kind": "local", "bindPort": 1, "destPort": 1, "name": 5 })),
            Err("名称无效".into())
        );
        assert_eq!(
            validate(json!({ "kind": "local", "bindPort": 1, "destPort": 1, "name": "x".repeat(81) })),
            Err("名称最多 80 个字符".into())
        );
        assert_eq!(
            validate(json!({ "kind": "local", "bindPort": 1, "destPort": 1, "bindHost": 3 })),
            Err("监听地址无效".into())
        );
        // 只有字面量 false 才算关
        assert_eq!(
            validate(json!({ "kind": "local", "bindPort": 1, "destPort": 1, "enabled": 0 })).map(|f| f.enabled),
            Ok(true)
        );
        // 名字长度按 UTF-16 码元：40 个 emoji = 80 码元，不超
        assert!(validate(json!({ "kind": "local", "bindPort": 1, "destPort": 1, "name": "😀".repeat(40) })).is_ok());
        assert!(validate(json!({ "kind": "local", "bindPort": 1, "destPort": 1, "name": "😀".repeat(41) })).is_err());
    }
}
