//! 中转的同端口槽位：与服务端的 `sessions/relay_spec.rs`（`forward_slot` / `share_slot`）同一口径，
//! 设置「中转」页用它给同端口的规则挂徽标（ADR 0016）。
//!
//! 同端口可以存多条通道，同时只能一条生效。「同端口」按监听真正落在哪台机器上算：
//! - 本地转发（ssh -L）的监听在 falcon 后端本机，不管挂在哪台 SSH Host 上都抢同一张端口表；
//! - 远端转发（ssh -R）的监听在远端，只和同一台主机上的远端转发冲突；
//! - 公网发布指的是发布同一台机器上的同一个目标端口。
//!
//! 只比端口不比监听地址：界面上监听那一头只能填端口（目标那一头可以填地址，见
//! [`parse_forward_dest`]）。

use std::collections::HashMap;

use falcon_proto::{ForwardKind, PortForward, PublicShare, RelayList};

/// 公网发布挂在本机时 hostId 缺省，槽位里用这个占位
const LOCAL: &str = "local";

pub fn forward_slot(f: &PortForward) -> String {
    match f.kind {
        ForwardKind::Remote => format!("remote\0{}\0{}", f.host_id, f.bind_port),
        _ => format!("local\0{}", f.bind_port),
    }
}

pub fn share_slot(s: &PublicShare) -> String {
    format!("{}\0{}", s.host_id.as_deref().unwrap_or(LOCAL), s.dest_port)
}

/// 转发表单「目标地址」一栏（IP 或主机名）的输入，返回 `(地址, 顺带的端口)`：
/// - 空 → `(None, None)`：不带 destHost，服务端按默认的 127.0.0.1；
/// - IPv6 可以带方括号（`[fd00::5]`），去掉——服务端拿它直接连，带着括号连不上；
/// - 粘进来的 `host:port` / `[v6]:port` 拆开，端口交给目标端口那一栏。裸 IPv6 至少两个冒号，
///   只有一个冒号的才当成带端口；
/// - 中间有空白、超过 253 个字符、端口不是 1–65535 → `Err`（服务端 `parse_host` 同样会拒，
///   这里先拦住，提示就地给出）
pub fn parse_forward_dest(raw: &str) -> Result<(Option<String>, Option<u16>), ()> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok((None, None));
    }
    if s.chars().any(char::is_whitespace) || s.chars().count() > 253 {
        return Err(());
    }
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p >= 1).ok_or(());
    if let Some(rest) = s.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or(())?;
        if host.is_empty() {
            return Err(());
        }
        let port = match after {
            "" => None,
            p => Some(port(p.strip_prefix(':').ok_or(())?)?),
        };
        return Ok((Some(host.to_string()), port));
    }
    if s.matches(':').count() == 1 {
        let (host, p) = s.split_once(':').ok_or(())?;
        if host.is_empty() {
            return Err(());
        }
        return Ok((Some(host.to_string()), Some(port(p)?)));
    }
    Ok((Some(s.to_string()), None))
}

/// 列表里显示的一端：回环地址是默认值，只显示端口；别的带上地址，IPv6 加方括号
pub fn endpoint_label(host: &str, port: u16) -> String {
    if matches!(host, "127.0.0.1" | "localhost" | "::1") {
        port.to_string()
    } else if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 规则 id → 它所在槽位上共有几条规则（含自己，不论启用与否）。大于 1 的才需要徽标
pub fn same_port_counts(list: &RelayList) -> HashMap<String, usize> {
    let mut by_slot: HashMap<String, usize> = HashMap::new();
    for f in &list.forwards {
        *by_slot.entry(format!("f\0{}", forward_slot(f))).or_default() += 1;
    }
    for s in &list.shares {
        *by_slot.entry(format!("s\0{}", share_slot(s))).or_default() += 1;
    }
    let mut out = HashMap::new();
    for f in &list.forwards {
        out.insert(f.id.clone(), by_slot[&format!("f\0{}", forward_slot(f))]);
    }
    for s in &list.shares {
        out.insert(s.id.clone(), by_slot[&format!("s\0{}", share_slot(s))]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::ForwardState;

    fn fwd(id: &str, host: &str, kind: ForwardKind, port: u16) -> PortForward {
        PortForward {
            id: id.into(),
            host_id: host.into(),
            name: None,
            kind,
            bind_host: "127.0.0.1".into(),
            bind_port: port,
            dest_host: "127.0.0.1".into(),
            dest_port: port,
            enabled: false,
            state: ForwardState::Stopped,
            error: None,
            created_at: 0,
        }
    }

    fn share(id: &str, host: Option<&str>, port: u16) -> PublicShare {
        PublicShare {
            id: id.into(),
            host_id: host.map(Into::into),
            name: None,
            dest_host: "127.0.0.1".into(),
            dest_port: port,
            enabled: false,
            state: ForwardState::Stopped,
            public_url: None,
            error: None,
            created_at: 0,
        }
    }

    #[test]
    fn local_forwards_share_one_port_table_across_hosts() {
        assert_eq!(
            forward_slot(&fwd("a", "h1", ForwardKind::Local, 5432)),
            forward_slot(&fwd("b", "h2", ForwardKind::Local, 5432))
        );
    }

    #[test]
    fn remote_forwards_only_clash_on_the_same_host() {
        assert_ne!(
            forward_slot(&fwd("a", "h1", ForwardKind::Remote, 8080)),
            forward_slot(&fwd("b", "h2", ForwardKind::Remote, 8080))
        );
        assert_ne!(
            forward_slot(&fwd("a", "h1", ForwardKind::Local, 80)),
            forward_slot(&fwd("b", "h1", ForwardKind::Remote, 80))
        );
    }

    #[test]
    fn dest_host_input() {
        assert_eq!(parse_forward_dest(""), Ok((None, None)));
        assert_eq!(parse_forward_dest("  "), Ok((None, None)));
        assert_eq!(parse_forward_dest(" 10.0.0.5 "), Ok((Some("10.0.0.5".into()), None)));
        assert_eq!(parse_forward_dest("db.internal"), Ok((Some("db.internal".into()), None)));
        // 粘进来的 host:port 拆开
        assert_eq!(parse_forward_dest("10.0.0.5:5432"), Ok((Some("10.0.0.5".into()), Some(5432))));
        // IPv6：裸的不拆，方括号去掉，带端口的拆开
        assert_eq!(parse_forward_dest("fd00::5"), Ok((Some("fd00::5".into()), None)));
        assert_eq!(parse_forward_dest("[fd00::5]"), Ok((Some("fd00::5".into()), None)));
        assert_eq!(parse_forward_dest("[fd00::5]:8080"), Ok((Some("fd00::5".into()), Some(8080))));
        for bad in ["10.0.0.5 x", "10.0.0.5:0", "10.0.0.5:99999", ":80", "[]", "[fd00::5", "[fd00::5]x"] {
            assert_eq!(parse_forward_dest(bad), Err(()), "{bad}");
        }
        assert_eq!(parse_forward_dest(&"a".repeat(254)), Err(()));
    }

    #[test]
    fn endpoint_labels() {
        assert_eq!(endpoint_label("127.0.0.1", 5432), "5432");
        assert_eq!(endpoint_label("::1", 5432), "5432");
        assert_eq!(endpoint_label("10.0.0.5", 5432), "10.0.0.5:5432");
        assert_eq!(endpoint_label("fd00::5", 5432), "[fd00::5]:5432");
    }

    #[test]
    fn counts() {
        let list = RelayList {
            forwards: vec![
                fwd("a", "h1", ForwardKind::Local, 5432),
                fwd("b", "h2", ForwardKind::Local, 5432),
                fwd("c", "h1", ForwardKind::Remote, 5432),
            ],
            shares: vec![share("s1", None, 5432), share("s2", None, 5432), share("s3", Some("h1"), 5432)],
        };
        let n = same_port_counts(&list);
        assert_eq!((n["a"], n["b"], n["c"]), (2, 2, 1));
        // 转发与发布不互相计数；本机与主机的发布是两台机器
        assert_eq!((n["s1"], n["s2"], n["s3"]), (2, 2, 1));
    }
}
