//! 附属项目（worktree）与多仓库项目：派生前探测、派生请求、删除前预检。
//!
//! 删除护栏的全部判据都在服务端（ADR 0002 / 0003）；客户端只负责把预检结果
//! 原样呈现在确认框里，尤其是 ignored 文件数（.env 通常是全世界唯一一份）。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;

/// 附属项目独有的属性。存在与否即判别式：`worktree != null` ⇔ 这是附属项目。
///
/// 不给 ProjectType 加第三个值：「附属」与「local/ssh」是正交的两个维度——
/// 附属项目同时也是 local 或 ssh 项目，用同一条链路、同一台宿主机。塞进同一个
/// 枚举会让每一处 `type === "ssh"` 都要改成两条判断。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInfo {
    /// 派生自哪个 Project
    pub source_project_id: String,
    /// 检出的分支短名（不带 refs/heads/）
    pub branch: String,
    /// 派生时记录的仓库根。删除时绝不去读源项目的 workingDir——那一列用户可改、
    /// 源项目行还可能已被删，而护栏必须拿创建那一刻记下的值去比对。
    pub repo_dir: String,
    /// 目录是不是 falcon 建的。false 时删除项目绝不删目录
    pub created_by_falcon: bool,
    /// 存档时间（unix 毫秒）。存在 ⇔ 已存档：侧栏默认隐藏，worktree 目录原样保留，
    /// 到期（WORKTREE_ARCHIVE_TTL_MS）由后台清扫自动删除；此前随时可恢复。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<i64>,
}

/// 存档的附属项目多久后自动删除并清理 worktree 目录。
/// 后端清扫与前端"还剩几天"的倒计时用同一个数，两边永远对得上。
pub const WORKTREE_ARCHIVE_TTL_DAYS: i64 = 7;
pub const WORKTREE_ARCHIVE_TTL_MS: i64 = WORKTREE_ARCHIVE_TTL_DAYS * 24 * 60 * 60 * 1000;

wire_enum! {
    /// worktree 操作的失败原因。与 ZellijInstallFailure 同构：闭集字符串联合，
    /// 前后端共用，前端据此渲染具体说明而不是笼统的"操作失败"。
    ///
    /// 不做自动重试（与 Zellij 安装刻意不同）：那边一轮是 14 MiB 下载，重试期望收益极高；
    /// 这边每条 git 命令都是宿主机上百毫秒级的本地操作，用户重按一次按钮的成本约等于零，
    /// 加一套重试循环只会多一条没人走的代码路径。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类，服务端会随 git 的新失败模式加值。
    pub enum WorktreeFailure open {
        /// 宿主机上没有 git，或 git 不在非登录 shell 的 PATH 里
        GitMissing => "git-missing",
        /// 工作目录不在任何 git 仓库里（裸仓库也归到这里）
        NotARepo => "not-a-repo",
        /// SSH 项目没指定工作目录，无从派生同级路径
        NoWorkingDir => "no-working-dir",
        /// 分支已在另一个 worktree 中检出——git 不允许同一分支检出两次
        BranchInUse => "branch-in-use",
        /// 新建模式下同名分支已存在
        BranchExists => "branch-exists",
        /// 分支 / 基点不存在（浅克隆上检出远程分支时常见）
        BranchUnknown => "branch-unknown",
        /// 目标目录已存在
        PathOccupied => "path-occupied",
        /// 目标目录落在仓库内部——会被源仓库当成一堆未跟踪文件
        PathInsideRepo => "path-inside-repo",
        /// 路径过长，Windows 上建得出来却删不掉
        PathTooLong => "path-too-long",
        /// git worktree add 以其他理由失败，detail 带原始 stderr
        WorktreeAddFailed => "worktree-add-failed",
        /// 命令根本没跑起来：SSH 抖动 / spawn 失败
        LinkFailed => "link-failed",
    }
}

/// 派生用的分支清单里的一条。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RepoBranch {
    /// 短名。本地为 main，远程为 origin/main
    pub name: String,
    pub remote: bool,
    /// 远程分支对应的本地分支名（origin/feat/x → feat/x）；本地分支没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_name: Option<String>,
    /// 已在某个 worktree 中检出的路径。有值 ⇒ 不能再检出一次
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_out_at: Option<String>,
    /// 是不是源项目当前的 HEAD
    pub head: bool,
    /// 按该分支派生时的建议目录
    pub suggested_dir: String,
    /// 建议目录是否已被占用
    pub dir_occupied: bool,
}

/// 源项目的仓库信息，`GET /api/projects/:id/repo`。
///
/// 探测端点永不因环境事实报错——形状与 HostZellijStatus 同源：返回一份
/// "能不能干、为什么不能"的报告，而不是 4xx。创建端点则把同样的事实当状态冲突。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    pub derivable: bool,
    /// derivable=false 时必定有值
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<WorktreeFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_dir: Option<String>,
    /// 当前 HEAD 分支；detached 时没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    /// detached 时的短 sha，作为新建分支的默认基点显示
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub branches: Vec<RepoBranch>,
}

wire_enum! {
    /// [`WorktreeInput::mode`]
    pub enum WorktreeMode {
        NewBranch => "new-branch",
        ExistingBranch => "existing-branch",
    }
}

/// 派生附属项目的请求体，`POST /api/projects/:id/worktrees`（源项目不是多仓库时）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeInput {
    /// 留空则用分支名
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub mode: WorktreeMode,
    /// 目标分支的**本地**名。检出远程分支走 new-branch + startPoint=origin/x——
    /// 直接把 origin/x 当 commit-ish 会得到 detached HEAD，而 detached worktree 里的
    /// 提交在 remove 之后立刻不可达、可被 gc 回收，那是真数据丢失。
    pub branch: String,
    /// mode=new-branch 的基点，缺省 HEAD
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_point: Option<String>,
    /// 目标目录；缺省用服务端派生的同级平铺路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// 多仓库项目的一个成员。存在于两种行上，dir 的语义不同：
/// - 容器：用户选的路径（可以是仓库子目录，派生时才 rev-parse 出仓库根）。
/// - 派生产物：该成员 worktree 的绝对路径——git 自己报的，删除护栏拿它比对。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiRepoMember {
    pub dir: String,
    /// 仅派生产物有值：成员所属仓库根（主 worktree 路径），删除取证用。
    /// 容器成员没有——与 WorktreeInfo.repoDir 同理，判据必须是创建那一刻记下的值。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_dir: Option<String>,
}

/// 存在 ⇔ 多仓库项目。与 `worktree != null ⇔ 附属项目` 是同一个判别式模式：
/// 「多仓库」与「local/ssh」正交（成员全在容器自己的宿主机上），与「容器/附属」
/// 也正交——multi 与 worktree 同时存在 = 批量派生出的多仓库附属项目。
/// 不给 ProjectType 加第三个值的理由见 WorktreeInfo 的注释。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiRepoInfo {
    pub repos: Vec<MultiRepoMember>,
}

/// 容器成员数上限。批量派生逐仓库串行，每条 add 最长 2 分钟，上限同时是耗时上限
pub const MULTI_REPO_MAX: usize = 16;

wire_enum! {
    /// [`MultiWorktreeInput::mode`]。比 [`WorktreeMode`] 多一个 auto。
    pub enum MultiWorktreeMode {
        NewBranch => "new-branch",
        ExistingBranch => "existing-branch",
        /// 分支存在则检出、不存在则从各自 HEAD 新建
        Auto => "auto",
    }
}

/// 批量派生请求（源项目是多仓库容器时的 `POST /api/projects/:id/worktrees`）。
///
/// 与 WorktreeInput 刻意分开：请求体没有 startPoint——每个成员的基点缺省是各自的
/// HEAD。源项目若配置了 defaultWorktreeBranch，服务端在新建分支时用那个引用当
/// 共同基点（跨成员的 main / master 是常态；跨成员挑某一个成员的 HEAD 才没有意义）。
/// 多一个 auto 模式，因为统一分支名跨 N 个仓库时"有的仓库已有这条分支、有的没有"
/// 是常态，没有 auto，全有或全无的语义会让混合状态永远派生不出来。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiWorktreeInput {
    /// 留空则用分支名
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub mode: MultiWorktreeMode,
    /// 目标分支的**本地**名，对全部成员统一
    pub branch: String,
    /// 集中目录；缺省 = 第一个成员仓库根的父目录 + <容器名slug>-<分支slug>
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

/// 多仓库容器的派生前探测（`GET /api/projects/:id/repos`）。
/// 与 RepoInfo 同哲学：环境事实不报错，逐成员写在 derivable / reason 里。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiRepoProbe {
    /// 与容器成员同序；dir 是容器里配置的成员路径
    pub members: Vec<MultiRepoProbeMember>,
    /// 集中目录会建在哪个父目录下（第一个成员仓库根的父目录），供前端预览
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_dir: Option<String>,
}

/// [`MultiRepoProbe::members`] 的元素（TS 里是内联的匿名类型）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiRepoProbeMember {
    pub dir: String,
    pub info: RepoInfo,
}

/// 批量派生的失败响应体。error 仍是一句可以直接展示的话，
/// member / leftover 供前端标明"哪个仓库、什么原因、回滚剩了什么"。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiDeriveError {
    pub error: String,
    /// 第一个失败的成员（全有或全无 ⇒ 至多一个）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<MultiDeriveFailedMember>,
    /// 回滚后仍残留在磁盘上的绝对路径；缺省/空 = 回滚干净。非空 ⇒ HTTP 502
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leftover: Option<Vec<String>>,
}

/// [`MultiDeriveError::member`]（TS 里是内联的匿名类型）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MultiDeriveFailedMember {
    /// 容器里配置的成员路径
    pub dir: String,
    pub reason: WorktreeFailure,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// 附属项目的工作区状态，删除前的预检（`GET /api/projects/:id/worktree`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeStatus {
    /// 目录还在不在（用户可能手工删了）
    pub present: bool,
    /// 已跟踪改动 + 未跟踪文件
    pub dirty_count: u32,
    /// 前若干条样例路径，直接进确认框的 list
    pub dirty_sample: Vec<String>,
    /// 被 .gitignore 忽略的文件数。必须单列：git status --porcelain 默认不含它们，
    /// 但 .env、本地 sqlite、上传目录会跟着一起被删——.env 通常是全世界唯一一份。
    pub ignored_count: u32,
    /// 未推送提交数；null = 该分支没有 upstream
    #[serde(default)]
    pub ahead: Option<u32>,
    /// 读不到状态时的原因，确认框据此改口说"无法确认"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;

    #[test]
    fn ttl_matches_ts() {
        assert_eq!(WORKTREE_ARCHIVE_TTL_MS, 604_800_000);
    }

    #[test]
    fn failure_literals_and_unknown() {
        for f in WorktreeFailure::ALL {
            let json = format!("\"{}\"", f.as_str());
            assert_eq!(roundtrip::<WorktreeFailure>(&json), *f);
        }
        assert_eq!(
            serde_json::from_str::<WorktreeFailure>(r#""submodule-dirty""#).unwrap(),
            WorktreeFailure::Unknown
        );
    }

    #[test]
    fn repo_info_derivable() {
        let r = roundtrip::<RepoInfo>(
            r#"{"derivable":true,"repoDir":"/Users/fay/Code/mojito","headBranch":"main",
                "branches":[
                  {"name":"main","remote":false,"checkedOutAt":"/Users/fay/Code/mojito","head":true,
                   "suggestedDir":"/Users/fay/Code/mojito-main","dirOccupied":false},
                  {"name":"origin/feat/x","remote":true,"localName":"feat/x","head":false,
                   "suggestedDir":"/Users/fay/Code/mojito-feat-x","dirOccupied":true}]}"#,
        );
        assert_eq!(r.branches[1].local_name.as_deref(), Some("feat/x"));
    }

    #[test]
    fn repo_info_not_derivable() {
        let r = roundtrip::<RepoInfo>(
            r#"{"derivable":false,"reason":"no-working-dir","detail":"SSH 项目没有指定工作目录","branches":[]}"#,
        );
        assert_eq!(r.reason, Some(WorktreeFailure::NoWorkingDir));
    }

    #[test]
    fn inputs() {
        roundtrip::<WorktreeInput>(r#"{"mode":"existing-branch","branch":"feat/x"}"#);
        roundtrip::<WorktreeInput>(
            r#"{"name":"x","mode":"new-branch","branch":"feat/x","startPoint":"origin/feat/x","dir":"/tmp/x"}"#,
        );
        let m = roundtrip::<MultiWorktreeInput>(r#"{"mode":"auto","branch":"feat/y","dir":"/w/g-feat-y"}"#);
        assert_eq!(m.mode, MultiWorktreeMode::Auto);
    }

    #[test]
    fn multi_probe_and_error() {
        roundtrip::<MultiRepoProbe>(
            r#"{"members":[
                 {"dir":"/w/api","info":{"derivable":true,"repoDir":"/w/api","headBranch":"main","branches":[]}},
                 {"dir":"/w/docs","info":{"derivable":false,"reason":"not-a-repo","detail":"不是 git 仓库","branches":[]}}],
               "baseDir":"/w"}"#,
        );
        roundtrip::<MultiRepoProbe>(
            r#"{"members":[{"dir":"/w/api","info":{"derivable":false,"reason":"link-failed","detail":"ssh: timeout","branches":[]}}]}"#,
        );
        let e = roundtrip::<MultiDeriveError>(
            r#"{"error":"web：分支已在另一个 worktree 中检出","member":{"dir":"/w/web","reason":"branch-in-use","detail":"fatal: 'feat/y' is already checked out"},
                "leftover":["/w/g-feat-y/api"]}"#,
        );
        assert_eq!(e.member.unwrap().reason, WorktreeFailure::BranchInUse);
        roundtrip::<MultiDeriveError>(r#"{"error":"派生失败"}"#);
    }

    #[test]
    fn worktree_status() {
        let s = roundtrip::<WorktreeStatus>(
            r#"{"present":true,"dirtyCount":2,"dirtySample":["src/a.ts","b.md"],"ignoredCount":1,"ahead":3}"#,
        );
        assert_eq!(s.ahead, Some(3));
        let s = roundtrip::<WorktreeStatus>(
            r#"{"present":true,"dirtyCount":0,"dirtySample":[],"ignoredCount":0,"ahead":null,"error":"ssh: handshake failed"}"#,
        );
        assert_eq!(s.ahead, None);
    }
}
