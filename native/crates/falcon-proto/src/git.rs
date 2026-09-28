//! 右侧 Git 面板（修改 / 差异 / 历史）与侧栏的改动计数。
//!
//! 这一组端点有一条共同的规矩：**环境事实与 git 命令失败都不抛 4xx**，写在
//! `available` / `reason` / `detail`（或 `ok`）里——没装 git、不是仓库、凭据不对、
//! 有冲突，都是仓库的正常状态，git 的原话比一句"操作失败"有用得多。客户端据此
//! 渲染说明，而不是走错误分支。

use serde::{Deserialize, Serialize};

use crate::wire::{double_option, wire_enum};
use crate::worktree::WorktreeFailure;

wire_enum! {
    /// Git 面板"不可用"的原因。TS 里是 `Extract<WorktreeFailure, …>`，
    /// 只取与面板相关的四个值。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类，而且整个面板快照都挂在这一个字段上，
    /// 一个新值不该让快照解不出来。
    pub enum GitUnavailableReason open {
        /// 宿主机上没有 git，或 git 不在非登录 shell 的 PATH 里
        GitMissing => "git-missing",
        /// 工作目录不在任何 git 仓库里
        NotARepo => "not-a-repo",
        /// 没有工作目录
        NoWorkingDir => "no-working-dir",
        /// 命令根本没跑起来：SSH 抖动 / spawn 失败
        LinkFailed => "link-failed",
    }
}

impl From<GitUnavailableReason> for WorktreeFailure {
    fn from(r: GitUnavailableReason) -> Self {
        match r {
            GitUnavailableReason::GitMissing => WorktreeFailure::GitMissing,
            GitUnavailableReason::NotARepo => WorktreeFailure::NotARepo,
            GitUnavailableReason::NoWorkingDir => WorktreeFailure::NoWorkingDir,
            GitUnavailableReason::LinkFailed => WorktreeFailure::LinkFailed,
            GitUnavailableReason::Unknown => WorktreeFailure::Unknown,
        }
    }
}

/// porcelain status 里的一个改动文件。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitFileChange {
    pub path: String,
    /// 重命名 / 复制前的路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    /// porcelain X：暂存区状态，空格表示无
    pub index: String,
    /// porcelain Y：工作区状态，空格表示无
    pub work: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitRemote {
    pub name: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommit {
    pub sha: String,
    pub author: String,
    /// unix 毫秒
    pub authored_at: i64,
    pub subject: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeRef {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// 短 sha
    pub head: String,
    pub current: bool,
}

/// 右侧 Git 面板的仓库快照（`GET /api/projects/:id/git`）。源项目和附属项目都能问。
///
/// 与 RepoInfo 分开：那边是派生用的分支清单（建议目录、占用），这边是当前
/// 工作区状态。环境事实同样不抛 4xx，写在 available / reason 里。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_dir: Option<String>,
    /// 项目工作目录（可能是仓库里的子目录）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// TS 是 `ahead?: number | null`，三态：缺省（不可用的快照根本不带这项）/
    /// null（没有 upstream）/ 数。用 `Option<Option<_>>` 原样保留，读数用
    /// [`GitSnapshot::ahead_count`]。
    #[serde(
        default,
        deserialize_with = "double_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub ahead: Option<Option<u32>>,
    /// 同 `ahead`
    #[serde(
        default,
        deserialize_with = "double_option::deserialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub behind: Option<Option<u32>>,
    pub remotes: Vec<GitRemote>,
    pub files: Vec<GitFileChange>,
    /// 工作区改动总数；files 可能被截断
    pub file_count: u32,
    pub worktrees: Vec<GitWorktreeRef>,
    pub commits: Vec<GitCommit>,
}

impl GitSnapshot {
    /// 领先 upstream 的提交数；没有 upstream 或快照不可用时为 `None`。
    pub fn ahead_count(&self) -> Option<u32> {
        self.ahead.flatten()
    }

    /// 落后 upstream 的提交数；没有 upstream 或快照不可用时为 `None`。
    pub fn behind_count(&self) -> Option<u32> {
        self.behind.flatten()
    }
}

/// Git 面板里单个文件的 diff（`GET …/git/diff`、`GET …/git/commit/diff`）。
///
/// 基准一律是 HEAD（暂存 + 未暂存合在一起），未跟踪文件与空文件比对。
/// 环境事实与命令失败同样不抛 4xx，写在 available / reason / detail 里。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// unified diff 文本；没有差异（如空的未跟踪文件）时为空串
    pub diff: String,
    /// 超过上限时按行边界截断
    pub truncated: bool,
}

// ---------------- History 面板 ----------------

wire_enum! {
    pub enum GitRefKind {
        Local => "local",
        Remote => "remote",
        Tag => "tag",
    }
}

/// 提交上挂的 ref 标签。同一条提交可能同时有本地分支、远程分支、标签
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitRefLabel {
    /// 去掉 refs/heads/ refs/remotes/ refs/tags/ 前缀后的短名
    pub name: String,
    pub kind: GitRefKind,
    /// HEAD 指向的那条本地分支（%D 里的 `HEAD -> x`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<bool>,
}

/// History 列表里的一条提交。
///
/// sha 是**完整**的 40 位：提交图要靠它与 parents 对应，短 sha 在大仓库里
/// 有撞的可能，而撞一次画出来的线就整个错位。展示用短 sha 走 short 字段。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitLogCommit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub author_email: String,
    /// unix 毫秒
    pub authored_at: i64,
    pub subject: String,
    /// 完整 sha，按 git 的顺序（第一个是 first parent）
    pub parents: Vec<String>,
    pub refs: Vec<GitRefLabel>,
}

/// History 列表的一页（`GET …/git/log`）。环境事实同样不抛 4xx，写在 available / reason 里
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitLogPage {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub commits: Vec<GitLogCommit>,
    /// 还有下一页（服务端多取一条探出来的，不做 count）
    pub has_more: bool,
}

/// History 的 Branch 筛选项。刻意不复用 RepoBranch——那边的 suggestedDir /
/// dirOccupied 是派生用的，每次都要多跑一轮路径存在性探测，一个筛选下拉不值这个价。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchRef {
    /// 短名。本地为 main，远程为 origin/main
    pub name: String,
    pub remote: bool,
    pub head: bool,
    /// 本地分支跟踪的远程分支（origin/main）；没有跟踪关系时缺省
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

/// Branch / User 两个筛选下拉的候选值（`GET …/git/refs`）
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitRefsInfo {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub branches: Vec<GitBranchRef>,
    /// 短名，按创建时间新→旧。树里 Tags 那一组
    pub tags: Vec<String>,
    /// 近若干条提交里出现过的作者名，按出现次数降序
    pub authors: Vec<String>,
    /// 这台机器上 git 配的 user.name，作者下拉里的「我」。没配则缺省
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
}

/// 「修改」面板里的一个工作区改动文件。
///
/// 比 GitFileChange 多了增删行数：porcelain 只说"改了"，不说改了多少。
/// TS 里是 `extends GitFileChange`，这里用 flatten 摊平，线上形状不变。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorkingFile {
    #[serde(flatten)]
    pub change: GitFileChange,
    /// 二进制文件、或算不出来时为 null（与 GitCommitFile 同一个约定）
    #[serde(default)]
    pub added: Option<u64>,
    #[serde(default)]
    pub deleted: Option<u64>,
}

impl std::ops::Deref for GitWorkingFile {
    type Target = GitFileChange;
    fn deref(&self) -> &GitFileChange {
        &self.change
    }
}

wire_enum! {
    /// 进行中的合并 / 变基 / cherry-pick / revert。
    ///
    /// Rust 侧加了 `Unknown` 兜底：状态类，将来可能多一种（bisect / am），
    /// 不该因此让整个「修改」面板解不出来。
    pub enum GitConflictKind open {
        Merge => "merge",
        Rebase => "rebase",
        CherryPick => "cherry-pick",
        Revert => "revert",
    }
}

/// [`GitWorkingChanges::conflict`]（TS 里是内联的 `{ kind }`）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitConflict {
    pub kind: GitConflictKind,
}

/// 工作区里全部未提交的改动（`GET …/git/working`）。基准是 HEAD，暂存与未暂存
/// 合在一起看——面板不区分暂存区，它回答的是"我这次动了什么"。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorkingChanges {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 仓库根目录名，目录树视图拿它当根节点
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    pub files: Vec<GitWorkingFile>,
    /// 改动总数；files 可能被截断
    pub file_count: u32,
    /// HEAD 完整提交信息。修订上次提交时预填；空仓库没有
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_message: Option<String>,
    /// 进行中的合并 / 变基 / cherry-pick / revert。有值时面板出继续 / 中止
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<GitConflict>,
}

/// 提交详情里的一个改动文件
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitFile {
    pub path: String,
    /// 重命名 / 复制前的路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    /// raw diff 的状态字母：A M D R C T
    pub status: String,
    /// 二进制文件为 null（numstat 那两列是 `-`）
    #[serde(default)]
    pub added: Option<u64>,
    #[serde(default)]
    pub deleted: Option<u64>,
}

/// 选中提交的详情：完整提交信息 + 改动文件（`GET …/git/commit`）。
///
/// 合并提交按 first-parent 取 diff——不加 --diff-merges 的话 `git show` 对合并
/// 提交一个文件都不输出，面板上看着就像"这次合并什么都没改"。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDetail {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub sha: String,
    pub short: String,
    pub author: String,
    pub author_email: String,
    /// unix 毫秒
    pub authored_at: i64,
    pub committer: String,
    /// unix 毫秒
    pub committed_at: i64,
    pub parents: Vec<String>,
    pub refs: Vec<GitRefLabel>,
    /// 完整提交信息（含标题行）
    pub message: String,
    pub files: Vec<GitCommitFile>,
    /// 改动文件总数；files 可能被截断
    pub file_count: u32,
}

/// 提交请求（`POST …/git/commit`）。
///
/// all 与 paths 是两条不同的实现路径，不是同一件事的两种写法：
/// - all=true 走 `git add -A` + 无 pathspec 的 commit，命令行长度恒定，
///   所以"提交全部改动"不受文件数限制；
/// - 否则按 pathspec 提交选中的那些，路径要拼进命令行，有长度上限
///   （Windows 远端尤其紧，见服务端的 COMMIT_PATHSPEC_BUDGET）。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitInput {
    pub message: String,
    pub all: bool,
    /// all=false 时必填。重命名要同时给新旧两个路径
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
    /// 修订 HEAD。信息空则 --no-edit，沿用上次提交说明
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amend: Option<bool>,
    /// 提交成功后再 push。推送失败时 ok=false，detail 会写明提交已经落了
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<bool>,
}

/// pull / push / commit / op 的结果。
///
/// 失败**不抛 4xx**：凭据不对、有冲突、远端拒绝都是仓库的正常状态，
/// 前端要原样把 git 说的话给用户看，而不是一句"操作失败"。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitSyncResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<GitUnavailableReason>,
    /// git 的输出（成功时是 stdout，失败时优先 stderr），已截断到可读长度
    pub detail: String,
}

wire_enum! {
    pub enum GitResetMode {
        Soft => "soft",
        Mixed => "mixed",
        Hard => "hard",
    }
}

wire_enum! {
    /// [`GitOpInput::Take`] 取哪一边。
    pub enum GitTakeSide {
        Ours => "ours",
        Theirs => "theirs",
    }
}

/// 历史面板上对仓库动手的操作（`POST …/git/op`）。失败同样不抛 4xx，回 GitSyncResult。
///
/// checkout 的 detach 用来检出提交 / 标签（游离 HEAD）；检出已有本地分支走
/// checkout-branch。createTracking 只在远程分支还没有对应本地分支时建跟踪。
///
/// 客户端 → 服务端单向，不加 `Unknown` 兜底。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all_fields = "camelCase")]
pub enum GitOpInput {
    #[serde(rename = "fetch")]
    Fetch,
    #[serde(rename = "checkout")]
    Checkout {
        rev: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detach: Option<bool>,
    },
    #[serde(rename = "checkout-branch")]
    CheckoutBranch {
        branch: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        create_tracking: Option<bool>,
    },
    #[serde(rename = "cherry-pick")]
    CherryPick { sha: String },
    #[serde(rename = "revert")]
    Revert { sha: String },
    #[serde(rename = "branch-create")]
    BranchCreate {
        name: String,
        start_point: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checkout: Option<bool>,
    },
    #[serde(rename = "reset")]
    Reset { rev: String, mode: GitResetMode },
    #[serde(rename = "merge")]
    Merge { rev: String },
    #[serde(rename = "rebase")]
    Rebase { rev: String },
    #[serde(rename = "restore")]
    Restore {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untracked: Option<Vec<String>>,
    },
    /// 绝不带 --force。forceWithLease 才是远端领先时的那条路
    #[serde(rename = "push")]
    Push {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force_with_lease: Option<bool>,
    },
    #[serde(rename = "drop")]
    Drop { sha: String },
    #[serde(rename = "squash")]
    Squash { sha: String },
    #[serde(rename = "reword")]
    Reword { sha: String, message: String },
    #[serde(rename = "continue")]
    Continue,
    #[serde(rename = "abort")]
    Abort,
    #[serde(rename = "take")]
    Take { side: GitTakeSide, paths: Vec<String> },
}

/// 侧栏最后一层用的工作区文件计数（`GET …/git/changes`，批量版
/// `POST /api/git/changes` 回 `HashMap<项目 id, GitChangeCounts>`）。
/// 只跑 `status --porcelain`，不跑 numstat。
///
/// +added = 新增 / 未跟踪 / 已修改 / 重命名（文件还在的改动）；
/// -deleted = 删除。环境事实写在 available 里，不抛 4xx。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitChangeCounts {
    pub available: bool,
    pub added: u32,
    pub deleted: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::roundtrip;
    use std::collections::HashMap;

    #[test]
    fn snapshot_available_with_upstream() {
        let s = roundtrip::<GitSnapshot>(
            r#"{"remotes":[{"name":"origin","url":"git@github.com:fay/mojito.git"}],
                "files":[{"path":"src/a.ts","index":"M","work":" "},
                         {"path":"b.ts","origPath":"a.ts","index":"R","work":" "}],
                "fileCount":2,
                "worktrees":[{"path":"/Users/fay/Code/mojito","branch":"main","head":"65e13da","current":true},
                             {"path":"/Users/fay/Code/mojito-x","head":"7b37903","current":false}],
                "commits":[{"sha":"65e13da","author":"fay","authoredAt":1758700000000,"subject":"fix: x"}],
                "available":true,"repoDir":"/Users/fay/Code/mojito","workDir":"/Users/fay/Code/mojito",
                "headBranch":"main","headSha":"65e13da","detached":false,"upstream":"origin/main",
                "ahead":1,"behind":0}"#,
        );
        assert_eq!(s.ahead_count(), Some(1));
        assert_eq!(s.behind_count(), Some(0));
    }

    #[test]
    fn snapshot_ahead_three_states() {
        // 没有 upstream：ahead / behind 是 null，必须原样写回 null
        let s = roundtrip::<GitSnapshot>(
            r#"{"remotes":[],"files":[],"fileCount":0,"worktrees":[],"commits":[],
                "available":true,"repoDir":"/r","workDir":"/r","headBranch":"wip","detached":false,
                "ahead":null,"behind":null}"#,
        );
        assert_eq!(s.ahead, Some(None));
        assert_eq!(s.ahead_count(), None);
        // 不可用的快照（unavailableSnapshot）：根本不带这两项，写回也不能多出 null
        let s = roundtrip::<GitSnapshot>(
            r#"{"remotes":[],"files":[],"fileCount":0,"worktrees":[],"commits":[],
                "available":false,"reason":"git-missing","detail":"宿主机上没有 git"}"#,
        );
        assert_eq!(s.ahead, None);
        assert_eq!(s.reason, Some(GitUnavailableReason::GitMissing));
    }

    #[test]
    fn unavailable_reason_unknown() {
        let d = serde_json::from_str::<GitFileDiff>(
            r#"{"available":false,"reason":"safe-directory","diff":"","truncated":false}"#,
        )
        .unwrap();
        assert_eq!(d.reason, Some(GitUnavailableReason::Unknown));
        assert_eq!(WorktreeFailure::from(GitUnavailableReason::NotARepo), WorktreeFailure::NotARepo);
    }

    #[test]
    fn file_diff() {
        roundtrip::<GitFileDiff>(
            r#"{"available":true,"diff":"diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n","truncated":false}"#,
        );
        roundtrip::<GitFileDiff>(
            r#"{"available":false,"reason":"no-working-dir","detail":"没有工作目录","diff":"","truncated":false}"#,
        );
    }

    #[test]
    fn log_page_and_refs() {
        let p = roundtrip::<GitLogPage>(
            r#"{"available":true,"hasMore":true,"commits":[
                 {"sha":"65e13da0000000000000000000000000000000aa","short":"65e13da","author":"fay",
                  "authorEmail":"fay@example.com","authoredAt":1758700000000,"subject":"fix: 滚轮",
                  "parents":["7b37903000000000000000000000000000000bb","3aa1b94000000000000000000000000000000cc"],
                  "refs":[{"name":"main","kind":"local","head":true},{"name":"origin/main","kind":"remote"},
                          {"name":"v0.9.3","kind":"tag"}]}]}"#,
        );
        assert_eq!(p.commits[0].refs[2].kind, GitRefKind::Tag);
        roundtrip::<GitLogPage>(
            r#"{"available":false,"reason":"link-failed","detail":"ssh: EOF","commits":[],"hasMore":false}"#,
        );
        roundtrip::<GitRefsInfo>(
            r#"{"available":true,"branches":[{"name":"main","remote":false,"head":true,"upstream":"origin/main"},
                                             {"name":"origin/main","remote":true,"head":false}],
                "tags":["v0.9.3","v0.9.2"],"authors":["fay","bot"],"me":"fay"}"#,
        );
        roundtrip::<GitRefsInfo>(
            r#"{"available":false,"reason":"not-a-repo","branches":[],"tags":[],"authors":[]}"#,
        );
    }

    #[test]
    fn working_changes_with_conflict() {
        let w = roundtrip::<GitWorkingChanges>(
            r#"{"available":true,"repoName":"mojito","fileCount":2,"headMessage":"feat: x\n\nbody",
                "conflict":{"kind":"cherry-pick"},
                "files":[{"path":"a.ts","index":"U","work":"U","added":3,"deleted":1},
                         {"path":"logo.png","index":"?","work":"?","added":null,"deleted":null}]}"#,
        );
        assert_eq!(w.conflict.unwrap().kind, GitConflictKind::CherryPick);
        assert_eq!(w.files[0].path, "a.ts"); // Deref 到 GitFileChange
        assert_eq!(w.files[1].added, None);
        roundtrip::<GitWorkingChanges>(
            r#"{"available":false,"reason":"git-missing","files":[],"fileCount":0}"#,
        );
    }

    #[test]
    fn commit_detail() {
        roundtrip::<GitCommitDetail>(
            r#"{"available":true,"sha":"65e13da0000000000000000000000000000000aa","short":"65e13da",
                "author":"fay","authorEmail":"fay@example.com","authoredAt":1758700000000,
                "committer":"fay","committedAt":1758700001000,"parents":["7b37903000000000000000000000000000000bb"],
                "refs":[],"message":"fix: x\n\n详细说明","fileCount":2,
                "files":[{"path":"a.ts","status":"M","added":1,"deleted":1},
                         {"path":"new.ts","origPath":"old.ts","status":"R","added":0,"deleted":0},
                         {"path":"img.png","status":"A","added":null,"deleted":null}]}"#,
        );
    }

    #[test]
    fn commit_input_and_sync_result() {
        roundtrip::<GitCommitInput>(r#"{"message":"feat: x","all":true,"push":true}"#);
        roundtrip::<GitCommitInput>(
            r#"{"message":"","all":false,"paths":["a.ts","old.ts","new.ts"],"amend":true}"#,
        );
        roundtrip::<GitSyncResult>(r#"{"ok":true,"detail":"Already up to date."}"#);
        roundtrip::<GitSyncResult>(
            r#"{"ok":false,"reason":"no-working-dir","detail":"没有工作目录"}"#,
        );
    }

    #[test]
    fn git_op_every_branch() {
        let samples = [
            r#"{"op":"fetch"}"#,
            r#"{"op":"checkout","rev":"v0.9.3","detach":true}"#,
            r#"{"op":"checkout","rev":"main"}"#,
            r#"{"op":"checkout-branch","branch":"origin/feat/x","createTracking":true}"#,
            r#"{"op":"checkout-branch","branch":"main"}"#,
            r#"{"op":"cherry-pick","sha":"65e13da"}"#,
            r#"{"op":"revert","sha":"65e13da"}"#,
            r#"{"op":"branch-create","name":"feat/y","startPoint":"65e13da","checkout":true}"#,
            r#"{"op":"reset","rev":"HEAD~1","mode":"mixed"}"#,
            r#"{"op":"merge","rev":"origin/main"}"#,
            r#"{"op":"rebase","rev":"origin/main"}"#,
            r#"{"op":"restore","paths":["a.ts"],"untracked":["tmp.log"]}"#,
            r#"{"op":"restore","paths":["a.ts"]}"#,
            r#"{"op":"push","forceWithLease":true}"#,
            r#"{"op":"push"}"#,
            r#"{"op":"drop","sha":"65e13da"}"#,
            r#"{"op":"squash","sha":"65e13da"}"#,
            r#"{"op":"reword","sha":"65e13da","message":"fix: 更好的标题"}"#,
            r#"{"op":"continue"}"#,
            r#"{"op":"abort"}"#,
            r#"{"op":"take","side":"theirs","paths":["a.ts","b.ts"]}"#,
        ];
        let mut ops = std::collections::BTreeSet::new();
        for s in samples {
            let op = roundtrip::<GitOpInput>(s);
            let v: serde_json::Value = serde_json::from_str(s).unwrap();
            ops.insert(v["op"].as_str().unwrap().to_owned());
            if let GitOpInput::Reset { mode, .. } = op {
                assert_eq!(mode, GitResetMode::Mixed);
            }
        }
        // 17 种 op 一个不落
        assert_eq!(ops.len(), 17);
        assert!(serde_json::from_str::<GitOpInput>(r#"{"op":"gc"}"#).is_err());
    }

    #[test]
    fn change_counts_batch() {
        let m = roundtrip::<HashMap<String, GitChangeCounts>>(
            r#"{"p1":{"available":true,"added":3,"deleted":1},"p2":{"available":false,"added":0,"deleted":0}}"#,
        );
        assert!(m["p1"].available);
    }
}
