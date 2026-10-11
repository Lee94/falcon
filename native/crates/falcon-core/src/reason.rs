//! 失败 / 非持久原因 → 给人看的一句话。对应旧 React 版的 `lib/reason.ts`。
//!
//! 文案本身在 i18n 资源里（key 沿用 React 版 `i18n.ts` 里的名字，设计文档 §4.6），这里只管
//! "哪个原因用哪个 key、带什么参数"。翻译函数由调用方给：`t(key, params)`。

use falcon_proto::{NonDurableReason, WorktreeFailure};

/// `t(key, { name: value, … })`
pub trait Translate {
    fn t(&self, key: &str, params: &[(&str, &str)]) -> String;
}

impl<F: Fn(&str, &[(&str, &str)]) -> String> Translate for F {
    fn t(&self, key: &str, params: &[(&str, &str)]) -> String {
        self(key, params)
    }
}

/// `worktree.reason_<原因，- 换成 _>`；没有原因是 `common.error`
pub fn worktree_reason_key(reason: Option<WorktreeFailure>) -> String {
    match reason {
        None => "common.error".into(),
        Some(r) => format!("worktree.reason_{}", r.as_str().replace('-', "_")),
    }
}

/// `zellij.reason_<原因，- 换成 _>`；没有原因按"没授权"说
pub fn non_durable_reason_key(reason: Option<NonDurableReason>) -> String {
    match reason {
        None => "zellij.reason_not_authorized".into(),
        Some(r) => format!("zellij.reason_{}", r.as_str().replace('-', "_")),
    }
}

/// 把 WorktreeFailure 翻成给人看的一句话。key 约定与 `zellij.reason_*` 一致
pub fn worktree_reason_text(t: &impl Translate, reason: Option<WorktreeFailure>) -> String {
    t.t(&worktree_reason_key(reason), &[])
}

/// 把 NonDurableReason 翻成给人看的一句话
pub fn reason_text(t: &impl Translate, reason: Option<NonDurableReason>) -> String {
    t.t(&non_durable_reason_key(reason), &[])
}

/// 持久性徽标的 hover 文案：先说保住了什么，再说丢了什么
pub fn durability_hint(
    t: &impl Translate,
    durable: bool,
    reason: Option<NonDurableReason>,
    zellij_version: Option<&str>,
) -> String {
    if durable {
        return match zellij_version.filter(|v| !v.is_empty()) {
            Some(version) => t.t("session.durableHintZellij", &[("version", version)]),
            None => t.t("session.durableHint", &[]),
        };
    }
    let reason = reason_text(t, reason);
    t.t("session.nonDurableHint", &[("reason", &reason)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把 key 与参数原样写出来，断言起来一目了然
    fn echo(key: &str, params: &[(&str, &str)]) -> String {
        let p: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        if p.is_empty() { key.to_string() } else { format!("{key}({})", p.join(",")) }
    }

    #[test]
    fn keys_follow_the_web_convention() {
        assert_eq!(worktree_reason_key(Some(WorktreeFailure::BranchInUse)), "worktree.reason_branch_in_use");
        assert_eq!(worktree_reason_key(None), "common.error");
        assert_eq!(non_durable_reason_key(Some(NonDurableReason::NotAuthorized)), "zellij.reason_not_authorized");
        assert_eq!(non_durable_reason_key(None), "zellij.reason_not_authorized");
        assert_eq!(worktree_reason_text(&echo, Some(WorktreeFailure::GitMissing)), "worktree.reason_git_missing");
    }

    #[test]
    fn durability_hint_says_what_is_kept_then_what_is_lost() {
        assert_eq!(durability_hint(&echo, true, None, Some("0.44.1")), "session.durableHintZellij(version=0.44.1)");
        assert_eq!(durability_hint(&echo, true, None, None), "session.durableHint");
        assert_eq!(
            durability_hint(&echo, false, Some(NonDurableReason::WindowsJobObject), None),
            "session.nonDurableHint(reason=zellij.reason_windows_job_object)"
        );
    }
}
