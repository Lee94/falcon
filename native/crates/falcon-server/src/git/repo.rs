//! git 执行层：把 argv 交给宿主机跑，把结果翻译成结构化事实或 WorktreeError。
//! 移植自 `packages/server/src/git/repo.ts`。
//!
//! 核心约定（继承自 zellij/install 的执行器契约）：**非零退出码是正常返回值，
//! 绝不当错误**——本地执行器连 spawn 失败都收成 `code: None`。所以这里判错一律显式查
//! `res.code`；只有 exec 本身失败（`Err`）才是链路故障。
//!
//! 移植约定：TS 里 `a || b || c` 的字符串兜底链照搬（空串为假）；`trim()` 一律用 JS 的
//! 空白集合（[`js_trim`]）；模板里的 `${res.code}` 在没有退出码时是 `null`（[`code_text`]）；
//! `.slice(0, n)` 按 UTF-16 码元截（[`js_prefix`]）。这些都是给用户看的文本，与 TS 逐字
//! 一致可以让同一份 fixture 两边都过。

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use falcon_proto::{
    GitBranchRef, GitChangeCounts, GitCommitDetail, GitCommitInput, GitConflict, GitConflictKind, GitFileChange,
    GitFileDiff, GitLogCommit, GitLogPage, GitOpInput, GitRefsInfo, GitSnapshot, GitSyncResult, GitUnavailableReason,
    GitWorkingChanges, GitWorkingFile, GitWorktreeRef, RepoBranch, RepoInfo, WorktreeFailure, WorktreeMode,
    WorktreeStatus,
};
use regex::Regex;
use tokio_util::sync::CancellationToken;

use super::command::{self as gc, js_trim};
use super::error::{WorktreeError, worktree_failure_text};
use super::host::GitHost;
use super::path::{canon_key, is_absolute, normalize_sep, sibling_worktree_path};
use crate::exec::ExecResult;
use crate::zellij::host::HostKind;

/// 各类命令的超时。
///
/// 必须有：本地执行器与 SshLink 的 exec 都没有超时，HTTP 层（TS 版是 Fastify）也没配
/// request timeout。git 一旦停在那里（等凭据、stale NFS、半死的 SSH 通道），请求就永远
/// 不返回。GIT_TERMINAL_PROMPT=0 挡住了最常见的一种，超时兜住其余。
pub const TIMEOUT_READ: Duration = Duration::from_secs(15);
pub const TIMEOUT_ADD: Duration = Duration::from_secs(120);
pub const TIMEOUT_REMOVE: Duration = Duration::from_secs(60);

/// pull / push 的超时。比读操作宽得多——网络慢的时候一次 push 走几十秒是常事，
/// 但仍必须有：卡住的 git 会让请求永远不返回（见 [`TIMEOUT_READ`] 的注释）。
pub const TIMEOUT_SYNC: Duration = Duration::from_secs(120);

/// 到点之后给执行器多少时间收尾。执行器收到取消会立刻杀进程 / 关通道并返回；只有卡在
/// 建连或开通道这一步时才会拖过这个宽限——那时直接丢掉 future，当作"没跑出结果"。
const CANCEL_GRACE: Duration = Duration::from_secs(5);

/// 一次执行的附加选项（TS 的 `RunOpts`）。`timeout` 缺省 [`TIMEOUT_READ`]；`cancel` 是
/// 外部的取消信号（TS 的 AbortSignal），与超时合成一个，任一触发都让命令尽快结束。
#[derive(Debug, Clone, Default)]
pub struct RunOpts {
    pub timeout: Option<Duration>,
    pub cancel: Option<CancellationToken>,
}

impl RunOpts {
    pub fn timeout(d: Duration) -> Self {
        RunOpts { timeout: Some(d), cancel: None }
    }

    /// TS 的 `{ timeoutMs: X, ...opts }`：调用方给了超时就用调用方的
    fn with_default_timeout(&self, d: Duration) -> Self {
        RunOpts { timeout: Some(self.timeout.unwrap_or(d)), cancel: self.cancel.clone() }
    }
}

fn no_opts() -> RunOpts {
    RunOpts::default()
}

// ---------------- JS 语义辅助 ----------------

/// 模板字符串里的 `${res.code}`：没有退出码时是 `null`
pub(crate) fn code_text(code: Option<i32>) -> String {
    code.map_or_else(|| "null".into(), |c| c.to_string())
}

/// `a.trim() || b.trim() || fallback`
fn first_text(parts: &[&str], fallback: impl FnOnce() -> String) -> String {
    parts.iter().map(|p| js_trim(p)).find(|p| !p.is_empty()).map_or_else(fallback, str::to_string)
}

/// `s.slice(0, n)`：前 n 个 UTF-16 码元。切在代理对中间时那半个字符变成 U+FFFD
/// （TS 里是孤立代理，序列化成 JSON 时同样会被替换）
pub(crate) fn js_prefix(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string(); // UTF-16 码元数 ≤ UTF-8 字节数
    }
    let units: Vec<u16> = s.encode_utf16().take(n).collect();
    String::from_utf16_lossy(&units)
}

/// `stdout.replace(/\r?\n$/, "")`：只去掉末尾**一个**换行
fn strip_one_newline(s: &str) -> &str {
    s.strip_suffix("\r\n").or_else(|| s.strip_suffix('\n')).unwrap_or(s)
}

/// `/^\d+$/.test(s.trim()) ? Number(s.trim()) : null`
fn count_or_null(code: Option<i32>, stdout: &str) -> Option<u32> {
    let t = js_trim(stdout);
    if code != Some(0) || t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

fn link_failed(detail: String) -> WorktreeError {
    WorktreeError::new(WorktreeFailure::LinkFailed, worktree_failure_text(WorktreeFailure::LinkFailed), Some(detail))
}

fn failure(reason: WorktreeFailure, detail: String) -> WorktreeError {
    WorktreeError::new(reason, worktree_failure_text(reason), Some(detail))
}

// ---------------- 执行原语 ----------------

/// 原样执行一条命令行（给 exists_command / git_file_head_command 这类非 git 命令用）。
///
/// 超时到点与外部取消都是"让命令结束"，不是错误：本地杀进程、远端关通道，结果的
/// `code` 为 `None`（TS 版 abort 之后也是 `code: null`），由调用方照"命令失败"处理。
/// 只有执行本身起不来才是 link-failed。
pub async fn exec_raw(host: &GitHost, command_line: &str, opts: &RunOpts) -> Result<ExecResult, WorktreeError> {
    let timeout = opts.timeout.unwrap_or(TIMEOUT_READ);
    let token = match &opts.cancel {
        Some(outer) => outer.child_token(),
        None => CancellationToken::new(),
    };
    let mut fut = std::pin::pin!(host.exec.exec(command_line, Some(&token)));
    let res = tokio::select! {
        r = &mut fut => r,
        _ = tokio::time::sleep(timeout) => {
            token.cancel();
            match tokio::time::timeout(CANCEL_GRACE, &mut fut).await {
                Ok(r) => r,
                Err(_) => Ok(ExecResult { code: None, stdout: String::new(), stderr: "命令超时".into() }),
            }
        }
    };
    res.map_err(|e| link_failed(format!("{e:#}")))
}

/// 跑一条 git 命令，**允许非零退出码**——退出码本身就是答案时用它。
/// `env` 通常是 [`gc::GIT_ENV_RO`]（TS 的缺省值）或 [`gc::GIT_ENV`]
pub async fn probe_git(
    host: &GitHost,
    argv: &[String],
    env: &[(&str, &str)],
    opts: &RunOpts,
) -> Result<ExecResult, WorktreeError> {
    exec_raw(host, &gc::build_git_command_line(host.kind, argv, env), opts).await
}

/// [`batch_git`] 的结果：与入参同序的逐条结果，外加整批共用的 stderr
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchOutput {
    pub results: Vec<gc::BatchGitResult>,
    pub stderr: String,
}

/// 一次 exec 跑多条 git 命令（见 batch_git_command_line），返回与入参同序的
/// 结果与整批的 stderr。SSH 上一条 exec 就是一次 channel 往返，快照类探测
/// 的往返数直接决定面板延迟。
pub async fn batch_git(
    host: &GitHost,
    argvs: &[Vec<String>],
    env: &[(&str, &str)],
    opts: &RunOpts,
) -> Result<BatchOutput, WorktreeError> {
    let res = exec_raw(host, &gc::batch_git_command_line(host.kind, argvs, env), opts).await?;
    Ok(BatchOutput { results: gc::parse_git_batch(&res.stdout, argvs.len()), stderr: res.stderr })
}

/// `{ code, stdout }`：单条 exec 的结果与批量里的一条都能拿来判
pub trait ProbeLike {
    fn code(&self) -> Option<i32>;
    fn stdout(&self) -> &str;
}

impl ProbeLike for ExecResult {
    fn code(&self) -> Option<i32> {
        self.code
    }
    fn stdout(&self) -> &str {
        &self.stdout
    }
}

impl ProbeLike for gc::BatchGitResult {
    fn code(&self) -> Option<i32> {
        self.code
    }
    fn stdout(&self) -> &str {
        &self.stdout
    }
}

/// `git --version` 握手成功的缓存（key 是宿主机标识，只缓存成功）。
/// 握手的意义在区分"没装 git"与"不是仓库"，一台宿主机隔几分钟验一次足够，
/// 不必每个仓库根都多付一个往返。
static VERSION_OK_UNTIL: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);
const VERSION_TTL: Duration = Duration::from_secs(5 * 60);

fn version_cache() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    VERSION_OK_UNTIL.lock().unwrap_or_else(|e| e.into_inner())
}

/// 给 multi 的批量探测复用（batch 里第一条就是 version，往返已省，缓存无所谓）。
/// `/git version/i`：JS 不带 u 旗标的 /i 只折叠 ASCII 字母
pub fn ensure_version(host: &GitHost, res: &impl ProbeLike, stderr: &str) -> Result<(), WorktreeError> {
    if res.code() == Some(0) && res.stdout().to_ascii_lowercase().contains("git version") {
        version_cache().insert(host.key.clone(), Instant::now() + VERSION_TTL);
        return Ok(());
    }
    Err(failure(
        WorktreeFailure::GitMissing,
        first_text(&[stderr, res.stdout()], || format!("退出码 {}", code_text(res.code()))),
    ))
}

pub fn root_from(host: &GitHost, res: &impl ProbeLike, stderr: &str) -> Result<String, WorktreeError> {
    if res.code() != Some(0) {
        return Err(failure(
            WorktreeFailure::NotARepo,
            first_text(&[stderr], || format!("退出码 {}", code_text(res.code()))),
        ));
    }
    let raw = js_trim(res.stdout());
    let root = normalize_sep(host.kind, raw);
    if root.is_empty() || !is_absolute(host.kind, &root) {
        return Err(WorktreeError::new(WorktreeFailure::NotARepo, "无法解析仓库根", Some(raw.to_string())));
    }
    Ok(root)
}

/// 跑一条 git 命令，非零退出码翻译成带 reason 的 WorktreeError
pub async fn run_git(
    host: &GitHost,
    argv: &[String],
    reason: WorktreeFailure,
    env: &[(&str, &str)],
    opts: &RunOpts,
) -> Result<ExecResult, WorktreeError> {
    let res = probe_git(host, argv, env, opts).await?;
    if res.code != Some(0) {
        // 三级兜底与 install 的 run() 一致：有些命令一个字都不往 stderr 写，
        // 只留一个退出码，而"该改环境还是该重试"全靠这一句话
        return Err(failure(
            reason,
            first_text(&[&res.stderr, &res.stdout], || format!("命令退出码 {}", code_text(res.code))),
        ));
    }
    Ok(res)
}

pub async fn path_exists(host: &GitHost, p: &str, opts: &RunOpts) -> Result<bool, WorktreeError> {
    let res = exec_raw(host, &gc::exists_command(host.kind, p), opts).await?;
    Ok(res.stdout.contains("yes"))
}

/// 分片批量探测，返回与入参同序的布尔数组。任何一片对不上就整体退化为"未知（false）"
pub async fn paths_exist(host: &GitHost, paths: &[String]) -> Result<Vec<bool>, WorktreeError> {
    let mut out = Vec::with_capacity(paths.len());
    for slice in paths.chunks(gc::EXISTS_BATCH) {
        let res = exec_raw(host, &gc::exists_many_command(host.kind, slice), &no_opts()).await?;
        let parsed = gc::parse_exists_many(&res.stdout);
        // 条数对不上说明输出被 profile 之类污染了，宁可当作"未知"也不要错位
        if parsed.len() != slice.len() {
            return Ok(vec![false; paths.len()]);
        }
        out.extend(parsed);
    }
    Ok(out)
}

/// 仓库根。
///
/// 必须先握手 `git --version` 再解释 rev-parse 的失败：POSIX 上缺 git 是 127 +
/// "command not found"，而 Windows 的 `& 'git'` 在 git 缺失时抛
/// CommandNotFoundException、退出码 1、错误文本还是本地化的——跟"不是仓库"完全
/// 分不开。先握手一次就把两件事彻底分开，代价是一次往返。
/// （同一个套路在 zellij 那边叫 verify()，理由一模一样。）
///
/// 裸仓库也走 not-a-repo：`--show-toplevel` 在裸仓库里直接 fatal，而"没有工作树的
/// 仓库"对派生来说与"不是仓库"是同一件事。
pub async fn repo_root(host: &GitHost, dir: &str, opts: &RunOpts) -> Result<String, WorktreeError> {
    let fresh = version_cache().get(&host.key).is_some_and(|until| *until >= Instant::now());
    if !fresh {
        let ver = probe_git(host, &gc::version_args(&host.git), gc::GIT_ENV, opts).await?;
        ensure_version(host, &ver, &ver.stderr)?;
    }
    let res = probe_git(host, &gc::repo_root_args(&host.git, dir), gc::GIT_ENV_RO, opts).await?;
    root_from(host, &res, &res.stderr)
}

pub async fn list_worktrees(
    host: &GitHost,
    repo: &str,
    opts: &RunOpts,
) -> Result<Vec<gc::GitWorktreeEntry>, WorktreeError> {
    let res = run_git(
        host,
        &gc::worktree_list_args(&host.git, repo),
        WorktreeFailure::WorktreeAddFailed,
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    Ok(gc::parse_worktree_list(&res.stdout))
}

/// 主 worktree 的绝对路径。
///
/// `worktree list` 的第一条永远是主 worktree。派生一律以它为基准，于是：
/// 工作目录指到仓库子目录时不会把 worktree 建进仓库内部；从附属项目再派生时
/// （虽然接口层已禁止）也不会长成 repo-a-b。
pub fn main_worktree_of(entries: &[gc::GitWorktreeEntry], fallback: &str) -> String {
    entries.first().map_or_else(|| fallback.to_string(), |e| e.path.clone())
}

/// 分支 → 已检出它的 worktree 路径。git 不允许同一分支被检出两次
pub fn checked_out_map(entries: &[gc::GitWorktreeEntry]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for e in entries {
        if let Some(b) = &e.branch {
            m.insert(b.clone(), e.path.clone());
        }
    }
    m
}

/// Quick Open 用的文件清单。不是仓库或 git 报错时返回 `None`，由调用方改走目录遍历——
/// 没装 git 的项目仍然要能 ⌘P。
///
/// 非零退出码是正常返回（执行器铁律），这里不报错。
pub async fn list_repo_files(host: &GitHost, dir: &str, opts: &RunOpts) -> Result<Option<Vec<String>>, WorktreeError> {
    let res = probe_git(host, &gc::ls_files_index_args(&host.git, dir), gc::GIT_ENV_RO, opts).await?;
    if res.code != Some(0) {
        return Ok(None);
    }
    Ok(Some(gc::parse_ls_files(&res.stdout)))
}

/// `git remote` 的输出：一行一个远程名
fn remote_names(stdout: &str) -> Vec<String> {
    stdout.split('\n').map(js_trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// 源项目的仓库信息：能不能派生、有哪些分支、每条分支会建到哪里。
///
/// 这是**探测**，不是操作：环境事实（没装 git、不是仓库）如实写进 reason 返回，
/// 由调用方决定是渲染成一句说明（GET）还是一个 409（POST）。
pub async fn describe_repo(host: &GitHost, working_dir: &str, opts: &RunOpts) -> Result<RepoInfo, WorktreeError> {
    // 第一批：握手 + 仓库根 + worktree 列表。后续命令要以主 worktree（列表第一条）
    // 为基准（headBranch 问的是主 worktree 的 HEAD，不一定等于 workingDir 的），
    // 所以拆成两批——两个往返，仍远好于原先的 7+。
    let git = &host.git;
    let first = batch_git(
        host,
        &[gc::version_args(git), gc::repo_root_args(git, working_dir), gc::worktree_list_args(git, working_dir)],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [ver_res, root_res, wt_res] = &first.results[..] else { unreachable!("parse_git_batch 按条数返回") };
    ensure_version(host, ver_res, &first.stderr)?;
    let root = root_from(host, root_res, &first.stderr)?;
    if wt_res.code != Some(0) {
        return Err(failure(
            WorktreeFailure::WorktreeAddFailed,
            first_text(&[&first.stderr], || format!("退出码 {}", code_text(wt_res.code))),
        ));
    }
    let entries = gc::parse_worktree_list(&wt_res.stdout);
    let main = main_worktree_of(&entries, &root);
    let checked_out = checked_out_map(&entries);

    let second = batch_git(
        host,
        &[
            gc::remote_list_args(git, &main),
            gc::branch_list_args(git, &main),
            gc::head_branch_args(git, &main),
            gc::head_short_sha_args(git, &main),
        ],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [remotes_res, branch_res, head_res, sha_res] = &second.results[..] else {
        unreachable!("parse_git_batch 按条数返回")
    };
    let remotes = remote_names(&remotes_res.stdout);

    if branch_res.code != Some(0) {
        return Err(failure(
            WorktreeFailure::WorktreeAddFailed,
            first_text(&[&second.stderr, &branch_res.stdout], || format!("命令退出码 {}", code_text(branch_res.code))),
        ));
    }
    let parsed = gc::parse_branch_list(&branch_res.stdout, &remotes);

    let head_raw = if head_res.code == Some(0) { js_trim(&head_res.stdout) } else { "" };
    let head_branch = (!head_raw.is_empty() && head_raw != "HEAD").then(|| head_raw.to_string());
    let head_sha = if head_branch.is_none() && sha_res.code == Some(0) {
        Some(js_trim(&sha_res.stdout)).filter(|s| !s.is_empty()).map(str::to_string)
    } else {
        None
    };

    // 远程分支的本地名：剥掉最长匹配的 remote 前缀（origin/feat/x → feat/x）
    let local_name_of = |name: &str| -> String {
        let mut hits: Vec<&String> = remotes.iter().filter(|r| name.starts_with(&format!("{r}/"))).collect();
        // 稳定排序、按 UTF-16 长度降序（同 TS 的 sort((a, b) => b.length - a.length)）
        hits.sort_by_key(|r| std::cmp::Reverse(r.encode_utf16().count()));
        match hits.first() {
            Some(hit) => name[hit.len() + 1..].to_string(),
            None => name.to_string(),
        }
    };

    let draft: Vec<RepoBranch> = parsed
        .iter()
        .map(|b| {
            let local_name = b.remote.then(|| local_name_of(&b.name));
            let target = local_name.as_deref().unwrap_or(&b.name);
            RepoBranch {
                name: b.name.clone(),
                remote: b.remote,
                checked_out_at: checked_out.get(target).cloned(),
                head: b.head,
                suggested_dir: sibling_worktree_path(host.kind, &main, target),
                local_name,
                dir_occupied: false,
            }
        })
        .collect();

    // 占用探测批量做：一个下拉框不值几十个 SSH 往返
    let dirs: Vec<String> = draft.iter().map(|b| b.suggested_dir.clone()).collect();
    let occupied = if dirs.is_empty() { Vec::new() } else { paths_exist(host, &dirs).await? };
    let branches = draft
        .into_iter()
        .enumerate()
        .map(|(i, b)| RepoBranch { dir_occupied: occupied.get(i).copied().unwrap_or(false), ..b })
        .collect();

    Ok(RepoInfo { derivable: true, reason: None, detail: None, repo_dir: Some(main), head_branch, head_sha, branches })
}

// ---------------- Git 面板 ----------------

const GIT_FILE_CAP: usize = 200;

fn blank_snapshot(available: bool) -> GitSnapshot {
    GitSnapshot {
        available,
        reason: None,
        detail: None,
        repo_dir: None,
        work_dir: None,
        head_branch: None,
        head_sha: None,
        detached: None,
        upstream: None,
        ahead: None,
        behind: None,
        remotes: Vec::new(),
        files: Vec::new(),
        file_count: 0,
        worktrees: Vec::new(),
        commits: Vec::new(),
    }
}

pub fn unavailable_snapshot(reason: GitUnavailableReason, detail: Option<String>) -> GitSnapshot {
    GitSnapshot { reason: Some(reason), detail, ..blank_snapshot(false) }
}

pub fn unavailable_changes() -> GitChangeCounts {
    GitChangeCounts { available: false, added: 0, deleted: 0 }
}

pub fn unavailable_diff(reason: GitUnavailableReason, detail: Option<String>) -> GitFileDiff {
    GitFileDiff { available: false, reason: Some(reason), detail, diff: String::new(), truncated: false }
}

/// [`describe_git_diff`] 的目标
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitDiffTarget {
    /// 仓库根相对路径，前端从快照原样带回
    pub path: String,
    /// 重命名 / 复制前的路径
    pub orig_path: Option<String>,
    /// porcelain 的 ?? —— 未跟踪文件走 --no-index 伪 diff
    pub untracked: bool,
}

/// Git 面板里单个文件的 diff。
///
/// 与 describe_git 同一类探测：环境事实报 WorktreeError，由路由写成 200 +
/// available:false。基准取 HEAD（暂存 + 未暂存一起看）——面板列的是
/// `status --porcelain` 的合并视图，点开看到的也该是同一份合并答案。
pub async fn describe_git_diff(
    host: &GitHost,
    working_dir: &str,
    target: &GitDiffTarget,
    opts: &RunOpts,
) -> Result<GitFileDiff, WorktreeError> {
    let root = repo_root(host, working_dir, opts).await?;
    let ro = gc::GIT_ENV_RO;

    let res = if target.untracked {
        let res = probe_git(host, &gc::diff_untracked_args(&host.git, &root, &target.path), ro, opts).await?;
        // 退出码 1 = 有差异，这就是预期答案；但"文件访问不了"也是 1，只是 stdout 为空
        let failed = (res.code != Some(0) && res.code != Some(1))
            || (res.code == Some(1) && js_trim(&res.stdout).is_empty() && !js_trim(&res.stderr).is_empty());
        if failed {
            return Err(diff_error(&res));
        }
        res
    } else {
        let orig = target.orig_path.as_deref();
        let res = probe_git(host, &gc::diff_file_args(&host.git, &root, "HEAD", &target.path, orig), ro, opts).await?;
        if res.code == Some(0) {
            res
        } else {
            // 最常见的失败是空仓库没有 HEAD（bad revision）：退回与空树比
            let res =
                probe_git(host, &gc::diff_file_args(&host.git, &root, gc::EMPTY_TREE, &target.path, orig), ro, opts)
                    .await?;
            if res.code != Some(0) {
                return Err(diff_error(&res));
            }
            res
        }
    };

    let t = gc::truncate_diff(&res.stdout, None);
    Ok(GitFileDiff { available: true, reason: None, detail: None, diff: t.text, truncated: t.truncated })
}

fn diff_error(res: &ExecResult) -> WorktreeError {
    WorktreeError::new(
        WorktreeFailure::LinkFailed,
        "git diff 失败",
        Some(first_text(&[&res.stderr, &res.stdout], || format!("退出码 {}", code_text(res.code)))),
    )
}

fn change_counts(res: &impl ProbeLike) -> GitChangeCounts {
    if res.code() != Some(0) {
        return unavailable_changes();
    }
    let c = gc::count_status_changes(&gc::parse_status_entries(res.stdout()));
    GitChangeCounts { available: true, added: c.added, deleted: c.deleted }
}

/// 侧栏最后一层的 +N −M。只跑一条 status，SSH 上每棵检出都问一次也扛得住。
/// 读不到就 available:false，徽标不画——侧栏不能因为一台主机抖动整列空白。
pub async fn describe_git_changes(
    host: &GitHost,
    working_dir: &str,
    opts: &RunOpts,
) -> Result<GitChangeCounts, WorktreeError> {
    let res = probe_git(host, &gc::status_args(&host.git, working_dir), gc::GIT_ENV_RO, opts).await?;
    Ok(change_counts(&res))
}

/// 同一台宿主机上多棵检出的 +N −M，一次 exec 全拿。
/// 侧栏轮询按主机分组后调它：同主机 N 个项目从 N 条 SSH channel 压成 1 条。
/// 任何一条失败只影响自己那格（unavailable），不拖累同批的其他项目。
pub async fn describe_git_changes_many(
    host: &GitHost,
    dirs: &[String],
    opts: &RunOpts,
) -> Result<Vec<GitChangeCounts>, WorktreeError> {
    if dirs.is_empty() {
        return Ok(Vec::new());
    }
    let argvs: Vec<Vec<String>> = dirs.iter().map(|d| gc::status_args(&host.git, d)).collect();
    let out = batch_git(host, &argvs, gc::GIT_ENV_RO, opts).await?;
    Ok(out.results.iter().map(change_counts).collect())
}

/// 右侧 Git 面板的仓库快照。
///
/// 跟 describe_repo 一样是探测：环境事实（没装 git、不是仓库）报 WorktreeError，
/// 由路由写成 200 + available:false。
///
/// 全部命令批成**一次** exec：SSH 一条连接默认 MaxSessions=10，逐条串行虽然
/// 躲开了并发上限，但 100ms RTT 的链路上十来条就是 1.5s+，而这个快照每 5 秒
/// 轮询一次。批量后既没有并发压力也没有串行延迟。所有命令用 -C workingDir
/// 而不是先等仓库根：porcelain / rev-parse / log / worktree list 在仓库内
/// 任何目录下答案都一样（porcelain 路径本就相对仓库根），根从批里那条
/// rev-parse 拿。
pub async fn describe_git(host: &GitHost, working_dir: &str, opts: &RunOpts) -> Result<GitSnapshot, WorktreeError> {
    let git = &host.git;
    let out = batch_git(
        host,
        &[
            gc::version_args(git),
            gc::repo_root_args(git, working_dir),
            gc::head_branch_args(git, working_dir),
            gc::head_short_sha_args(git, working_dir),
            gc::upstream_args(git, working_dir),
            gc::ahead_args(git, working_dir),
            gc::behind_args(git, working_dir),
            gc::status_args(git, working_dir),
            gc::remote_verbose_args(git, working_dir),
            gc::log_args(git, working_dir, None),
            gc::worktree_list_args(git, working_dir),
        ],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [
        ver_res,
        root_res,
        head_branch_res,
        head_sha_res,
        upstream_res,
        ahead_res,
        behind_res,
        status_res,
        remotes_res,
        log_res,
        wt_res,
    ] = &out.results[..]
    else {
        unreachable!("parse_git_batch 按条数返回")
    };

    ensure_version(host, ver_res, &out.stderr)?;
    let root = root_from(host, root_res, &out.stderr)?;

    let head_raw = if head_branch_res.code == Some(0) { js_trim(&head_branch_res.stdout) } else { "" };
    let detached = head_raw.is_empty() || head_raw == "HEAD";
    let head_branch = (!detached).then(|| head_raw.to_string());
    let head_sha = if head_sha_res.code == Some(0) {
        Some(js_trim(&head_sha_res.stdout)).filter(|s| !s.is_empty()).map(str::to_string)
    } else {
        None
    };

    let upstream_raw = if upstream_res.code == Some(0) { js_trim(&upstream_res.stdout) } else { "" };
    let upstream = (!upstream_raw.is_empty() && upstream_raw != "HEAD").then(|| upstream_raw.to_string());

    let files = if status_res.code == Some(0) { gc::parse_status_entries(&status_res.stdout) } else { Vec::new() };
    let remotes = if remotes_res.code == Some(0) { gc::parse_remotes(&remotes_res.stdout) } else { Vec::new() };
    let commits = if log_res.code == Some(0) { gc::parse_log(&log_res.stdout) } else { Vec::new() };

    let worktrees: Vec<GitWorktreeRef> = if wt_res.code == Some(0) {
        gc::parse_worktree_list(&wt_res.stdout)
            .into_iter()
            .map(|e| GitWorktreeRef {
                path: normalize_sep(host.kind, &e.path),
                current: path_eq(host.kind, &e.path, &root),
                head: js_prefix(&e.head, 7),
                branch: e.branch,
            })
            .collect()
    } else {
        Vec::new()
    };

    let (ahead, behind) = if upstream.is_some() {
        (count_or_null(ahead_res.code, &ahead_res.stdout), count_or_null(behind_res.code, &behind_res.stdout))
    } else {
        (None, None)
    };
    let file_count = files.len() as u32;
    Ok(GitSnapshot {
        repo_dir: Some(root),
        work_dir: Some(normalize_sep(host.kind, working_dir)),
        head_branch,
        head_sha,
        detached: Some(detached),
        upstream,
        // 有快照就一定带这两项：没有 upstream 时是 null，不是缺省
        ahead: Some(ahead),
        behind: Some(behind),
        remotes,
        files: files.into_iter().take(GIT_FILE_CAP).collect(),
        file_count,
        worktrees,
        commits,
        ..blank_snapshot(true)
    })
}

// ---------------- History 面板 ----------------

/// git 输出给用户看的上限。一次 pull 的输出可以很长，面板上放不下也没人读
const SYNC_OUTPUT_CAP: usize = 4_000;

pub fn unavailable_log(reason: GitUnavailableReason, detail: Option<String>) -> GitLogPage {
    GitLogPage { available: false, reason: Some(reason), detail, commits: Vec::new(), has_more: false }
}

pub fn unavailable_refs(reason: GitUnavailableReason, detail: Option<String>) -> GitRefsInfo {
    GitRefsInfo {
        available: false,
        reason: Some(reason),
        detail,
        branches: Vec::new(),
        tags: Vec::new(),
        authors: Vec::new(),
        me: None,
    }
}

pub fn unavailable_commit(reason: GitUnavailableReason, detail: Option<String>) -> GitCommitDetail {
    GitCommitDetail {
        available: false,
        reason: Some(reason),
        detail,
        sha: String::new(),
        short: String::new(),
        author: String::new(),
        author_email: String::new(),
        authored_at: 0,
        committer: String::new(),
        committed_at: 0,
        parents: Vec::new(),
        refs: Vec::new(),
        message: String::new(),
        files: Vec::new(),
        file_count: 0,
    }
}

/// History 列表的一页。
///
/// 与 describe_git 一样批成一次 exec：远程名单是解析 %D 所必需的（要判断
/// origin/main 是远程分支还是一条叫 "origin/main" 的本地分支），单发一条就是
/// 白白多一个 SSH 往返，而翻页 / 改筛选都会重跑这个查询。
pub async fn describe_git_log(
    host: &GitHost,
    working_dir: &str,
    query: &gc::LogQuery,
    opts: &RunOpts,
) -> Result<GitLogPage, WorktreeError> {
    static NO_HISTORY: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("(?i-u)does not have any commits|bad default revision|unknown revision").unwrap());
    let git = &host.git;
    let limit = query.limit.unwrap_or(gc::LOG_PAGE);
    let out = batch_git(
        host,
        &[
            gc::version_args(git),
            gc::remote_list_args(git, working_dir),
            gc::log_page_args(git, working_dir, &gc::LogQuery { limit: Some(limit), ..query.clone() }),
        ],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [ver_res, remotes_res, log_res] = &out.results[..] else { unreachable!("parse_git_batch 按条数返回") };
    ensure_version(host, ver_res, &out.stderr)?;
    if log_res.code != Some(0) {
        // 空仓库（还没有任何提交）也走这里：log 在没有 HEAD 时退出码 128。
        // 那不是错误，是"还没有历史"，所以给一页空的而不是报 not-a-repo。
        if NO_HISTORY.is_match(&out.stderr) {
            return Ok(GitLogPage {
                available: true,
                reason: None,
                detail: None,
                commits: Vec::new(),
                has_more: false,
            });
        }
        return Err(failure(
            WorktreeFailure::NotARepo,
            first_text(&[&out.stderr], || format!("退出码 {}", code_text(log_res.code))),
        ));
    }

    let remotes = if remotes_res.code == Some(0) { remote_names(&remotes_res.stdout) } else { Vec::new() };
    let parsed = gc::parse_log_page(&log_res.stdout);
    let has_more = parsed.len() > limit;
    let commits = parsed
        .into_iter()
        .take(limit)
        .map(|c| GitLogCommit {
            refs: gc::parse_ref_labels(&c.decoration, &remotes),
            sha: c.sha,
            short: c.short,
            author: c.author,
            author_email: c.author_email,
            authored_at: c.authored_at,
            subject: c.subject,
            parents: c.parents,
        })
        .collect();
    Ok(GitLogPage { available: true, reason: None, detail: None, commits, has_more })
}

/// Branch / User 下拉的候选值。面板挂载时取一次，不参与轮询
pub async fn describe_git_refs(
    host: &GitHost,
    working_dir: &str,
    opts: &RunOpts,
) -> Result<GitRefsInfo, WorktreeError> {
    let git = &host.git;
    let out = batch_git(
        host,
        &[
            gc::version_args(git),
            gc::remote_list_args(git, working_dir),
            gc::branch_list_args(git, working_dir),
            gc::tag_list_args(git, working_dir),
            gc::log_authors_args(git, working_dir, None),
            gc::config_user_name_args(git, working_dir),
        ],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [ver_res, remotes_res, branch_res, tag_res, author_res, me_res] = &out.results[..] else {
        unreachable!("parse_git_batch 按条数返回")
    };
    ensure_version(host, ver_res, &out.stderr)?;

    let remotes = if remotes_res.code == Some(0) { remote_names(&remotes_res.stdout) } else { Vec::new() };
    let branches: Vec<GitBranchRef> =
        if branch_res.code == Some(0) { gc::parse_branch_list(&branch_res.stdout, &remotes) } else { Vec::new() };
    let tags = if tag_res.code == Some(0) { gc::parse_tag_list(&tag_res.stdout, None) } else { Vec::new() };
    let authors = if author_res.code == Some(0) { gc::rank_authors(&author_res.stdout) } else { Vec::new() };
    // 没配 user.name 时 git config 退出码 1，那是正常状态不是错误
    let me = if me_res.code == Some(0) {
        Some(js_trim(&me_res.stdout)).filter(|s| !s.is_empty()).map(str::to_string)
    } else {
        None
    };
    Ok(GitRefsInfo { available: true, reason: None, detail: None, branches, tags, authors, me })
}

pub fn unavailable_working(reason: GitUnavailableReason, detail: Option<String>) -> GitWorkingChanges {
    GitWorkingChanges {
        available: false,
        reason: Some(reason),
        detail,
        repo_name: None,
        files: Vec::new(),
        file_count: 0,
        head_message: None,
        conflict: None,
    }
}

/// 「修改」面板：工作区里全部未提交的改动，带增删行数。
///
/// 两轮 exec，第二轮只在有未跟踪文件时才发：
/// 1. version + 仓库根 + status + numstat(HEAD)；
/// 2. 每个未跟踪文件一条 `diff --no-index --numstat`。
///
/// 行数与状态**按路径配对**而不是按下标：status 会列出未跟踪文件而 numstat
/// 不会，两边条数本来就对不上。配不上的记 null（"未知"），不记 0——
/// "改了但不知道多少行"和"一行没改"在界面上是两回事。
pub async fn describe_working_changes(
    host: &GitHost,
    working_dir: &str,
    opts: &RunOpts,
) -> Result<GitWorkingChanges, WorktreeError> {
    let git = &host.git;
    let out = batch_git(
        host,
        &[
            gc::version_args(git),
            gc::repo_root_args(git, working_dir),
            gc::status_all_args(git, working_dir),
            gc::working_numstat_args(git, working_dir, "HEAD"),
            gc::head_message_args(git, working_dir),
            gc::rev_exists_args(git, working_dir, "REBASE_HEAD"),
            gc::rev_exists_args(git, working_dir, "MERGE_HEAD"),
            gc::rev_exists_args(git, working_dir, "CHERRY_PICK_HEAD"),
            gc::rev_exists_args(git, working_dir, "REVERT_HEAD"),
        ],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [ver_res, root_res, status_res, numstat_res, head_msg_res, rebase_res, merge_res, cherry_res, revert_res] =
        &out.results[..]
    else {
        unreachable!("parse_git_batch 按条数返回")
    };
    ensure_version(host, ver_res, &out.stderr)?;
    let root = root_from(host, root_res, &out.stderr)?;

    if status_res.code != Some(0) {
        return Err(failure(
            WorktreeFailure::NotARepo,
            first_text(&[&out.stderr], || format!("退出码 {}", code_text(status_res.code))),
        ));
    }
    let entries = gc::parse_status_entries(&status_res.stdout);

    // 空仓库没有 HEAD，numstat 会失败；退回与空树比，语义不变
    let mut numstat =
        if numstat_res.code == Some(0) { gc::parse_numstat_map(&numstat_res.stdout) } else { Default::default() };
    if numstat_res.code != Some(0) {
        let retry =
            probe_git(host, &gc::working_numstat_args(git, &root, gc::EMPTY_TREE), gc::GIT_ENV_RO, opts).await?;
        if retry.code == Some(0) {
            numstat = gc::parse_numstat_map(&retry.stdout);
        }
    }

    // 未跟踪文件不在 diff 里，逐个与空文件比，批成一次 exec
    let untracked: Vec<&GitFileChange> =
        entries.iter().filter(|e| e.index == "?").take(gc::UNTRACKED_NUMSTAT_CAP).collect();
    if !untracked.is_empty() {
        let argvs: Vec<Vec<String>> =
            untracked.iter().map(|e| gc::untracked_numstat_args(git, &root, &e.path)).collect();
        let ures = batch_git(host, &argvs, gc::GIT_ENV_RO, opts).await?;
        for (res, e) in ures.results.iter().zip(&untracked) {
            // --no-index 有差异时退出码 1，那是答案不是错误
            if res.code != Some(0) && res.code != Some(1) {
                continue;
            }
            // 输出里的路径是 git 自己拼的，未必与入参逐字相同（比如 ./ 前缀），
            // 一条命令只可能有一个结果，直接取它
            if let Some(first) = gc::parse_numstat_map(&res.stdout).values().next().copied() {
                numstat.insert(e.path.clone(), first);
            }
        }
    }

    let file_count = entries.len() as u32;
    let files: Vec<GitWorkingFile> = entries
        .into_iter()
        .take(gc::WORKING_FILE_CAP)
        .map(|e| {
            let n = numstat.get(&e.path).copied().unwrap_or_default();
            GitWorkingFile { change: e, added: n.added, deleted: n.deleted }
        })
        .collect();

    let head_message = if head_msg_res.code == Some(0) {
        Some(strip_one_newline(&head_msg_res.stdout)).filter(|s| !s.is_empty()).map(str::to_string)
    } else {
        None
    };

    let conflict_kind = if rebase_res.code == Some(0) {
        Some(GitConflictKind::Rebase)
    } else if merge_res.code == Some(0) {
        Some(GitConflictKind::Merge)
    } else if cherry_res.code == Some(0) {
        Some(GitConflictKind::CherryPick)
    } else if revert_res.code == Some(0) {
        Some(GitConflictKind::Revert)
    } else {
        None
    };

    Ok(GitWorkingChanges {
        available: true,
        reason: None,
        detail: None,
        repo_name: Some(base_name(host.kind, &root)),
        files,
        file_count,
        head_message,
        conflict: conflict_kind.map(|kind| GitConflict { kind }),
    })
}

/// 路径最后一段。不用 std::path——后端可能跑在 Windows 上为 Linux 远端算路径。
///
/// 照搬 TS：归一化之后按 `/` 切。Windows 宿主机上 normalize_sep 把分隔符换成了 `\`，
/// 按 `/` 切不开，结果是整条路径（TS 原版的行为，见 repo.ts 的 baseName；只影响
/// 「修改」面板目录树的根节点名，不碰任何判据）。
fn base_name(kind: HostKind, p: &str) -> String {
    let n = normalize_sep(kind, p);
    n.split('/').rfind(|s| !s.is_empty()).map_or_else(|| p.to_string(), str::to_string)
}

/// 选中提交的详情。元数据 / 提交信息 / 改动文件三条命令批成一次 exec——
/// 用户在列表里上下点，每一下都是一轮往返，SSH 上尤其明显。
pub async fn describe_commit(
    host: &GitHost,
    working_dir: &str,
    sha: &str,
    opts: &RunOpts,
) -> Result<GitCommitDetail, WorktreeError> {
    let git = &host.git;
    let out = batch_git(
        host,
        &[
            gc::version_args(git),
            gc::remote_list_args(git, working_dir),
            gc::commit_meta_args(git, working_dir, sha),
            gc::commit_message_args(git, working_dir, sha),
            gc::commit_files_args(git, working_dir, sha),
        ],
        gc::GIT_ENV_RO,
        opts,
    )
    .await?;
    let [ver_res, remotes_res, meta_res, msg_res, files_res] = &out.results[..] else {
        unreachable!("parse_git_batch 按条数返回")
    };
    ensure_version(host, ver_res, &out.stderr)?;

    let meta = if meta_res.code == Some(0) { gc::parse_commit_meta(&meta_res.stdout) } else { None };
    let Some(meta) = meta else {
        // 最常见的是提交在这一侧不存在（面板还停在旧数据上，而分支已经被强推重写）
        return Err(WorktreeError::new(
            WorktreeFailure::NotARepo,
            "读不到这条提交",
            Some(first_text(&[&out.stderr], || format!("退出码 {}", code_text(meta_res.code)))),
        ));
    };

    let remotes = if remotes_res.code == Some(0) { remote_names(&remotes_res.stdout) } else { Vec::new() };
    let files = if files_res.code == Some(0) { gc::parse_commit_files(&files_res.stdout) } else { Vec::new() };
    let file_count = files.len() as u32;
    Ok(GitCommitDetail {
        available: true,
        reason: None,
        detail: None,
        refs: gc::parse_ref_labels(&meta.decoration, &remotes),
        sha: meta.sha,
        short: meta.short,
        author: meta.author,
        author_email: meta.author_email,
        authored_at: meta.authored_at,
        committer: meta.committer,
        committed_at: meta.committed_at,
        parents: meta.parents,
        // %B 结尾恒带一个换行，去掉它免得详情区多出一行空白
        message: if msg_res.code == Some(0) { strip_one_newline(&msg_res.stdout).to_string() } else { String::new() },
        files: files.into_iter().take(gc::COMMIT_FILE_CAP).collect(),
        file_count,
    })
}

/// 某条提交里单个文件的 diff。与工作区那份共用 GitFileDiff 形状
pub async fn describe_commit_diff(
    host: &GitHost,
    working_dir: &str,
    sha: &str,
    path: &str,
    orig_path: Option<&str>,
    opts: &RunOpts,
) -> Result<GitFileDiff, WorktreeError> {
    let res =
        probe_git(host, &gc::commit_file_diff_args(&host.git, working_dir, sha, path, orig_path), gc::GIT_ENV_RO, opts)
            .await?;
    if res.code != Some(0) {
        return Err(diff_error(&res));
    }
    let t = gc::truncate_diff(&res.stdout, None);
    Ok(GitFileDiff { available: true, reason: None, detail: None, diff: t.text, truncated: t.truncated })
}

/// [`sync_git`] 的动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncAction {
    Pull,
    Push,
}

/// pull / push。
///
/// **失败不报错**：凭据不对、非快进、远端拒绝都是仓库的正常状态，把 git 说的话
/// 原样回给前端。GIT_ENV（不是 RO）——这两条是写操作，本来就要拿锁；用
/// GIT_OPTIONAL_LOCKS=0 反而会让 pull 在需要更新 index 时行为古怪。
pub async fn sync_git(
    host: &GitHost,
    working_dir: &str,
    action: SyncAction,
    opts: &RunOpts,
) -> Result<GitSyncResult, WorktreeError> {
    let (argv, label) = match action {
        SyncAction::Pull => (gc::pull_args(&host.git, working_dir), "pull"),
        SyncAction::Push => (gc::push_args(&host.git, working_dir, false), "push"),
    };
    let res = probe_git(host, &argv, gc::GIT_ENV, &opts.with_default_timeout(TIMEOUT_SYNC)).await?;
    Ok(sync_result(&res, label))
}

/// HEAD 的完整 sha（小写）；读不到时是空串
async fn head_sha(host: &GitHost, working_dir: &str, opts: &RunOpts) -> Result<String, WorktreeError> {
    let head = probe_git(host, &gc::head_sha_args(&host.git, working_dir), gc::GIT_ENV_RO, opts).await?;
    Ok(if head.code == Some(0) { js_trim(&head.stdout).to_lowercase() } else { String::new() })
}

/// sha 是不是 HEAD（两边都可能是短 sha，按前缀互认）。
///
/// **与 TS 的一处刻意差别**：TS 在 HEAD 读不到（`full` 为空串）时 `target.startsWith("")`
/// 恒真，于是把任意提交都当成 HEAD——drop 会落到 `reset --hard HEAD~1`，丢掉的是 HEAD
/// 那一笔外加工作区里全部未提交的改动，而不是用户点的那一条。读不到 HEAD 一律当
/// "不是 HEAD"：drop 走 rebase --onto（脏工作区 git 自己会拒绝），squash / reword 直接
/// 拒绝，都不会悄悄毁数据。
fn is_head(full: &str, sha: &str) -> bool {
    let target = sha.to_lowercase();
    !full.is_empty() && (full == target || full.starts_with(&target) || target.starts_with(full))
}

/// 历史面板的写操作。与 sync_git 同一条规矩：失败不报错，git 的原话进 detail。
///
/// checkout / cherry-pick / revert 都会动工作区；fetch 只更新远程跟踪。
/// 冲突不 abort——面板没有 merge tool，终端就在旁边。
pub async fn run_git_op(
    host: &GitHost,
    working_dir: &str,
    input: &GitOpInput,
    opts: &RunOpts,
) -> Result<GitSyncResult, WorktreeError> {
    let git = &host.git;
    let sync_opts = opts.with_default_timeout(TIMEOUT_SYNC);
    let run = async |argv: Vec<String>, label: &str| -> Result<GitSyncResult, WorktreeError> {
        let res = probe_git(host, &argv, gc::GIT_ENV, &sync_opts).await?;
        Ok(sync_result(&res, label))
    };
    let refuse = |detail: &str| Ok(GitSyncResult { ok: false, reason: None, detail: detail.to_string() });

    match input {
        GitOpInput::Fetch => run(gc::fetch_args(git, working_dir), "fetch").await,
        GitOpInput::Checkout { rev, detach } => {
            let argv = if *detach == Some(true) {
                gc::checkout_detach_args(git, working_dir, rev)
            } else {
                gc::checkout_branch_args(git, working_dir, rev)
            };
            run(argv, "checkout").await
        }
        GitOpInput::CheckoutBranch { branch, create_tracking } => {
            if *create_tracking != Some(true) {
                return run(gc::checkout_branch_args(git, working_dir, branch), "checkout").await;
            }
            let remotes_res = probe_git(host, &gc::remote_list_args(git, working_dir), gc::GIT_ENV_RO, opts).await?;
            let remotes = if remotes_res.code == Some(0) { remote_names(&remotes_res.stdout) } else { Vec::new() };
            let Some(local) = gc::tracking_local_name(branch, &remotes) else {
                return refuse(&format!("无法从 {branch} 解析本地分支名"));
            };
            let exists =
                probe_git(host, &gc::branch_exists_args(git, working_dir, &local), gc::GIT_ENV_RO, opts).await?;
            // 本地已有同名分支：切过去，不要再 -b 一次（git 会拒绝 already exists）
            if exists.code == Some(0) {
                return run(gc::checkout_branch_args(git, working_dir, &local), "checkout").await;
            }
            run(gc::checkout_track_args(git, working_dir, &local, branch), "checkout").await
        }
        GitOpInput::CherryPick { sha } => run(gc::cherry_pick_args(git, working_dir, sha), "cherry-pick").await,
        GitOpInput::Revert { sha } => run(gc::revert_args(git, working_dir, sha), "revert").await,
        GitOpInput::BranchCreate { name, start_point, checkout } => {
            if *checkout == Some(true) {
                run(gc::checkout_new_branch_args(git, working_dir, name, start_point), "checkout").await
            } else {
                run(gc::branch_create_args(git, working_dir, name, start_point), "branch").await
            }
        }
        GitOpInput::Reset { rev, mode } => run(gc::reset_args(git, working_dir, rev, *mode), "reset").await,
        GitOpInput::Merge { rev } => run(gc::merge_args(git, working_dir, rev), "merge").await,
        GitOpInput::Rebase { rev } => run(gc::rebase_args(git, working_dir, rev), "rebase").await,
        GitOpInput::Restore { paths, untracked } => {
            if !paths.is_empty() {
                let restored = run(gc::restore_args(git, working_dir, paths), "restore").await?;
                if !restored.ok {
                    return Ok(restored);
                }
            }
            if let Some(untracked) = untracked.as_ref().filter(|u| !u.is_empty()) {
                let cleaned = run(gc::clean_paths_args(git, working_dir, untracked), "clean").await?;
                if !cleaned.ok {
                    return Ok(cleaned);
                }
            }
            Ok(GitSyncResult { ok: true, reason: None, detail: "已丢弃所选改动".into() })
        }
        GitOpInput::Push { force_with_lease } => {
            run(gc::push_args(git, working_dir, *force_with_lease == Some(true)), "push").await
        }
        GitOpInput::Drop { sha } => {
            let full = head_sha(host, working_dir, opts).await?;
            let argv = if is_head(&full, sha) {
                gc::drop_head_args(git, working_dir)
            } else {
                gc::drop_commit_args(git, working_dir, sha)
            };
            run(argv, "rebase").await
        }
        GitOpInput::Squash { sha } => {
            let full = head_sha(host, working_dir, opts).await?;
            if !is_head(&full, sha) {
                return refuse("只能把最新一次提交压进上一条。更早的提交请在终端里 rebase -i。");
            }
            let soft = run(gc::squash_head_soft_args(git, working_dir), "reset").await?;
            if !soft.ok {
                return Ok(soft);
            }
            let commit_opts = gc::CommitOpts { amend: true, no_edit: true };
            run(gc::commit_args::<String>(git, working_dir, "", &[], commit_opts), "commit").await
        }
        GitOpInput::Reword { sha, message } => {
            let full = head_sha(host, working_dir, opts).await?;
            if !is_head(&full, sha) {
                return refuse("只能改最新一次提交的说明。更早的提交请在终端里 rebase -i。");
            }
            let commit_opts = gc::CommitOpts { amend: true, no_edit: false };
            run(gc::commit_args::<String>(git, working_dir, message, &[], commit_opts), "commit").await
        }
        GitOpInput::Continue | GitOpInput::Abort => {
            let Some(kind) = detect_conflict(host, working_dir, opts).await? else {
                return refuse("现在没有进行中的合并或变基");
            };
            if matches!(input, GitOpInput::Continue) {
                run(gc::continue_conflict_args(git, working_dir, kind), "continue").await
            } else {
                run(gc::abort_conflict_args(git, working_dir, kind), "abort").await
            }
        }
        GitOpInput::Take { side, paths } => {
            let checked = run(gc::checkout_conflict_side_args(git, working_dir, *side, paths), "checkout").await?;
            if !checked.ok {
                return Ok(checked);
            }
            run(gc::add_paths_args(git, working_dir, paths), "add").await
        }
    }
}

async fn detect_conflict(
    host: &GitHost,
    working_dir: &str,
    opts: &RunOpts,
) -> Result<Option<GitConflictKind>, WorktreeError> {
    const ORDER: [(GitConflictKind, &str); 4] = [
        (GitConflictKind::Rebase, "REBASE_HEAD"),
        (GitConflictKind::Merge, "MERGE_HEAD"),
        (GitConflictKind::CherryPick, "CHERRY_PICK_HEAD"),
        (GitConflictKind::Revert, "REVERT_HEAD"),
    ];
    let argvs: Vec<Vec<String>> =
        ORDER.iter().map(|(_, rev)| gc::rev_exists_args(&host.git, working_dir, rev)).collect();
    let out = batch_git(host, &argvs, gc::GIT_ENV_RO, opts).await?;
    Ok(ORDER.iter().zip(&out.results).find(|(_, r)| r.code == Some(0)).map(|((k, _), _)| *k))
}

/// git 把进度写 stderr、结果写 stdout；两边都空只剩退出码时至少说清是哪一步
pub(crate) fn sync_result(res: &ExecResult, label: &str) -> GitSyncResult {
    let (a, b) = if res.code == Some(0) { (&res.stdout, &res.stderr) } else { (&res.stderr, &res.stdout) };
    // `(a || b).trim()`：先挑非空的那一边，再 trim
    let text = js_trim(if a.is_empty() { b } else { a });
    // 成功且 git 一声不吭（fetch 已经是最新）也要有回声，否则按钮像没反应。
    // 失败才把退出码写进 detail——成功时「退出码 0」看着像报错。
    let detail = if !text.is_empty() {
        text.to_string()
    } else if res.code == Some(0) {
        format!("git {label} 完成")
    } else {
        format!("git {label} 退出码 {}", code_text(res.code))
    };
    GitSyncResult { ok: res.code == Some(0), reason: None, detail: js_prefix(&detail, SYNC_OUTPUT_CAP) }
}

/// 提交。与 sync_git 一样，**失败不报错**——没配 user.name、pre-commit 钩子拒绝、
/// 没有可提交的改动，都是仓库的正常状态，把 git 的原话回给前端。
///
/// 两条路径见 GitCommitInput 的注释。未跟踪文件必须先 add：实测
/// `git commit -- newfile` 直接报 "pathspec did not match any file(s) known to git"。
pub async fn commit_working(
    host: &GitHost,
    working_dir: &str,
    input: &GitCommitInput,
    opts: &RunOpts,
) -> Result<GitSyncResult, WorktreeError> {
    let git = &host.git;
    let message = js_trim(&input.message);
    let amend = input.amend == Some(true);
    let refuse = |detail: String| GitSyncResult { ok: false, reason: None, detail };
    if !amend && message.is_empty() {
        return Ok(refuse("提交信息不能为空".into()));
    }

    let sync_opts = opts.with_default_timeout(TIMEOUT_SYNC);
    let run = async |argv: Vec<String>| probe_git(host, &argv, gc::GIT_ENV, &sync_opts).await;
    let fail = |res: &ExecResult, fallback: String| {
        refuse(js_prefix(&first_text(&[&res.stderr, &res.stdout], || fallback), SYNC_OUTPUT_CAP))
    };
    let commit_opts = gc::CommitOpts { amend, no_edit: amend && message.is_empty() };

    let commit_argv = if input.all {
        let add = run(gc::add_all_args(git, working_dir)).await?;
        if add.code != Some(0) {
            return Ok(fail(&add, format!("git add 退出码 {}", code_text(add.code))));
        }
        gc::commit_args::<String>(git, working_dir, message, &[], commit_opts)
    } else {
        let paths: Vec<String> = input.paths.iter().flatten().filter(|p| !p.is_empty()).cloned().collect();
        if paths.is_empty() {
            return Ok(refuse("没有选中任何文件".into()));
        }
        if gc::pathspec_too_long(&paths) {
            return Ok(refuse("选中的文件太多，路径拼不进一条命令行。改用全选提交，或者分两次提交。".into()));
        }
        // **只** add 未跟踪的那些。已跟踪文件（含删除）pathspec commit 会直接取
        // 工作区，不需要进 index；对它们多跑一次 add 看着无害，但只要 commit 被
        // pre-commit 钩子挡下来，用户原本没暂存的改动就凭空变成已暂存了——
        // 那种"我什么都没做它自己变了"的意外最难排查。多一次 status 换掉它。
        let st = probe_git(host, &gc::status_all_args(git, working_dir), gc::GIT_ENV_RO, opts).await?;
        if st.code == Some(0) {
            let untracked: HashSet<String> =
                gc::parse_status_entries(&st.stdout).into_iter().filter(|e| e.index == "?").map(|e| e.path).collect();
            let to_add: Vec<&String> = paths.iter().filter(|p| untracked.contains(*p)).collect();
            if !to_add.is_empty() {
                let add = run(gc::add_paths_args(git, working_dir, &to_add)).await?;
                if add.code != Some(0) {
                    return Ok(fail(&add, format!("git add 退出码 {}", code_text(add.code))));
                }
            }
        }
        gc::commit_args(git, working_dir, message, &paths, commit_opts)
    };

    let res = run(commit_argv).await?;
    if res.code != Some(0) {
        return Ok(fail(&res, format!("git commit 退出码 {}", code_text(res.code))));
    }
    let committed = js_prefix(&first_text(&[&res.stdout, &res.stderr], String::new), SYNC_OUTPUT_CAP);
    if input.push != Some(true) {
        return Ok(GitSyncResult { ok: true, reason: None, detail: committed });
    }

    let pushed = sync_git(host, working_dir, SyncAction::Push, opts).await?;
    if !pushed.ok {
        return Ok(refuse(js_prefix(&format!("已提交。推送失败：\n{}", pushed.detail), SYNC_OUTPUT_CAP)));
    }
    let joined = [committed.as_str(), pushed.detail.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Ok(GitSyncResult { ok: true, reason: None, detail: js_prefix(&joined, SYNC_OUTPUT_CAP) })
}

/// 附属项目的工作区状态：删除前的预检。
///
/// 脏与否**只看退出码**，不解析文本——Windows 远端上原生命令的输出按控制台代码页
/// 解码，中文路径会变乱码。文本解析只用来给出样例清单，读不到就退化成计数，
/// 不影响任何判断。
pub async fn worktree_status(host: &GitHost, dir: &str, opts: &RunOpts) -> Result<WorktreeStatus, WorktreeError> {
    let present = path_exists(host, dir, opts).await?;
    if !present {
        return Ok(WorktreeStatus {
            present: false,
            dirty_count: 0,
            dirty_sample: Vec::new(),
            ignored_count: 0,
            ahead: None,
            error: None,
        });
    }
    match worktree_status_inner(host, dir, opts).await {
        Ok(s) => Ok(s),
        // 读不到状态不阻断删除，只是让确认框改口说"无法确认里面有没有未保存的东西"
        Err(err) => Ok(WorktreeStatus {
            present: true,
            dirty_count: 0,
            dirty_sample: Vec::new(),
            ignored_count: 0,
            ahead: None,
            error: Some(err.detail.unwrap_or(err.message)),
        }),
    }
}

async fn worktree_status_inner(host: &GitHost, dir: &str, opts: &RunOpts) -> Result<WorktreeStatus, WorktreeError> {
    let git = &host.git;
    let ro = gc::GIT_ENV_RO;
    let (dirty, cached, others) =
        (gc::diff_dirty_args(git, dir), gc::diff_cached_dirty_args(git, dir), gc::untracked_args(git, dir));
    // 三条并发（TS 的 Promise.all）：互不依赖，SSH 上是三条并行的 channel
    let (unstaged, staged, untracked) = futures::try_join!(
        probe_git(host, &dirty, ro, opts),
        probe_git(host, &cached, ro, opts),
        probe_git(host, &others, ro, opts),
    )?;
    let untracked_lines = untracked.stdout.split('\n').filter(|l| !js_trim(l).is_empty()).count();
    let dirty_by_code = unstaged.code == Some(1) || staged.code == Some(1) || untracked_lines > 0;

    // 展示用清单：拿不到就只报"有改动"，不阻断
    let mut dirty_count = if dirty_by_code { untracked_lines.max(1) } else { 0 };
    let mut dirty_sample = Vec::new();
    let st = probe_git(host, &gc::status_args(git, dir), ro, opts).await?;
    if st.code == Some(0) {
        let parsed = gc::parse_status(&st.stdout, None);
        dirty_count = parsed.count;
        dirty_sample = parsed.files;
    }

    let ign = probe_git(host, &gc::status_ignored_args(git, dir), ro, opts).await?;
    let ignored_count = if ign.code == Some(0) { gc::count_ignored(&ign.stdout) } else { 0 };

    // 没有 upstream 时 rev-list 退出码 128，这不是错误，只是"无从比较"
    let ahead_res = probe_git(host, &gc::ahead_args(git, dir), ro, opts).await?;
    let ahead = count_or_null(ahead_res.code, &ahead_res.stdout);

    Ok(WorktreeStatus {
        present: true,
        dirty_count: dirty_count as u32,
        dirty_sample,
        ignored_count: ignored_count as u32,
        ahead,
        error: None,
    })
}

/// [`add_worktree`] 的参数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddWorktreeOptions {
    pub mode: WorktreeMode,
    /// 目标分支的本地名
    pub branch: String,
    /// mode=new-branch 的基点，缺省（含空串）HEAD
    pub start_point: Option<String>,
    /// 目标目录（调用方已做过静态否决与占用探测）
    pub dir: String,
}

/// 建一棵新的 worktree，返回 **git 自己报的**绝对路径。
///
/// 返回值必须取自 `worktree list --porcelain` 而不是我们算出来的字符串：删除护栏靠
/// 路径比对，落库的那一份就得是宿主机的权威说法（Windows 上 git 吐正斜杠、大小写
/// 也可能与用户输入不同）。
///
/// 断链认领：worktree add 完全可能在宿主机上**已经成功、只是响应丢了**（SSH 抖动）。
/// 那样磁盘上多一个目录而 DB 里什么都没有——不可见的孤儿，而删除护栏全靠 DB 记录，
/// 我们再也无法安全清理它。所以链路故障时 best-effort 回查一次：真落地了就认领。
/// 这比引入 creating/deleting 状态机 + 启动对账便宜得多，取向也一致：
/// 出问题时把事实原样告诉用户，而不是替他记账。
pub async fn add_worktree(host: &GitHost, main: &str, o: &AddWorktreeOptions) -> Result<String, WorktreeError> {
    let argv = match o.mode {
        WorktreeMode::NewBranch => {
            let start = o.start_point.as_deref().filter(|s| !s.is_empty()).unwrap_or("HEAD");
            gc::worktree_add_new_args(&host.git, main, &o.dir, &o.branch, start)
        }
        WorktreeMode::ExistingBranch => gc::worktree_add_existing_args(&host.git, main, &o.dir, &o.branch),
    };

    let res = match probe_git(host, &argv, gc::GIT_ENV, &RunOpts::timeout(TIMEOUT_ADD)).await {
        Ok(res) => res,
        Err(err) => {
            if let Ok(Some(claimed)) = claim_worktree(host, main, o).await {
                return Ok(claimed);
            }
            let detail =
                format!("{}（如果宿主机上已经建出了 {}，请手动检查）", err.detail.as_deref().unwrap_or(""), o.dir);
            return Err(WorktreeError::new(err.reason, err.message, Some(js_trim(&detail).to_string())));
        }
    };

    if res.code != Some(0) {
        let reason = classify_add_error(&res.stderr);
        return Err(failure(
            reason,
            first_text(&[&res.stderr, &res.stdout], || format!("git worktree add 退出码 {}", code_text(res.code))),
        ));
    }

    match claim_worktree(host, main, o).await? {
        Some(claimed) => Ok(claimed),
        // add 报成功却查不到，只可能是路径归一化出了岔子。宁可报错也不要落一条
        // 指向未知位置的记录——那条记录将来会被拿去当删除目标。
        None => Err(failure(WorktreeFailure::WorktreeAddFailed, format!("已创建但在 worktree 列表中找不到 {}", o.dir))),
    }
}

/// 目标目录是否已经是本仓库的一棵 worktree 且分支相符；是则返回 git 报的路径
async fn claim_worktree(host: &GitHost, main: &str, o: &AddWorktreeOptions) -> Result<Option<String>, WorktreeError> {
    let entries = list_worktrees(host, main, &no_opts()).await?;
    let Some(hit) = entries.into_iter().find(|e| path_eq(host.kind, &e.path, &o.dir)) else {
        return Ok(None);
    };
    Ok((hit.branch.as_deref() == Some(o.branch.as_str())).then_some(hit.path))
}

/// `git worktree add` 的失败分类。
///
/// 预检与 add 之间用户可能刚在别的终端检出了同一分支，所以这些判据是 TOCTOU 兜底，
/// 不是"预检的替代"。判据全是英文原文——GIT_ENV 里锁了 LC_ALL=C 就是为了这一刻。
pub fn classify_add_error(stderr: &str) -> WorktreeFailure {
    // JS 的 `.` 不匹配四种行终止符
    static BRANCH_EXISTS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("a branch named [^\n\r\u{2028}\u{2029}]* already exists").unwrap());
    let s = stderr.to_lowercase();
    // 两种措辞都要认：worktree add 说的是 "is already used by worktree at"
    // （git 2.48 实测），而 "is already checked out at" 是 checkout/switch 的说法，
    // 老版本的 worktree add 也用过。只认一条会把 branch-in-use 降级成
    // 一句笼统的 worktree-add-failed，用户就不知道该去哪个目录看了。
    if s.contains("is already used by worktree at") || s.contains("is already checked out at") {
        return WorktreeFailure::BranchInUse;
    }
    if BRANCH_EXISTS.is_match(&s) {
        return WorktreeFailure::BranchExists;
    }
    if s.contains("invalid reference") || s.contains("not a valid object name") {
        return WorktreeFailure::BranchUnknown;
    }
    if s.contains("already exists") {
        return WorktreeFailure::PathOccupied;
    }
    WorktreeFailure::WorktreeAddFailed
}

/// 两个路径是否指同一处（归一化后比较，Windows 大小写不敏感）。TS 吃 host，这里只要 kind
pub fn path_eq(kind: HostKind, a: &str, b: &str) -> bool {
    canon_key(kind, a) == canon_key(kind, b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::exec::{Exec, LocalBoxFuture, LocalExec};
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    // ---------------- 纯函数 ----------------

    #[test]
    fn classify_add_error_reads_git_wording() {
        let c = classify_add_error;
        assert_eq!(
            c("Preparing worktree\nfatal: 'feat' is already used by worktree at '/a'"),
            WorktreeFailure::BranchInUse
        );
        assert_eq!(c("fatal: 'feat' is already checked out at '/a'"), WorktreeFailure::BranchInUse);
        assert_eq!(c("fatal: a branch named 'feat' already exists"), WorktreeFailure::BranchExists);
        assert_eq!(c("fatal: invalid reference: origin/nope"), WorktreeFailure::BranchUnknown);
        assert_eq!(c("fatal: Not a valid object name: 'x'"), WorktreeFailure::BranchUnknown);
        assert_eq!(c("fatal: '/a/b' already exists"), WorktreeFailure::PathOccupied);
        assert_eq!(c("fatal: something else"), WorktreeFailure::WorktreeAddFailed);
        // "a branch named" 与 "already exists" 不在同一行：不是 branch-exists，落到 path-occupied
        assert_eq!(c("a branch named x\nalready exists"), WorktreeFailure::PathOccupied);
    }

    #[test]
    fn sync_result_echoes_git_or_names_the_step() {
        let r = |code, out: &str, err: &str| ExecResult { code, stdout: out.into(), stderr: err.into() };
        assert_eq!(sync_result(&r(Some(0), "Already up to date.\n", "noise"), "pull").detail, "Already up to date.");
        // 成功时 stdout 为空就看 stderr（push 的结果写在 stderr）
        assert_eq!(sync_result(&r(Some(0), "", "To x\n  a..b main\n"), "push").detail, "To x\n  a..b main");
        assert_eq!(sync_result(&r(Some(0), "", ""), "fetch").detail, "git fetch 完成");
        let fail = sync_result(&r(Some(1), "out", "fatal: no"), "push");
        assert!(!fail.ok);
        assert_eq!(fail.detail, "fatal: no");
        assert_eq!(sync_result(&r(None, "", ""), "pull").detail, "git pull 退出码 null");
        // 先挑非空的一边再 trim：stdout 只有空白时不会退到 stderr
        assert_eq!(sync_result(&r(Some(0), "  \n", "err"), "fetch").detail, "git fetch 完成");
        let long = "长".repeat(5000);
        assert_eq!(sync_result(&r(Some(0), &long, ""), "pull").detail.chars().count(), SYNC_OUTPUT_CAP);
    }

    #[test]
    fn head_matching_is_prefix_both_ways_but_never_on_unknown_head() {
        let full = "65e13da0000000000000000000000000000000aa";
        assert!(is_head(full, "65E13DA"));
        assert!(is_head(full, full));
        assert!(is_head("65e13da", full));
        assert!(!is_head(full, "7b37903"));
        // TS 在这里是 true（"x".startsWith("")），见 is_head 的注释
        assert!(!is_head("", "7b37903"));
    }

    #[test]
    fn js_helpers() {
        assert_eq!(code_text(None), "null");
        assert_eq!(code_text(Some(128)), "128");
        assert_eq!(first_text(&["  ", "\tb \n"], || "f".into()), "b");
        assert_eq!(first_text(&["", ""], || "f".into()), "f");
        assert_eq!(js_prefix("ab🦅c", 3), "ab\u{FFFD}");
        assert_eq!(js_prefix("abc", 10), "abc");
        assert_eq!(strip_one_newline("msg\n\n"), "msg\n");
        assert_eq!(strip_one_newline("msg\r\n"), "msg");
        assert_eq!(count_or_null(Some(0), " 12\n"), Some(12));
        assert_eq!(count_or_null(Some(0), "-1"), None);
        assert_eq!(count_or_null(Some(128), "3"), None);
        // TS 原样：Windows 上按 / 切不开，拿到整条路径
        assert_eq!(base_name(HostKind::Posix, "/a/b/repo/"), "repo");
        assert_eq!(base_name(HostKind::Windows, "D:/code/repo"), "D:\\code\\repo");
    }

    // ---------------- 执行层：假执行器 ----------------

    type Respond = Box<dyn Fn(&str) -> anyhow::Result<ExecResult>>;

    /// 按命令给出预设结果的假执行器，记下所有命令
    pub(crate) struct Script {
        pub calls: RefCell<Vec<String>>,
        pub respond: Respond,
    }

    impl Exec for Script {
        fn exec<'a>(
            &'a self,
            cmd: &'a str,
            _cancel: Option<&'a CancellationToken>,
        ) -> LocalBoxFuture<'a, anyhow::Result<ExecResult>> {
            self.calls.borrow_mut().push(cmd.to_string());
            let r = (self.respond)(cmd);
            Box::pin(async move { r })
        }
    }

    pub(crate) fn script_host(
        key: &str,
        respond: impl Fn(&str) -> anyhow::Result<ExecResult> + 'static,
    ) -> (GitHost, Rc<Script>) {
        let script = Rc::new(Script { calls: RefCell::default(), respond: Box::new(respond) });
        let host = GitHost {
            exec: script.clone(),
            kind: HostKind::Posix,
            git: "git".into(),
            home: "/home/u".into(),
            key: key.into(),
        };
        (host, script)
    }

    #[tokio::test]
    async fn exec_errors_become_link_failed_and_nonzero_codes_do_not() {
        let (host, _) = script_host("test:link", |_| Err(anyhow::anyhow!("ssh: EOF")));
        let err = path_exists(&host, "/x", &no_opts()).await.unwrap_err();
        assert_eq!(err.reason, WorktreeFailure::LinkFailed);
        assert_eq!(err.detail.as_deref(), Some("ssh: EOF"));

        let (host, _) = script_host("test:nonzero", |_| {
            Ok(ExecResult { code: Some(128), stdout: String::new(), stderr: "fatal: x\n".into() })
        });
        let res = probe_git(&host, &gc::version_args("git"), gc::GIT_ENV_RO, &no_opts()).await.unwrap();
        assert_eq!(res.code, Some(128));
        let err = run_git(&host, &gc::version_args("git"), WorktreeFailure::NotARepo, gc::GIT_ENV_RO, &no_opts())
            .await
            .unwrap_err();
        assert_eq!(err.reason, WorktreeFailure::NotARepo);
        assert_eq!(err.detail.as_deref(), Some("fatal: x"));
    }

    #[tokio::test]
    async fn timeout_ends_the_command_with_no_exit_code() {
        let host = GitHost {
            exec: Rc::new(LocalExec),
            kind: HostKind::Posix,
            git: "git".into(),
            home: "/home/u".into(),
            key: "test:timeout".into(),
        };
        let started = Instant::now();
        let res = exec_raw(&host, "sleep 5; echo late", &RunOpts::timeout(Duration::from_millis(100))).await.unwrap();
        assert_eq!(res.code, None);
        assert!(!res.stdout.contains("late"));
        assert!(started.elapsed() < Duration::from_secs(3));

        // 外部取消同样只是"命令结束"
        let token = CancellationToken::new();
        token.cancel();
        let res = exec_raw(&host, "sleep 5", &RunOpts { timeout: None, cancel: Some(token) }).await.unwrap();
        assert_eq!(res.code, None);
    }

    #[tokio::test]
    async fn paths_exist_degrades_to_unknown_when_output_is_polluted() {
        let (host, script) = script_host("test:exists", |_| {
            Ok(ExecResult { code: Some(0), stdout: "motd\nyes\n".into(), stderr: String::new() })
        });
        let got = paths_exist(&host, &["/a".into(), "/b".into()]).await.unwrap();
        assert_eq!(got, [false, false]);
        assert_eq!(script.calls.borrow().len(), 1);

        // 超过一片按 EXISTS_BATCH 分片
        let (host, script) = script_host("test:exists-chunks", |cmd| {
            let n = cmd.matches("'/p").count();
            Ok(ExecResult { code: Some(0), stdout: "yes\n".repeat(n), stderr: String::new() })
        });
        let many: Vec<String> = (0..gc::EXISTS_BATCH + 3).map(|i| format!("/p{i}")).collect();
        let got = paths_exist(&host, &many).await.unwrap();
        assert_eq!(got.len(), many.len());
        assert!(got.iter().all(|b| *b));
        assert_eq!(script.calls.borrow().len(), 2);
    }

    #[tokio::test]
    async fn repo_root_handshakes_once_then_trusts_the_cache() {
        let (host, script) = script_host("test:handshake", |cmd| {
            let out = if cmd.ends_with("'--version'") { "git version 2.48.1\n" } else { "/srv/repo\n" };
            Ok(ExecResult { code: Some(0), stdout: out.into(), stderr: String::new() })
        });
        assert_eq!(repo_root(&host, "/srv/repo/sub", &no_opts()).await.unwrap(), "/srv/repo");
        assert_eq!(repo_root(&host, "/srv/repo/sub", &no_opts()).await.unwrap(), "/srv/repo");
        {
            let calls = script.calls.borrow();
            assert_eq!(calls.iter().filter(|c| c.ends_with("'--version'")).count(), 1);
            assert_eq!(calls.len(), 3);
        }

        // 没装 git：握手失败报 git-missing，而不是 not-a-repo
        let (host, _) = script_host("test:handshake-missing", |_| {
            Ok(ExecResult { code: Some(127), stdout: String::new(), stderr: "sh: git: command not found\n".into() })
        });
        let err = repo_root(&host, "/srv", &no_opts()).await.unwrap_err();
        assert_eq!(err.reason, WorktreeFailure::GitMissing);
        assert_eq!(err.detail.as_deref(), Some("sh: git: command not found"));
    }

    #[tokio::test]
    async fn run_git_op_refusals_never_touch_the_repo() {
        // HEAD 读不到时 squash / reword 拒绝，drop 不会落到 reset --hard
        let (host, script) = script_host("test:op-head", |_| {
            Ok(ExecResult { code: Some(128), stdout: String::new(), stderr: "fatal: bad HEAD\n".into() })
        });
        let r = run_git_op(&host, "/r", &GitOpInput::Squash { sha: "65e13da".into() }, &no_opts()).await.unwrap();
        assert!(!r.ok);
        let r = run_git_op(&host, "/r", &GitOpInput::Drop { sha: "65e13da".into() }, &no_opts()).await.unwrap();
        assert!(!r.ok);
        let calls = script.calls.borrow();
        assert!(calls.iter().all(|c| !c.contains("'--hard'")));
        assert!(calls.last().unwrap().contains("'--onto'"));
    }

    // ---------------- 真 git（临时目录）----------------

    /// 本机有 git 才跑；没有就跳过（CI 镜像可能没装）
    pub(crate) fn have_git() -> bool {
        std::process::Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
    }

    pub(crate) fn local_host(key: &str) -> GitHost {
        GitHost {
            exec: Rc::new(LocalExec),
            kind: HostKind::Posix,
            git: "git".into(),
            // 删除护栏的 ⑥ 要一个家目录；测试里给一个与临时目录无关的
            home: "/nonexistent-home-for-tests".into(),
            key: key.into(),
        }
    }

    pub(crate) fn sh(dir: &Path, script: &str) {
        let out = std::process::Command::new("/bin/sh").arg("-c").arg(script).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{script}\n{}", String::from_utf8_lossy(&out.stderr));
    }

    /// 建一个有一笔提交的仓库。本地 config 关掉签名与钩子，免得用户的全局配置干扰测试
    pub(crate) fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let hooks = dir.join(".no-hooks");
        sh(
            dir,
            &format!(
                "git init -q -b main . && git config user.name t && git config user.email t@example.com \
                 && git config commit.gpgsign false && git config core.hooksPath {} \
                 && printf 'a\\n' > a.txt && git add a.txt && git commit -q -m init",
                quote(hooks.to_str().unwrap())
            ),
        );
    }

    pub(crate) fn quote(s: &str) -> String {
        crate::zellij::host::quote_posix(s)
    }

    /// 临时目录的物理路径：macOS 的 /var 是 /private/var 的符号链接，git 报的是物理路径
    pub(crate) fn real_tempdir() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = std::fs::canonicalize(t.path()).unwrap();
        (t, p)
    }

    #[tokio::test]
    async fn snapshot_working_changes_commit_and_log_on_a_real_repo() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let repo = base.join("repo");
        init_repo(&repo);
        let host = local_host("local:test-real-repo");
        let dir = repo.to_str().unwrap();
        let o = no_opts();

        sh(&repo, "printf 'a\\nb\\n' > a.txt && printf 'x\\ny\\nz\\n' > new.txt && mkdir -p sub");
        let snap = describe_git(&host, &format!("{dir}/sub"), &o).await.unwrap();
        assert!(snap.available);
        assert_eq!(snap.repo_dir.as_deref(), Some(dir));
        assert_eq!(snap.head_branch.as_deref(), Some("main"));
        assert_eq!(snap.detached, Some(false));
        assert_eq!(snap.ahead, Some(None));
        assert_eq!(snap.file_count, 2);
        assert_eq!(snap.worktrees.len(), 1);
        assert!(snap.worktrees[0].current);
        let json = serde_json::to_value(&snap).unwrap();
        assert!(json["ahead"].is_null() && json.get("upstream").is_none());

        let changes = describe_git_changes(&host, dir, &o).await.unwrap();
        assert_eq!((changes.available, changes.added, changes.deleted), (true, 2, 0));
        let many =
            describe_git_changes_many(&host, &[dir.to_string(), base.to_str().unwrap().to_string()], &o).await.unwrap();
        assert!(many[0].available && !many[1].available);

        let w = describe_working_changes(&host, dir, &o).await.unwrap();
        assert_eq!(w.repo_name.as_deref(), Some("repo"));
        assert_eq!(w.head_message.as_deref(), Some("init\n"));
        let by_path: HashMap<_, _> = w.files.iter().map(|f| (f.path.clone(), (f.added, f.deleted))).collect();
        assert_eq!(by_path["a.txt"], (Some(1), Some(0)));
        assert_eq!(by_path["new.txt"], (Some(3), Some(0)));

        let diff = describe_git_diff(
            &host,
            dir,
            &GitDiffTarget { path: "new.txt".into(), untracked: true, ..Default::default() },
            &o,
        )
        .await
        .unwrap();
        assert!(diff.diff.contains("+z"));

        // 只选 new.txt 提交：未跟踪的先 add，a.txt 的改动原样留在工作区
        let input = GitCommitInput {
            message: " add new ".into(),
            all: false,
            paths: Some(vec!["new.txt".into()]),
            amend: None,
            push: None,
        };
        let r = commit_working(&host, dir, &input, &o).await.unwrap();
        assert!(r.ok, "{}", r.detail);
        let w = describe_working_changes(&host, dir, &o).await.unwrap();
        assert_eq!(w.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt"]);
        assert_eq!(w.files[0].index, " ");

        let empty = GitCommitInput { message: "  ".into(), all: true, paths: None, amend: None, push: None };
        assert_eq!(commit_working(&host, dir, &empty, &o).await.unwrap().detail, "提交信息不能为空");

        let log = describe_git_log(&host, dir, &gc::LogQuery::default(), &o).await.unwrap();
        assert_eq!(log.commits.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["add new", "init"]);
        assert!(!log.has_more);
        assert!(log.commits[0].refs.iter().any(|r| r.name == "main" && r.head == Some(true)));
        let page =
            describe_git_log(&host, dir, &gc::LogQuery { limit: Some(1), ..Default::default() }, &o).await.unwrap();
        assert!(page.has_more);

        let detail = describe_commit(&host, dir, &log.commits[0].sha, &o).await.unwrap();
        // `show -s --format=%B` 实际以两个换行结尾（正文自带一个 + tformat 的行终止符），
        // TS 只去掉一个，于是还剩一个——照搬
        assert_eq!(detail.message, "add new\n");
        assert_eq!(detail.files.len(), 1);
        let cd = describe_commit_diff(&host, dir, &log.commits[0].sha, "new.txt", None, &o).await.unwrap();
        assert!(cd.diff.contains("+x"));
        let missing = describe_commit(&host, dir, "0000000", &o).await.unwrap_err();
        assert_eq!(missing.message, "读不到这条提交");

        let refs = describe_git_refs(&host, dir, &o).await.unwrap();
        assert_eq!(refs.me.as_deref(), Some("t"));
        assert!(refs.branches.iter().any(|b| b.name == "main" && b.head));

        // 历史面板写操作：建分支并切过去，再 reword HEAD
        let r = run_git_op(
            &host,
            dir,
            &GitOpInput::BranchCreate { name: "feat/x".into(), start_point: "main".into(), checkout: Some(true) },
            &o,
        )
        .await
        .unwrap();
        assert!(r.ok, "{}", r.detail);
        let r = run_git_op(
            &host,
            dir,
            &GitOpInput::Reword { sha: log.commits[0].sha.clone(), message: "add new.txt".into() },
            &o,
        )
        .await
        .unwrap();
        assert!(r.ok, "{}", r.detail);
        let r = run_git_op(&host, dir, &GitOpInput::Continue, &o).await.unwrap();
        assert_eq!((r.ok, r.detail.as_str()), (false, "现在没有进行中的合并或变基"));

        let files = list_repo_files(&host, dir, &o).await.unwrap().unwrap();
        assert!(files.contains(&"new.txt".to_string()));
        assert_eq!(list_repo_files(&host, base.to_str().unwrap(), &o).await.unwrap(), None);
        let not_repo = describe_git(&host, base.to_str().unwrap(), &o).await.unwrap_err();
        assert_eq!(not_repo.reason, WorktreeFailure::NotARepo);
    }

    #[tokio::test]
    async fn describe_repo_add_worktree_and_status_on_a_real_repo() {
        if !have_git() {
            return;
        }
        let (_t, base) = real_tempdir();
        let repo = base.join("repo");
        init_repo(&repo);
        sh(&repo, "git branch feat/a");
        let host = local_host("local:test-real-add");
        let dir = repo.to_str().unwrap();
        let o = no_opts();

        let info = describe_repo(&host, dir, &o).await.unwrap();
        assert_eq!(info.repo_dir.as_deref(), Some(dir));
        assert_eq!(info.head_branch.as_deref(), Some("main"));
        let feat = info.branches.iter().find(|b| b.name == "feat/a").unwrap();
        let wt = format!("{}/repo-feat-a", base.to_str().unwrap());
        assert_eq!(feat.suggested_dir, wt);
        assert!(!feat.dir_occupied);
        let main = info.branches.iter().find(|b| b.name == "main").unwrap();
        assert_eq!(main.checked_out_at.as_deref(), Some(dir));

        let add = AddWorktreeOptions {
            mode: WorktreeMode::ExistingBranch,
            branch: "feat/a".into(),
            start_point: None,
            dir: wt.clone(),
        };
        assert_eq!(add_worktree(&host, dir, &add).await.unwrap(), wt);
        // 同一分支再检出一次：git 的原话被分类成 branch-in-use
        let again = AddWorktreeOptions { dir: format!("{wt}-2"), ..add.clone() };
        assert_eq!(add_worktree(&host, dir, &again).await.unwrap_err().reason, WorktreeFailure::BranchInUse);
        let exists = AddWorktreeOptions { mode: WorktreeMode::NewBranch, dir: format!("{wt}-3"), ..add.clone() };
        assert_eq!(add_worktree(&host, dir, &exists).await.unwrap_err().reason, WorktreeFailure::BranchExists);

        let clean = worktree_status(&host, &wt, &o).await.unwrap();
        assert_eq!((clean.present, clean.dirty_count, clean.ignored_count, clean.ahead), (true, 0, 0, None));
        sh(Path::new(&wt), "printf 'x\\n' > b.txt && printf 'SECRET=1\\n' > .env && printf '.env\\n' > .gitignore");
        let dirty = worktree_status(&host, &wt, &o).await.unwrap();
        assert_eq!(dirty.dirty_count, 2);
        assert_eq!(dirty.ignored_count, 1);
        assert!(dirty.dirty_sample.contains(&"b.txt".to_string()));
        let gone = worktree_status(&host, &format!("{wt}-nope"), &o).await.unwrap();
        assert!(!gone.present);

        let info = describe_repo(&host, dir, &o).await.unwrap();
        let feat = info.branches.iter().find(|b| b.name == "feat/a").unwrap();
        assert!(feat.dir_occupied);
        assert_eq!(feat.checked_out_at.as_deref(), Some(wt.as_str()));
    }
}
