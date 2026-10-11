//! 中转的同端口槽位：与服务端的 `sessions/relay_spec.rs`（`forward_slot` / `share_slot`）同一口径，
//! 设置「中转」页用它给同端口的规则挂徽标（ADR 0016）。
//!
//! 同端口可以存多条通道，同时只能一条生效。「同端口」按监听真正落在哪台机器上算：
//! - 本地转发（ssh -L）的监听在 falcon 后端本机，不管挂在哪台 SSH Host 上都抢同一张端口表；
//! - 远端转发（ssh -R）的监听在远端，只和同一台主机上的远端转发冲突；
//! - 公网发布指的是发布同一台机器上的同一个目标端口。
//!
//! 只比端口不比监听地址：界面上只能填端口。

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
