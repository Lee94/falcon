//! 从 falcon 后端本机的 Tailscale 列出可添加的远端主机。`GET /api/tailscale/devices`。
//!
//! SSH 是后端拨出去的，所以看的是后端这台机器所在的 tailnet，不是开着界面的那台。
//! 没安装、没在跑、没登录都是 200：`available: false` 加 `reason`。这是环境事实，不是请求错误。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

wire_enum! {
    /// 读不到设备列表的原因。客户端按这个换文案。
    pub enum TailscaleUnavailable {
        /// 后端机器上没有 tailscale 可执行文件
        NotInstalled => "not_installed",
        /// 有 CLI，但本机的 tailscaled 没在跑
        NotRunning => "not_running",
        /// 守护进程在，还没有登录 tailnet
        NeedsLogin => "needs_login",
        /// CLI 跑了，但输出不是一份可用的状态
        Failed => "failed",
    }
}

/// tailnet 里的一台设备，足够开一张预填好的主机表单。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleDevice {
    /// 稳定节点 id。同一台设备每次查询都一样
    pub id: String,
    /// 建议写入主机的显示名（Tailscale 的 HostName）
    pub name: String,
    /// 建议写入 SSH 主机字段的地址。MagicDNS 开着时是去掉末尾点的 DNS 名，否则是 Tailscale IPv4
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<String>,
    /// Tailscale 报的 OS 字符串（linux / macOS / windows / iOS …），原样
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    pub online: bool,
    /// 这台设备在 Tailscale 上的所有者。是登录名，不是 SSH 用户名
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// `GET /api/tailscale/devices`
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleDevices {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<TailscaleUnavailable>,
    /// `reason = failed` 时给人看的补充（CLI 的报错，截过）。其余原因客户端用自己的文案
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// 后端本机的登录名。远端 SSH 用户经常与此相同，表单里拿它当用户名初值
    pub suggested_username: String,
    pub devices: Vec<TailscaleDevice>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_roundtrip_camel_case() {
        let v = TailscaleDevices {
            available: true,
            reason: None,
            message: None,
            suggested_username: "ada".into(),
            devices: vec![TailscaleDevice {
                id: "na".into(),
                name: "work".into(),
                host: "work.tail.ts.net".into(),
                dns_name: Some("work.tail.ts.net".into()),
                ipv4: Some("100.2.2.2".into()),
                os: Some("linux".into()),
                online: true,
                owner: Some("Ada".into()),
            }],
        };
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("suggestedUsername"));
        assert!(!s.contains("reason"));
        assert!(!s.contains("dns_name"));
        let back: TailscaleDevices = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }
}
