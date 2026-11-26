//! 宿主机节点网关：资源只在所属节点管理，入口服务实时读取列表并转发请求。
//!
//! SSH 凭据留在入口服务；节点 ID 来自宿主数据库的 instance_id，与连接地址无关。
//! 内部请求带 SSH 读取的节点令牌，只列节点本地资源，避免两台节点互连时递归聚合。

pub mod bootstrap;
mod gateway;
mod legacy_update;
pub mod migration;
pub mod transport;
mod update;

pub use gateway::{Gateway, middleware, router};

use base64::Engine as _;
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;
pub const TOKEN_HEADER: &str = "x-falcon-node-token";
pub const LOCAL_HEADER: &str = "x-falcon-node-local";

/// 编码后不含斜杠，原有以项目/会话 ID 构造 URL 的客户端可以继续使用。
pub fn resource_id(node_id: &str, id: &str) -> String {
    format!(
        "node:{node_id}:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(id)
    )
}

pub fn split_resource(id: &str) -> Option<(&str, String)> {
    let rest = id.strip_prefix("node:")?;
    let (node, encoded) = rest.split_once(':')?;
    if node.is_empty()
        || node.len() > 128
        || !node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(encoded)
        .ok()?;
    let id = String::from_utf8(bytes).ok()?;
    (!id.is_empty()).then_some((node, id))
}

/// 只改协议中明确代表资源的字段，不触碰文件内容、终端标题或插件文字。
pub fn map_resource_fields(value: &mut Value, map: &impl Fn(&str) -> String) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(|v| map_resource_fields(v, map)),
        Value::Object(object) => {
            let resource = object.contains_key("projectId")
                || object.get("type").and_then(Value::as_str) == Some("askpass")
                || (object.contains_key("workingDir") || object.contains_key("type"))
                    && object.contains_key("createdAt");
            for (key, value) in object.iter_mut() {
                if matches!(key.as_str(), "projectId" | "sourceProjectId" | "threadEnvironmentId" | "sessionId")
                    || key == "id" && resource
                {
                    if let Some(id) = value.as_str() {
                        *value = Value::String(map(id));
                    }
                } else {
                    map_resource_fields(value, map);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resource_identity_survives_transport_and_keeps_node_boundary() {
        for id in ["session-1", "standalone:local", "项目/成员"] {
            let encoded = resource_id("node-a", id);
            assert!(!encoded.contains('/'));
            assert_eq!(split_resource(&encoded), Some(("node-a", id.to_string())));
            assert_ne!(encoded, resource_id("node-b", id));
        }
        assert!(split_resource("node:../bad:eA").is_none());
        assert!(split_resource("node:n:***").is_none());
    }

    #[test]
    fn resource_mapping_does_not_rewrite_user_content_or_unrelated_ids() {
        let mut v =
            json!({"id":"s", "projectId":"p", "name":"p", "content":"p", "tool":{"id":"call"}});
        map_resource_fields(&mut v, &|id| resource_id("n", id));
        assert_eq!(v["id"], resource_id("n", "s"));
        assert_eq!(v["projectId"], resource_id("n", "p"));
        assert_eq!(v["content"], "p");
        assert_eq!(v["tool"]["id"], "call");
    }
}
