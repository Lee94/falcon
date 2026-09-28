//! 「服务器 → 文件夹 → 检出」的分组视图模型。对应 web 的 `lib/projectTree.ts`。
//!
//! 侧栏与切换面板共用：两边必须看到同一棵树，分组规则只能有一份。纯函数，零 I/O。
//! 这里的"服务器"是 SSH Host 分组（本机 / 已保存主机 / 没绑主机的存量 SSH），
//! 不是 falcon 服务端。

use std::collections::{HashMap, HashSet};

use falcon_proto::{Project, ProjectType, SshHost};

use crate::host_color::{HostBar, host_bar_from_ssh, ssh_bar, ssh_conn};

/// 源项目工作区当前 HEAD，侧栏在没有 worktree 时用分支名代表默认仓库
/// （web 的 `store.ts` 里的 `ProjectHead`）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectHead {
    pub branch: Option<String>,
    pub sha: Option<String>,
}

/// 第一层的种类
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    Local,
    /// 已保存的主机
    Host,
    /// 没有绑定主机的存量 SSH 项目，按 user@host:port 聚成一组
    Legacy,
}

impl ServerKind {
    /// web 里的字面量（`"local" | "host" | "legacy"`）
    pub fn as_str(self) -> &'static str {
        match self {
            ServerKind::Local => "local",
            ServerKind::Host => "host",
            ServerKind::Legacy => "legacy",
        }
    }
}

/// 第一层：本机、已保存主机、以及没有绑定主机的存量 SSH
#[derive(Debug, Clone, PartialEq)]
pub struct ServerGroup {
    /// `s:local` / `s:host:<id>` / `s:legacy:<conn>`，展开状态的键
    pub key: String,
    pub kind: ServerKind,
    pub name: String,
    pub conn: Option<String>,
    /// 身份色条；web 是 CSS 字符串，这里是结构化的（见 [`crate::host_color`]）
    pub bar: Option<HostBar>,
    pub host: Option<SshHost>,
    pub folders: Vec<FolderGroup>,
}

/// 第二层：一个源项目（文件夹）。第三层永远是当前检出 + 附属 worktree
#[derive(Debug, Clone, PartialEq)]
pub struct FolderGroup {
    pub project: Project,
    pub worktrees: Vec<Project>,
}

pub fn folder_key(project_id: &str) -> String {
    format!("p:{project_id}")
}

/// 检出行的标签。
///
/// 多仓库容器没有单一 HEAD，返回空串让调用方给「N 个仓库」这类多仓库专用标签。
/// `??` 语义照抄：探到的分支名是空串也原样用，不往下落。
pub fn checkout_label(project: &Project, head: Option<&ProjectHead>) -> String {
    if let Some(wt) = &project.worktree {
        return wt.branch.clone();
    }
    if project.multi.is_some() {
        return String::new();
    }
    head.and_then(|h| h.branch.clone().or_else(|| h.sha.clone())).unwrap_or_else(|| project.name.clone())
}

/// JS 真值：`p.hostId` 是空串时当没有
fn truthy(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// 存档的附属项目：`p.worktree?.archivedAt` 为真（0 在 JS 里是假）
fn archived(p: &Project) -> bool {
    p.worktree.as_ref().and_then(|w| w.archived_at).is_some_and(|t| t != 0)
}

/// 存量 SSH 项目的分组键：有连接配置按 user@host:port，没有就是 `"ssh"`
fn legacy_conn(p: &Project) -> String {
    p.ssh.as_ref().map(ssh_conn).unwrap_or_else(|| "ssh".to_string())
}

pub fn group_servers(projects: &[Project], hosts: &[SshHost], local_name: &str, show_archived: bool) -> Vec<ServerGroup> {
    // 存档的附属项目默认不占侧栏；开关一开就在原来的位置出现
    let shown: Vec<&Project> = projects.iter().filter(|p| show_archived || !archived(p)).collect();
    let sources: Vec<&Project> = shown.iter().copied().filter(|p| p.worktree.is_none()).collect();
    let source_ids: HashSet<&str> = sources.iter().map(|p| p.id.as_str()).collect();
    let mut kids_by_source: HashMap<&str, Vec<&Project>> = HashMap::new();
    let mut orphans: Vec<&Project> = Vec::new();
    for p in &shown {
        // sourceProjectId 为空串的附属项目在 TS 里既不是源、也不挂在谁下面，整个不出现
        let Some(src) = p.worktree.as_ref().map(|w| w.source_project_id.as_str()).filter(|s| !s.is_empty()) else {
            continue;
        };
        if source_ids.contains(src) {
            kids_by_source.entry(src).or_default().push(p);
        } else {
            orphans.push(p);
        }
    }

    let folders_of = |matches: &dyn Fn(&Project) -> bool| -> Vec<FolderGroup> {
        let mut out: Vec<FolderGroup> = sources
            .iter()
            .filter(|p| matches(p))
            .map(|p| FolderGroup {
                project: (*p).clone(),
                worktrees: kids_by_source.get(p.id.as_str()).map(|v| v.iter().map(|w| (*w).clone()).collect()).unwrap_or_default(),
            })
            .collect();
        out.extend(orphans.iter().filter(|p| matches(p)).map(|p| FolderGroup { project: (*p).clone(), worktrees: Vec::new() }));
        out
    };

    let mut servers = vec![ServerGroup {
        key: "s:local".into(),
        kind: ServerKind::Local,
        name: local_name.to_string(),
        conn: None,
        bar: None,
        host: None,
        folders: folders_of(&|p| p.project_type == ProjectType::Local),
    }];

    for host in hosts {
        servers.push(ServerGroup {
            key: format!("s:host:{}", host.id),
            kind: ServerKind::Host,
            name: host.name.clone(),
            conn: Some(ssh_conn(host)),
            bar: Some(host_bar_from_ssh(host)),
            host: Some(host.clone()),
            folders: folders_of(&|p| p.project_type == ProjectType::Ssh && p.host_id.as_deref() == Some(host.id.as_str())),
        });
    }

    let mut seen: HashSet<String> = HashSet::new();
    for p in &shown {
        if p.project_type != ProjectType::Ssh || truthy(&p.host_id).is_some() {
            continue;
        }
        let conn = legacy_conn(p);
        if !seen.insert(conn.clone()) {
            continue;
        }
        servers.push(ServerGroup {
            key: format!("s:legacy:{conn}"),
            kind: ServerKind::Legacy,
            name: p.ssh.as_ref().map(|s| s.host.clone()).unwrap_or_else(|| conn.clone()),
            conn: Some(conn.clone()),
            bar: ssh_bar(Some(p)),
            host: None,
            folders: folders_of(&|x| x.project_type == ProjectType::Ssh && truthy(&x.host_id).is_none() && legacy_conn(x) == conn),
        });
    }

    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(json: &str) -> Project {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn hosts_and_legacy_servers_follow_the_local_group() {
        let hosts: Vec<SshHost> = serde_json::from_str(
            r#"[{"id":"h1","name":"dev","host":"10.0.0.2","port":22,"username":"root","authMethod":"agent",
                 "hasSecret":false,"projectCount":1,"createdAt":0},
                {"id":"h2","name":"idle","host":"10.0.0.3","port":22,"username":"root","authMethod":"agent",
                 "hasSecret":false,"projectCount":0,"createdAt":0}]"#,
        )
        .unwrap();
        let ssh = r#""ssh":{"host":"old.box","port":2222,"username":"u","authMethod":"key","hasSecret":false}"#;
        let projects = vec![
            p(r#"{"id":"a","name":"a","type":"ssh","hostId":"h1","createdAt":0}"#),
            p(&format!(r#"{{"id":"b","name":"b","type":"ssh",{ssh},"createdAt":0}}"#)),
            p(&format!(r#"{{"id":"c","name":"c","type":"ssh","hostId":"",{ssh},"createdAt":0}}"#)),
            p(r#"{"id":"d","name":"d","type":"ssh","createdAt":0}"#),
            p(r#"{"id":"e","name":"e","type":"local","createdAt":0}"#),
        ];
        let servers = group_servers(&projects, &hosts, "本机", false);
        let keys: Vec<&str> = servers.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["s:local", "s:host:h1", "s:host:h2", "s:legacy:u@old.box:2222", "s:legacy:ssh"]);
        let ids = |s: &ServerGroup| s.folders.iter().map(|f| f.project.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&servers[0]), ["e"]);
        assert_eq!(ids(&servers[1]), ["a"]);
        assert!(servers[2].folders.is_empty());
        // hostId 为空串在 JS 里是假：与没有 hostId 的 b 归到同一组存量 SSH
        assert_eq!(ids(&servers[3]), ["b", "c"]);
        assert_eq!(servers[3].name, "old.box");
        assert_eq!(servers[4].name, "ssh");
        assert_eq!(servers[4].bar, Some(HostBar::Neutral));
        assert_eq!(servers[1].conn.as_deref(), Some("root@10.0.0.2:22"));
        assert!(matches!(servers[1].bar, Some(HostBar::Hue(_))));
    }

    #[test]
    fn archived_worktrees_hide_until_asked_for() {
        let projects = vec![
            p(r#"{"id":"s","name":"s","type":"local","createdAt":0}"#),
            p(r#"{"id":"w","name":"w","type":"local","createdAt":0,
                  "worktree":{"sourceProjectId":"s","branch":"x","repoDir":"/r","createdByFalcon":true,"archivedAt":5}}"#),
        ];
        assert!(group_servers(&projects, &[], "本机", false)[0].folders[0].worktrees.is_empty());
        assert_eq!(group_servers(&projects, &[], "本机", true)[0].folders[0].worktrees.len(), 1);
        assert_eq!(folder_key("s"), "p:s");
    }
}
