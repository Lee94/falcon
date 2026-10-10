//! 右侧 Git 面板与派生前探测的路由（routes.ts 从 `GET /api/projects/:id/repo` 到
//! `POST /api/projects/:id/git/:action` 那一段）。
//!
//! **探测端点永不因环境事实报错**：没装 git、不是仓库、没填工作目录、连不上，一律 200 +
//! available/derivable:false + reason，让前端渲染一句具体说明。写操作（commit / op / pull /
//! push）的失败同样不是 4xx——凭据不对、非快进、钩子拒绝都是仓库的正常状态，git 的原话
//! 比"操作失败"有用得多。只有请求本身造错了（缺参数、repo 不是成员）才 400。
//!
//! git 命令都在会话引擎里跑（SSH 项目复用项目链路）。客户端中途断开不影响写操作跑完：
//! 引擎里的任务不随 HTTP 处理器一起被丢掉。

use std::collections::HashMap;
use std::future::Future;
use std::rc::Rc;
use std::sync::LazyLock;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse as _;
use axum::routing::{get, post};
use falcon_proto::{
    GitChangeCounts, GitCommitDetail, GitCommitInput, GitFileDiff, GitLogPage, GitRefsInfo, GitSnapshot, GitSyncResult,
    GitUnavailableReason, GitWorkingChanges, MultiRepoProbe, MultiRepoProbeMember, RepoInfo, WorktreeFailure,
};
use regex::Regex;
use serde_json::Value;

use super::AppState;
use super::auth_routes::LenientJson;
use super::error::{ApiError, ApiResult};
use super::input::field;
use crate::db::{Db, ProjectRow};
use crate::git::command::{LogQuery, parse_git_op_input};
use crate::git::error::{WorktreeError, worktree_failure_text};
use crate::git::host::{GitHost, git_host_for, host_key_of};
use crate::git::lock::{repo_lock_key, with_repo_lock};
use crate::git::path::dirname_of;
use crate::git::repo::{
    GitDiffTarget, RunOpts, SyncAction, commit_working, describe_commit, describe_commit_diff, describe_git,
    describe_git_changes, describe_git_changes_many, describe_git_diff, describe_git_log, describe_git_refs,
    describe_repo, describe_working_changes, repo_root, run_git_op, sync_git, unavailable_changes, unavailable_commit,
    unavailable_diff, unavailable_log, unavailable_refs, unavailable_snapshot, unavailable_working,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/projects/{id}/repo", get(repo))
        .route("/api/projects/{id}/repos", get(repos))
        .route("/api/projects/{id}/git", get(snapshot))
        .route("/api/projects/{id}/git/changes", get(changes))
        .route("/api/git/changes", post(changes_many))
        .route("/api/projects/{id}/git/diff", get(diff))
        .route("/api/projects/{id}/git/working", get(working))
        .route("/api/projects/{id}/git/log", get(log))
        .route("/api/projects/{id}/git/refs", get(refs))
        .route("/api/projects/{id}/git/commit", get(commit_detail).post(commit))
        .route("/api/projects/{id}/git/commit/diff", get(commit_diff))
        .route("/api/projects/{id}/git/op", post(op))
        .route("/api/projects/{id}/git/{action}", post(sync))
}

type Q = Query<HashMap<String, String>>;

/// 探测类端点共用：把 WorktreeError 的 reason 收敛到面板认识的那四种。
/// 派生专属的失败（branch-in-use 之类）在这些端点上根本不该出现，出现了
/// 也只能当"命令没跑起来"报出去。
pub fn git_reason_of(e: &WorktreeError) -> GitUnavailableReason {
    match e.reason {
        WorktreeFailure::GitMissing => GitUnavailableReason::GitMissing,
        WorktreeFailure::NotARepo => GitUnavailableReason::NotARepo,
        WorktreeFailure::NoWorkingDir => GitUnavailableReason::NoWorkingDir,
        _ => GitUnavailableReason::LinkFailed,
    }
}

/// `e.detail ?? e.message`
pub fn detail_of(e: &WorktreeError) -> String {
    e.detail.clone().unwrap_or_else(|| e.message.clone())
}

fn no_working_dir_text() -> Option<String> {
    Some(worktree_failure_text(WorktreeFailure::NoWorkingDir).to_string())
}

fn project(state: &AppState, id: &str) -> ApiResult<ProjectRow> {
    state.db.get_project(id).ok_or_else(|| ApiError::not_found("项目不存在"))
}

/// 在项目宿主机上拿到 git 执行环境，再跑 `f`。git 探测失败（没装、连不上）原样作为 Err 交回
async fn on_host<T, F, Fut>(state: &AppState, row: ProjectRow, f: F) -> ApiResult<Result<T, WorktreeError>>
where
    T: Send + 'static,
    F: FnOnce(Rc<GitHost>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, WorktreeError>> + 'static,
{
    let res = state
        .engine
        .call(move |engine| async move {
            let host = Rc::new(git_host_for(&row, || engine.sessions.get_link(&row)).await?);
            f(host).await
        })
        .await?;
    Ok(res)
}

/// git 面板端点的目标目录
enum Target {
    Dir(String),
    BadRequest(&'static str),
    NoWorkingDir,
}

/// git 面板端点的目标目录解析。
///
/// 非多仓库行忽略 repo 参数，照旧用 working_dir。多仓库行的 working_dir 不是
/// 仓库（容器 = 可选会话 cwd，派生行 = 集中目录），git 目标是某个成员：
/// repo 给了必须**精确等于**某个成员 dir——前端从 project.multi.repos 原样带回，
/// 不需要归一化比较，不匹配就是请求造错了（400）；repo 缺省时读端点回退第一个
/// 成员（面板至少有东西看），写端点（commit / pull / push / op）拒绝——写操作不猜。
fn git_target_of(row: &ProjectRow, repo: Option<&str>, write: bool) -> Target {
    let Some(members) = Db::parse_multi_repos(row.multi_repos.as_deref()) else {
        return match &row.working_dir {
            Some(d) if !d.is_empty() => Target::Dir(d.clone()),
            _ => Target::NoWorkingDir,
        };
    };
    if let Some(repo) = repo.filter(|r| !r.is_empty()) {
        return match members.iter().find(|m| m.dir == repo) {
            Some(m) => Target::Dir(m.dir.clone()),
            None => Target::BadRequest("repo 不是该项目的成员仓库"),
        };
    }
    if write {
        return Target::BadRequest("多仓库项目必须指定 repo 参数");
    }
    match members.into_iter().next() {
        Some(m) => Target::Dir(m.dir),
        None => Target::NoWorkingDir,
    }
}

/// 读端点的目标：400 直接报错，没有工作目录交给调用方给 unavailable
fn read_target(row: &ProjectRow, q: &HashMap<String, String>) -> ApiResult<Option<String>> {
    match git_target_of(row, q.get("repo").map(String::as_str), false) {
        Target::Dir(d) => Ok(Some(d)),
        Target::BadRequest(msg) => Err(ApiError::bad_request(msg)),
        Target::NoWorkingDir => Ok(None),
    }
}

fn non_empty(q: &HashMap<String, String>, key: &str) -> Option<String> {
    q.get(key).filter(|v| !v.is_empty()).cloned()
}

fn repo_unavailable(reason: WorktreeFailure, detail: Option<String>) -> RepoInfo {
    RepoInfo {
        derivable: false,
        reason: Some(reason),
        detail,
        repo_dir: None,
        head_branch: None,
        head_sha: None,
        branches: Vec::new(),
    }
}

/// 源项目的仓库信息（派生单个附属项目之前）。同样的事实在 POST 那边才当状态冲突（409）。
async fn repo(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<RepoInfo>> {
    let row = project(&state, &id)?;
    if row.source_project_id.is_some() {
        return Err(ApiError::bad_request("附属项目不能再派生"));
    }
    if row.multi_repos.is_some() {
        return Err(ApiError::bad_request("多仓库项目请用批量派生（GET /repos）"));
    }
    let Some(wd) = row.working_dir.clone().filter(|d| !d.is_empty()) else {
        return Ok(Json(repo_unavailable(WorktreeFailure::NoWorkingDir, no_working_dir_text())));
    };
    let res = on_host(&state, row, move |host| async move { describe_repo(&host, &wd, &RunOpts::default()).await }).await?;
    Ok(Json(res.unwrap_or_else(|e| repo_unavailable(e.reason, Some(detail_of(&e))))))
}

/// 多仓库容器的派生前探测：逐成员的 RepoInfo。与 GET /repo 同一条规矩——
/// 环境事实（没装 git、不是仓库、连不上）永不 4xx，写在每个成员的
/// derivable/reason 里，前端据此逐行渲染、任一成员不可派生就禁用提交。
async fn repos(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<MultiRepoProbe>> {
    let row = project(&state, &id)?;
    let members = Db::parse_multi_repos(row.multi_repos.as_deref());
    let Some(members) = members.filter(|_| row.source_project_id.is_none()) else {
        return Err(ApiError::bad_request("不是多仓库容器"));
    };
    let dirs: Vec<String> = members.iter().map(|m| m.dir.clone()).collect();
    let probe_dirs = dirs.clone();
    let res = on_host(&state, row, move |host| async move {
        let mut out = MultiRepoProbe { members: Vec::new(), base_dir: None };
        // 成员间串行：describe_repo 内部已是批量往返，一个探测端点不值得并发压宿主机
        for dir in probe_dirs {
            let info = match describe_repo(&host, &dir, &RunOpts::default()).await {
                Ok(info) => {
                    // 集中目录默认建在第一个成员仓库根的父目录下，给前端预览用
                    if out.base_dir.is_none()
                        && let Some(repo_dir) = &info.repo_dir
                    {
                        out.base_dir = Some(dirname_of(host.kind, repo_dir));
                    }
                    info
                }
                Err(e) => repo_unavailable(e.reason, Some(detail_of(&e))),
            };
            out.members.push(MultiRepoProbeMember { dir, info });
        }
        Ok(out)
    })
    .await?;
    // 链路 / git 探测失败：所有成员同一个答案，不用逐个再试
    Ok(Json(res.unwrap_or_else(|e| MultiRepoProbe {
        members: dirs
            .into_iter()
            .map(|dir| MultiRepoProbeMember { dir, info: repo_unavailable(e.reason, Some(detail_of(&e))) })
            .collect(),
        base_dir: None,
    })))
}

/// 右侧 Git 面板的仓库快照。源项目和附属项目都能问（跟 GET /repo 不同，那边拒绝附属项目）。
async fn snapshot(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitSnapshot>> {
    let row = project(&state, &id)?;
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_snapshot(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let res = on_host(&state, row, move |host| async move { describe_git(&host, &dir, &RunOpts::default()).await }).await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_snapshot(git_reason_of(&e), Some(detail_of(&e))))))
}

/// 侧栏最后一层的 +N −M。比 GET /git 轻一个数量级（一条 status），环境事实同样不抛 4xx。
async fn changes(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult<Json<GitChangeCounts>> {
    let row = project(&state, &id)?;
    let Some(wd) = row.working_dir.clone().filter(|d| !d.is_empty()) else {
        return Ok(Json(unavailable_changes()));
    };
    let res =
        on_host(&state, row, move |host| async move { describe_git_changes(&host, &wd, &RunOpts::default()).await }).await?;
    Ok(Json(res.unwrap_or_else(|_| unavailable_changes())))
}

/// 侧栏轮询的批量版：一次拿全部项目的 +N −M。
///
/// 按宿主机分组，同主机的所有检出批成一次 exec——N 个项目逐个 GET 就是
/// N 条 SSH channel，每 8 秒一轮，弱网上会持续排队。主机之间并行互不拖累；
/// 任何一台失败只让它自己的项目 unavailable，形状与单个端点一致。
async fn changes_many(State(state): State<AppState>, body: LenientJson) -> ApiResult<axum::response::Response> {
    let ids = match field(&body.0, "ids") {
        Some(Value::Array(a)) if a.len() <= 500 && a.iter().all(Value::is_string) => {
            a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>()
        }
        _ => return Err(ApiError::bad_request("ids 必须是字符串数组")),
    };
    let mut out: Vec<(String, GitChangeCounts)> = Vec::new();
    // 保序的分组：键是宿主机标识
    let mut groups: Vec<(String, Vec<ProjectRow>)> = Vec::new();
    for id in &ids {
        // 轮询窗口里刚被删掉的项目，跳过即可
        let Some(row) = state.db.get_project(id) else { continue };
        if row.working_dir.as_deref().is_none_or(str::is_empty) {
            out.push((id.clone(), unavailable_changes()));
            continue;
        }
        let key = host_key_of(&row);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, rows)) => rows.push(row),
            None => groups.push((key, vec![row])),
        }
    }
    let counted = state
        .engine
        .call(move |engine| async move {
            let per_host = groups.into_iter().map(|(_, rows)| {
                let engine = engine.clone();
                async move {
                    let res = async {
                        let host = git_host_for(&rows[0], || engine.sessions.get_link(&rows[0])).await?;
                        let dirs: Vec<String> = rows.iter().map(|r| r.working_dir.clone().unwrap_or_default()).collect();
                        describe_git_changes_many(&host, &dirs, &RunOpts::default()).await
                    }
                    .await;
                    match res {
                        Ok(counts) => rows
                            .iter()
                            .enumerate()
                            .map(|(i, r)| (r.id.clone(), counts.get(i).cloned().unwrap_or_else(unavailable_changes)))
                            .collect::<Vec<_>>(),
                        Err(_) => rows.iter().map(|r| (r.id.clone(), unavailable_changes())).collect(),
                    }
                }
            });
            futures::future::join_all(per_host).await.into_iter().flatten().collect::<Vec<_>>()
        })
        .await?;
    out.extend(counted);
    // 手拼对象：经 serde_json::Value 走一道会把每格里的键排序，与单个端点的字段顺序对不上。
    // 外层键序照 Node 版：先是没有工作目录的（同步填的），再按主机组（这里按组序，Node 是
    // 按完成先后——客户端按 id 取值，不看顺序）；同一个 id 出现两次时后写的赢，同 JS 对象
    let mut seen: Vec<String> = Vec::new();
    let mut cells: Vec<(String, String)> = Vec::new();
    for (id, counts) in out {
        let cell = serde_json::to_string(&counts).unwrap_or_else(|_| "null".into());
        match seen.iter().position(|k| *k == id) {
            Some(i) => cells[i].1 = cell,
            None => {
                seen.push(id.clone());
                cells.push((id, cell));
            }
        }
    }
    let body = format!(
        "{{{}}}",
        cells
            .iter()
            .map(|(id, cell)| format!("{}:{cell}", serde_json::to_string(id).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join(",")
    );
    Ok(([(axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8")], body).into_response())
}

/// Git 面板里单个文件的 diff。path / origPath 由前端从快照原样带回
/// （仓库根相对路径），untracked=1 表示走 --no-index 伪 diff。
async fn diff(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitFileDiff>> {
    let row = project(&state, &id)?;
    let Some(path) = non_empty(&q, "path") else { return Err(ApiError::bad_request("缺少 path 参数")) };
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_diff(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let target = GitDiffTarget {
        path,
        orig_path: non_empty(&q, "origPath"),
        untracked: q.get("untracked").map(String::as_str) == Some("1"),
    };
    let res =
        on_host(&state, row, move |host| async move { describe_git_diff(&host, &dir, &target, &RunOpts::default()).await })
            .await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_diff(git_reason_of(&e), Some(detail_of(&e))))))
}

/// 「修改」面板：工作区里全部未提交的改动，带每个文件的 +N −M。
///
/// 比 GET /git/changes 重（多一条 numstat，未跟踪文件还要各来一条），
/// 所以那个轻量端点留给侧栏轮询，这个只在面板打开时问。
async fn working(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitWorkingChanges>> {
    let row = project(&state, &id)?;
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_working(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let res =
        on_host(&state, row, move |host| async move { describe_working_changes(&host, &dir, &RunOpts::default()).await })
            .await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_working(git_reason_of(&e), Some(detail_of(&e))))))
}

/// `Number(x)`，再按 `Number.isFinite(skip) && skip > 0 ? Math.floor(skip) : 0`
fn parse_skip(raw: Option<&String>) -> usize {
    let n = match raw.map(|s| s.trim()) {
        None => return 0,
        Some("") => 0.0,
        Some(s) => s.parse::<f64>().unwrap_or(f64::NAN),
    };
    if n.is_finite() && n > 0.0 { n.floor() as usize } else { 0 }
}

/// History 列表的一页。筛选与分页全在 query 里：branch / author / q / skip。
async fn log(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitLogPage>> {
    let row = project(&state, &id)?;
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_log(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let query = LogQuery {
        rev: non_empty(&q, "branch"),
        author: non_empty(&q, "author"),
        grep: non_empty(&q, "q"),
        skip: Some(parse_skip(q.get("skip"))),
        limit: None,
    };
    let res =
        on_host(&state, row, move |host| async move { describe_git_log(&host, &dir, &query, &RunOpts::default()).await })
            .await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_log(git_reason_of(&e), Some(detail_of(&e))))))
}

/// Branch / User 两个筛选下拉的候选值。面板挂载时取一次，不参与轮询
async fn refs(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitRefsInfo>> {
    let row = project(&state, &id)?;
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_refs(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let res = on_host(&state, row, move |host| async move { describe_git_refs(&host, &dir, &RunOpts::default()).await })
        .await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_refs(git_reason_of(&e), Some(detail_of(&e))))))
}

/// 只认十六进制：sha 要作为 rev 传给 git，形状先钉死，别指望下游转义
fn valid_sha(sha: Option<&String>) -> Option<String> {
    static SHA: LazyLock<Regex> = LazyLock::new(|| Regex::new("(?i)^[0-9a-f]{4,40}$").unwrap());
    sha.filter(|s| SHA.is_match(s)).cloned()
}

/// 选中提交的详情：完整提交信息 + 改动文件
async fn commit_detail(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitCommitDetail>> {
    let row = project(&state, &id)?;
    let Some(sha) = valid_sha(q.get("sha")) else { return Err(ApiError::bad_request("sha 参数不合法")) };
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_commit(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let res = on_host(&state, row, move |host| async move { describe_commit(&host, &dir, &sha, &RunOpts::default()).await })
        .await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_commit(git_reason_of(&e), Some(detail_of(&e))))))
}

/// 某条提交里单个文件的 diff。path / origPath 由前端从详情原样带回
async fn commit_diff(State(state): State<AppState>, Path(id): Path<String>, Query(q): Q) -> ApiResult<Json<GitFileDiff>> {
    let row = project(&state, &id)?;
    let Some(sha) = valid_sha(q.get("sha")) else { return Err(ApiError::bad_request("sha 参数不合法")) };
    let Some(path) = non_empty(&q, "path") else { return Err(ApiError::bad_request("缺少 path 参数")) };
    let Some(dir) = read_target(&row, &q)? else {
        return Ok(Json(unavailable_diff(GitUnavailableReason::NoWorkingDir, no_working_dir_text())));
    };
    let orig = non_empty(&q, "origPath");
    let res = on_host(&state, row, move |host| async move {
        describe_commit_diff(&host, &dir, &sha, &path, orig.as_deref(), &RunOpts::default()).await
    })
    .await?;
    Ok(Json(res.unwrap_or_else(|e| unavailable_diff(git_reason_of(&e), Some(detail_of(&e))))))
}

/// 写操作的目标：多仓库项目必须显式指定成员，不猜。没有工作目录是结果，不是错误
fn write_target(row: &ProjectRow, q: &HashMap<String, String>) -> ApiResult<Result<String, GitSyncResult>> {
    match git_target_of(row, q.get("repo").map(String::as_str), true) {
        Target::Dir(d) => Ok(Ok(d)),
        Target::BadRequest(msg) => Err(ApiError::bad_request(msg)),
        Target::NoWorkingDir => Ok(Err(GitSyncResult {
            ok: false,
            reason: Some(GitUnavailableReason::NoWorkingDir),
            detail: worktree_failure_text(WorktreeFailure::NoWorkingDir).into(),
        })),
    }
}

fn sync_failure(e: &WorktreeError) -> GitSyncResult {
    GitSyncResult { ok: false, reason: Some(git_reason_of(e)), detail: detail_of(e) }
}

/// 写操作的公共部分：拿仓库根，按「宿主机 + 仓库根」加锁（不是 projectId——同一个仓库
/// 完全可能挂着好几个 Project，按 id 加锁等于没加）再跑 `f`
async fn locked_write<F, Fut>(state: &AppState, row: ProjectRow, dir: String, f: F) -> ApiResult<Json<GitSyncResult>>
where
    F: FnOnce(Rc<GitHost>, String) -> Fut + Send + 'static,
    Fut: Future<Output = Result<GitSyncResult, WorktreeError>> + 'static,
{
    let res = on_host(state, row, move |host| async move {
        let root = repo_root(&host, &dir, &RunOpts::default()).await?;
        with_repo_lock(repo_lock_key(&host, &root), f(host.clone(), root)).await
    })
    .await?;
    Ok(Json(res.unwrap_or_else(|e| sync_failure(&e))))
}

/// 提交工作区改动。与 pull/push 同样两条规矩：走仓库锁，失败不是 4xx（没配 user.name、
/// pre-commit 钩子拒绝、没有可提交的改动，都是仓库的正常状态）。
async fn commit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Q,
    body: LenientJson,
) -> ApiResult<Json<GitSyncResult>> {
    let row = project(&state, &id)?;
    let b = &body.0;
    let amend = field(b, "amend") == Some(&Value::Bool(true));
    let message = field(b, "message").and_then(Value::as_str).unwrap_or("").to_string();
    if !amend && crate::term_env::js::trim(&message).is_empty() {
        return Err(ApiError::bad_request("缺少提交信息"));
    }
    let all = field(b, "all") == Some(&Value::Bool(true));
    let paths: Vec<Value> = match field(b, "paths") {
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    if !all && (paths.is_empty() || paths.iter().any(|p| p.as_str().is_none_or(str::is_empty))) {
        return Err(ApiError::bad_request("paths 必须是非空字符串数组"));
    }
    let input = GitCommitInput {
        message,
        all,
        paths: Some(paths.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()),
        amend: Some(amend),
        push: Some(field(b, "push") == Some(&Value::Bool(true))),
    };
    let dir = match write_target(&row, &q)? {
        Ok(d) => d,
        Err(res) => return Ok(Json(res)),
    };
    locked_write(&state, row, dir, move |host, root| async move {
        commit_working(&host, &root, &input, &RunOpts::default()).await
    })
    .await
}

/// 历史面板写操作：fetch / checkout / cherry-pick / revert / 建分支。规矩与 commit / pull /
/// push 相同：仓库锁、多仓库强制 repo、git 失败回 GitSyncResult 而不是 4xx。
async fn op(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Q,
    body: LenientJson,
) -> ApiResult<Json<GitSyncResult>> {
    let row = project(&state, &id)?;
    let input = parse_git_op_input(&body.0).map_err(ApiError::bad_request)?;
    let dir = match write_target(&row, &q)? {
        Ok(d) => d,
        Err(res) => return Ok(Json(res)),
    };
    locked_write(&state, row, dir, move |host, root| async move {
        run_git_op(&host, &root, &input, &RunOpts::default()).await
    })
    .await
}

/// Pull / Push。同一棵检出上并发 pull 会争 index.lock，报出来的错对用户毫无意义，所以走仓库锁。
async fn sync(
    State(state): State<AppState>,
    Path((id, action)): Path<(String, String)>,
    Query(q): Q,
) -> ApiResult<Json<GitSyncResult>> {
    let action = match action.as_str() {
        "pull" => SyncAction::Pull,
        "push" => SyncAction::Push,
        _ => return Err(ApiError::not_found("未知操作")),
    };
    let row = project(&state, &id)?;
    let dir = match write_target(&row, &q)? {
        Ok(d) => d,
        Err(res) => return Ok(Json(res)),
    };
    locked_write(&state, row, dir, move |host, root| async move {
        sync_git(&host, &root, action, &RunOpts::default()).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(working_dir: Option<&str>, multi: Option<&str>) -> ProjectRow {
        ProjectRow {
            id: "p".into(),
            project_type: "local".into(),
            working_dir: working_dir.map(str::to_string),
            multi_repos: multi.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn target_resolution_matches_node() {
        assert!(matches!(git_target_of(&row(Some("/r"), None), Some("/x"), true), Target::Dir(d) if d == "/r"));
        assert!(matches!(git_target_of(&row(None, None), None, false), Target::NoWorkingDir));
        let multi = r#"[{"dir":"/a"},{"dir":"/b"}]"#;
        assert!(matches!(git_target_of(&row(None, Some(multi)), Some("/b"), true), Target::Dir(d) if d == "/b"));
        assert!(matches!(git_target_of(&row(None, Some(multi)), None, false), Target::Dir(d) if d == "/a"));
        assert!(matches!(git_target_of(&row(None, Some(multi)), None, true), Target::BadRequest(_)));
        assert!(matches!(git_target_of(&row(None, Some(multi)), Some("/c"), false), Target::BadRequest(_)));
        assert!(matches!(git_target_of(&row(None, Some("[]")), None, false), Target::NoWorkingDir));
    }

    #[test]
    fn skip_and_sha_parsing() {
        let s = |v: &str| Some(v.to_string());
        assert_eq!(parse_skip(None), 0);
        assert_eq!(parse_skip(s("40").as_ref()), 40);
        assert_eq!(parse_skip(s("2.7").as_ref()), 2);
        assert_eq!(parse_skip(s("-3").as_ref()), 0);
        assert_eq!(parse_skip(s("abc").as_ref()), 0);
        assert_eq!(valid_sha(s("ABCDEF12").as_ref()).as_deref(), Some("ABCDEF12"));
        assert_eq!(valid_sha(s("abc").as_ref()), None);
        assert_eq!(valid_sha(s("abcd;rm").as_ref()), None);
    }
}
