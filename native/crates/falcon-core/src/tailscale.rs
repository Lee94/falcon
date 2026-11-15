//! 从后端本机的 Tailscale 设备预填 SSH 主机：搜索、识别已添加设备、显示名避重。
//!
//! 设备列表由 `GET /api/tailscale/devices` 提供；这里不接触网络，也不把 Tailscale 所有者
//! 当成 SSH 用户名。SSH 用户名的初值来自响应里的 `suggestedUsername`，仍可在表单里修改。

use falcon_proto::{SshHost, TailscaleDevice};

/// 名称、两种地址、系统和所有者都参与搜索；MagicDNS 名末尾的点不影响匹配。
pub fn matches_query(device: &TailscaleDevice, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let mut blob = format!(
        "{} {} {}",
        device.name,
        device.host,
        device.os.as_deref().unwrap_or("")
    );
    for extra in [
        device.dns_name.as_deref(),
        device.ipv4.as_deref(),
        device.owner.as_deref(),
    ] {
        if let Some(extra) = extra {
            blob.push(' ');
            blob.push_str(extra);
        }
    }
    blob.to_lowercase().contains(&query)
}

/// 同一台设备可能先按 MagicDNS 名保存、后来又按 Tailscale IPv4 出现，两种地址都要对账。
pub fn already_added(device: &TailscaleDevice, hosts: &[SshHost]) -> bool {
    hosts.iter().any(|host| {
        same_addr(&host.host, &device.host)
            || device
                .dns_name
                .as_deref()
                .is_some_and(|name| same_addr(&host.host, name))
            || device
                .ipv4
                .as_deref()
                .is_some_and(|ip| same_addr(&host.host, ip))
    })
}

fn same_addr(saved: &str, candidate: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('.').to_ascii_lowercase();
    !saved.trim().is_empty() && norm(saved) == norm(candidate)
}

/// 显示名和已保存主机撞了就加序号，避免一保存就因重名被服务端拒绝。
pub fn fresh_name(base: &str, hosts: &[SshHost]) -> String {
    let base = base.trim();
    let base = if base.is_empty() { "tailscale" } else { base };
    let taken = |name: &str| hosts.iter().any(|host| host.name.eq_ignore_ascii_case(name));
    if !taken(base) {
        return base.to_string();
    }
    for i in 2..1000 {
        let candidate = format!("{base} ({i})");
        if !taken(&candidate) {
            return candidate;
        }
    }
    format!("{base} ({})", hosts.len() + 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::SshAuthMethod;

    fn device() -> TailscaleDevice {
        TailscaleDevice {
            id: "node-1".into(),
            name: "Work-Desktop".into(),
            host: "work-desktop.tail.ts.net".into(),
            dns_name: Some("work-desktop.tail.ts.net.".into()),
            ipv4: Some("100.64.0.8".into()),
            os: Some("windows".into()),
            online: true,
            owner: Some("fay@example.com".into()),
        }
    }

    fn host(name: &str, address: &str) -> SshHost {
        SshHost {
            id: name.into(),
            name: name.into(),
            host: address.into(),
            port: 22,
            username: "fay".into(),
            auth_method: SshAuthMethod::Agent,
            key_path: None,
            has_secret: false,
            project_count: 0,
            created_at: 0,
        }
    }

    #[test]
    fn search_covers_aliases_os_and_owner() {
        let device = device();
        for query in ["work", "WINDOWS", "100.64.0.8", "example.com", "  TAIL.TS.NET  "] {
            assert!(matches_query(&device, query), "query: {query}");
        }
        assert!(matches_query(&device, ""));
        assert!(!matches_query(&device, "linux"));
    }

    #[test]
    fn added_device_matches_any_address_and_ignores_dns_dot() {
        let device = device();
        for address in [
            "WORK-DESKTOP.TAIL.TS.NET.",
            "work-desktop.tail.ts.net.",
            "100.64.0.8",
        ] {
            assert!(already_added(&device, &[host("saved", address)]));
        }
        assert!(!already_added(&device, &[host("saved", "100.64.0.9")]));
        assert!(!already_added(&device, &[host("saved", "")]));
    }

    #[test]
    fn fresh_name_avoids_case_insensitive_collisions() {
        let hosts = &[host("Work-Desktop", "a"), host("work-desktop (2)", "b")];
        assert_eq!(fresh_name("work-desktop", hosts), "work-desktop (3)");
        assert_eq!(fresh_name("other", hosts), "other");
        assert_eq!(fresh_name("  ", hosts), "tailscale");
    }
}
