//! 公网发布规则的入参校验与归一化。移植自 `packages/server/src/sessions/shareSpec.ts`（ADR 0014 / 0016）。
//! 纯函数，零 I/O。入参同 [`super::forward_spec`]：原样的 JSON，保留 TS 的宽松语义。

use serde_json::Value;

use super::forward_spec::{enabled_flag, field, is_object_like, is_valid_port, parse_host, parse_name, parse_port};

/// 挂在哪台机器上不在这里校验：那是创建时一次性的事，由 ShareManager 查主机表
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedShare {
    pub name: Option<String>,
    pub dest_host: String,
    pub dest_port: u16,
    pub enabled: bool,
}

pub fn validate_share_input(input: Option<&Value>) -> Result<NormalizedShare, String> {
    let input = match input {
        Some(v) if is_object_like(Some(v)) => v,
        _ => return Err("缺少发布配置".into()),
    };

    let dest_port = match parse_port(field(input, "destPort")) {
        Some(p) if is_valid_port(p) => p as u16,
        _ => return Err("目标端口无效".into()),
    };

    let Some(dest_host) = parse_host(field(input, "destHost")) else {
        return Err("目标地址无效".into());
    };

    // 名称规则（缺省 / 空串 / 非字符串 / 80 字上限）与转发同一段，见 forward_spec::parse_name
    let name = parse_name(field(input, "name"))?;

    Ok(NormalizedShare { name, dest_host, dest_port, enabled: enabled_flag(field(input, "enabled")) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn validate(v: Value) -> Result<NormalizedShare, String> {
        validate_share_input(Some(&v))
    }

    // describe("validateShareInput")

    #[test]
    fn validate_share_input_fills_loopback_and_enables_by_default() {
        assert_eq!(
            validate(json!({ "destPort": 5173 })),
            Ok(NormalizedShare { name: None, dest_host: "127.0.0.1".into(), dest_port: 5173, enabled: true })
        );
    }

    #[test]
    fn validate_share_input_trims_the_name_and_keeps_enabled_false() {
        let got = validate(json!({ "destPort": 3000, "name": "  vite  ", "enabled": false })).expect("ok");
        assert_eq!(got.name.as_deref(), Some("vite"));
        assert!(!got.enabled);
    }

    #[test]
    fn validate_share_input_rejects_junk_port() {
        assert!(validate(json!({})).is_err());
        assert!(validate(json!({ "destPort": 0 })).is_err());
    }

    // 不在 TS 测试里：错误文案与 TS 实跑一致

    #[test]
    fn error_messages_match_ts() {
        assert_eq!(validate_share_input(None), Err("缺少发布配置".into()));
        assert_eq!(validate(json!({})), Err("目标端口无效".into()));
        assert_eq!(validate(json!({ "destPort": 1, "destHost": "a b" })), Err("目标地址无效".into()));
        assert_eq!(validate(json!({ "destPort": 1, "name": [] })), Err("名称无效".into()));
    }
}
