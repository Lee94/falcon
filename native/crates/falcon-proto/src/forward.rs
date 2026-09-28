//! 中转（Relay）：SSH 端口转发（Port Forward）与公网发布（Public Share，ADR 0014 / 0016）。
//!
//! 中转按**机器**挂，不按项目：端口转发挂在一台已保存的 SSH Host 上（`hostId`），走该主机
//! 自己的那条 SshLink；公网发布挂在 falcon 后端本机（`hostId` 缺省）或一台 SSH Host 上。
//! 设置里的「中转」页用 `GET /api/relays` 一次拉全。
//!
//! 规则持久化、运行时状态是此刻的事实：`enabled` 表示该不该跑，`state` / `error` /
//! `publicUrl` 是现在跑成什么样。环境事实（连不上、端口占用）写在每条规则的 state/error 里，
//! 列表本身不因某条失败而 5xx。
//!
//! 同端口的规则可以建多条，同时只有一条 enabled：带 enabled 的写入会让服务端顺手停掉同端口
//! 的其它规则，所以写完要重拉整张列表，不能只替换这一行。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// local：在 falcon 后端监听，经 SSH 打到远端能到达的地址（ssh -L）。
    /// remote：在远端监听，打回后端能到达的地址（ssh -R）。
    pub enum ForwardKind {
        Local => "local",
        Remote => "remote",
    }
}

wire_enum! {
    /// 运行时状态。规则本身用 enabled 表示「该不该跑」，state 是此刻的事实。
    ///
    /// Rust 侧加了 `Unknown` 兜底：状态类，新服务端多一个状态（比如 reconnecting）
    /// 不该让整张转发 / 发布列表解不出来。
    pub enum ForwardState open {
        Stopped => "stopped",
        Starting => "starting",
        Active => "active",
        Error => "error",
    }
}

/// `GET /api/relays` 里 `forwards` 的元素，建 / 改规则的响应。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PortForward {
    pub id: String,
    /// 挂在哪台已保存的 SSH Host 上
    pub host_id: String,
    /// 可选备注，如 vite / postgres
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: ForwardKind,
    /// 监听地址。local = 后端本机，remote = 远端
    pub bind_host: String,
    pub bind_port: u16,
    pub dest_host: String,
    pub dest_port: u16,
    pub enabled: bool,
    pub state: ForwardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// unix 毫秒
    pub created_at: i64,
}

/// 建规则的请求体（`POST /api/forwards`）。改规则见 [`PortForwardPatch`]。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardInput {
    /// 只在创建时生效；规则建好后不能换主机
    pub host_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub kind: ForwardKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_host: Option<String>,
    pub bind_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    pub dest_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// 改规则的请求体（`PATCH /api/forwards/:id`），即 TS 的 `Partial<PortForwardInput>`：
/// 只带要改的字段（不含 hostId——规则建好后不能换主机）。开关按钮只发 `{ enabled }`。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortForwardPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ForwardKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// 把一台机器上的 HTTP 服务经 Cloudflare Quick Tunnel 发到公网。`GET /api/relays` 里
/// `shares` 的元素，建 / 改规则的响应。
///
/// 本机（`host_id` 缺省）：直接打后端本机端口。SSH Host：目标在远端，先经 SSH 接到后端，
/// 再由本机 cloudflared 发出去。cloudflared 永远只在 falcon 后端本机跑。
/// 界面文案照 web 写明"任何拿到链接的人都能访问"。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PublicShare {
    pub id: String,
    /// 缺省 = falcon 后端本机
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// 可选备注，如 vite / storybook
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub dest_host: String,
    pub dest_port: u16,
    pub enabled: bool,
    pub state: ForwardState,
    /// 当前这条 Quick Tunnel 的公网 URL。进程一重启就会变，不持久化。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// unix 毫秒
    pub created_at: i64,
}

/// 建规则的请求体（`POST /api/shares`）。改规则见 [`PublicSharePatch`]。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PublicShareInput {
    /// 只在创建时生效；缺省 = 本机
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    pub dest_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// 改规则的请求体（`PATCH /api/shares/:id`），即 TS 的 `Partial<PublicShareInput>`：
/// 只带要改的字段（不含 hostId）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PublicSharePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dest_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// `GET /api/relays`：本机与所有 SSH Host 的转发与发布，设置里「中转」页一次拉全
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RelayList {
    pub forwards: Vec<PortForward>,
    pub shares: Vec<PublicShare>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn relays_list() {
        let list = roundtrip::<RelayList>(
            r#"{"forwards":[{"id":"f1","hostId":"h1","name":"vite","kind":"local","bindHost":"127.0.0.1","bindPort":5173,
                 "destHost":"localhost","destPort":5173,"enabled":true,"state":"active","createdAt":1758700000000},
                {"id":"f2","hostId":"h1","kind":"remote","bindHost":"0.0.0.0","bindPort":8080,
                 "destHost":"127.0.0.1","destPort":4923,"enabled":true,"state":"error",
                 "error":"remote port forwarding failed for listen port 8080","createdAt":1758700000001}],
                "shares":[{"id":"s1","destHost":"127.0.0.1","destPort":3000,"enabled":false,"state":"stopped","createdAt":1}]}"#,
        );
        assert_eq!(list.forwards[1].state, ForwardState::Error);
        assert_eq!(list.forwards[0].bind_port, 5173);
        assert_eq!(list.shares[0].host_id, None);
    }

    #[test]
    fn forward_state_unknown() {
        let f = serde_json::from_str::<PortForward>(
            r#"{"id":"f1","hostId":"h1","kind":"local","bindHost":"127.0.0.1","bindPort":1,
                "destHost":"h","destPort":2,"enabled":true,"state":"reconnecting","createdAt":0}"#,
        )
        .unwrap();
        assert_eq!(f.state, ForwardState::Unknown);
    }

    #[test]
    fn forward_input() {
        roundtrip::<PortForwardInput>(r#"{"hostId":"h1","kind":"local","bindPort":5432,"destPort":5432}"#);
        roundtrip::<PortForwardInput>(
            r#"{"hostId":"h1","name":"pg","kind":"remote","bindHost":"0.0.0.0","bindPort":5432,"destHost":"db","destPort":5432,"enabled":false}"#,
        );
        let p = roundtrip::<PortForwardPatch>(r#"{"enabled":false}"#);
        assert_eq!(p, PortForwardPatch { enabled: Some(false), ..Default::default() });
        roundtrip::<PortForwardPatch>(r#"{"bindPort":5433,"destHost":"db2"}"#);
    }

    #[test]
    fn shares() {
        let s = roundtrip::<PublicShare>(
            r#"{"id":"s1","hostId":"h1","name":"storybook","destHost":"127.0.0.1",
                "destPort":6006,"enabled":true,"state":"active",
                "publicUrl":"https://quiet-river-1234.trycloudflare.com","createdAt":1758700000000}"#,
        );
        assert_eq!(s.host_id.as_deref(), Some("h1"));
        roundtrip::<PublicShare>(
            r#"{"id":"s2","destHost":"127.0.0.1","destPort":3000,
                "enabled":false,"state":"stopped","createdAt":1758700000001}"#,
        );
        roundtrip::<PublicShareInput>(r#"{"destPort":3000}"#);
        roundtrip::<PublicShareInput>(
            r#"{"hostId":"h1","name":"api","destHost":"10.0.0.3","destPort":8000,"enabled":true}"#,
        );
        roundtrip::<PublicSharePatch>(r#"{"enabled":true}"#);
    }
}
