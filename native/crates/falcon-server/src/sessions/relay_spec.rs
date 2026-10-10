//! 中转（端口转发 + 公网发布）的纯函数层：同端口互斥与旧规则迁移。零 I/O。
//! 移植自 `packages/server/src/sessions/relaySpec.ts`（ADR 0016）。
//!
//! 同端口可以存多条通道，同时只能一条生效。「同端口」按监听真正落在哪台机器上算：
//! - 本地转发（ssh -L）的监听在 falcon 后端本机，不管挂在哪台 SSH Host 上都抢
//!   同一张端口表——本机 5432 → A 与本机 5432 → B 就是这条规则要解决的「切换目标」；
//! - 远端转发（ssh -R）的监听在远端，只和同一台主机上的远端转发冲突；
//! - 公网发布不占固定端口（本机桥是 listen(0)），「同端口」指发布的是同一个目标
//!   端口：同一台机器上同一个服务发两条 Quick Tunnel 只会多一个没人用的 URL。
//!
//! 只比端口不比监听地址：界面上只能填端口，127.0.0.1 与 0.0.0.0 的同号端口在
//! 系统层面本来就互斥。
//!
//! 槽位键的字面形状与 falcon-core 的 `relay::forward_slot` / `share_slot`（客户端「同端口」
//! 徽标）逐字节一致，测试里有对照。没有直接调它们：那两个吃的是线上形状
//! （`PortForward` / `PublicShare`，带 state 等运行时字段），这里吃的是库里的行。

/// 公网发布挂在本机时 host_id 为 null，槽位里用这个占位
const LOCAL: &str = "local";

/// `host_forwards` 表里算槽位要用到的列
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardSlotRow {
    pub id: String,
    pub host_id: String,
    /// `local` / `remote`（库里是 TEXT）
    pub kind: String,
    pub bind_port: i64,
    /// 0 / 1（库里是 INTEGER）
    pub enabled: i64,
    pub created_at: i64,
}

/// `host_shares` 表里算槽位要用到的列
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSlotRow {
    pub id: String,
    /// None = falcon 后端本机
    pub host_id: Option<String>,
    pub dest_port: i64,
    pub enabled: i64,
    pub created_at: i64,
}

/// [`displaced_by`] / [`excess_enabled`] 对行的要求（TS 的 `T extends { id; enabled; created_at }`）
pub trait SlotRow {
    fn id(&self) -> &str;
    fn enabled(&self) -> i64;
    fn created_at(&self) -> i64;
}

impl SlotRow for ForwardSlotRow {
    fn id(&self) -> &str {
        &self.id
    }
    fn enabled(&self) -> i64 {
        self.enabled
    }
    fn created_at(&self) -> i64 {
        self.created_at
    }
}

impl SlotRow for ShareSlotRow {
    fn id(&self) -> &str {
        &self.id
    }
    fn enabled(&self) -> i64 {
        self.enabled
    }
    fn created_at(&self) -> i64 {
        self.created_at
    }
}

/// 转发的槽位键（TS 的 `forwardSlot` 收 `Pick<row, "host_id" | "kind" | "bind_port">`，
/// 这里把三列直接摊成参数，别的行类型也能用）。kind 不是 `local` 一律按远端算。
pub fn forward_slot_key(host_id: &str, kind: &str, bind_port: i64) -> String {
    if kind == "local" { format!("local\0{bind_port}") } else { format!("remote\0{host_id}\0{bind_port}") }
}

/// 发布的槽位键（TS 的 `shareSlot` 收 `Pick<row, "host_id" | "dest_port">`）
pub fn share_slot_key(host_id: Option<&str>, dest_port: i64) -> String {
    format!("{}\0{dest_port}", host_id.unwrap_or(LOCAL))
}

pub fn forward_slot(row: &ForwardSlotRow) -> String {
    forward_slot_key(&row.host_id, &row.kind, row.bind_port)
}

pub fn share_slot(row: &ShareSlotRow) -> String {
    share_slot_key(row.host_id.as_deref(), row.dest_port)
}

/// 让 target 生效时要停掉的规则：与它同槽位、此刻 enabled 的其它行。
/// target 自己不在返回值里，不论它现在是否 enabled。
pub fn displaced_by<T: SlotRow>(rows: &[T], target: &T, slot: impl Fn(&T) -> String) -> Vec<String> {
    let key = slot(target);
    rows.iter()
        .filter(|r| r.id() != target.id() && r.enabled() == 1 && slot(r) == key)
        .map(|r| r.id().to_string())
        .collect()
}

/// 一组规则里违反「同槽位只有一条 enabled」的那些，按创建先后留最早的一条。
/// 只给迁移用：旧的按项目挂的规则并到一台主机上以后，可能撞出同端口的一对。
pub fn excess_enabled<T: SlotRow>(rows: &[T], slot: impl Fn(&T) -> String) -> Vec<String> {
    let mut kept = std::collections::HashSet::new();
    let mut excess = Vec::new();
    let mut enabled: Vec<&T> = rows.iter().filter(|r| r.enabled() == 1).collect();
    // 稳定排序，与 JS 的 Array.prototype.sort 一致：同一时刻建的按原顺序
    enabled.sort_by_key(|r| r.created_at());
    for row in enabled {
        let key = slot(row);
        if kept.contains(&key) {
            excess.push(row.id().to_string());
        } else {
            kept.insert(key);
        }
    }
    excess
}

/// 旧的按项目挂的规则所在项目的连接信息（`projects` 表的几列）
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LegacyProjectConn {
    pub host_id: Option<String>,
    pub ssh_host: Option<String>,
    pub ssh_port: Option<i64>,
    pub ssh_username: Option<String>,
}

/// 已保存主机的连接三元组（`hosts` 表的几列）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConn {
    pub id: String,
    pub host: String,
    pub port: i64,
    pub username: String,
}

/// JS 的字符串真值：null / undefined / "" 为假
fn truthy(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

/// 旧的按项目挂的规则迁到哪台已保存主机上。
///
/// 先认项目上记的 host_id（主机还在才算）；存量项目没绑主机时按连接三元组
/// （host / port / username）找一台一模一样的——隧道走的就是这组凭据，换成别的
/// 用户登录，远端监听的权限与可达地址都可能不同。都找不到返回 None，由调用方丢弃
/// 并告警：没有落点的规则在新模型里无处可挂。
pub fn legacy_forward_host(project: &LegacyProjectConn, hosts: &[HostConn]) -> Option<String> {
    if let Some(id) = truthy(project.host_id.as_deref())
        && hosts.iter().any(|h| h.id == id)
    {
        return Some(id.to_string());
    }
    let ssh_host = truthy(project.ssh_host.as_deref())?;
    let ssh_username = truthy(project.ssh_username.as_deref())?;
    let port = project.ssh_port.unwrap_or(22);
    hosts.iter().find(|h| h.host == ssh_host && h.port == port && h.username == ssh_username).map(|h| h.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fwd(id: &str, host_id: &str, kind: &str, bind_port: i64, enabled: i64, created_at: i64) -> ForwardSlotRow {
        ForwardSlotRow { id: id.into(), host_id: host_id.into(), kind: kind.into(), bind_port, enabled, created_at }
    }

    /// TS 的 `fwd(id, host_id, kind, bind_port)`：enabled 缺省 1、created_at 缺省 0
    fn fwd1(id: &str, host_id: &str, kind: &str, bind_port: i64) -> ForwardSlotRow {
        fwd(id, host_id, kind, bind_port, 1, 0)
    }

    fn share(id: &str, host_id: Option<&str>, dest_port: i64, enabled: i64, created_at: i64) -> ShareSlotRow {
        ShareSlotRow { id: id.into(), host_id: host_id.map(Into::into), dest_port, enabled, created_at }
    }

    fn share1(id: &str, host_id: Option<&str>, dest_port: i64) -> ShareSlotRow {
        share(id, host_id, dest_port, 1, 0)
    }

    // describe("forwardSlot")

    #[test]
    fn forward_slot_local_forwards_share_one_port_table_across_hosts() {
        // they all listen on the backend
        assert_eq!(forward_slot(&fwd1("a", "h1", "local", 5432)), forward_slot(&fwd1("b", "h2", "local", 5432)));
    }

    #[test]
    fn forward_slot_remote_forwards_only_clash_on_the_same_host() {
        assert_ne!(forward_slot(&fwd1("a", "h1", "remote", 8080)), forward_slot(&fwd1("b", "h2", "remote", 8080)));
        assert_eq!(forward_slot(&fwd1("a", "h1", "remote", 8080)), forward_slot(&fwd1("b", "h1", "remote", 8080)));
    }

    #[test]
    fn forward_slot_local_and_remote_of_the_same_port_never_clash() {
        assert_ne!(forward_slot(&fwd1("a", "h1", "local", 80)), forward_slot(&fwd1("b", "h1", "remote", 80)));
    }

    // describe("shareSlot")

    #[test]
    fn share_slot_keys_by_machine() {
        // 本机 and a host with the same port are distinct
        assert_ne!(share_slot(&share1("a", None, 3000)), share_slot(&share1("b", Some("h1"), 3000)));
        assert_eq!(share_slot(&share1("a", Some("h1"), 3000)), share_slot(&share1("b", Some("h1"), 3000)));
    }

    // describe("displacedBy")

    #[test]
    fn displaced_by_returns_other_enabled_rows_on_the_same_slot_never_the_target() {
        let rows = [
            fwd("a", "h1", "local", 5432, 1, 0),
            fwd("b", "h2", "local", 5432, 0, 0),
            fwd("c", "h2", "local", 5433, 1, 0),
            fwd("d", "h3", "local", 5432, 1, 0),
        ];
        assert_eq!(displaced_by(&rows, &rows[1], forward_slot), vec!["a", "d"]);
        assert_eq!(displaced_by(&rows, &rows[0], forward_slot), vec!["d"]);
    }

    #[test]
    fn displaced_by_ignores_disabled_rows() {
        // they are not holding the port
        let rows = [fwd("a", "h1", "local", 1, 0, 0), fwd("b", "h1", "local", 1, 0, 0)];
        assert!(displaced_by(&rows, &rows[0], forward_slot).is_empty());
    }

    // describe("excessEnabled")

    #[test]
    fn excess_enabled_keeps_the_earliest_enabled_row_per_slot() {
        let rows = [
            share("late", Some("h1"), 3000, 1, 20),
            share("early", Some("h1"), 3000, 1, 10),
            share("off", Some("h1"), 3000, 0, 5),
            share("other", None, 3000, 1, 30),
        ];
        assert_eq!(excess_enabled(&rows, share_slot), vec!["late"]);
    }

    // describe("legacyForwardHost")

    fn hosts() -> Vec<HostConn> {
        vec![
            HostConn { id: "h1".into(), host: "10.0.0.1".into(), port: 22, username: "fay".into() },
            HostConn { id: "h2".into(), host: "10.0.0.1".into(), port: 22, username: "root".into() },
        ]
    }

    fn conn(host_id: Option<&str>, ssh_host: &str, ssh_port: Option<i64>, ssh_username: &str) -> LegacyProjectConn {
        LegacyProjectConn {
            host_id: host_id.map(Into::into),
            ssh_host: Some(ssh_host.into()),
            ssh_port,
            ssh_username: Some(ssh_username.into()),
        }
    }

    #[test]
    fn legacy_forward_host_prefers_the_projects_bound_host() {
        assert_eq!(legacy_forward_host(&conn(Some("h2"), "10.0.0.1", Some(22), "fay"), &hosts()), Some("h2".into()));
    }

    #[test]
    fn legacy_forward_host_falls_back_to_the_exact_host_port_user_triple_when_unbound_or_the_host_is_gone() {
        assert_eq!(legacy_forward_host(&conn(None, "10.0.0.1", None, "fay"), &hosts()), Some("h1".into()));
        assert_eq!(legacy_forward_host(&conn(Some("gone"), "10.0.0.1", Some(22), "root"), &hosts()), Some("h2".into()));
    }

    #[test]
    fn legacy_forward_host_does_not_guess_across_usernames_or_ports() {
        assert_eq!(legacy_forward_host(&conn(None, "10.0.0.1", Some(22), "bob"), &hosts()), None);
        assert_eq!(legacy_forward_host(&conn(None, "10.0.0.1", Some(2222), "fay"), &hosts()), None);
    }

    // 不在 TS 测试里：与 falcon-core（客户端徽标）的槽位口径逐字节对照

    #[test]
    fn slot_keys_match_falcon_core() {
        use falcon_proto::{ForwardKind, ForwardState, PortForward, PublicShare};

        let wire_fwd = |host: &str, kind: ForwardKind, port: u16| PortForward {
            id: "x".into(),
            host_id: host.into(),
            name: None,
            kind,
            bind_host: "127.0.0.1".into(),
            bind_port: port,
            dest_host: "127.0.0.1".into(),
            dest_port: port,
            enabled: true,
            state: ForwardState::Stopped,
            error: None,
            created_at: 0,
        };
        for (host, kind, wire_kind, port) in
            [("h1", "local", ForwardKind::Local, 5432u16), ("h2", "remote", ForwardKind::Remote, 8080)]
        {
            assert_eq!(
                forward_slot(&fwd1("a", host, kind, i64::from(port))),
                falcon_core::relay::forward_slot(&wire_fwd(host, wire_kind, port))
            );
        }

        let wire_share = |host: Option<&str>, port: u16| PublicShare {
            id: "x".into(),
            host_id: host.map(Into::into),
            name: None,
            dest_host: "127.0.0.1".into(),
            dest_port: port,
            enabled: true,
            state: ForwardState::Stopped,
            public_url: None,
            error: None,
            created_at: 0,
        };
        for host in [None, Some("h1")] {
            assert_eq!(share_slot(&share1("a", host, 3000)), falcon_core::relay::share_slot(&wire_share(host, 3000)));
        }
    }
}
