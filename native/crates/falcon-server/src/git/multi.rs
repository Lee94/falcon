//! 多仓库项目：成员清单校验与批量派生编排。移植自 `packages/server/src/git/multi.ts`，
//! 设计见 `docs/adr/0003-multi-repo-projects.md`。
//!
//! 批量派生是**全有或全无**：任一成员失败，回滚已建好的 worktree（刚建的、
//! claim_worktree 确认过是我们自己建的），不留半成品项目行。回滚住在 remove.rs——
//! 那里仍是全仓库唯一删除用户可见路径的地方，这里只做编排。
//!
//! 预检（分支存在性、检出占用、目录占用）都在锁外做，是为了在**动手之前**把
//! 可预见的失败拦下来——拦住一个就省一轮回滚；真正的 TOCTOU 兜底仍是
//! add_worktree 的 classify_add_error，与单仓库派生"预检不是替代"的立场一致。

use std::collections::HashMap;

use falcon_proto::{
    MULTI_REPO_MAX, MultiRepoMember, MultiWorktreeInput, MultiWorktreeMode, WorktreeFailure, WorktreeMode,
};
use serde_json::Value;

use super::command::{self as gc, js_trim};
use super::error::{WorktreeError, worktree_failure_text};
use super::host::GitHost;
use super::lock::{repo_lock_key, with_repo_lock};
use super::path::{
    WINDOWS_PATH_BUDGET, is_absolute, is_ancestor, is_unc, member_worktree_path, multi_central_path, normalize_sep,
    path_depth, veto_multi_roots, veto_target_dir,
};
use super::remove::{CreatedWorktree, rollback_created_worktrees};
use super::repo::{
    AddWorktreeOptions, RunOpts, add_worktree, batch_git, checked_out_map, code_text, ensure_version, main_worktree_of,
    path_exists, paths_exist, root_from,
};
use crate::zellij::host::HostKind;

/// 成员级失败：在 WorktreeError 上多带"是哪个成员"（容器里配置的路径，用户认得
/// 那一个）与回滚残留。路由据此组装 MultiDeriveError 响应体（TS 里是
/// `class MultiWorktreeError extends WorktreeError`，这里把父类的三个字段摊平）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MultiWorktreeError {
    pub reason: WorktreeFailure,
    pub message: String,
    pub detail: Option<String>,
    pub member_dir: String,
    /// 回滚后仍残留在磁盘上的绝对路径；非空 ⇒ HTTP 502
    pub leftover: Vec<String>,
}

/// 容器级校验失败（成员重复、basename 撞名、目录位置不合适……）：纯文本 409
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MultiVetoError(pub String);

/// [`derive_multi_worktrees`] 的三类失败，路由分别映射：
/// - `Veto` → 409 纯文本（容器级校验）；
/// - `Member` → 按 reason 映射状态码 + member 归因（leftover 非空 ⇒ 502）；
/// - `Worktree` → 不带成员归因的 WorktreeError（握手失败、链路故障、集中目录已存在），
///   按 reason 映射状态码。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeriveMultiError {
    #[error(transparent)]
    Veto(#[from] MultiVetoError),
    #[error(transparent)]
    Member(#[from] MultiWorktreeError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
}

/// TS 正则 `/(?<=.)[\\/]+$/`：去掉尾部的分隔符，但至少留一个字符——裸 `/` 不能被
/// 剥成空串。`.` 不匹配行终止符，所以分隔符串紧跟在换行后面时也要留一个（照搬到底）
fn strip_trailing_seps(s: &str) -> &str {
    let body = s.trim_end_matches(['\\', '/']);
    if body.len() == s.len() {
        return s;
    }
    let prev_ok = body.chars().next_back().is_some_and(|c| !matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}'));
    if prev_ok { body } else { &s[..body.len() + 1] }
}

/// 成员清单的请求校验（创建/编辑容器时）。纯函数，宿主机 kind 未知——
/// 大小写别名与"同仓库两个子目录"这类要 rev-parse 才能判的，留给派生时的
/// veto_multi_roots；嵌套成员（一个成员是另一个成员的子目录）合法：嵌套的独立仓库
/// 是真实存在的布局，同仓库子目录则会在派生时按"仓库根重复"拒掉。
///
/// 吃请求体里的原值（TS 的 `unknown`），`Err` 是给用户看的一句话（路由回 400）。
pub fn validate_member_list(repos: &Value) -> Result<Vec<String>, String> {
    let Value::Array(items) = repos else {
        return Err("成员仓库必须是路径数组".into());
    };
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let Value::String(item) = item else {
            return Err("成员仓库必须是路径数组".into());
        };
        let dir = strip_trailing_seps(js_trim(item));
        if dir.is_empty() {
            return Err("成员仓库路径不能为空".into());
        }
        if out.iter().any(|d| d == dir) {
            return Err(format!("成员仓库重复：{dir}"));
        }
        out.push(dir.to_string());
    }
    if out.is_empty() {
        return Err("至少添加一个成员仓库".into());
    }
    if out.len() > MULTI_REPO_MAX {
        return Err(format!("成员仓库最多 {MULTI_REPO_MAX} 个"));
    }
    Ok(out)
}

/// 批量派生的结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiDeriveOutcome {
    pub central_dir: String,
    /// 与容器成员同序；dir = git 报的 worktree 路径，repo_dir = 主 worktree 路径
    pub members: Vec<CreatedWorktree>,
}

/// 成员级失败的便捷构造：把底下报上来的 WorktreeError 换成带成员归因的版本
fn member_error(err: WorktreeError, member_dir: &str, leftover: Vec<String>) -> MultiWorktreeError {
    MultiWorktreeError {
        reason: err.reason,
        message: err.message,
        detail: err.detail,
        member_dir: member_dir.to_string(),
        leftover,
    }
}

fn member_fail(reason: WorktreeFailure, message: String, detail: Option<String>, member_dir: &str) -> DeriveMultiError {
    member_error(WorktreeError::new(reason, message, detail), member_dir, Vec::new()).into()
}

/// 批量派生：对容器全部成员按统一分支名各建一棵 worktree，集中放进一个新目录。
///
/// `container_name` / `members` 是容器行的名字与成员（成员 dir 是用户配置的路径）；
/// `start_point` 是新建分支的共同基点（源项目的 defaultWorktreeBranch），缺省各自 HEAD。
/// 失败的三类见 [`DeriveMultiError`]。
pub async fn derive_multi_worktrees(
    host: &GitHost,
    container_name: &str,
    members: &[MultiRepoMember],
    input: &MultiWorktreeInput,
    start_point: Option<&str>,
) -> Result<MultiDeriveOutcome, DeriveMultiError> {
    let git = &host.git;
    let kind = host.kind;
    let branch = &input.branch;
    let o = RunOpts::default();
    // TS 在空清单上会撞 `mains[0]!` 的 undefined 抛 TypeError（路由回 502）；
    // 容器创建时已校验非空，这里只是不让它 panic
    if members.is_empty() {
        return Err(MultiVetoError("至少添加一个成员仓库".into()).into());
    }

    // ---- 预检批 1：握手 + 逐成员仓库根 ----
    let mut argvs = vec![gc::version_args(git)];
    argvs.extend(members.iter().map(|m| gc::repo_root_args(git, &m.dir)));
    let first = batch_git(host, &argvs, gc::GIT_ENV_RO, &o).await?;
    let (ver_res, root_results) = first.results.split_first().expect("parse_git_batch 按条数返回");
    ensure_version(host, ver_res, &first.stderr)?;
    let mut roots = Vec::with_capacity(members.len());
    for (res, m) in root_results.iter().zip(members) {
        roots.push(root_from(host, res, &first.stderr).map_err(|e| member_error(e, &m.dir, Vec::new()))?);
    }

    // ---- 预检批 2：逐成员 worktree 列表 → 主 worktree（派生基准），顺带检出占用 ----
    // 成员完全可能本身指向一棵 linked worktree，基准必须取主 worktree（单派生同一条规矩）
    let argvs: Vec<Vec<String>> = roots.iter().map(|r| gc::worktree_list_args(git, r)).collect();
    let second = batch_git(host, &argvs, gc::GIT_ENV_RO, &o).await?;
    let mut mains: Vec<String> = Vec::with_capacity(members.len());
    let mut checked_out: Vec<HashMap<String, String>> = Vec::with_capacity(members.len());
    for ((res, root), m) in second.results.iter().zip(&roots).zip(members) {
        if res.code != Some(0) {
            let detail = Some(js_trim(&second.stderr))
                .filter(|s| !s.is_empty())
                .map_or_else(|| format!("退出码 {}", code_text(res.code)), str::to_string);
            return Err(member_fail(
                WorktreeFailure::WorktreeAddFailed,
                worktree_failure_text(WorktreeFailure::WorktreeAddFailed).into(),
                Some(detail),
                &m.dir,
            ));
        }
        let entries = gc::parse_worktree_list(&res.stdout);
        mains.push(main_worktree_of(&entries, root));
        checked_out.push(checked_out_map(&entries));
    }

    if let Some(veto) = veto_multi_roots(kind, &mains) {
        return Err(MultiVetoError(veto).into());
    }

    // ---- 集中目录与逐成员落点的静态检查（照单派生 routes 的顺序与措辞） ----
    let requested = input.dir.as_deref().map(js_trim).filter(|d| !d.is_empty());
    let central = normalize_sep(
        kind,
        &requested.map_or_else(|| multi_central_path(kind, &mains[0], container_name, branch), str::to_string),
    );
    if !is_absolute(kind, &central) {
        return Err(MultiVetoError("目标目录必须是绝对路径".into()).into());
    }
    if is_unc(kind, &central) {
        return Err(MultiVetoError("暂不支持在 UNC 网络路径上派生".into()).into());
    }
    if path_depth(kind, &central) < 2 {
        return Err(MultiVetoError("目标目录过于靠近文件系统根，请换一个位置".into()).into());
    }
    let targets: Vec<String> = mains.iter().map(|root| member_worktree_path(kind, &central, root)).collect();
    for ((root, target), m) in mains.iter().zip(&targets).zip(members) {
        if is_ancestor(kind, &central, root) {
            return Err(MultiVetoError(format!("目标目录包含仓库根（{root}），拒绝创建")).into());
        }
        if veto_target_dir(kind, root, &central) == Some(WorktreeFailure::PathInsideRepo) {
            let r = WorktreeFailure::PathInsideRepo;
            return Err(member_fail(
                r,
                format!("{}：源仓库会把它当成一堆未跟踪文件", worktree_failure_text(r)),
                None,
                &m.dir,
            ));
        }
        // 长度预算按成员落点算——集中目录本身短不代表最长的成员落点也短
        let len = target.encode_utf16().count();
        if kind == HostKind::Windows && len > WINDOWS_PATH_BUDGET {
            let r = WorktreeFailure::PathTooLong;
            return Err(member_fail(
                r,
                format!("{}：{len} 字符，Windows 上建得出来却删不掉", worktree_failure_text(r)),
                None,
                &m.dir,
            ));
        }
    }

    // 集中目录必须是全新的：占用语义与单派生锁内复查一致（"接管已存在目录"不是
    // 用户要的语义），也让回滚与删除的 rmdir 有据可依——目录一定是我们建的
    if path_exists(host, &central, &o).await? {
        let r = WorktreeFailure::PathOccupied;
        return Err(
            WorktreeError::new(r, format!("{}：{central}", worktree_failure_text(r)), Some(central.clone())).into()
        );
    }
    let occupied = paths_exist(host, &targets).await?;
    if let Some(at) = occupied.iter().position(|b| *b) {
        let r = WorktreeFailure::PathOccupied;
        return Err(member_fail(r, worktree_failure_text(r).into(), Some(targets[at].clone()), &members[at].dir));
    }

    // ---- 预检批 3：分支存在性，解析每个成员的实际模式 ----
    // auto = 存在则检出、不存在则从各自 HEAD 新建；new/existing 两种严格模式下
    // 这批结果用来在动手之前拦住必然的失败（拦住一个就省一轮回滚）
    let argvs: Vec<Vec<String>> = roots.iter().map(|r| gc::branch_exists_args(git, r, branch)).collect();
    let exists = batch_git(host, &argvs, gc::GIT_ENV_RO, &o).await?;
    let mut modes: Vec<WorktreeMode> = Vec::with_capacity(members.len());
    for (i, (res, m)) in exists.results.iter().zip(members).enumerate() {
        let Some(code) = res.code else {
            let r = WorktreeFailure::LinkFailed;
            return Err(member_fail(
                r,
                worktree_failure_text(r).into(),
                Some(js_trim(&exists.stderr).to_string()),
                &m.dir,
            ));
        };
        let has = code == 0;
        if input.mode == MultiWorktreeMode::NewBranch && has {
            let r = WorktreeFailure::BranchExists;
            return Err(member_fail(r, worktree_failure_text(r).into(), Some(branch.clone()), &m.dir));
        }
        if input.mode == MultiWorktreeMode::ExistingBranch && !has {
            let r = WorktreeFailure::BranchUnknown;
            return Err(member_fail(r, worktree_failure_text(r).into(), Some(branch.clone()), &m.dir));
        }
        let mode = match input.mode {
            MultiWorktreeMode::NewBranch => WorktreeMode::NewBranch,
            MultiWorktreeMode::ExistingBranch => WorktreeMode::ExistingBranch,
            MultiWorktreeMode::Auto if has => WorktreeMode::ExistingBranch,
            MultiWorktreeMode::Auto => WorktreeMode::NewBranch,
        };
        if mode == WorktreeMode::ExistingBranch
            && let Some(at) = checked_out[i].get(branch)
        {
            let r = WorktreeFailure::BranchInUse;
            return Err(member_fail(r, worktree_failure_text(r).into(), Some(format!("已检出在 {at}")), &m.dir));
        }
        modes.push(mode);
    }

    // ---- 创建：逐成员顺序进锁（不持多锁；全有或全无本就串行） ----
    // 每条 add 最长 TIMEOUT_ADD（2 分钟），MULTI_REPO_MAX 同时是整个请求的耗时上限。
    // HTTP 层没有 request timeout 是既有事实，这里不另开熔断。
    let mut created: Vec<CreatedWorktree> = Vec::with_capacity(members.len());
    for i in 0..members.len() {
        let (main, target) = (&mains[i], &targets[i]);
        let attempt = with_repo_lock(repo_lock_key(host, main), async {
            // 锁内再查一次占用：预检与创建之间用户可能刚建了同名目录（空目录也拒绝，
            // "接管一个已存在的空目录"不该继承"删项目会删这个目录"的承诺）
            if path_exists(host, target, &o).await? {
                let r = WorktreeFailure::PathOccupied;
                return Err(WorktreeError::new(r, worktree_failure_text(r), Some(target.clone())));
            }
            let opts = AddWorktreeOptions {
                mode: modes[i],
                branch: branch.clone(),
                // 只对 new-branch 有意义；existing-branch 路径会忽略
                start_point: start_point.map(str::to_string),
                dir: target.clone(),
            };
            add_worktree(host, main, &opts).await
        })
        .await;
        match attempt {
            Ok(reported) => created.push(CreatedWorktree { dir: reported, repo_dir: main.clone() }),
            Err(err) => {
                // 集中目录是本次新建的（上面已确认原先不存在），回滚要一并收掉
                let leftover = rollback_created_worktrees(host, &created, &central, true).await;
                return Err(member_error(err, &members[i].dir, leftover).into());
            }
        }
    }

    Ok(MultiDeriveOutcome { central_dir: central, members: created })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::tests::{have_git, init_repo, local_host, real_tempdir, sh};
    use serde_json::json;
    use std::path::Path;

    // ---------------- validate_member_list（multi.test.ts 原样）----------------

    #[test]
    fn trims_and_strips_trailing_separators() {
        assert_eq!(
            validate_member_list(&json!(["  /a/web/  ", "D:\\code\\srv\\"])),
            Ok(vec!["/a/web".into(), "D:\\code\\srv".into()])
        );
    }

    #[test]
    fn rejects_non_arrays_and_non_string_items() {
        assert!(validate_member_list(&json!("not-an-array")).is_err());
        assert!(validate_member_list(&json!([1])).is_err());
        assert!(validate_member_list(&json!([null])).is_err());
    }

    #[test]
    fn rejects_empty_entries_and_empty_lists() {
        assert!(validate_member_list(&json!(["  "])).is_err());
        assert!(validate_member_list(&json!([])).is_err());
    }

    #[test]
    fn rejects_exact_duplicates_after_normalization() {
        assert_eq!(validate_member_list(&json!(["/a/web", "/a/web/"])), Err("成员仓库重复：/a/web".into()));
    }

    #[test]
    fn keeps_case_different_paths_alias_detection_waits_for_derive() {
        assert!(validate_member_list(&json!(["/a/Web", "/a/web"])).is_ok());
    }

    #[test]
    fn allows_nested_members() {
        // 嵌套的独立仓库是真实存在的布局
        assert!(validate_member_list(&json!(["/a/web", "/a/web/vendor/lib"])).is_ok());
    }

    #[test]
    fn caps_the_member_count_at_multi_repo_max() {
        let many: Vec<String> = (0..=MULTI_REPO_MAX).map(|i| format!("/r/{i}")).collect();
        assert!(validate_member_list(&json!(many)).is_err());
        assert!(validate_member_list(&json!(many[..MULTI_REPO_MAX])).is_ok());
    }

    #[test]
    fn keeps_a_bare_slash_intact_instead_of_stripping_it_to_nothing() {
        assert_eq!(validate_member_list(&json!(["/"])), Ok(vec!["/".into()]));
        assert_eq!(validate_member_list(&json!(["///"])), Ok(vec!["/".into()]));
        // `.` 不匹配换行：分隔符紧跟换行时留一个（TS 正则的原样行为）
        assert_eq!(strip_trailing_seps("a\n//"), "a\n/");
        assert_eq!(strip_trailing_seps("a//"), "a");
        assert_eq!(strip_trailing_seps("a"), "a");
    }

    // ---------------- 批量派生（真 git，临时目录）----------------

    fn input(mode: MultiWorktreeMode, branch: &str) -> MultiWorktreeInput {
        MultiWorktreeInput { name: None, mode, branch: branch.into(), dir: None }
    }

    fn member(dir: &Path) -> MultiRepoMember {
        MultiRepoMember { dir: dir.to_str().unwrap().into(), repo_dir: None }
    }

    #[tokio::test]
    async fn auto_mode_mixes_checkout_and_new_branch_then_cleanup_removes_everything() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let (web, srv) = (base.join("web"), base.join("srv"));
        init_repo(&web);
        init_repo(&srv);
        sh(&web, "git branch feat/x");
        let host = local_host("local");
        let members = [member(&web.join("sub-not-root")), member(&srv)];
        std::fs::create_dir_all(web.join("sub-not-root")).unwrap();

        let out = derive_multi_worktrees(&host, "组合", &members, &input(MultiWorktreeMode::Auto, "feat/x"), None)
            .await
            .unwrap();
        let central = format!("{}/组合-feat-x", base.to_str().unwrap());
        assert_eq!(out.central_dir, central);
        assert_eq!(
            out.members,
            [
                CreatedWorktree { dir: format!("{central}/web"), repo_dir: web.to_str().unwrap().into() },
                CreatedWorktree { dir: format!("{central}/srv"), repo_dir: srv.to_str().unwrap().into() },
            ]
        );
        sh(Path::new(&format!("{central}/srv")), "test \"$(git rev-parse --abbrev-ref HEAD)\" = feat/x");

        // 再派生一次：集中目录已存在，不带成员归因
        let again = derive_multi_worktrees(&host, "组合", &members, &input(MultiWorktreeMode::Auto, "feat/x"), None)
            .await
            .unwrap_err();
        assert!(
            matches!(&again, DeriveMultiError::Worktree(e) if e.reason == WorktreeFailure::PathOccupied),
            "{again:?}"
        );

        // 预检拦截：分支已被检出 → 零创建
        let other = format!("{}/other", base.to_str().unwrap());
        let mut i = input(MultiWorktreeMode::ExistingBranch, "feat/x");
        i.dir = Some(other.clone());
        let err = derive_multi_worktrees(&host, "组合", &members, &i, None).await.unwrap_err();
        let DeriveMultiError::Member(e) = err else { panic!("{err:?}") };
        assert_eq!((e.reason, e.leftover.len()), (WorktreeFailure::BranchInUse, 0));
        assert!(!Path::new(&other).exists());

        // 删除派生行：成员逐棵注销、集中目录（连 falcon 写的清单）一起消失、成员仓库纹丝不动
        std::fs::write(
            format!("{central}/AGENTS.md"),
            format!("<!-- {} -->\n# x\n", crate::virtualdir::FALCON_GENERATED_MARK),
        )
        .unwrap();
        std::fs::write(format!("{central}/CLAUDE.md"), crate::virtualdir::CLAUDE_MD_BODY).unwrap();
        let repos = serde_json::to_string(
            &out.members
                .iter()
                .map(|c| MultiRepoMember { dir: c.dir.clone(), repo_dir: Some(c.repo_dir.clone()) })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let row = crate::db::ProjectRow {
            id: "d1".into(),
            name: "组合-feat-x".into(),
            project_type: "local".into(),
            working_dir: Some(central.clone()),
            source_project_id: Some("c1".into()),
            worktree_branch: Some("feat/x".into()),
            worktree_created_by_mojito: Some(1),
            multi_repos: Some(repos),
            ..Default::default()
        };
        let warn = crate::git::remove::cleanup_worktree(&row, &host, &[] as &[&str]).await;
        assert!(warn.is_empty(), "{warn:?}");
        assert!(!Path::new(&central).exists());
        assert!(web.join("a.txt").exists() && srv.join("a.txt").exists());
    }

    #[tokio::test]
    async fn member_failure_after_creation_rolls_back_the_created_ones() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let (web, srv) = (base.join("web"), base.join("srv"));
        init_repo(&web);
        init_repo(&srv);
        let host = local_host("local");
        let members = [member(&web), member(&srv)];
        // 共同基点只在 web 里有：预检看不出来（基点只在 add 时解析），web 建成之后 srv 才失败
        sh(&web, "git branch base-ref");
        let err = derive_multi_worktrees(
            &host,
            "g",
            &members,
            &input(MultiWorktreeMode::NewBranch, "feat/r"),
            Some("base-ref"),
        )
        .await
        .unwrap_err();
        let DeriveMultiError::Member(e) = err else { panic!("{err:?}") };
        assert_eq!(e.member_dir, srv.to_str().unwrap());
        assert_eq!(e.reason, WorktreeFailure::BranchUnknown);
        // web 那棵被 remove、集中目录被收掉，没有残留
        assert!(e.leftover.is_empty(), "{:?}", e.leftover);
        assert!(!base.join("g-feat-r").exists());
        sh(&web, "test \"$(git worktree list --porcelain | grep -c '^worktree ')\" = 1");
        // 回滚不删分支（ADR 0002 的铁律）：web 上 add -b 建出的分支留着
        sh(&web, "git rev-parse --verify --quiet refs/heads/feat/r");

        // 直接验回滚：已建成员逆序 remove、集中目录空目录删除
        let central = format!("{}/g-feat-s", base.to_str().unwrap());
        let wt = format!("{central}/web");
        let opts = AddWorktreeOptions {
            mode: WorktreeMode::NewBranch,
            branch: "feat/s".into(),
            start_point: None,
            dir: wt.clone(),
        };
        let reported = add_worktree(&host, web.to_str().unwrap(), &opts).await.unwrap();
        let created = [CreatedWorktree { dir: reported, repo_dir: web.to_str().unwrap().into() }];
        let leftover = rollback_created_worktrees(&host, &created, &central, true).await;
        assert!(leftover.is_empty(), "{leftover:?}");
        assert!(!Path::new(&central).exists());
        // 回滚不删分支（ADR 0002 的铁律）
        sh(&web, "git rev-parse --verify --quiet refs/heads/feat/s");
    }

    #[tokio::test]
    async fn container_level_vetoes() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let (a, b) = (base.join("x/app"), base.join("y/app"));
        init_repo(&a);
        init_repo(&b);
        let host = local_host("local");
        // basename 撞名：没法在同一个集中目录里平铺
        let err =
            derive_multi_worktrees(&host, "g", &[member(&a), member(&b)], &input(MultiWorktreeMode::Auto, "f"), None)
                .await
                .unwrap_err();
        assert!(matches!(&err, DeriveMultiError::Veto(v) if v.0.contains("同名")), "{err:?}");
        // 集中目录落在仓库里面
        let mut i = input(MultiWorktreeMode::Auto, "f");
        i.dir = Some(format!("{}/inside", a.to_str().unwrap()));
        let err = derive_multi_worktrees(&host, "g", &[member(&a)], &i, None).await.unwrap_err();
        let DeriveMultiError::Member(e) = err else { panic!("{err:?}") };
        assert_eq!(e.reason, WorktreeFailure::PathInsideRepo);
        // 相对路径
        i.dir = Some("rel/dir".into());
        let err = derive_multi_worktrees(&host, "g", &[member(&a)], &i, None).await.unwrap_err();
        assert_eq!(err.to_string(), "目标目录必须是绝对路径");
        // 不是仓库：成员归因
        let plain = base.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let err = derive_multi_worktrees(&host, "g", &[member(&plain)], &input(MultiWorktreeMode::Auto, "f"), None)
            .await
            .unwrap_err();
        let DeriveMultiError::Member(e) = err else { panic!("{err:?}") };
        assert_eq!((e.reason, e.member_dir.as_str()), (WorktreeFailure::NotARepo, plain.to_str().unwrap()));
    }
}
