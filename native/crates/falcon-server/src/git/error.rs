//! git / worktree 操作的结构化错误。移植自 `packages/server/src/git/error.ts`。
//!
//! 形状与 zellij/install.ts 的 InstallError 完全一致：reason 是定义在 falcon-proto 的
//! 闭集枚举（TS 里是 shared 的联合类型），前后端共用，前端据此渲染具体说明——用户需要
//! 知道该去查什么，而不是收到一句"操作失败"。
//!
//! 单独一个文件是为了避免 host ↔ repo 的循环引用（TS 那边是 import 环，这里照原样分开）。

use falcon_proto::WorktreeFailure;

use super::command::js_trim;

/// TS 的 `class WorktreeError extends Error`。`Display` 只给 message（= TS 的 `err.message`），
/// detail 是 git 的原始输出，由调用方决定要不要拼上（见 [`git_error_line`]）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct WorktreeError {
    pub reason: WorktreeFailure,
    pub message: String,
    pub detail: Option<String>,
}

impl WorktreeError {
    pub fn new(reason: WorktreeFailure, message: impl Into<String>, detail: Option<String>) -> Self {
        Self { reason, message: message.into(), detail }
    }
}

/// 每种失败原因给用户看的一句话。
///
/// `Unknown` 是 Rust 侧反序列化兜底用的（服务端自己不会产出它），TS 的表里没有这一项。
pub fn worktree_failure_text(reason: WorktreeFailure) -> &'static str {
    match reason {
        WorktreeFailure::GitMissing => "宿主机上没有 git，或 git 不在 PATH 中",
        WorktreeFailure::NotARepo => "项目工作目录不在任何 git 仓库里",
        WorktreeFailure::NoWorkingDir => "项目没有指定工作目录，无从判断在哪个仓库里",
        WorktreeFailure::BranchInUse => "该分支已在另一个 worktree 中检出",
        WorktreeFailure::BranchExists => "已存在同名分支",
        WorktreeFailure::BranchUnknown => "找不到这个分支或基点",
        WorktreeFailure::PathOccupied => "目标目录已存在",
        WorktreeFailure::PathInsideRepo => "目标目录落在仓库内部",
        WorktreeFailure::PathTooLong => "目标路径过长",
        WorktreeFailure::WorktreeAddFailed => "git worktree add 失败",
        WorktreeFailure::LinkFailed => "命令没能在宿主机上跑起来",
        WorktreeFailure::Unknown => "未知错误",
    }
}

/// `/^(fatal|error|warning):/i`。JS 不带 u 旗标的 /i 不会让非 ASCII 字符折叠成 ASCII
/// 字母（Rust regex 的 Unicode 大小写折叠会，比如 U+212A 开尔文号 ≈ k），所以手写 ASCII 比较。
fn has_git_severity_prefix(line: &str) -> bool {
    ["fatal:", "error:", "warning:"].iter().any(|p| {
        line.len() >= p.len() && line.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes())
    })
}

/// 从 git 的多行输出里挑出真正有信息的那一行。
///
/// 直接取第一行是不行的：`worktree add` 失败时 stderr 的第一行是
/// "Preparing worktree (checking out 'x')"，真正的原因在第二行的 `fatal:` 上。
/// 拿第一行给用户，等于告诉他"操作失败了，因为我们正在准备操作"。
pub fn git_error_line(text: &str) -> &str {
    let mut first = None;
    for raw in text.split('\n') {
        // split(/\r?\n/) 再 trim()（JS 的空白集合）：trim 本来就会吃掉行尾的 \r
        let line = js_trim(raw);
        if line.is_empty() {
            continue;
        }
        if has_git_severity_prefix(line) {
            return line;
        }
        first.get_or_insert(line);
    }
    first.unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_error_line_prefers_the_fatal_line_over_the_preparing_banner() {
        let stderr = "Preparing worktree (checking out 'x')\r\nfatal: 'x' is already used by worktree at '/a'\r\n";
        assert_eq!(git_error_line(stderr), "fatal: 'x' is already used by worktree at '/a'");
    }

    #[test]
    fn git_error_line_matches_severity_case_insensitively() {
        assert_eq!(git_error_line("  hint: foo\n  ERROR: bar  \n"), "ERROR: bar");
    }

    #[test]
    fn git_error_line_falls_back_to_first_non_empty_line() {
        assert_eq!(git_error_line("\n\n  something odd \nsecond"), "something odd");
        assert_eq!(git_error_line(""), "");
        assert_eq!(git_error_line(" \r\n\t"), "");
    }

    #[test]
    fn worktree_error_displays_message_only() {
        let e = WorktreeError::new(WorktreeFailure::NotARepo, "读不到这条提交", Some("fatal: bad object".into()));
        assert_eq!(e.to_string(), "读不到这条提交");
        assert_eq!(worktree_failure_text(e.reason), "项目工作目录不在任何 git 仓库里");
    }
}
