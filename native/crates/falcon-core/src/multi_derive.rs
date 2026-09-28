//! 批量派生表单的纯逻辑：按模式与分支名推演每个成员会发生什么。
//! 对应 web 的 `lib/multiDerive.ts`。
//!
//! 只是**预演**——权威判定在服务端（预检 + add 时的 TOCTOU 兜底），这里的结论用于
//! 提交前把可预见的失败摆在成员行上，别让用户白交一张表。

use falcon_proto::{MultiWorktreeMode, RepoInfo, WorktreeFailure};

/// 一个成员在这次派生里会发生什么
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberAction {
    /// 成员环境不可派生（没装 git / 不是仓库 / 连不上）
    Blocked { reason: Option<WorktreeFailure>, detail: Option<String> },
    /// 将从该成员自己的 HEAD 新建分支
    Create,
    /// 将检出该成员已有的同名分支
    Checkout,
    /// new-branch 模式下同名分支已存在
    BranchExists,
    /// existing-branch 模式下该成员没有这条分支
    BranchMissing,
    /// 分支已在该成员的另一棵 worktree 中检出
    BranchInUse { at: String },
}

impl MemberAction {
    /// web 里的 `kind` 字面量
    pub fn kind(&self) -> &'static str {
        match self {
            MemberAction::Blocked { .. } => "blocked",
            MemberAction::Create => "create",
            MemberAction::Checkout => "checkout",
            MemberAction::BranchExists => "branch-exists",
            MemberAction::BranchMissing => "branch-missing",
            MemberAction::BranchInUse { .. } => "branch-in-use",
        }
    }
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

pub fn evaluate_member(info: &RepoInfo, mode: MultiWorktreeMode, branch: &str) -> MemberAction {
    if !info.derivable {
        return MemberAction::Blocked { reason: info.reason, detail: info.detail.clone() };
    }
    let hit = info.branches.iter().find(|b| !b.remote && b.name == branch);
    let new_branch = match mode {
        MultiWorktreeMode::Auto => hit.is_none(),
        MultiWorktreeMode::NewBranch => true,
        MultiWorktreeMode::ExistingBranch => false,
    };
    if new_branch {
        return if hit.is_some() { MemberAction::BranchExists } else { MemberAction::Create };
    }
    let Some(hit) = hit else { return MemberAction::BranchMissing };
    if let Some(at) = non_empty(&hit.checked_out_at) {
        return MemberAction::BranchInUse { at: at.to_string() };
    }
    MemberAction::Checkout
}

/// 提交必须被拦下的动作（全有或全无：一个成员会失败就整单失败）
pub fn action_blocks_submit(a: &MemberAction) -> bool {
    !matches!(a, MemberAction::Create | MemberAction::Checkout)
}

/// existing-branch 模式下拉里的一项
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonBranch {
    pub name: String,
    /// 任一成员里已检出的路径（下拉里禁用）
    pub used_at: Option<String>,
}

/// existing-branch 模式的候选：全体成员共有的本地分支交集。
/// 任一成员里已检出的标记 `used_at`。顺序取第一个成员的分支顺序。
pub fn common_local_branches(members: &[&RepoInfo]) -> Vec<CommonBranch> {
    let lists: Vec<Vec<&falcon_proto::RepoBranch>> =
        members.iter().map(|m| m.branches.iter().filter(|b| !b.remote).collect()).collect();
    let Some(first) = lists.first() else { return Vec::new() };
    first
        .iter()
        .filter(|b| lists.iter().all(|l| l.iter().any(|x| x.name == b.name)))
        .map(|b| {
            let used_at = lists
                .iter()
                .find_map(|l| l.iter().find(|x| x.name == b.name).and_then(|hit| non_empty(&hit.checked_out_at)))
                .map(str::to_string);
            CommonBranch { name: b.name.clone(), used_at }
        })
        .collect()
}

/// 成员路径末段，成员行与脏文件样例的展示名。posix 与 windows 分隔符都认
pub fn member_basename(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches(['\\', '/']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => &trimmed[i + 1..],
        None => trimmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use falcon_proto::RepoBranch;

    fn branch(name: &str) -> RepoBranch {
        RepoBranch {
            name: name.into(),
            remote: false,
            local_name: None,
            checked_out_at: None,
            head: false,
            suggested_dir: String::new(),
            dir_occupied: false,
        }
    }

    fn repo(branches: Vec<RepoBranch>) -> RepoInfo {
        RepoInfo {
            derivable: true,
            reason: None,
            detail: None,
            repo_dir: Some("/r".into()),
            head_branch: Some("main".into()),
            head_sha: None,
            branches,
        }
    }

    fn blocked() -> RepoInfo {
        RepoInfo {
            derivable: false,
            reason: Some(WorktreeFailure::NotARepo),
            detail: None,
            repo_dir: None,
            head_branch: None,
            head_sha: None,
            branches: vec![],
        }
    }

    use MultiWorktreeMode::{Auto, ExistingBranch, NewBranch};

    #[test]
    fn blocked_members_block_regardless_of_mode() {
        let a = evaluate_member(&blocked(), Auto, "feat");
        assert_eq!(a.kind(), "blocked");
        assert!(action_blocks_submit(&a));
    }

    #[test]
    fn auto_checkout_when_the_branch_exists_create_otherwise() {
        assert_eq!(evaluate_member(&repo(vec![branch("feat")]), Auto, "feat").kind(), "checkout");
        assert_eq!(evaluate_member(&repo(vec![]), Auto, "feat").kind(), "create");
    }

    #[test]
    fn auto_an_existing_but_checked_out_branch_is_in_use() {
        let a = evaluate_member(
            &repo(vec![RepoBranch { checked_out_at: Some("/w".into()), ..branch("feat") }]),
            Auto,
            "feat",
        );
        assert_eq!(a, MemberAction::BranchInUse { at: "/w".into() });
        assert!(action_blocks_submit(&a));
    }

    #[test]
    fn new_branch_existing_branch_is_a_conflict_remote_branches_dont_count() {
        assert_eq!(evaluate_member(&repo(vec![branch("feat")]), NewBranch, "feat").kind(), "branch-exists");
        assert_eq!(
            evaluate_member(&repo(vec![RepoBranch { remote: true, ..branch("origin/feat") }]), NewBranch, "feat").kind(),
            "create"
        );
    }

    #[test]
    fn existing_branch_missing_branch_blocks() {
        assert_eq!(evaluate_member(&repo(vec![]), ExistingBranch, "feat").kind(), "branch-missing");
    }

    #[test]
    fn common_local_branches_intersects_and_marks_in_use_ones() {
        let a = repo(vec![branch("main"), branch("feat"), RepoBranch { remote: true, ..branch("origin/x") }]);
        let b = repo(vec![RepoBranch { checked_out_at: Some("/w".into()), ..branch("feat") }, branch("main")]);
        assert_eq!(
            common_local_branches(&[&a, &b]),
            [
                CommonBranch { name: "main".into(), used_at: None },
                CommonBranch { name: "feat".into(), used_at: Some("/w".into()) },
            ]
        );
    }

    #[test]
    fn common_local_branches_empty_input_yields_empty() {
        assert!(common_local_branches(&[]).is_empty());
    }

    #[test]
    fn member_basename_handles_both_separators_and_trailing_slashes() {
        assert_eq!(member_basename("/a/b/web/"), "web");
        assert_eq!(member_basename("D:\\code\\Web"), "Web");
        assert_eq!(member_basename("web"), "web");
    }
}
