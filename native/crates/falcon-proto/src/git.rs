//! 右侧「修改」面板、工作区差异与侧栏的改动计数。
//!
//! 这一组端点有一条共同的规矩：**环境事实与 git 命令失败都不抛 4xx**，写在
//! `available` / `reason` / `detail`（或 `ok`）里——没装 git、不是仓库、凭据不对、
//! 有冲突，都是仓库的正常状态，git 的原话比一句"操作失败"有用得多。客户端据此
//! 渲染说明，而不是走错误分支。

use serde::{Deserialize, Serialize};

use crate::wire::wire_enum;
use crate::worktree::WorktreeFailure;

wire_enum! {
    /// 「修改」面板与差异"不可用"的原因。TS 里是 `Extract<WorktreeFailure, …>`，
    /// 只取与面板相关的四个值。
    ///
    /// Rust 侧加了 `Unknown` 兜底：原因类，工作区改动、差异和写操作的结果都挂在
    /// 这一个字段上，一个新值不该让整份响应解不出来。
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

/// 「修改」面板里单个文件的 diff（`GET …/git/diff`）。
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

/// 「修改」面板里的一个工作区改动文件。
///
/// 比 GitFileChange 多了增删行数：porcelain 只说"改了"，不说改了多少。
/// TS 里是 `extends GitFileChange`，这里用 flatten 摊平，线上形状不变。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GitWorkingFile {
    #[serde(flatten)]
    pub change: GitFileChange,
    /// 二进制文件、或算不出来时为 null（numstat 那两列是 `-`）
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
    /// 不该因此让整个「修改」面板解不出来。终端里起的冲突也走这里：面板照样能继续或中止。
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

/// commit / op / 提交后推送的结果。
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
    /// [`GitOpInput::Take`] 取哪一边。
    pub enum GitTakeSide {
        Ours => "ours",
        Theirs => "theirs",
    }
}

/// 「修改」面板上对仓库动手的操作（`POST …/git/op`）。失败同样不抛 4xx，回 GitSyncResult。
///
/// 客户端 → 服务端单向，不加 `Unknown` 兜底。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "op", rename_all_fields = "camelCase")]
pub enum GitOpInput {
    #[serde(rename = "restore")]
    Restore {
        paths: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        untracked: Option<Vec<String>>,
    },
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
            r#"{"op":"restore","paths":["a.ts"],"untracked":["tmp.log"]}"#,
            r#"{"op":"restore","paths":["a.ts"]}"#,
            r#"{"op":"continue"}"#,
            r#"{"op":"abort"}"#,
            r#"{"op":"take","side":"theirs","paths":["a.ts","b.ts"]}"#,
        ];
        let mut ops = std::collections::BTreeSet::new();
        for s in samples {
            let _op = roundtrip::<GitOpInput>(s);
            let v: serde_json::Value = serde_json::from_str(s).unwrap();
            ops.insert(v["op"].as_str().unwrap().to_owned());
        }
        assert_eq!(ops.len(), 4);
        assert!(serde_json::from_str::<GitOpInput>(r#"{"op":"fetch"}"#).is_err());
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
