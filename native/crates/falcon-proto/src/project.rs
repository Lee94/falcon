//! 项目与远端主机（SSH Host）。
//!
//! 术语照 CONTEXT.md：Project（项目）分 local / ssh；附属项目（worktree）与
//! 多仓库项目（multi）是与 local / ssh 正交的两个维度，靠可选字段的**存在与否**
//! 判别，见 [`crate::worktree`]。

use serde::{Deserialize, Serialize};

use crate::system::HostKind;
use crate::wire::wire_enum;
use crate::worktree::{MultiRepoInfo, WorktreeInfo};

wire_enum! {
    /// 闭集：附属 / 多仓库刻意不进这个枚举（见 [`WorktreeInfo`] 的注释），
    /// 服务端明说不会加第三个值。
    pub enum ProjectType {
        Local => "local",
        Ssh => "ssh",
    }
}

wire_enum! {
    pub enum SshAuthMethod {
        Key => "key",
        Password => "password",
        Agent => "agent",
    }
}

/// 项目上的 SSH 连接配置（响应方向）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    /// 后端所在机器上的私钥文件路径（authMethod = key 时）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// 密码 / passphrase 是否已保存（内容绝不出现在 API 响应中）
    pub has_secret: bool,
}

/// 预先保存的远端 SSH 主机，`GET /api/hosts` 的元素。
///
/// 项目创建时从这里选一台，连接配置**复制**到项目上——会话链路仍然读项目自己的
/// ssh 字段，不按 hostId 解引用。改主机时再把连接配置刷回引用它的项目。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshHost {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    pub has_secret: bool,
    /// 当前引用此主机的项目数（含附属项目）
    pub project_count: u32,
    /// unix 毫秒
    pub created_at: i64,
}

/// 创建 / 更新远端主机。secret 仅在写入方向出现。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshHostInput {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// SSH 连通性探测（`POST /api/hosts/:id/test`、`POST /api/hosts/test`）。
///
/// 形状与 RepoInfo 同源：环境事实写在 ok/error 里，不抛 4xx。测的是「现在这组
/// 凭据能不能登上」，不是 Zellij / git 好不好用。
///
/// TS 是按布尔字段 `ok` 判别的联合；serde 的内部标签只认字符串，所以经
/// [`SshProbeWire`] 这个平铺形状转一道。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(try_from = "SshProbeWire", into = "SshProbeWire")]
pub enum SshProbeResult {
    /// `{ ok: true, kind, home }`
    Reachable { kind: HostKind, home: String },
    /// `{ ok: false, error }`
    Failed { error: String },
}

/// [`SshProbeResult`] 在线上的平铺形状。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshProbeWire {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<HostKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TryFrom<SshProbeWire> for SshProbeResult {
    type Error = String;

    fn try_from(w: SshProbeWire) -> Result<Self, Self::Error> {
        if w.ok {
            match (w.kind, w.home) {
                (Some(kind), Some(home)) => Ok(SshProbeResult::Reachable { kind, home }),
                _ => Err("SshProbeResult: ok=true 却缺 kind / home".into()),
            }
        } else {
            // 服务端的 ok:false 恒带 error；真缺了也别让整个响应解不出来
            Ok(SshProbeResult::Failed { error: w.error.unwrap_or_default() })
        }
    }
}

impl From<SshProbeResult> for SshProbeWire {
    fn from(r: SshProbeResult) -> Self {
        match r {
            SshProbeResult::Reachable { kind, home } => SshProbeWire {
                ok: true,
                kind: Some(kind),
                home: Some(home),
                error: None,
            },
            SshProbeResult::Failed { error } => SshProbeWire {
                ok: false,
                kind: None,
                home: None,
                error: Some(error),
            },
        }
    }
}

/// `GET /api/projects` 的元素，以及建 / 改 / 派生 / 存档 / 恢复项目的响应。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub project_type: ProjectType,
    /// 终端初始 cwd：本地为后端机器路径，SSH 为远端路径（可选）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    /// 覆盖默认 shell，可选
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// type = ssh 时恒有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshConfig>,
    /// 创建时选中的已保存主机；存量项目或手写 ssh 字段的请求没有这项
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    /// 存在 ⇔ 这是附属项目（工作目录是某个 git 仓库的 worktree）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeInfo>,
    /// 存在 ⇔ 这是多仓库项目（容器或批量派生的产物），见 MultiRepoInfo
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi: Option<MultiRepoInfo>,
    /// 派生附属项目时，新建分支默认从这个引用切出（本地名或 `origin/main` 这种远程名）。
    /// 空 = 当前 HEAD。只对源项目有意义；附属项目不能再派生。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_worktree_branch: Option<String>,
    /// unix 毫秒
    pub created_at: i64,
}

/// 创建 / 更新项目的请求体。secret 为明文密码或 passphrase，仅在写入方向出现。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInput {
    pub name: String,
    #[serde(rename = "type")]
    pub project_type: ProjectType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    /// 已保存主机。有值时连接配置从该主机复制，忽略 ssh 字段。
    /// 新建 SSH 项目走这条；存量项目仍可用下面的 ssh 手写。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshConfigInput>,
    /// 多仓库容器的成员仓库路径。创建时带上 ⇒ 建的是容器；编辑容器时带上 ⇒ 替换成员清单。
    /// 判别式在创建时定死：普通项目带 repos、或附属项目改 repos，一律 400——
    /// 派生产物的成员清单是删除目标，写入路径必须不可达（与 worktree 四列同罪）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    /// 派生时新建分支的默认基点。空 / 省略 = 当前 HEAD
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_worktree_branch: Option<String>,
}

/// [`ProjectInput::ssh`]：手写的 SSH 连接配置（TS 里是内联的匿名类型）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigInput {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: SshAuthMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

/// 删除项目的结果（`DELETE /api/projects/:id`）。
///
/// warnings 是文件系统清理的**非致命**失败，含残留路径。删除一律返回 200：
/// DB 行无条件删掉，文件系统清理 best-effort——留一条删不掉的项目行，用户唯一的
/// 出路是去改 SQLite；残留目录他自己删得掉，前提是我们把路径原样告诉他。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProjectResult {
    /// TS 里是字面量 `true`
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warnings: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;
    use crate::worktree::MultiRepoMember;

    #[test]
    fn local_project_minimal() {
        let p = roundtrip::<Project>(
            r#"{"id":"6f1c","name":"mojito","type":"local","workingDir":"/Users/fay/Code/mojito","createdAt":1758700000000}"#,
        );
        assert_eq!(p.project_type, ProjectType::Local);
        assert!(p.worktree.is_none() && p.multi.is_none() && p.ssh.is_none());
    }

    #[test]
    fn ssh_project_with_host() {
        let p = roundtrip::<Project>(
            r#"{"id":"a2","name":"box","type":"ssh","workingDir":"/home/fay/app","shell":"/bin/bash",
                "ssh":{"host":"172.16.25.134","port":22,"username":"fay","authMethod":"key",
                       "keyPath":"/Users/fay/.ssh/id_ed25519","hasSecret":false},
                "hostId":"h1","defaultWorktreeBranch":"origin/main","createdAt":1758700000001}"#,
        );
        let ssh = p.ssh.unwrap();
        assert_eq!(ssh.auth_method, SshAuthMethod::Key);
        assert_eq!(ssh.port, 22);
    }

    #[test]
    fn worktree_and_multi_derived_project() {
        // 批量派生出的多仓库附属项目：worktree 与 multi 同时存在
        let p = roundtrip::<Project>(
            r#"{"id":"w1","name":"feat-x","type":"local","workingDir":"/Users/fay/Code/group-feat-x",
                "worktree":{"sourceProjectId":"c1","branch":"feat/x","repoDir":"/Users/fay/Code/group",
                            "createdByFalcon":true,"archivedAt":1758790000000},
                "multi":{"repos":[{"dir":"/Users/fay/Code/group-feat-x/api","repoDir":"/Users/fay/Code/api"},
                                  {"dir":"/Users/fay/Code/group-feat-x/web","repoDir":"/Users/fay/Code/web"}]},
                "createdAt":1758700000002}"#,
        );
        assert_eq!(p.worktree.unwrap().archived_at, Some(1758790000000));
        assert_eq!(
            p.multi.unwrap().repos[0],
            MultiRepoMember {
                dir: "/Users/fay/Code/group-feat-x/api".into(),
                repo_dir: Some("/Users/fay/Code/api".into()),
            }
        );
    }

    #[test]
    fn project_input_both_ways() {
        roundtrip::<ProjectInput>(
            r#"{"name":"box","type":"ssh","workingDir":"/srv","hostId":"h1"}"#,
        );
        let i = roundtrip::<ProjectInput>(
            r#"{"name":"box","type":"ssh","ssh":{"host":"h","port":2222,"username":"u","authMethod":"password","secret":"pw"}}"#,
        );
        assert_eq!(i.ssh.unwrap().secret.as_deref(), Some("pw"));
        roundtrip::<ProjectInput>(
            r#"{"name":"group","type":"local","repos":["/a","/b/sub"],"defaultWorktreeBranch":""}"#,
        );
    }

    #[test]
    fn ssh_host_and_input() {
        let h = roundtrip::<SshHost>(
            r#"{"id":"h1","name":"dev","host":"10.0.0.2","port":22,"username":"root","authMethod":"agent",
                "hasSecret":false,"projectCount":3,"createdAt":1758700000000}"#,
        );
        assert_eq!(h.project_count, 3);
        roundtrip::<SshHostInput>(
            r#"{"name":"dev","host":"10.0.0.2","port":22,"username":"root","authMethod":"key","keyPath":"~/.ssh/id","secret":"pp"}"#,
        );
    }

    #[test]
    fn ssh_probe_both_branches() {
        let ok = roundtrip::<SshProbeResult>(r#"{"ok":true,"kind":"windows","home":"C:\\Users\\fay"}"#);
        assert_eq!(
            ok,
            SshProbeResult::Reachable { kind: HostKind::Windows, home: r"C:\Users\fay".into() }
        );
        let bad = roundtrip::<SshProbeResult>(r#"{"ok":false,"error":"All configured authentication methods failed"}"#);
        assert!(matches!(bad, SshProbeResult::Failed { .. }));
        assert!(serde_json::from_str::<SshProbeResult>(r#"{"ok":true}"#).is_err());
    }

    #[test]
    fn delete_result() {
        roundtrip::<DeleteProjectResult>(r#"{"ok":true}"#);
        let r = roundtrip::<DeleteProjectResult>(
            r#"{"ok":true,"warnings":["目录未删除：/Users/fay/Code/x-feat（非空）"]}"#,
        );
        assert_eq!(r.warnings.unwrap().len(), 1);
    }
}
