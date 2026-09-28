//! 派生目录的**预览**计算。对应 web 的 `lib/worktreePath.ts`。
//!
//! 权威实现在服务端 `git/path.ts`（branchSlug / nameSlug / siblingWorktreePath /
//! multiCentralPath），这里只是让用户敲分支名时看得见结果。提交时预览值不上送
//! （单派生仅在用户手动改过目录时才发 dir），预览与真实值万一漂移也绝不会建到
//! 别处去。改动 slug 规则时请三边（server / web / 这里）对照。

use crate::js::is_js_whitespace;

/// 把连续的"非法字符"压成一个 `-`，再把 `-{2,}` 压成一个、剥掉首尾的 `-` / `.`，
/// 按码位截 48 个，再剥一次尾巴；什么都不剩就用 `fallback`。
fn slug(input: &str, illegal: impl Fn(char) -> bool, fallback: &str) -> String {
    // replace(/[...]+/g, "-")
    let mut replaced = String::with_capacity(input.len());
    let mut in_run = false;
    for c in input.chars() {
        if illegal(c) {
            if !in_run {
                replaced.push('-');
            }
            in_run = true;
        } else {
            replaced.push(c);
            in_run = false;
        }
    }
    // replace(/-{2,}/g, "-")
    let mut collapsed = String::with_capacity(replaced.len());
    for c in replaced.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let edge = |c: char| c == '-' || c == '.';
    let cleaned = collapsed.trim_start_matches(edge).trim_end_matches(edge);
    // Array.from(cleaned).slice(0, 48)：按码位截，不按 UTF-16
    let head: String = cleaned.chars().take(48).collect();
    let head = head.trim_end_matches(edge);
    if head.is_empty() { fallback.to_string() } else { head.to_string() }
}

/// 分支名 → 目录名后缀。镜像服务端 branchSlug：`[/"<>|\s]` 算非法
pub fn branch_slug_preview(branch: &str) -> String {
    slug(branch, |c| matches!(c, '/' | '"' | '<' | '>' | '|') || is_js_whitespace(c), "wt")
}

/// 容器名 → 集中目录名前半段。镜像服务端 nameSlug（比 branchSlug 多挡 `: * ? \` 与控制字符）
pub fn name_slug_preview(name: &str) -> String {
    slug(
        name,
        |c| matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\u{0}'..='\u{1f}') || is_js_whitespace(c),
        "multi",
    )
}

/// 单仓库派生：`<仓库根父目录>/<仓库根基名>-<分支slug>`。镜像 siblingWorktreePath
pub fn preview_dir(repo_dir: &str, branch: &str) -> String {
    if repo_dir.is_empty() || branch.is_empty() {
        return String::new();
    }
    let sep = if repo_dir.contains('\\') { "\\" } else { "/" };
    let root = repo_dir.trim_end_matches(['\\', '/']);
    // TS：i <= 0 时 parent 取整个 root（`/repo` 这种根下的仓库会得到 `/repo/repo-x`，照抄）
    let (parent, base) = match root.rfind(['\\', '/']) {
        Some(i) if i > 0 => (&root[..i], &root[i + 1..]),
        Some(i) => (root, &root[i + 1..]),
        None => (root, root),
    };
    format!("{parent}{sep}{base}-{}", branch_slug_preview(branch))
}

/// 批量派生的集中目录：`<baseDir>/<容器名slug>-<分支slug>`。镜像 multiCentralPath
pub fn preview_multi_dir(base_dir: &str, container_name: &str, branch: &str) -> String {
    if base_dir.is_empty() || branch.is_empty() {
        return String::new();
    }
    let sep = if base_dir.contains('\\') { "\\" } else { "/" };
    let root = base_dir.trim_end_matches(['\\', '/']);
    format!("{root}{sep}{}-{}", name_slug_preview(container_name), branch_slug_preview(branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_dir_mirrors_sibling_worktree_path() {
        assert_eq!(preview_dir("/home/u/code/web", "feat/x"), "/home/u/code/web-feat-x");
        assert_eq!(preview_dir("D:\\code\\web\\", "feat/x"), "D:\\code\\web-feat-x");
    }

    #[test]
    fn preview_dir_keeps_cjk_strips_illegal_chars_truncates_by_code_point() {
        assert_eq!(preview_dir("/a/repo", "功能/中文"), "/a/repo-功能-中文");
        let long = "x".repeat(60);
        assert_eq!(preview_dir("/a/repo", &long), format!("/a/repo-{}", "x".repeat(48)));
    }

    #[test]
    fn preview_dir_returns_empty_until_both_inputs_exist() {
        assert_eq!(preview_dir("", "feat"), "");
        assert_eq!(preview_dir("/a/repo", ""), "");
    }

    #[test]
    fn preview_multi_dir_mirrors_multi_central_path() {
        assert_eq!(preview_multi_dir("/home/u/code", "我的组合", "feat/x"), "/home/u/code/我的组合-feat-x");
        assert_eq!(preview_multi_dir("D:\\code\\", "App Suite", "feat/x"), "D:\\code\\App-Suite-feat-x");
    }

    #[test]
    fn name_slug_is_harsher_than_branch_slug() {
        assert_eq!(preview_multi_dir("/a", "s:u*ite?", "b"), "/a/s-u-ite-b");
    }

    #[test]
    fn falls_back_to_multi_when_the_name_has_nothing_usable() {
        assert_eq!(preview_multi_dir("/a", "///", "b"), "/a/multi-b");
    }

    #[test]
    fn slug_edges() {
        // 以下是 Rust 侧补的：首尾的 - / . 剥掉、截断后的尾巴再剥一次、全剥光用兜底
        assert_eq!(branch_slug_preview("..feat.."), "feat");
        assert_eq!(branch_slug_preview("a -/b"), "a-b");
        assert_eq!(branch_slug_preview("---"), "wt");
        assert_eq!(branch_slug_preview(&format!("{}.y", "x".repeat(47))), "x".repeat(47));
        assert_eq!(preview_dir("repo", "b"), "repo/repo-b");
        assert_eq!(preview_dir("/repo", "b"), "/repo/repo-b");
    }
}
